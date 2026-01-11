//! Request identifiers for latest-only async results.
//!
//! ## Request-ID Gating for Previews
//!
//! Thread preview uses request-id gating to avoid stale updates:
//!
//! 1. When user navigates to a thread, `preview_request.begin()` returns a `RequestId`
//! 2. The `PreviewThread` effect is emitted with this request id
//! 3. When `PreviewLoaded` arrives, reducer checks `preview_request.finish_if_active(req)`
//! 4. If the request id doesn't match (user navigated away), the result is ignored
//!
//! This is simpler than cancellation tokens for preview because:
//! - Preview is low-cost I/O, wasted work is acceptable
//! - Request-id gating is purely synchronous state checks
//! - No need to coordinate async cancellation

/// Opaque request id for matching async results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RequestId(pub(crate) u64);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_latest_only_begin_and_finish() {
        let mut tracker = LatestOnly::default();

        // Begin first request
        let req1 = tracker.begin();
        assert!(tracker.is_active(req1));
        assert!(tracker.has_active());

        // Finish it
        assert!(tracker.finish_if_active(req1));
        assert!(!tracker.has_active());

        // Can't finish again
        assert!(!tracker.finish_if_active(req1));
    }

    #[test]
    fn test_latest_only_supersedes_previous() {
        let mut tracker = LatestOnly::default();

        // Begin first request
        let req1 = tracker.begin();
        assert!(tracker.is_active(req1));

        // Begin second request - supersedes first
        let req2 = tracker.begin();
        assert!(!tracker.is_active(req1));
        assert!(tracker.is_active(req2));

        // First request can't be finished (it's stale)
        assert!(!tracker.finish_if_active(req1));

        // Second request can be finished
        assert!(tracker.finish_if_active(req2));
    }

    #[test]
    fn test_preview_gating_ignores_stale_results() {
        // Simulates the preview scenario: A->B->C selection causes stale PreviewLoaded
        let mut tracker = LatestOnly::default();

        // User selects thread A, we start preview
        let req_a = tracker.begin();

        // User quickly navigates to thread B, we start new preview
        let req_b = tracker.begin();

        // User quickly navigates to thread C, we start new preview
        let req_c = tracker.begin();

        // Preview for A completes - should be ignored (stale)
        assert!(!tracker.finish_if_active(req_a));

        // Preview for B completes - should be ignored (stale)
        assert!(!tracker.finish_if_active(req_b));

        // Preview for C completes - should succeed (current)
        assert!(tracker.finish_if_active(req_c));
    }

    #[test]
    fn test_cancel_clears_active() {
        let mut tracker = LatestOnly::default();

        let req = tracker.begin();
        assert!(tracker.has_active());

        tracker.cancel();
        assert!(!tracker.has_active());
        assert!(!tracker.is_active(req));
    }
}
