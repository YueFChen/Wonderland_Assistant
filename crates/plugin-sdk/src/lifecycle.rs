use super::*;
use wonderland_plugin_protocol::ActivityHandle;

impl HostClient {
    /// Persist only this plugin's need to restore its own durable background state.
    /// Enable: report true before committing business state. Disable: commit first.
    pub fn set_background_restore(&self, required: bool) -> Result<(), PluginError> {
        let _serial = self
            .restore_updates
            .lock()
            .map_err(|_| PluginError::new("INTERNAL", "Restore reporter is unavailable."))?;
        self.call_core(
            "core.lifecycle.set_background_restore",
            json!({"required":required}),
        )?;
        Ok(())
    }

    /// Protect work that will outlive the current RPC. This does not persist a task.
    pub fn begin_activity(&self, label: &str) -> Result<ActivityGuard, PluginError> {
        let result = self.call_core("core.lifecycle.activity.begin", json!({"label":label}))?;
        let handle: ActivityHandle = serde_json::from_value(result).map_err(|_| {
            PluginError::new(
                "INVALID_RESPONSE",
                "Core returned an invalid activity handle.",
            )
        })?;
        let mut cleanup_host = self.clone();
        // Releasing a process-owned count must not reuse an expired request context.
        // The handle never grants filesystem, device or network access.
        cleanup_host.service_context = None;
        Ok(ActivityGuard {
            host: cleanup_host,
            id: Some(handle.activity_id),
        })
    }

    fn queue_activity_release(&self, id: String) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let Ok(mut cleanup) = self.lifecycle_cleanup.lock() else {
            return;
        };
        if cleanup.is_none() {
            let (sender, receiver) = mpsc::channel::<String>();
            let writer = self.writer.clone();
            let closed = self.closed.clone();
            let next_id = self.next_id.clone();
            let worker = thread::Builder::new().name("plugin-activity-cleanup".into()).spawn(move || {
                while !closed.load(Ordering::Acquire) {
                    match receiver.recv_timeout(Duration::from_millis(20)) {
                        Ok(activity_id) => {
                            let id = format!("cleanup-{}", next_id.fetch_add(1, Ordering::Relaxed));
                            let mut frame = json!({"protocol":PROTOCOL,"version":VERSION,"type":"request","id":id,
                                "method":"core.lifecycle.activity.end","params":{"activityId":activity_id}});
                            if write_service_request(&writer, &mut frame, None, &closed).is_err() { break; }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            });
            if worker.is_err() {
                return;
            }
            *cleanup = Some(sender);
        }
        if let Some(sender) = &*cleanup {
            let _ = sender.send(id);
        }
    }
}

/// Releases asynchronously on Drop; transport closure lets Core clear the whole instance.
pub struct ActivityGuard {
    host: HostClient,
    id: Option<String>,
}

impl ActivityGuard {
    /// Release and await acknowledgement when the caller needs a definite drain point.
    pub fn finish(mut self) -> Result<(), PluginError> {
        if let Some(id) = &self.id {
            self.host
                .call_core("core.lifecycle.activity.end", json!({"activityId":id}))?;
            self.id = None;
        }
        Ok(())
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            self.host.queue_activity_release(id);
        }
    }
}
