//! Convenience helpers over the single host HTTP/file byte stream contract.
use crate::{HostClient, PluginError};
use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
pub use wonderland_plugin_protocol::{
    ByteStream, FileWriteMode, HttpBody, HttpHeader, HttpRedirects, HttpRequestParams,
    HttpResponseMeta, SecretHeader, StreamChunk,
};
use wonderland_plugin_protocol::{FileResult, STREAM_CHUNK_BYTES};

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, PluginError> {
    serde_json::from_value(value).map_err(|e| PluginError::new("INVALID_RESPONSE", e.to_string()))
}

/// Local native selection for persistent sources. Available starting with Core 0.2.1.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct SelectedPaths {
    pub cancelled: bool,
    pub paths: Vec<String>,
}

// A local SDK deadline can win the race with Core's detailed error response.
// Without proof of preparation-only failure, callers must not assume safe replay.
fn interrupted_transport(mut error: PluginError, stage: &str) -> PluginError {
    if error.details.is_none()
        && matches!(
            error.code.as_str(),
            "TIMEOUT" | "CANCELLED" | "INVALID_STREAM"
        )
    {
        error.details = Some(
            json!({"stage":stage,"kind":if error.code == "TIMEOUT" { "deadline_exceeded" } else { "cancelled" },"outcomeUnknown":true}),
        );
    }
    error
}

pub(super) fn interrupted_service(method: &str, error: PluginError) -> PluginError {
    match method {
        "core.network.request" => interrupted_transport(error, "send"),
        "core.streams.read" => interrupted_transport(error, "body"),
        _ => error,
    }
}

