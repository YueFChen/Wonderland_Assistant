//! Serialize tests that change the process-wide proxy with live HTTP fixtures.
use std::sync::{Mutex, MutexGuard};
use wonderland_net::{ProxyConfiguration, ProxyMode, ProxySettings};

static LOCK: Mutex<()> = Mutex::new(());

pub(crate) struct ProxyTestGuard {
    _lock: MutexGuard<'static, ()>,
    previous: ProxyConfiguration,
}

pub(crate) fn direct() -> ProxyTestGuard {
    let lock = LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let previous = wonderland_net::proxy::current_proxy()
        .unwrap()
        .configuration;
    wonderland_net::proxy::set_current_proxy(
        ProxyConfiguration::new(
            ProxySettings {
                mode: ProxyMode::Direct,
                ..ProxySettings::default()
            },
            None,
        )
        .unwrap(),
    )
    .unwrap();
    ProxyTestGuard {
        _lock: lock,
        previous,
    }
}

impl Drop for ProxyTestGuard {
    fn drop(&mut self) {
        wonderland_net::proxy::set_current_proxy(self.previous.clone()).unwrap();
    }
}
