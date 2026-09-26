//! 应用设置实现
//!
//! ```text
//! <app_data>/settings.json   # 主题设置 + 背景库记录（原始文件名）+ 日志级别
//! <app_data>/theme/bg-<n>.<ext>  # 背景图原图（导入时拷贝进来）
//! ```
//!

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use wonderland_kernel::logging::LogSettings;
use wonderland_kernel::logging::warn;
use wonderland_kernel::{
    BackgroundAsset, BackgroundKind, KernelError, SettingsService, ThemeMode, ThemeSettings,
    ThemeState,
};

/// 单张背景图的最大字节数。
const MAX_BACKGROUND_BYTES: usize = 20 * 1024 * 1024;
/// 允许的图片类型：MIME → 落盘扩展名。
const IMAGE_TYPES: [(&str, &str); 5] = [
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/webp", "webp"),
    ("image/gif", "gif"),
    ("image/bmp", "bmp"),
];
/// 库内文件名的前缀；完整形态是 `bg-<序号>.<扩展名>`。
const ID_PREFIX: &str = "bg-";

/// 文件落盘的设置服务。
pub struct FsSettingsService {
    path: PathBuf,
    theme_dir: PathBuf,
    file: RwLock<SettingsFile>,
}

/// `settings.json` 的完整形态。
#[derive(Clone, Default, Serialize, Deserialize)]
struct SettingsFile {
    #[serde(default)]
    theme: ThemeSettings,
    /// 背景库记录（最近导入在前）。图片本体在 `theme/` 目录，这里只留展示用的原始文件名。
    #[serde(default)]
    backgrounds: Vec<BackgroundRecord>,
    /// 日志级别；缺失时使用默认设置。
    #[serde(default)]
    logging: LogSettings,
}

#[derive(Clone, Serialize, Deserialize)]
struct BackgroundRecord {
    id: String,
    name: String,
}

