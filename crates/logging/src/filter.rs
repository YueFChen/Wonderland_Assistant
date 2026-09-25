//! 日志过滤指令构造与运行时级别更新。

use tracing_subscriber::reload;
use tracing_subscriber::{EnvFilter, Registry};
use wonderland_kernel::KernelError;
use wonderland_kernel::logging::LogLevel;

/// 第一方 crate 清单，值即 target 的 crate 根模块名。
pub const FIRST_PARTY_TARGETS: [&str; 9] = [
    "wonderland_kernel",
    "wonderland_account",
    "wonderland_config",
    "wonderland_mihoyo",
    "wonderland_net",
    "wonderland_logging",
    "wonderland_desktop",
    "wonderland_my_wonderland",
    "wonderland_comment_collector",
];

/// 第三方依赖的最低日志级别。
const THIRD_PARTY_LEVEL: &str = "warn";

/// 按配置级别过滤第一方 crate，并将第三方依赖限制为警告级别。
pub fn directives(level: LogLevel) -> String {
    let mut directives = String::new();
    for target in FIRST_PARTY_TARGETS {
        directives.push_str(target);
        directives.push('=');
        directives.push_str(level.as_str());
        directives.push(',');
    }
    directives.push_str(THIRD_PARTY_LEVEL);
    directives
}

/// `RUST_LOG` 的值；未设置或只剩空白时视为没设。
pub(crate) fn rust_log() -> Option<String> {
    std::env::var("RUST_LOG")
        .ok()
        .filter(|spec| !spec.trim().is_empty())
}

/// 按设置构造过滤器；`RUST_LOG` 的值由调用方传入，`None` 表示未设置。
///
/// `None` 表示未设置 `RUST_LOG`。
pub(crate) fn build(rust_log: Option<&str>, level: LogLevel) -> EnvFilter {
    match rust_log {
        Some(spec) => EnvFilter::new(spec),
        None => EnvFilter::new(directives(level)),
    }
}

/// 在线改级别的句柄：换一份 `EnvFilter` 即生效，无需重启。
pub struct FilterHandle {
    handle: reload::Handle<EnvFilter, Registry>,
    /// 构造时生效的 `RUST_LOG` 覆盖值。
    ///
    /// 初始化时读取的 `RUST_LOG` 覆盖值。
    rust_log: Option<String>,
}

impl FilterHandle {
    pub fn new(handle: reload::Handle<EnvFilter, Registry>, rust_log: Option<String>) -> Self {
        Self { handle, rust_log }
    }

    /// 使用新的级别更新过滤器；失败时保留当前过滤器。
    pub fn set_level(&self, level: LogLevel) -> Result<(), KernelError> {
        self.handle
            .reload(build(self.rust_log.as_deref(), level))
            .map_err(|error| KernelError::Other(format!("日志级别切换失败：{error}")))
    }
}

#[cfg(test)]
mod tests {
    use tracing::field::FieldSet;
    use tracing::metadata::Kind;
    use tracing::subscriber::Interest;
    use tracing::{Callsite, Level, Metadata};
    use tracing_subscriber::{Layer, Registry};

    use super::*;
    static CALLSITE: EventCallsite = EventCallsite;

    struct EventCallsite;

    impl Callsite for EventCallsite {
        fn set_interest(&self, _: Interest) {}

        fn metadata(&self) -> &Metadata<'_> {
            unreachable!("仅用于构造 Metadata 的标识")
        }
    }
    fn interest(filter: &EnvFilter, target: &'static str, level: Level) -> Interest {
        let metadata: &'static Metadata<'static> = Box::leak(Box::new(Metadata::new(
            "event",
            target,
            level,
            None,
            None,
            Some("tests"),
            FieldSet::new(&["message"], tracing::callsite::Identifier(&CALLSITE)),
            Kind::EVENT,
        )));
        <EnvFilter as Layer<Registry>>::register_callsite(filter, metadata)
    }

    fn enabled(filter: &EnvFilter, target: &'static str, level: Level) -> bool {
        interest(filter, target, level).is_always()
    }

    #[test]
    fn directives_cover_every_first_party_crate() {
        let spec = directives(LogLevel::Info);
        for target in FIRST_PARTY_TARGETS {
            assert!(
                spec.contains(&format!("{target}=info,")),
                "第一方清单里的 crate 必须出现在指令里：{target}"
            );
        }
        assert!(spec.ends_with(THIRD_PARTY_LEVEL));

        let filter = build(None, LogLevel::Info);
        for target in FIRST_PARTY_TARGETS {
            assert!(
                enabled(&filter, target, Level::INFO),
                "{target} 的 info 应当记录"
            );
            assert!(
                interest(&filter, target, Level::DEBUG).is_never(),
                "{target} 的 debug 应当被配置级别挡住"
            );
        }
    }

    #[test]
    fn third_party_is_capped_at_warn() {
        let filter = build(None, LogLevel::Trace);
        assert!(enabled(&filter, "wonderland_net", Level::TRACE));
        assert!(enabled(&filter, "reqwest::blocking", Level::WARN));
        assert!(interest(&filter, "reqwest::blocking", Level::INFO).is_never());
        assert!(interest(&filter, "wry", Level::DEBUG).is_never());
    }

    #[test]
    fn per_target_override_amplifies_one_crate() {
        let filter = build(
            Some(&format!(
                "{},wonderland_comment_collector=debug",
                directives(LogLevel::Info)
            )),
            LogLevel::Info,
        );
        assert!(enabled(
            &filter,
            "wonderland_comment_collector::bbs",
            Level::DEBUG
        ));
        assert!(interest(&filter, "wonderland_net", Level::DEBUG).is_never());
        assert!(enabled(&filter, "wonderland_net", Level::INFO));
    }

    #[test]
    fn rust_log_takes_over() {
        let filter = build(Some("wonderland_net=trace"), LogLevel::Error);
        assert!(enabled(&filter, "wonderland_net", Level::TRACE));
        assert!(interest(&filter, "wonderland_account", Level::ERROR).is_never());
    }
}
