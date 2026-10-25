//! Durable package recovery. Business data is never part of an installation rollback.
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plugin_distribution::atomic_write;
use crate::plugin_package::ensure_real_directory;

const JOURNAL: &str = ".install-recovery.json";
const MAX_RECORD: u64 = 2 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct InstallRecovery {
    schema_version: u32,
    plugin_id: String,
    version: String,
    transaction_id: String,
    backup: Option<String>,
    previous_config: Option<String>,
    committed: bool,
    #[serde(skip)]
    root: PathBuf,
    #[serde(skip)]
    config: PathBuf,
}

fn io(error: impl std::fmt::Display) -> String {
    format!("Installation recovery: {error}")
}

fn read_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io(error)),
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(io("reparse file"));
        }
    }
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_RECORD {
        return Err(io("invalid or oversized recovery file"));
    }
    fs::read(path).map(Some).map_err(io)
}

impl InstallRecovery {
    pub(crate) fn requires_recovery(root: &Path, id: &str) -> bool {
        let bytes = match read_file(&root.join(id).join(JOURNAL)) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return false,
            Err(_) => return true,
        };
        let Ok(mut record) = serde_json::from_slice::<Self>(&bytes) else {
            return true;
        };
        record.root = root.into();
        record.plugin_id != id || record.validate().is_err() || !record.committed
    }
    pub(crate) fn prepare(
        root: &Path,
        config: &Path,
        id: &str,
        version: &str,
        transaction_id: String,
        backup: Option<String>,
    ) -> Result<Self, String> {
        let parent = root.join(id);
        ensure_real_directory(root)?;
        ensure_real_directory(&parent)?;
        if parent.join(JOURNAL).exists() {
            Self::recover_one(root, config, id)?;
        }
        let previous_config = read_file(config)?
            .map(|bytes| {
                if bytes.len() as u64 > MAX_RECORD / 2 {
                    return Err(io(
                        "manager configuration is too large for a durable recovery record",
                    ));
                }
                Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
            })
            .transpose()?;
        let record = Self {
            schema_version: 1,
            plugin_id: id.into(),
            version: version.into(),
            transaction_id,
            backup,
            previous_config,
            committed: false,
            root: root.into(),
            config: config.into(),
        };
        record.validate()?;
        record.persist()?;
        Ok(record)
    }

    fn parent(&self) -> PathBuf {
        self.root.join(&self.plugin_id)
    }
    fn path(&self) -> PathBuf {
        self.parent().join(JOURNAL)
    }
    fn validate(&self) -> Result<(), String> {
        let id = self.plugin_id.as_bytes();
        if self.schema_version != 1
            || id.is_empty()
            || id.len() > 64
            || !id[0].is_ascii_lowercase()
            || !id
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(b))
            || semver::Version::parse(&self.version).is_err()
            || self.transaction_id.is_empty()
            || !self.transaction_id.bytes().all(|b| b.is_ascii_hexdigit())
            || self
                .backup
                .as_ref()
                .is_some_and(|v| v != &format!(".backup-{}", self.transaction_id))
        {
            return Err(io("invalid transaction record; original files retained"));
        }
        ensure_real_directory(&self.root)?;
        ensure_real_directory(&self.parent())?;
        if let Some(value) = &self.previous_config {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(value)
                .map_err(io)?;
            let _: Value = serde_json::from_slice(&bytes).map_err(io)?;
        }
        Ok(())
    }

    fn persist(&self) -> Result<(), String> {
        atomic_write(&self.path(), &serde_json::to_vec(self).map_err(io)?).map_err(io)
    }

    fn restore_config(&self) -> Result<(), String> {
        let previous = self
            .previous_config
            .as_ref()
            .map(|v| {
                base64::engine::general_purpose::STANDARD
                    .decode(v)
                    .map_err(io)
            })
            .transpose()?;
        let current = read_file(&self.config)?;
        if current == previous {
            return Ok(());
        }
        let old: Value = previous
            .as_ref()
            .map(|v| serde_json::from_slice(v).map_err(io))
            .transpose()?
            .unwrap_or_else(|| serde_json::json!({"schemaVersion":4}));
        let mut next: Value = current
            .as_ref()
            .map(|v| serde_json::from_slice(v).map_err(io))
            .transpose()?
            .unwrap_or_else(|| old.clone());
        if !next.is_object() || !old.is_object() {
            return Err(io("manager configuration needs repair"));
        }
        for key in [
            "enabledPlugins",
            "activeVersions",
            "installationSources",
            "publisherIdentities",
        ] {
            if old.get(key).is_some_and(|v| !v.is_object()) {
                next[key] = old[key].clone();
            } else {
                if !next.get(key).is_some_and(Value::is_object) {
                    next[key] = serde_json::json!({});
                }
                let map = next[key].as_object_mut().unwrap();
                if let Some(value) = old.get(key).and_then(|v| v.get(&self.plugin_id)) {
                    map.insert(self.plugin_id.clone(), value.clone());
                } else {
                    map.remove(&self.plugin_id);
                }
                if map.is_empty() && old.get(key).is_none() {
                    next.as_object_mut().unwrap().remove(key);
                }
            }
        }
        if next == old {
            if let Some(bytes) = previous {
                atomic_write(&self.config, &bytes).map_err(io)?;
            } else if self.config.exists() {
                fs::remove_file(&self.config).map_err(io)?;
            }
        } else {
            atomic_write(&self.config, &serde_json::to_vec_pretty(&next).map_err(io)?)
                .map_err(io)?;
        }
        Ok(())
    }

    pub(crate) fn rollback(&self) -> Result<(), String> {
        self.validate()?;
        let destination = self.parent().join(&self.version);
        let backup = self.backup.as_ref().map(|v| self.parent().join(v));
        // With an existing version, an absent backup means the first rename never
        // ran, or a previous recovery already restored it. Do not delete that version.
        if backup.as_ref().is_none_or(|v| v.exists()) {
            if destination.exists() {
                ensure_real_directory(&destination)?;
                let retained = self
                    .parent()
                    .join(format!(".interrupted-{}", self.transaction_id));
                if retained.exists() {
                    return Err(io(
                        "retained candidate already exists; inspect it before retrying",
                    ));
                }
                fs::rename(&destination, retained).map_err(io)?;
            }
            if let Some(backup) = &backup {
                ensure_real_directory(backup)?;
                fs::rename(backup, &destination).map_err(io)?;
            }
        } else if !destination.exists() {
            return Err(io("both original and backup are missing"));
        }
        self.restore_config()?;
        fs::remove_file(self.path()).map_err(io)
    }

    pub(crate) fn commit(&mut self) -> Result<(), String> {
        self.committed = true;
        if let Err(error) = self.persist() {
            self.committed = false;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn cleanup(&self) -> Result<(), String> {
        self.validate()?;
        if let Some(backup) = &self.backup {
            let path = self.parent().join(backup);
            if path.exists() {
                ensure_real_directory(&path)?;
                fs::remove_dir_all(path).map_err(io)?;
            }
        }
        fs::remove_file(self.path()).map_err(io)
    }

    pub(crate) fn recover_one(root: &Path, config: &Path, id: &str) -> Result<(), String> {
        ensure_real_directory(root)?;
        ensure_real_directory(&root.join(id))?;
        let Some(bytes) = read_file(&root.join(id).join(JOURNAL))? else {
            return Ok(());
        };
        let mut record: Self = serde_json::from_slice(&bytes).map_err(io)?;
        if record.plugin_id != id {
            return Err(io("transaction plugin ID does not match its directory"));
        }
        record.root = root.into();
        record.config = config.into();
        record.validate()?;
        if record.committed {
            record.cleanup()
        } else {
            record.rollback()
        }
    }
}

pub(crate) fn recover_pending(root: &Path, config: &Path) -> Result<Vec<(String, String)>, String> {
    ensure_real_directory(root)?;
    let mut failures = Vec::new();
    for entry in fs::read_dir(root).map_err(io)? {
        let entry = entry.map_err(io)?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if id.starts_with('.') {
            continue;
        }
        if entry.path().join(JOURNAL).exists()
            && let Err(error) = InstallRecovery::recover_one(root, config, &id)
        {
            failures.push((id, error));
        }
    }
    Ok(failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "wla-install-recovery-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(path.join("installed/a/1.0.0")).unwrap();
            fs::create_dir_all(path.join("plugin-data/a")).unwrap();
            fs::write(path.join("installed/a/1.0.0/package"), b"original").unwrap();
            fs::write(
                path.join("plugin-data/a/business"),
                b"retained business data",
            )
            .unwrap();
            fs::write(path.join("manager.json"), serde_json::to_vec_pretty(&json!({"schemaVersion":4,"activeVersions":{"a":"1.0.0"},"enabledPlugins":{"a":true},"publisherIdentities":{"a":"original-key"},"installationSources":{"a":{"kind":"local","origin":"original"}}})).unwrap()).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    #[ignore = "Subprocess helper; started only by the crash recovery test"]
    fn crash_helper() {
        let root = PathBuf::from(std::env::var_os("WLA_INSTALL_CRASH_ROOT").expect("test root"));
        let phase = std::env::var("WLA_INSTALL_CRASH_PHASE").unwrap();
        let installed = root.join("installed");
        let mut record = InstallRecovery::prepare(
            &installed,
            &root.join("manager.json"),
            "a",
            "1.0.0",
            "face".into(),
            Some(".backup-face".into()),
        )
        .unwrap();
        if phase != "prepared" {
            fs::rename(installed.join("a/1.0.0"), installed.join("a/.backup-face")).unwrap();
            if phase != "backup" {
                fs::create_dir(installed.join("a/1.0.0")).unwrap();
                fs::write(installed.join("a/1.0.0/package"), b"replacement").unwrap();
                if phase != "activated" {
                    let mut next: Value =
                        serde_json::from_slice(&fs::read(root.join("manager.json")).unwrap())
                            .unwrap();
                    next["publisherIdentities"]["a"] = json!("replacement-key");
                    next["enabledPlugins"]["a"] = json!(false);
                    fs::write(
                        root.join("manager.json"),
                        serde_json::to_vec_pretty(&next).unwrap(),
                    )
                    .unwrap();
                    if phase == "committed" {
                        record.commit().unwrap();
                    }
                }
            }
        }
        fs::write(root.join("ready"), b"ready").unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    #[test]
    fn forced_process_exit_recovers_all_package_and_metadata_boundaries() {
        for phase in [
            "prepared",
            "backup",
            "activated",
            "preferences",
            "committed",
        ] {
            let fixture = Fixture::new();
            let original_config = fs::read(fixture.0.join("manager.json")).unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "plugin_install_recovery::tests::crash_helper",
                    "--ignored",
                    "--nocapture",
                ])
                .env("WLA_INSTALL_CRASH_ROOT", &fixture.0)
                .env("WLA_INSTALL_CRASH_PHASE", phase)
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000);
            }
            let mut child = command.spawn().unwrap();
            let started = Instant::now();
            while !fixture.0.join("ready").exists() && started.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(5));
            }
            let ready = fixture.0.join("ready").exists();
            child.kill().unwrap();
            child.wait().unwrap();
            assert!(ready, "crash helper failed at {phase}");
            assert_eq!(
                InstallRecovery::requires_recovery(&fixture.0.join("installed"), "a"),
                phase != "committed"
            );
            assert!(
                recover_pending(
                    &fixture.0.join("installed"),
                    &fixture.0.join("manager.json")
                )
                .unwrap()
                .is_empty()
            );
            assert_eq!(
                fs::read(fixture.0.join("installed/a/1.0.0/package")).unwrap(),
                if phase == "committed" {
                    b"replacement".as_slice()
                } else {
                    b"original".as_slice()
                },
                "{phase}"
            );
            if phase != "committed" {
                assert_eq!(
                    fs::read(fixture.0.join("manager.json")).unwrap(),
                    original_config,
                    "{phase}"
                );
            }
            assert_eq!(
                fs::read(fixture.0.join("plugin-data/a/business")).unwrap(),
                b"retained business data"
            );
            assert!(
                !fixture
                    .0
                    .join("installed/a/.install-recovery.json")
                    .exists()
            );
        }
    }

    #[test]
    fn recovery_preserves_other_plugin_preferences_and_rejects_path_changes() {
        let fixture = Fixture::new();
        let root = fixture.0.join("installed");
        let config = fixture.0.join("manager.json");
        let record = InstallRecovery::prepare(
            &root,
            &config,
            "a",
            "1.0.0",
            "face".into(),
            Some(".backup-face".into()),
        )
        .unwrap();
        let mut next: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        next["publisherIdentities"]["a"] = json!("changed");
        next["enabledPlugins"]["b"] = json!(false);
        fs::write(&config, serde_json::to_vec(&next).unwrap()).unwrap();
        record.rollback().unwrap();
        let restored: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        assert_eq!(restored["publisherIdentities"]["a"], "original-key");
        assert_eq!(restored["enabledPlugins"]["b"], false);
        let mut record =
            InstallRecovery::prepare(&root, &config, "a", "1.0.0", "face".into(), None).unwrap();
        record.version = "../other".into();
        record.persist().unwrap();
        assert_eq!(recover_pending(&root, &config).unwrap().len(), 1);
        assert_eq!(fs::read(root.join("a/1.0.0/package")).unwrap(), b"original");
    }
}
