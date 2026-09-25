use std::sync::{Arc, Mutex};

/// Tracks one in-flight long request so a protocol cancellation only reaches its owner.
#[derive(Clone, Default)]
pub struct RequestTracker(Arc<Mutex<Option<String>>>);

impl RequestTracker {
    /// Claim the single long-operation slot. The returned guard releases it on every exit path.
    pub fn begin(&self, request_id: &str) -> Option<ActiveRequestGuard> {
        if request_id.is_empty() {
            return None;
        }
        let mut active = self.0.lock().ok()?;
        if active.is_some() {
            return None;
        }
        *active = Some(request_id.to_owned());
        Some(ActiveRequestGuard {
            tracker: self.clone(),
            request_id: request_id.to_owned(),
        })
    }

    /// Whether `request_id` still owns the active long-operation slot.
    pub fn matches(&self, request_id: &str) -> bool {
        self.0
            .lock()
            .is_ok_and(|active| active.as_deref() == Some(request_id))
    }
}

pub struct ActiveRequestGuard {
    tracker: RequestTracker,
    request_id: String,
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.tracker.0.lock()
            && active.as_deref() == Some(self.request_id.as_str())
        {
            *active = None;
        }
    }
}
