use super::*;
use crate::remote_interaction::RemoteBroker;

/// Tests own one resolved child of the OS temporary directory. Cleanup never
/// follows a computed target outside that child.
struct TestDirectory {
    base: PathBuf,
    path: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let base = fs::canonicalize(std::env::temp_dir()).unwrap();
        let token = crate::web_access::random_key().unwrap();
        let path = base.join(format!("wonderland-stream-test-{token}"));
        fs::create_dir(&path).unwrap();
        Self { base, path }
    }

    fn temporary_files(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".wonderland-")
            })
            .collect()
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if let Ok(path) = fs::canonicalize(&self.path)
            && path.parent() == Some(self.base.as_path())
            && path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with("wonderland-stream-test-")
            })
        {
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn invocation() -> HostInvocation {
    HostInvocation::for_call(None, None, None).unwrap()
}

fn write_chunk(streams: &ByteStreams, id: &str, bytes: &[u8], caller: &HostInvocation) {
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    streams.write(id, &encoded, caller).unwrap();
}

#[tokio::test]
async fn files_round_trip_above_the_old_total_limit_using_bounded_chunks() {
    let directory = TestDirectory::new();
    let streams = ByteStreams::default();
    let caller = invocation();
    // Over the removed 20/128 MiB limits, while every wire payload stays
    // within one stream chunk. A tail exercises non-aligned final reads.
    let total = 129 * STREAM_CHUNK_BYTES + 123;
    let destination = directory.path.join("large.bin");
    let writer = streams
        .open_write(
            &directory.path,
            WriteTarget::Path {
                destination: destination.clone(),
                mode: FileWriteMode::Create,
            },
            &caller,
        )
        .unwrap();
    let mut position = 0;
    while position < total {
        let end = (position + STREAM_CHUNK_BYTES).min(total);
        let bytes: Vec<_> = (position..end).map(|index| (index % 251) as u8).collect();
        write_chunk(&streams, &writer.stream_id, &bytes, &caller);
        position = end;
    }
    let result = streams.finish(&writer.stream_id, &caller).unwrap();
    assert_eq!(result["size"].as_u64(), Some(total as u64));
    assert_eq!(fs::metadata(&destination).unwrap().len(), total as u64);
    assert!(directory.temporary_files().is_empty());

    let reader = streams.open_read(&destination, &caller).unwrap();
    assert_eq!(reader.size, Some(total as u64));
    let mut position = 0;
    loop {
        let chunk = streams
            .read(&reader.stream_id, STREAM_CHUNK_BYTES, &caller)
            .await
            .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(chunk.content_base64)
            .unwrap();
        assert!(bytes.len() <= STREAM_CHUNK_BYTES);
        for (offset, byte) in bytes.iter().enumerate() {
            assert_eq!(*byte, ((position + offset) % 251) as u8);
        }
        position += bytes.len();
        if chunk.eof {
            assert!(bytes.is_empty());
            break;
        }
        assert!(!bytes.is_empty());
    }
    assert_eq!(position, total);
    assert_eq!(
        streams
            .read(&reader.stream_id, STREAM_CHUNK_BYTES, &caller)
            .await
            .unwrap_err()
            .code,
        "INVALID_STREAM"
    );
}

#[test]
fn create_does_not_overwrite_an_existing_file_or_leave_a_temporary_file() {
    let directory = TestDirectory::new();
    let streams = ByteStreams::default();
    let caller = invocation();
    let destination = directory.path.join("existing.bin");
    fs::write(&destination, b"original bytes").unwrap();
    let writer = streams
        .open_write(
            &directory.path,
            WriteTarget::Path {
                destination: destination.clone(),
                mode: FileWriteMode::Create,
            },
            &caller,
        )
        .unwrap();
    write_chunk(&streams, &writer.stream_id, b"new bytes", &caller);
    assert_eq!(
        streams.finish(&writer.stream_id, &caller).unwrap_err().code,
        "FILE_EXISTS"
    );
    assert_eq!(fs::read(&destination).unwrap(), b"original bytes");
    assert!(streams.entries.lock().unwrap().is_empty());
    assert!(directory.temporary_files().is_empty());
}

#[test]
fn cancelled_replace_origin_preserves_the_previous_file_and_releases_its_writer() {
    let directory = TestDirectory::new();
    let streams = ByteStreams::default();
    let origin = invocation();
    let caller = invocation();
    let destination = directory.path.join("existing.bin");
    fs::write(&destination, b"original bytes").unwrap();
    let writer = streams
        .open_write(
            &directory.path,
            WriteTarget::Path {
                destination: destination.clone(),
                mode: FileWriteMode::Replace,
            },
            &origin,
        )
        .unwrap();
    write_chunk(&streams, &writer.stream_id, b"replacement bytes", &caller);
    assert_eq!(directory.temporary_files().len(), 1);
    origin.cancel();
    assert_eq!(
        streams.finish(&writer.stream_id, &caller).unwrap_err().code,
        "CANCELLED"
    );
    streams.prune();
    assert_eq!(fs::read(&destination).unwrap(), b"original bytes");
    assert!(streams.entries.lock().unwrap().is_empty());
    assert!(directory.temporary_files().is_empty());
}

#[test]
fn explicit_replace_commits_the_complete_new_file() {
    let directory = TestDirectory::new();
    let streams = ByteStreams::default();
    let caller = invocation();
    let destination = directory.path.join("existing.bin");
    fs::write(&destination, b"old").unwrap();
    let writer = streams
        .open_write(
            &directory.path,
            WriteTarget::Path {
                destination: destination.clone(),
                mode: FileWriteMode::Replace,
            },
            &caller,
        )
        .unwrap();
    write_chunk(&streams, &writer.stream_id, b"replacement ", &caller);
    write_chunk(&streams, &writer.stream_id, b"complete", &caller);
    let result = streams.finish(&writer.stream_id, &caller).unwrap();
    assert_eq!(result["size"].as_u64(), Some(20));
    assert_eq!(fs::read(&destination).unwrap(), b"replacement complete");
    assert!(streams.entries.lock().unwrap().is_empty());
    assert!(directory.temporary_files().is_empty());
}

#[tokio::test]
async fn a_stream_handle_cannot_be_used_by_another_remote_device() {
    let directory = TestDirectory::new();
    let streams = ByteStreams::default();
    let broker = Arc::new(RemoteBroker::default());
    let remote = |owner: &str| {
        HostInvocation::for_call(
            Some(Arc::new(RemoteContext::new(
                broker.clone(),
                owner.into(),
                "test_plugin".into(),
            ))),
            None,
            None,
        )
        .unwrap()
    };
    let device_a = remote("device-a");
    let device_b = remote("device-b");
    let destination = directory.path.join("device.bin");
    fs::write(&destination, b"device-a bytes").unwrap();
    let reader = streams.open_read(&destination, &device_a).unwrap();
    assert_eq!(
        streams
            .read(&reader.stream_id, STREAM_CHUNK_BYTES, &device_b)
            .await
            .unwrap_err()
            .code,
        "UNAUTHORIZED"
    );
    assert_eq!(
        streams
            .close(&reader.stream_id, &device_b)
            .unwrap_err()
            .code,
        "UNAUTHORIZED"
    );
    assert_eq!(
        streams
            .read(&reader.stream_id, STREAM_CHUNK_BYTES, &invocation())
            .await
            .unwrap_err()
            .code,
        "UNAUTHORIZED"
    );
    let another_a_call = remote("device-a");
    let chunk = streams
        .read(&reader.stream_id, STREAM_CHUNK_BYTES, &another_a_call)
        .await
        .unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(chunk.content_base64)
            .unwrap(),
        b"device-a bytes"
    );
    streams.close(&reader.stream_id, &another_a_call).unwrap();
}

