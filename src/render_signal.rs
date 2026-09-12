// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::layout::PaneId;

#[derive(Debug, Default)]
pub(crate) struct RenderRequest {
    pub(crate) generic: bool,
    pub(crate) pty_sources: HashSet<PaneId>,
}

/// Coalesces render requests while retaining enough origin information for the
/// headless server to discard PTY-only updates hidden from every client.
#[derive(Debug, Default)]
pub(crate) struct RenderSignal {
    pending: AtomicBool,
    state: Mutex<RenderSignalState>,
}

#[derive(Debug, Default)]
struct RenderSignalState {
    request: RenderRequest,
    immediate_pty_sources: HashSet<PaneId>,
}

impl RenderSignal {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    pub(crate) fn request_generic(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.request.generic = true;
        self.pending.store(true, Ordering::Release);
    }

    /// Returns true when the signal becomes pending or visible PTY work joins it.
    pub(crate) fn request_pty(&self, pane_id: PaneId) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source_added = state.request.pty_sources.insert(pane_id);
        let wake_for_source = source_added && state.immediate_pty_sources.contains(&pane_id);
        let became_pending = !self.pending.swap(true, Ordering::AcqRel);
        became_pending || wake_for_source
    }

    pub(crate) fn set_immediate_pty_sources(&self, sources: HashSet<PaneId>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .immediate_pty_sources = sources;
    }

    pub(crate) fn has_immediate_work(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.request.generic
            || state
                .request
                .pty_sources
                .iter()
                .any(|pane_id| state.immediate_pty_sources.contains(pane_id))
    }

    pub(crate) fn take(&self) -> RenderRequest {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.pending.store(false, Ordering::Release);
        std::mem::take(&mut state.request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taking_a_request_clears_queries_and_rearms_the_same_source() {
        let signal = RenderSignal::new();
        let pane_id = PaneId::from_raw(10);
        signal.set_immediate_pty_sources(HashSet::from([pane_id]));
        assert!(signal.request_pty(pane_id));
        signal.request_generic();
        assert!(signal.has_immediate_work());
        let request = signal.take();
        assert!(request.generic);
        assert_eq!(request.pty_sources, HashSet::from([pane_id]));
        assert!(!signal.is_pending());
        assert!(!signal.has_immediate_work());
        assert!(signal.request_pty(pane_id));
    }

    #[test]
    fn coalesces_pty_sources_until_taken() {
        let signal = RenderSignal::new();
        let first = PaneId::from_raw(10);
        let second = PaneId::from_raw(20);

        assert!(signal.request_pty(first));
        assert!(!signal.request_pty(first));
        assert!(!signal.request_pty(second));

        let request = signal.take();
        assert!(!request.generic);
        assert_eq!(request.pty_sources, HashSet::from([first, second]));
        assert!(!signal.is_pending());
    }

    #[test]
    fn hidden_pty_sources_coalesce_to_one_wake() {
        let signal = RenderSignal::new();
        signal.set_immediate_pty_sources(HashSet::from([PaneId::from_raw(100)]));

        let wakes = (1..=50)
            .filter(|pane_id| signal.request_pty(PaneId::from_raw(*pane_id)))
            .count();

        assert_eq!(wakes, 1);
        assert!(!signal.has_immediate_work());
    }

    #[test]
    fn immediate_pty_source_wakes_pending_hidden_work() {
        let signal = RenderSignal::new();
        let hidden = PaneId::from_raw(10);
        let visible = PaneId::from_raw(20);
        signal.set_immediate_pty_sources(HashSet::from([visible]));

        assert!(signal.request_pty(hidden));
        assert!(!signal.request_pty(PaneId::from_raw(30)));
        assert!(!signal.has_immediate_work());
        assert!(signal.request_pty(visible));
        assert!(signal.has_immediate_work());
        assert!(!signal.request_pty(visible));
    }

    #[test]
    fn replacing_immediate_pty_sources_drops_stale_membership() {
        let signal = RenderSignal::new();
        let hidden = PaneId::from_raw(10);
        let old_visible = PaneId::from_raw(20);
        let new_visible = PaneId::from_raw(30);
        signal.set_immediate_pty_sources(HashSet::from([old_visible]));

        assert!(signal.request_pty(hidden));
        assert!(signal.request_pty(old_visible));
        assert!(signal.has_immediate_work());

        signal.set_immediate_pty_sources(HashSet::from([new_visible]));
        assert!(!signal.has_immediate_work());
        assert!(signal.request_pty(new_visible));
        assert!(signal.has_immediate_work());
    }

    #[test]
    fn keeps_generic_and_pty_requests_distinct() {
        let signal = RenderSignal::new();
        let pane_id = PaneId::from_raw(10);

        signal.request_generic();
        assert!(!signal.request_pty(pane_id));

        let request = signal.take();
        assert!(request.generic);
        assert_eq!(request.pty_sources, HashSet::from([pane_id]));
    }
}
