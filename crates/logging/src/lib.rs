//! 日志订阅器、文件轮转、输出脱敏与运行时过滤实现。

mod filter;
mod redact;

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::{fmt, registry, reload};
use wonderland_kernel::KernelError;
use wonderland_kernel::logging::{
    LOG_FILE_PREFIX, LOG_RETAIN_DAYS, LogLevel, LogSettings, error, log_dir,
};

/// 非阻塞写入队列的最大行数。
const BUFFERED_LINES_LIMIT: usize = 8_192;

/// 已安装的日志系统：改级别句柄 + 后台写入守卫。
///
/// 由宿主在启动时初始化，并在退出时关闭。
pub struct Logging {
    filter: filter::FilterHandle,
    /// 后台写入守卫。
    guard: Mutex<Option<WorkerGuard>>,
}

impl Logging {
    /// 即时改级别（设置页用），无需重启。
    pub fn set_level(&self, level: LogLevel) -> Result<(), KernelError> {
        self.filter.set_level(level)
    }

    /// **排空并停止**后台写入；只在退出路径调用。
    ///
    /// 排空待写日志并停止后台写入线程。仅在应用退出时调用。
    pub fn shutdown(&self) {
        if let Ok(mut guard) = self.guard.lock() {
            drop(guard.take());
        }
    }
}

/// 安装日志系统，返回供宿主持有的 [`Logging`]。
///
/// 安装全局 subscriber；重复安装或初始化失败时返回 [`KernelError`]。
pub fn init(settings: LogSettings, app_data_dir: &Path) -> Result<Logging, KernelError> {
    init_with(settings, app_data_dir, filter::rust_log())
}

/// 使用指定的 `RUST_LOG` 值初始化日志系统；`None` 表示未设置。
fn init_with(
    settings: LogSettings,
    app_data_dir: &Path,
    rust_log: Option<String>,
) -> Result<Logging, KernelError> {
    let dir = log_dir(app_data_dir);
    fs::create_dir_all(&dir).map_err(|error| init_error(&dir, error))?;

    let appender = file_appender(&dir)?;

    let (writer, guard) = NonBlockingBuilder::default()
        .lossy(true)
        .buffered_lines_limit(BUFFERED_LINES_LIMIT)
        .finish(appender);

    let (filter_layer, handle) =
        reload::Layer::new(filter::build(rust_log.as_deref(), settings.level));

    let file_layer = fmt::layer()
        .with_ansi(false)
        .with_target(true)
        .with_writer(redact::RedactingMakeWriter::new(writer));

    let subscriber = registry().with(filter_layer).with(file_layer);

    #[cfg(debug_assertions)]
    let subscriber = subscriber.with(
        fmt::layer()
            .with_ansi(true)
            .with_target(true)
            .with_writer(redact::RedactingMakeWriter::new(std::io::stderr)),
    );

    tracing::subscriber::set_global_default(subscriber)
        .map_err(|error| KernelError::Other(format!("日志系统已初始化过：{error}")))?;

    install_panic_hook();

    Ok(Logging {
        filter: filter::FilterHandle::new(handle, rust_log),
        guard: Mutex::new(Some(guard)),
    })
}

/// 创建按天轮转的日志文件 appender。
fn file_appender(dir: &Path) -> Result<RollingFileAppender, KernelError> {
    RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .max_log_files(LOG_RETAIN_DAYS + 1)
        .build(dir)
        .map_err(|error| init_error(dir, error))
}

/// 装 panic hook：先落一条 `error`，再调用原 hook，保留默认的 stderr 输出与 unwind 行为。
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|location| location.to_string())
            .unwrap_or_else(|| "未知位置".to_owned());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic 载荷不是字符串".to_owned());
        error!(location = %location, payload = %payload, "panic");
        previous(info);
    }));
}