#[test]
fn pruning_cancelled_origins_releases_unused_readers_and_writers_without_another_call() {
    let directory = TestDirectory::new();
    let streams = ByteStreams::default();
    let origin = invocation();
    let destination = directory.path.join("source.bin");
    fs::write(&destination, b"keep source").unwrap();
    streams.open_read(&destination, &origin).unwrap();
    let writer = streams
        .open_write(&directory.path, WriteTarget::Spool, &origin)
        .unwrap();
    write_chunk(&streams, &writer.stream_id, b"unused upload", &origin);
    assert_eq!(streams.entries.lock().unwrap().len(), 2);
    assert_eq!(directory.temporary_files().len(), 1);
    origin.cancel();
    streams.prune();
    assert!(streams.entries.lock().unwrap().is_empty());
    assert!(directory.temporary_files().is_empty());
    assert_eq!(fs::read(&destination).unwrap(), b"keep source");
}

#[tokio::test]
async fn closing_an_http_stream_interrupts_a_pending_read_without_blocking_the_registry() {
    let _proxy = crate::test_proxy::direct();
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::time::Duration;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/stalled", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut connection, _) = listener.accept().unwrap();
        connection
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&mut connection);
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
        }
        drop(reader);
        connection
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\nConnection: close\r\n\r\n")
            .unwrap();
        // No response bytes arrive. Closing the consumer must close its socket.
        let mut byte = [0; 1];
        assert_eq!(connection.read(&mut byte).unwrap(), 0);
    });
    let response = wonderland_net::HttpTransport::new()
        .send(wonderland_net::HttpRequest::new("GET", url))
        .await
        .unwrap();
    let streams = ByteStreams::default();
    let caller = invocation();
    let stream = streams.http(response.body, &caller).unwrap();
    let mut pending_read = Box::pin(streams.read(&stream.stream_id, STREAM_CHUNK_BYTES, &caller));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut pending_read)
            .await
            .is_err()
    );
    // This synchronous close would hang if the pending read held the entry mutex.
    streams.close(&stream.stream_id, &caller).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), pending_read)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "INVALID_STREAM"
    );
    assert!(streams.entries.lock().unwrap().is_empty());
    // Let reqwest's connection task observe the dropped response before joining
    // the blocking TCP fixture on a current-thread Tokio test runtime.
    tokio::task::spawn_blocking(move || server.join().unwrap())
        .await
        .unwrap();
}