impl FsSettingsService {
    /// 从应用数据目录读取设置；文件缺失或无效时返回默认设置。
    pub fn load(app_data_dir: PathBuf) -> Self {
        let file = fs::read(app_data_dir.join("settings.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<SettingsFile>(&bytes).ok())
            .unwrap_or_default();
        Self {
            path: app_data_dir.join("settings.json"),
            theme_dir: app_data_dir.join("theme"),
            file: RwLock::new(file),
        }
    }

    fn read(&self) -> Result<SettingsFile, KernelError> {
        self.file
            .read()
            .map(|guard| guard.clone())
            .map_err(poisoned)
    }

    /// 持久化更新后的设置，并在写入成功后更新内存状态。
    fn update(
        &self,
        edit: impl FnOnce(&mut SettingsFile) -> Result<(), KernelError>,
    ) -> Result<(), KernelError> {
        let mut guard = self.file.write().map_err(poisoned)?;
        let mut next = guard.clone();
        edit(&mut next)?;
        let bytes = serde_json::to_vec_pretty(&next).map_err(|_| KernelError::InvalidResponse)?;
        write_atomic(&self.path, &bytes)?;
        *guard = next;
        Ok(())
    }

    fn asset_path(&self, id: &str) -> Result<PathBuf, KernelError> {
        if !safe_id(id) {
            return Err(KernelError::InvalidInput);
        }
        Ok(self.theme_dir.join(id))
    }

    /// 下一个可用序号：按库里已有的最大序号递增，避免覆盖。
    fn next_id(&self, extension: &str) -> Result<String, KernelError> {
        let file = self.read()?;
        let next = file
            .backgrounds
            .iter()
            .filter_map(|record| record.id.strip_prefix(ID_PREFIX))
            .filter_map(|rest| rest.split_once('.'))
            .filter_map(|(index, _)| index.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        Ok(format!("{ID_PREFIX}{next}.{extension}"))
    }

    /// 背景图的 data URL；文件缺失或类型未知时当作没有。
    fn background_url(&self, id: &str) -> Result<Option<String>, KernelError> {
        let path = self.asset_path(id)?;
        let (Ok(bytes), Some(mime)) = (fs::read(&path), mime_of(id)) else {
            return Ok(None);
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(Some(format!("data:{mime};base64,{encoded}")))
    }
}

impl SettingsService for FsSettingsService {
    fn theme(&self) -> ThemeSettings {
        self.file
            .read()
            .map(|guard| guard.theme.clone())
            .unwrap_or_default()
    }

    fn theme_state(&self) -> Result<ThemeState, KernelError> {
        let file = self.read()?;
        let background_url = match (&file.theme.background, file.theme.mode) {
            (Some(BackgroundKind::Image { id }), ThemeMode::Custom) => self.background_url(id)?,
            _ => None,
        };
        Ok(ThemeState {
            settings: file.theme,
            background_url,
        })
    }

    fn set_theme(&self, settings: ThemeSettings) -> Result<ThemeState, KernelError> {
        if settings.background_opacity > 100 {
            return Err(KernelError::InvalidInput);
        }
        match &settings.background {
            Some(BackgroundKind::Color { hex }) if !valid_hex(hex) => {
                return Err(KernelError::InvalidInput);
            }
            Some(BackgroundKind::Image { id }) if !self.asset_path(id)?.is_file() => {
                return Err(KernelError::InvalidInput);
            }
            _ => {}
        }
        self.update(|file| {
            file.theme = settings;
            Ok(())
        })?;
        self.theme_state()
    }

    fn backgrounds(&self) -> Result<Vec<BackgroundAsset>, KernelError> {
        let file = self.read()?;
        let mut assets = Vec::new();
        for record in &file.backgrounds {
            let Ok(path) = self.asset_path(&record.id) else {
                continue;
            };
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            assets.push(BackgroundAsset {
                id: record.id.clone(),
                name: record.name.clone(),
                size: meta.len(),
            });
        }
        Ok(assets)
    }

    fn background_url(&self, id: &str) -> Result<Option<String>, KernelError> {
        self.background_url(id)
    }

    fn add_background(&self, data_url: &str, name: &str) -> Result<ThemeState, KernelError> {
        let (extension, bytes) = decode_data_url(data_url)?;
        fs::create_dir_all(&self.theme_dir).map_err(storage)?;
        let id = self.next_id(&extension)?;
        write_atomic(&self.theme_dir.join(&id), &bytes)?;
        let result = self.update(|file| {
            let background_opacity = file.theme.background_opacity;
            file.backgrounds.insert(
                0,
                BackgroundRecord {
                    id: id.clone(),
                    name: display_name(name, &id),
                },
            );
            file.theme = ThemeSettings {
                mode: ThemeMode::Custom,
                background: Some(BackgroundKind::Image { id: id.clone() }),
                background_opacity,
            };
            Ok(())
        });
        if result.is_err() {
            let _ = fs::remove_file(self.theme_dir.join(&id));
        }
        result?;
        self.theme_state()
    }

    fn select_background(&self, id: &str) -> Result<ThemeState, KernelError> {
        if !self.asset_path(id)?.is_file() {
            return Err(KernelError::InvalidInput);
        }
        let id = id.to_owned();
        self.update(|file| {
            let background_opacity = file.theme.background_opacity;
            file.theme = ThemeSettings {
                mode: ThemeMode::Custom,
                background: Some(BackgroundKind::Image { id: id.clone() }),
                background_opacity,
            };
            Ok(())
        })?;
        self.theme_state()
    }

    fn remove_background(&self, id: &str) -> Result<ThemeState, KernelError> {
        let path = self.asset_path(id)?;
        let id = id.to_owned();
        self.update(|file| {
            file.backgrounds.retain(|record| record.id != id);
            let selected = matches!(
                &file.theme.background,
                Some(BackgroundKind::Image { id: current }) if *current == id
            );
            if selected {
                let background_opacity = file.theme.background_opacity;
                file.theme = match file.backgrounds.first() {
                    Some(record) => ThemeSettings {
                        mode: ThemeMode::Custom,
                        background: Some(BackgroundKind::Image {
                            id: record.id.clone(),
                        }),
                        background_opacity,
                    },
                    None => ThemeSettings {
                        mode: ThemeMode::Dark,
                        background: None,
                        background_opacity,
                    },
                };
            }
            Ok(())
        })?;
        if path.is_file() {
            fs::remove_file(&path).map_err(storage)?;
        }
        self.theme_state()
    }

    fn logging(&self) -> LogSettings {
        self.file
            .read()
            .map(|guard| guard.logging.clone())
            .unwrap_or_default()
    }

    fn set_logging(&self, settings: LogSettings) -> Result<LogSettings, KernelError> {
        self.update(|file| {
            file.logging = settings;
            Ok(())
        })?;
        Ok(self.logging())
    }
}

/// 验证 `#RRGGBB` 格式的纯色值。
fn valid_hex(hex: &str) -> bool {
    hex.len() == 7 && hex.starts_with('#') && hex[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// 库内文件名只接受 `bg-<数字>.<白名单扩展名>`：不接受任何路径分隔符。
fn safe_id(id: &str) -> bool {
    let Some((stem, _)) = id.rsplit_once('.') else {
        return false;
    };
    let Some(index) = stem.strip_prefix(ID_PREFIX) else {
        return false;
    };
    !index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit()) && mime_of(id).is_some()
}

fn mime_of(id: &str) -> Option<&'static str> {
    let extension = id.rsplit_once('.')?.1;
    IMAGE_TYPES
        .iter()
        .find(|(_, candidate)| *candidate == extension)
        .map(|(mime, _)| *mime)
}

/// 展示名取原始文件名；为空或只剩控制字符时回落到库内文件名。
fn display_name(name: &str, fallback: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .take(120)
        .collect();
    if cleaned.is_empty() {
        fallback.to_owned()
    } else {
        cleaned
    }
}

/// 解析前端读出的 `data:image/*;base64,...`，返回落盘扩展名与字节。
fn decode_data_url(data_url: &str) -> Result<(String, Vec<u8>), KernelError> {
    let (header, payload) = data_url.split_once(',').ok_or(KernelError::InvalidInput)?;
    let mime = header
        .strip_prefix("data:")
        .and_then(|rest| rest.strip_suffix(";base64"))
        .ok_or(KernelError::InvalidInput)?;
    let extension = IMAGE_TYPES
        .iter()
        .find(|(candidate, _)| *candidate == mime)
        .map(|(_, extension)| *extension)
        .ok_or(KernelError::InvalidInput)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|_| KernelError::InvalidInput)?;
    if bytes.is_empty() || bytes.len() > MAX_BACKGROUND_BYTES {
        return Err(KernelError::InvalidInput);
    }
    Ok((extension.to_owned(), bytes))
}

/// 使用临时文件替换目标文件。
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), KernelError> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(storage)?;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = path.with_extension(format!("{}.{}.tmp", std::process::id(), nonce));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(storage)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(storage(error));
    }
    drop(file);
    if let Err(error) = replace_file(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(storage(error));
    }
    Ok(())
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

/// 设置分区读写失败：带底层原因（**不含任何凭据**），并落一条 warn。
fn storage(error: impl std::fmt::Display) -> KernelError {
    warn!(reason = %error, "设置读写失败");
    KernelError::Settings(error.to_string())
}

fn poisoned<T>(_: T) -> KernelError {
    warn!("设置状态已损坏");
    KernelError::Settings("设置状态已损坏".to_owned())
}

#[cfg(test)]
mod tests {
    use wonderland_kernel::logging::LogLevel;