/// 初始化失败的统一出口：带上目录，便于「日志本身不可用」时也能看懂原因。
fn init_error(dir: &Path, error: impl std::fmt::Display) -> KernelError {
    KernelError::Other(format!("日志目录 {} 不可用：{error}", dir.display()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use wonderland_kernel::logging::{debug, info, warn};

    use super::*;

    fn temp_root() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        std::env::temp_dir().join(format!(
            "wonderland-logging-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }
    fn read_log(app_data_dir: &Path) -> String {
        let files = log_files(app_data_dir);
        assert_eq!(files.len(), 1, "按天轮转应当只有一个当天文件：{files:?}");
        fs::read_to_string(&files[0]).unwrap()
    }
    fn log_files(app_data_dir: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(log_dir(app_data_dir))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(LOG_FILE_PREFIX))
            })
            .collect();
        files.sort();
        files
    }
    #[test]
    fn retention_prunes_old_files_and_leaves_neighbours_alone() {
        let root = temp_root();
        let dir = log_dir(&root);
        fs::create_dir_all(&dir).unwrap();
        for day in 1..=20 {
            fs::write(
                dir.join(format!("{LOG_FILE_PREFIX}.2026-01-{day:02}")),
                b"old\n",
            )
            .unwrap();
        }
        fs::write(dir.join("settings.json"), b"{}").unwrap();
        fs::write(dir.join("theme.png"), b"png").unwrap();

        let appender = file_appender(&dir).unwrap();
        drop(appender);

        let files = log_files(&root);
        assert!(
            files.len() <= LOG_RETAIN_DAYS + 1,
            "保留上限是 {} 个文件，实际留下 {files:?}",
            LOG_RETAIN_DAYS + 1
        );
        assert!(
            files.iter().any(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.len() > LOG_FILE_PREFIX.len() + 1)
            }),
            "应当建出当天的日志文件：{files:?}"
        );
        assert!(dir.join("settings.json").is_file());
        assert!(dir.join("theme.png").is_file());

        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn init_writes_redacted_lines_and_level_reloads() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();

        let logging = init_with(
            LogSettings {
                level: LogLevel::Debug,
            },
            &root,
            None,
        )
        .unwrap();

        info!(target: "wonderland_desktop", "启动引导完成");
        debug!(target: "wonderland_net", "翻页 page=2");
        warn!(target: "wonderland_account", "捕获凭据 cookie_token=secret-value ltoken=top-secret");
        {
            let span = tracing::info_span!(
                "collect",
                plugin = "comment_collector",
                level_id = %"7257762194"
            );
            let _entered = span.enter();
            info!(target: "wonderland_comment_collector", "翻页 page=3");
        }
        debug!(
            target: "wonderland_comment_collector",
            code = "connection",
            attempt = 3,
            "翻页失败，退避后重试"
        );
        warn!(
            target: "wonderland_comment_collector",
            code = "connection",
            page = 3,
            attempts = 3,
            "翻页失败，采集中止"
        );
        warn!(
            target: "wonderland_desktop",
            command = "comment_collect",
            code = "connection",
            "命令失败"
        );
        logging.set_level(LogLevel::Error).unwrap();
        info!(target: "wonderland_desktop", "reload 之后不该出现");
        error!(target: "wonderland_net", "reload 之后仍要出现");

        logging.shutdown();
        let contents = read_log(&root);

        assert!(contents.contains("启动引导完成"));
        assert!(contents.contains("翻页 page=2"));
        assert!(
            contents.contains("collect{plugin=\"comment_collector\" level_id=7257762194}"),
            "span 字段应渲染在事件行里：{contents}"
        );
        let failures: Vec<&str> = contents
            .lines()
            .filter(|line| line.contains("code=\"connection\""))
            .collect();
        assert_eq!(failures.len(), 3, "三条失败都应落盘：{contents}");
        assert!(
            failures[0].contains("wonderland_comment_collector")
                && failures[0].contains("attempt=3"),
            "第一跳要能看出第几次请求失败：{}",
            failures[0]
        );
        assert!(
            failures[1].contains("wonderland_comment_collector")
                && failures[1].contains("page=3")
                && failures[1].contains("attempts=3"),
            "第二跳要能看出是哪个插件的哪一页、总共试了几次：{}",
            failures[1]
        );
        assert!(
            failures[2].contains("command=\"comment_collect\""),
            "最后一跳要能看出是哪个命令：{}",
            failures[2]
        );
        let stamps: Vec<&str> = failures
            .iter()
            .map(|line| line.split(' ').next().unwrap_or_default())
            .collect();
        assert!(
            stamps.windows(2).all(|pair| pair[0] <= pair[1]),
            "时序应当单调：{stamps:?}"
        );
        assert!(!contents.contains("secret-value"));
        assert!(!contents.contains("top-secret"));
        assert!(contents.contains("cookie_token=***"));
        assert!(contents.contains("ltoken=***"));
        assert!(!contents.contains("reload 之后不该出现"));
        assert!(contents.contains("reload 之后仍要出现"));
        assert!(init(LogSettings::default(), &root).is_err());

        fs::remove_dir_all(root).unwrap();
    }
}