#[test]
fn failed_write_discards_the_transaction_even_if_the_caller_then_attempts_finish() {
    let directory = TestDirectory::new();
    let streams = ByteStreams::default();
    let caller = invocation();
    let destination = directory.path.join("old.bin");
    fs::write(&destination, b"original complete file").unwrap();
    let writer = streams
        .open_write(
            &directory.path,
            WriteTarget::Path {
                destination: destination.clone(),
                mode: FileWriteMode::Replace,
            },
            &caller,
        )
        .unwrap();
    write_chunk(&streams, &writer.stream_id, b"incomplete prefix", &caller);
    {
        let entry = streams.entry(&writer.stream_id, &caller).unwrap();
        let mut entry = entry.lock().unwrap();
        let Content::Writer {
            file, temporary, ..
        } = &mut entry.content
        else {
            unreachable!()
        };
        // Make the next real OS write fail after the transaction already has data.
        *file = File::open(&temporary.path).unwrap();
    }
    assert!(
        streams
            .write(&writer.stream_id, "dGFpbA==", &caller)
            .is_err()
    );
    assert_eq!(
        streams.finish(&writer.stream_id, &caller).unwrap_err().code,
        "INVALID_STREAM"
    );
    assert_eq!(fs::read(destination).unwrap(), b"original complete file");
    assert!(streams.entries.lock().unwrap().is_empty());
    assert!(directory.temporary_files().is_empty());
}

#[cfg(windows)]
#[test]
fn atomic_file_commit_supports_unprefixed_long_windows_paths() {
    let directory = TestDirectory::new();
    let canonical = directory.path.to_string_lossy();
    let plain = if let Some(unc) = canonical.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        canonical
            .strip_prefix(r"\\?\")
            .unwrap_or(&canonical)
            .to_owned()
    };
    let parent = PathBuf::from(plain)
        .join("long-nested-folder-".repeat(7))
        .join("nested-".repeat(12));
    fs::create_dir_all(&parent).unwrap();
    let destination = parent.join("导出的访问码.code");
    assert!(destination.as_os_str().len() > 260);
    let streams = ByteStreams::default();
    let caller = invocation();
    for (mode, content) in [
        (FileWriteMode::Create, b"first".as_slice()),
        (FileWriteMode::Replace, b"replacement".as_slice()),
    ] {
        let writer = streams
            .open_write(
                &parent,
                WriteTarget::Path {
                    destination: destination.clone(),
                    mode,
                },
                &caller,
            )
            .unwrap();
        write_chunk(&streams, &writer.stream_id, content, &caller);
        streams.finish(&writer.stream_id, &caller).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), content);
    }
    let writer = streams
        .open_write(
            &parent,
            WriteTarget::Path {
                destination: destination.clone(),
                mode: FileWriteMode::Create,
            },
            &caller,
        )
        .unwrap();
    write_chunk(&streams, &writer.stream_id, b"must not overwrite", &caller);
    assert!(streams.finish(&writer.stream_id, &caller).is_err());
    assert_eq!(fs::read(destination).unwrap(), b"replacement");
}