    use super::*;
    const PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    fn temp_dir() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        std::env::temp_dir().join(format!(
            "wonderland-config-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    fn service() -> (FsSettingsService, PathBuf) {
        let root = temp_dir();
        fs::create_dir_all(&root).unwrap();
        (FsSettingsService::load(root.clone()), root)
    }

    fn image_id(state: &ThemeState) -> String {
        match state.settings.background.clone() {
            Some(BackgroundKind::Image { id }) => id,
            other => panic!("期望图片背景，实际是 {other:?}"),
        }
    }

    #[test]
    fn defaults_to_dark_without_background() {
        let (settings, root) = service();
        assert_eq!(settings.theme().mode, ThemeMode::Dark);
        assert_eq!(settings.theme().background, None);
        assert_eq!(settings.theme_state().unwrap().background_url, None);
        assert!(settings.backgrounds().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_invalid_input() {
        let (settings, root) = service();
        let bad_color = ThemeSettings {
            mode: ThemeMode::Custom,
            background: Some(BackgroundKind::Color { hex: "red".into() }),
            background_opacity: 100,
        };
        assert_eq!(
            settings.set_theme(bad_color),
            Err(KernelError::InvalidInput)
        );
        assert_eq!(
            settings.add_background("data:image/svg+xml;base64,AAAA", "x"),
            Err(KernelError::InvalidInput)
        );
        assert_eq!(
            settings.add_background("not-a-data-url", "x"),
            Err(KernelError::InvalidInput)
        );
        assert_eq!(
            settings.remove_background("../../secret.png"),
            Err(KernelError::InvalidInput)
        );
        let missing = ThemeSettings {
            mode: ThemeMode::Custom,
            background: Some(BackgroundKind::Image {
                id: "bg-9.png".into(),
            }),
            background_opacity: 100,
        };
        assert_eq!(settings.set_theme(missing), Err(KernelError::InvalidInput));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn library_add_select_remove() {
        let (settings, root) = service();

        let first = settings.add_background(PNG, "夜景.png").unwrap();
        let first_id = image_id(&first);
        assert_eq!(first.settings.mode, ThemeMode::Custom);
        assert!(
            first
                .background_url
                .as_deref()
                .unwrap_or_default()
                .starts_with("data:image/png;base64,")
        );

        let second_bytes = base64::engine::general_purpose::STANDARD.encode(b"fake-jpeg");
        let second = settings
            .add_background(
                &format!("data:image/jpeg;base64,{second_bytes}"),
                "白昼.jpg",
            )
            .unwrap();
        let second_id = image_id(&second);
        assert_ne!(first_id, second_id);
        let assets = settings.backgrounds().unwrap();
        assert_eq!(assets.len(), 2);
        assert_eq!(assets[0].id, second_id);
        assert_eq!(assets[0].name, "白昼.jpg");
        assert!(assets[0].size > 0);
        assert_eq!(assets[1].name, "夜景.png");
        let selected = settings.select_background(&first_id).unwrap();
        assert_eq!(
            selected.settings.background,
            Some(BackgroundKind::Image {
                id: first_id.clone()
            })
        );
        assert!(settings.background_url(&first_id).unwrap().is_some());
        let after = settings.remove_background(&first_id).unwrap();
        assert_eq!(after.settings.mode, ThemeMode::Custom);
        assert_eq!(
            after.settings.background,
            Some(BackgroundKind::Image {
                id: second_id.clone()
            })
        );
        let empty = settings.remove_background(&second_id).unwrap();
        assert_eq!(empty.settings.mode, ThemeMode::Dark);
        assert_eq!(empty.settings.background, None);
        assert!(settings.backgrounds().unwrap().is_empty());
        assert_eq!(fs::read_dir(root.join("theme")).unwrap().count(), 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persists_across_reload() {
        let (settings, root) = service();
        let id = image_id(&settings.add_background(PNG, "夜景.png").unwrap());

        let reloaded = FsSettingsService::load(root.clone());
        assert_eq!(reloaded.theme().mode, ThemeMode::Custom);
        assert_eq!(reloaded.backgrounds().unwrap().len(), 1);
        assert!(reloaded.theme_state().unwrap().background_url.is_some());
        assert!(reloaded.background_url(&id).unwrap().is_some());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn blank_name_falls_back_to_id() {
        let (settings, root) = service();
        let id = image_id(&settings.add_background(PNG, "   ").unwrap());
        assert_eq!(settings.backgrounds().unwrap()[0].name, id);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn logging_level_defaults_and_persists() {
        let (settings, root) = service();
        assert_eq!(settings.logging(), LogSettings::default());

        let saved = settings
            .set_logging(LogSettings {
                level: LogLevel::Trace,
            })
            .unwrap();
        assert_eq!(saved.level, LogLevel::Trace);
        assert_eq!(settings.logging().level, LogLevel::Trace);
        let reloaded = FsSettingsService::load(root.clone());
        assert_eq!(reloaded.logging().level, LogLevel::Trace);
        let raw = fs::read_to_string(root.join("settings.json")).unwrap();
        assert!(raw.contains("\"logging\""));
        assert!(raw.contains("\"level\": \"trace\""));

        fs::remove_dir_all(root).unwrap();
    }
}
