//! Request identifiers for latest-only async results.

/// Opaque request id for matching async results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(u64);

/// Tracks the latest active request and ignores stale results.
#[derive(Debug, Default)]
pub struct LatestOnly {
    next: u64,
    active: Option<RequestId>,
}

impl LatestOnly {
    /// Start a new request and mark it as active.
    pub fn begin(&mut self) -> RequestId {
        let id = RequestId(self.next);
        self.next += 1;
        self.active = Some(id);
        id
    }

    /// Cancel any active request.
    pub fn cancel(&mut self) {
        self.active = None;
    }

    /// Returns true if the provided id is still the active request.
    pub fn is_active(&self, id: RequestId) -> bool {
        self.active == Some(id)
    }

    /// Returns true if any request is active.
    pub fn has_active(&self) -> bool {
        self.active.is_some()
    }

    /// Finish the request if it's still active.
    pub fn finish_if_active(&mut self, id: RequestId) -> bool {
        if self.is_active(id) {
            self.active = None;
            true
        } else {
            false
        }
    }
}
