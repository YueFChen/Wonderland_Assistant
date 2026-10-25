//! One bounded activation queue shared by every execution entry point.
use super::*;
use crate::host_services::HostInvocation;
use std::collections::VecDeque;
use std::sync::Condvar;

struct Attempt {
    plugin_id: String,
    revision: u64,
    reason: String,
    background: bool,
    cancelled: AtomicBool,
    result: Mutex<Option<Result<Arc<PluginProcess>, PluginError>>>,
    completed: Condvar,
    _work: PluginWorkGuard,
}

#[derive(Default)]
struct Queue {
    attempts: BTreeMap<String, Arc<Attempt>>,
    pending: VecDeque<Arc<Attempt>>,
    workers: usize,
    closed: bool,
}

#[derive(Default)]
pub(super) struct ActivationCoordinator {
    queue: Mutex<Queue>,
    waiters: Mutex<BTreeMap<(String, String), HostInvocation>>,
}

impl ActivationCoordinator {
    pub(super) fn pending_ids(&self) -> BTreeSet<String> {
        self.queue
            .lock()
            .map(|queue| queue.attempts.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub(super) fn activate(
        self: &Arc<Self>,
        manager: &Arc<PluginManagerInner>,
        id: &str,
        reason: &str,
        context: &HostInvocation,
        retry: bool,
    ) -> Result<Arc<PluginProcess>, PluginError> {
        context.check()?;
        let work = manager.begin_work()?;
        let mut queue = self
            .queue
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Activation queue is unavailable."))?;
        if queue.closed {
            return Err(plugin_error("SERVICE_UNAVAILABLE", "Core is stopping."));
        }
        let revision = {
            let mut state = manager
                .state
                .lock()
                .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
            let record = state.plugins.get_mut(id).ok_or_else(|| not_installed(id))?;
            if record.snapshot.installation == InstallationState::Incompatible {
                return Err(plugin_error(
                    "INCOMPATIBLE_CORE",
                    "Plugin is incompatible with this Core.",
                ));
            }
            if record.snapshot.installation != InstallationState::Installed {
                return Err(not_installed(id));
            }
            if !record.snapshot.enabled {
                return Err(plugin_error("DISABLED", "Plugin is disabled."));
            }
            if record.snapshot.runtime == RuntimeState::Running
                && let Some(process) = &record.process
                && process.is_alive()
            {
                return Ok(process.clone());
            }
            if record.snapshot.runtime == RuntimeState::Stopping {
                return Err(plugin_error("PLUGIN_BUSY", "Plugin is stopping."));
            }
            if record.snapshot.runtime == RuntimeState::Failed {
                if !retry {
                    return Err(record.snapshot.last_error.as_ref().map_or_else(
                        || {
                            plugin_error(
                                "PLUGIN_START_FAILED",
                                "Plugin failed; explicitly retry to start it.",
                            )
                        },
                        |failure| {
                            plugin_error(
                                &failure.code,
                                &format!("{} Retry the plugin explicitly.", failure.message),
                            )
                        },
                    ));
                }
                record.snapshot.runtime = RuntimeState::Stopped;
                record.snapshot.last_error = None;
            }
            record.revision
        };
        let background = matches!(reason, "background_restore" | "startup" | "scan_restore");
        let attempt = if let Some(existing) = queue.attempts.get(id).cloned() {
            // A real usage can advance a queued restoration without creating a second instance.
            if !background
                && existing.background
                && let Some(index) = queue
                    .pending
                    .iter()
                    .position(|item| Arc::ptr_eq(item, &existing))
            {
                queue.pending.remove(index);
                queue.pending.push_front(existing.clone());
            }
            existing
        } else {
            let attempt = Arc::new(Attempt {
                plugin_id: id.into(),
                revision,
                reason: reason.into(),
                background,
                cancelled: AtomicBool::new(false),
                result: Mutex::new(None),
                completed: Condvar::new(),
                _work: work,
            });
            queue.attempts.insert(id.into(), attempt.clone());
            if background {
                queue.pending.push_back(attempt.clone());
            } else {
                queue.pending.push_front(attempt.clone());
            }
            attempt
        };
        if queue.workers < 2 && !queue.pending.is_empty() {
            queue.workers += 1;
            let coordinator = self.clone();
            let weak = Arc::downgrade(manager);
            if let Err(error) = thread::Builder::new()
                .name("plugin-activation".into())
                .spawn(move || coordinator.run(weak))
            {
                queue.workers -= 1;
                // An existing worker can still drain this queue. If none remains,
                // complete every queued attempt rather than strand unrelated waiters.
                if queue.workers == 0 {
                    let pending: Vec<_> = queue.pending.drain(..).collect();
                    for pending in pending {
                        queue.attempts.remove(&pending.plugin_id);
                        *pending.result.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(Err(plugin_error(
                                "PLUGIN_START_FAILED",
                                &format!("Cannot start activation worker: {error}"),
                            )));
                        pending.completed.notify_all();
                    }
                }
            }
        }
        drop(queue);
        let mut result = attempt
            .result
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Activation result is unavailable."))?;
        loop {
            context.check()?;
            if let Some(result) = &*result {
                return result.clone();
            }
            result = attempt
                .completed
                .wait_timeout(result, Duration::from_millis(20))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    fn run(self: Arc<Self>, weak: Weak<PluginManagerInner>) {
        loop {
            let attempt = {
                let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
                if queue.closed || queue.pending.is_empty() {
                    queue.workers -= 1;
                    return;
                }
                queue.pending.pop_front().unwrap()
            };
            let result = if attempt.cancelled.load(Ordering::Acquire) {
                Err(plugin_error(
                    "CANCELLED",
                    "Plugin activation was invalidated.",
                ))
            } else if let Some(manager) = weak.upgrade() {
                manager
                    .lifecycle
                    .reason(&attempt.plugin_id, &attempt.reason);
                manager
                    .spawn_one(&attempt.plugin_id, attempt.revision)
                    .and_then(|()| {
                        if attempt.cancelled.load(Ordering::Acquire) {
                            return Err(plugin_error(
                                "CANCELLED",
                                "Plugin activation was invalidated.",
                            ));
                        }
                        manager
                            .state
                            .lock()
                            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?
                            .plugins
                            .get(&attempt.plugin_id)
                            .and_then(|r| r.process.clone())
                            .filter(|process| process.is_alive())
                            .ok_or_else(|| {
                                plugin_error("PLUGIN_CRASHED", "Plugin exited during activation.")
                            })
                    })
            } else {
                Err(plugin_error(
                    "SERVICE_UNAVAILABLE",
                    "Plugin manager was closed.",
                ))
            };
            let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            if queue
                .attempts
                .get(&attempt.plugin_id)
                .is_some_and(|active| Arc::ptr_eq(active, &attempt))
            {
                queue.attempts.remove(&attempt.plugin_id);
            }
            let mut completion = attempt.result.lock().unwrap_or_else(|e| e.into_inner());
            if completion.is_none() {
                *completion = Some(result);
            }
            attempt.completed.notify_all();
        }
    }

    pub(super) fn invalidate(&self, id: &str) {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(attempt) = queue.attempts.remove(id) {
            attempt.cancelled.store(true, Ordering::Release);
            *attempt.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(plugin_error(
                "CANCELLED",
                "Plugin state changed during activation.",
            )));
            attempt.completed.notify_all();
            queue.pending.retain(|entry| !Arc::ptr_eq(entry, &attempt));
        }
    }

    pub(super) fn close(&self) {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        queue.closed = true;
        for attempt in queue.attempts.values() {
            attempt.cancelled.store(true, Ordering::Release);
            *attempt.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(plugin_error(
                "SERVICE_UNAVAILABLE",
                "Core is stopping.",
            )));
            attempt.completed.notify_all();
        }
        queue.attempts.clear();
        queue.pending.clear();
    }

    pub(super) fn register(
        self: &Arc<Self>,
        plugin: &str,
        request: &str,
        context: HostInvocation,
    ) -> Result<WaiterGuard, PluginError> {
        let mut waiters = self
            .waiters
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin waiters are unavailable."))?;
        let key = (plugin.into(), request.into());
        if request.is_empty() || request.len() > 128 {
            return Err(plugin_error(
                "INVALID_REQUEST",
                "Request ID must contain 1 to 128 characters.",
            ));
        }
        if waiters.contains_key(&key) {
            return Err(plugin_error(
                "DUPLICATE_REQUEST",
                "Request ID is already active.",
            ));
        }
        waiters.insert(key.clone(), context);
        Ok(WaiterGuard {
            coordinator: self.clone(),
            key,
        })
    }

    pub(super) fn cancel(&self, plugin: &str, request: &str) -> bool {
        if let Ok(waiters) = self.waiters.lock()
            && let Some(context) = waiters.get(&(plugin.into(), request.into()))
        {
            context.cancel();
            return true;
        }
        false
    }
}

pub(super) struct WaiterGuard {
    coordinator: Arc<ActivationCoordinator>,
    key: (String, String),
}
impl Drop for WaiterGuard {
    fn drop(&mut self) {
        if let Ok(mut waiters) = self.coordinator.waiters.lock() {
            waiters.remove(&self.key);
        }
    }
}

impl PluginManager {
    pub(super) fn restore_previous_runtime(
        &self,
        id: &str,
        was_running: bool,
    ) -> Result<(), PluginError> {
        if self.snapshots().iter().any(|s| {
            s.manifest.id == id
                && s.enabled
                && (was_running
                    || (self.inner.desktop_host && self.inner.lifecycle.should_restore(s)))
        }) {
            self.inner.start_one(id)?;
        }
        Ok(())
    }

    pub(crate) fn register_starting_process(
        &self,
        id: &str,
        revision: u64,
        process: Arc<PluginProcess>,
    ) -> Result<(), PluginError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| plugin_error("INTERNAL", "Plugin state is unavailable."))?;
        let record = state.plugins.get_mut(id).ok_or_else(|| not_installed(id))?;
        if record.revision != revision
            || !record.snapshot.enabled
            || record.snapshot.runtime != RuntimeState::Starting
            || self.inner.shutting_down.load(Ordering::Acquire)
        {
            return Err(plugin_error(
                "CANCELLED",
                "Plugin activation was invalidated.",
            ));
        }
        record.process = Some(process);
        Ok(())
    }

    pub(crate) fn ensure_active(
        &self,
        id: &str,
        reason: &str,
        context: &HostInvocation,
        retry: bool,
    ) -> Result<Arc<PluginProcess>, PluginError> {
        self.inner
            .activation
            .activate(&self.inner, id, reason, context, retry)
    }

    pub(crate) fn retry_runtime(&self, id: &str) -> Result<Vec<PluginRuntimeState>, PluginError> {
        let _call = self.inner.lifecycle.enter_call(id)?;
        self.ensure_active(id, "retry", &HostInvocation::default(), true)?;
        Ok(self.snapshots())
    }

    pub(crate) fn prepare_ui(
        &self,
        id: &str,
        request: &str,
        retry: bool,
        context: &HostInvocation,
    ) -> Result<PluginUiLaunchInfo, PluginError> {
        let _work = self.inner.begin_work()?;
        let _target_call = self.inner.lifecycle.enter_call(id)?;
        self.plugin_ui_launch_info(id)?;
        let context = context.for_wait(None)?;
        let _wait = self
            .inner
            .activation
            .register(id, request, context.clone())?;
        self.ensure_active(id, "ui", &context, retry)?;
        context.check()?;
        self.plugin_ui_launch_info(id)
    }
}