impl HostClient {
    pub fn pick_paths(
        &self,
        directory: bool,
        extensions: &[&str],
    ) -> Result<SelectedPaths, PluginError> {
        decode(self.call_core(
            "core.files.pick_paths",
            json!({
                "kind": if directory { "directory" } else { "files" }, "extensions": extensions
            }),
        )?)
    }
    pub fn http_request(
        &self,
        request: &HttpRequestParams,
    ) -> Result<HttpResponseMeta, PluginError> {
        let params = serde_json::to_value(request)
            .map_err(|e| PluginError::new("INVALID_INPUT", e.to_string()))?;
        let response = match request.timeout_ms {
            Some(timeout_ms) => self.call_core_with_timeout(
                "core.network.request",
                params,
                std::time::Duration::from_millis(timeout_ms),
            ),
            None => self.call_core("core.network.request", params),
        }
        .map_err(|error| interrupted_transport(error, "send"))?;
        decode(response)
    }
    pub fn http_request_bytes(
        &self,
        request: &HttpRequestParams,
    ) -> Result<(HttpResponseMeta, Vec<u8>), PluginError> {
        let response = self.http_request(request)?;
        let bytes = self.read_stream(&response.body)?;
        Ok((response, bytes))
    }
    pub fn stream_read(&self, stream: &ByteStream) -> Result<StreamChunk, PluginError> {
        let chunk: StreamChunk = decode(
            self.call_core(
                "core.streams.read",
                json!({"streamId":stream.stream_id,"chunkBytes":STREAM_CHUNK_BYTES}),
            )
            .map_err(|error| interrupted_transport(error, "body"))?,
        )?;
        if chunk.content_base64.len() > STREAM_CHUNK_BYTES.div_ceil(3) * 4
            || (chunk.content_base64.is_empty() && !chunk.eof)
        {
            return Err(PluginError::new(
                "INVALID_RESPONSE",
                "Host stream chunk exceeds its frame budget or does not progress.",
            ));
        }
        Ok(chunk)
    }
    pub fn stream_close(&self, stream: &ByteStream) -> Result<(), PluginError> {
        self.call_core("core.streams.close", json!({"streamId":stream.stream_id}))?;
        Ok(())
    }
    pub fn read_stream(&self, stream: &ByteStream) -> Result<Vec<u8>, PluginError> {
        let result = (|| {
            let mut bytes = Vec::new();
            loop {
                let chunk = self.stream_read(stream)?;
                let data = base64::engine::general_purpose::STANDARD
                    .decode(chunk.content_base64)
                    .map_err(|_| {
                        PluginError::new("INVALID_RESPONSE", "Host returned invalid base64.")
                    })?;
                if data.len() > STREAM_CHUNK_BYTES {
                    return Err(PluginError::new(
                        "INVALID_RESPONSE",
                        "Host stream chunk is too large.",
                    ));
                }
                bytes
                    .try_reserve(data.len())
                    .map_err(|e| PluginError::new("RESOURCE_LIMIT", e.to_string()))?;
                bytes.extend_from_slice(&data);
                if chunk.eof {
                    return Ok(bytes);
                }
            }
        })();
        if result.is_err() {
            let _ = self.stream_close(stream);
        }
        result
    }
    /// Copy a response or file stream without collecting the complete content in memory.
    /// The caller owns the sink and any application-specific size policy.
    pub fn copy_stream_to<W: std::io::Write>(
        &self,
        stream: &ByteStream,
        output: &mut W,
    ) -> Result<u64, PluginError> {
        let result = (|| {
            let mut total = 0_u64;
            loop {
                let chunk = self.stream_read(stream)?;
                let data = base64::engine::general_purpose::STANDARD
                    .decode(chunk.content_base64)
                    .map_err(|_| {
                        PluginError::new("INVALID_RESPONSE", "Host returned invalid base64.")
                    })?;
                if data.len() > STREAM_CHUNK_BYTES {
                    return Err(PluginError::new(
                        "INVALID_RESPONSE",
                        "Host stream chunk is too large.",
                    ));
                }
                output
                    .write_all(&data)
                    .map_err(|error| PluginError::new("FILE_ERROR", error.to_string()))?;
                total = total.checked_add(data.len() as u64).ok_or_else(|| {
                    PluginError::new("RESOURCE_LIMIT", "Stream length cannot be represented.")
                })?;
                if chunk.eof {
                    return Ok(total);
                }
            }
        })();
        if result.is_err() {
            let _ = self.stream_close(stream);
        }
        result
    }
    /// Fill a file/spool writer one chunk at a time. Finish commits; close discards.
    pub fn stream_write_from<R: std::io::Read>(
        &self,
        stream: &ByteStream,
        input: &mut R,
    ) -> Result<u64, PluginError> {
        let result = (|| {
            let mut buffer = vec![0; STREAM_CHUNK_BYTES];
            let mut total = 0_u64;
            loop {
                let count = match input.read(&mut buffer) {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => {
                        result.map_err(|error| PluginError::new("FILE_ERROR", error.to_string()))?
                    }
                };
                if count == 0 {
                    return Ok(total);
                }
                self.stream_write(stream, &buffer[..count])?;
                total = total.checked_add(count as u64).ok_or_else(|| {
                    PluginError::new("RESOURCE_LIMIT", "Stream length cannot be represented.")
                })?;
            }
        })();
        if result.is_err() {
            let _ = self.stream_close(stream);
        }
        result
    }
    pub fn stream_write(&self, stream: &ByteStream, bytes: &[u8]) -> Result<(), PluginError> {
        for chunk in bytes.chunks(STREAM_CHUNK_BYTES) {
            self.call_core(
                "core.streams.write",
                json!({"streamId":stream.stream_id,
                "contentBase64":base64::engine::general_purpose::STANDARD.encode(chunk)}),
            )?;
        }
        Ok(())
    }
    pub fn export_bytes(&self, name: &str, bytes: &[u8]) -> Result<FileResult, PluginError> {
        let stream: ByteStream =
            decode(self.call_core("core.files.export_open", json!({"name":name}))?)?;
        self.finish_file(&stream, bytes)
    }
    pub fn write_file(
        &self,
        path: &str,
        mode: FileWriteMode,
        bytes: &[u8],
    ) -> Result<FileResult, PluginError> {
        let stream: ByteStream =
            decode(self.call_core("core.files.open_write", json!({"path":path,"mode":mode}))?)?;
        self.finish_file(&stream, bytes)
    }
    fn finish_file(&self, stream: &ByteStream, bytes: &[u8]) -> Result<FileResult, PluginError> {
        let result = self.stream_write(stream, bytes).and_then(|()| {
            decode(self.call_core("core.streams.finish", json!({"streamId":stream.stream_id}))?)
        });
        if result.is_err() {
            let _ = self.stream_close(stream);
        }
        result
    }
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, PluginError> {
        let stream: ByteStream =
            decode(self.call_core("core.files.open_read", json!({"path":path}))?)?;
        self.read_stream(&stream)
    }
    pub fn read_selected_file(&self, handle: &str) -> Result<Vec<u8>, PluginError> {
        let mut bytes = Vec::new();
        let mut offset = 0_u64;
        loop {
            let chunk = self.call_core(
                "core.files.read",
                json!({"handle":handle,"offset":offset,"chunkBytes":STREAM_CHUNK_BYTES}),
            )?;
            if chunk.get("offset").and_then(Value::as_u64) != Some(offset) {
                return Err(PluginError::new(
                    "INVALID_RESPONSE",
                    "Selected-file offset does not match.",
                ));
            }
            let encoded = chunk
                .get("contentBase64")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    PluginError::new("INVALID_RESPONSE", "Selected-file bytes are missing.")
                })?;
            let data = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|_| {
                    PluginError::new("INVALID_RESPONSE", "Selected-file bytes are invalid.")
                })?;
            let eof = chunk.get("eof").and_then(Value::as_bool).ok_or_else(|| {
                PluginError::new("INVALID_RESPONSE", "Selected-file EOF is missing.")
            })?;
            if data.len() > STREAM_CHUNK_BYTES || (data.is_empty() && !eof) {
                return Err(PluginError::new(
                    "INVALID_RESPONSE",
                    "Selected-file chunk does not progress.",
                ));
            }
            offset = offset.checked_add(data.len() as u64).ok_or_else(|| {
                PluginError::new("INVALID_RESPONSE", "Selected-file offset overflow.")
            })?;
            bytes
                .try_reserve(data.len())
                .map_err(|e| PluginError::new("RESOURCE_LIMIT", e.to_string()))?;
            bytes.extend_from_slice(&data);
            if eof {
                return Ok(bytes);
            }
        }
    }
}
