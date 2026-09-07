//! zynk fork: pause/idle coordination for the App-owned DB workers (live-handoff quiescence,
//! Codex Gate-2 round 9). A worker loop brackets every unit of work with `begin_work`/`end_work`;
//! the owner can `pause` (bounded wait until the loop is between units, after which no new unit
//! starts), `resume`, or `stop` (a dropped handle must never leave a paused loop waiting).

use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

#[derive(Default)]
struct PauseState {
    paused: bool,
    idle: bool,
    stopping: bool,
}

pub(crate) struct PauseControl {
    state: Mutex<PauseState>,
    changed: Condvar,
}

impl PauseControl {
    /// Busy until the loop reports idle (a startup phase counts as work).
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(PauseState::default()),
            changed: Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PauseState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The loop is between units of work.
    pub(crate) fn set_idle(&self) {
        self.lock().idle = true;
        self.changed.notify_all();
    }

    /// Poller variant: start a unit of work unless paused (`false` = skip this round, still idle).
    /// Atomic with `pause`: once `pause` has observed idle, this returns `false` until `resume`.
    pub(crate) fn try_begin_work(&self) -> bool {
        let mut state = self.lock();
        if state.paused || state.stopping {
            return false;
        }
        state.idle = false;
        true
    }

    /// Queue-consumer variant: block while paused, then start the unit. A stopping owner releases
    /// the wait (`stop`) so queued units DRAIN before the join instead of hanging.
    pub(crate) fn begin_work(&self) {
        let mut state = self.lock();
        while state.paused && !state.stopping {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        state.idle = false;
    }

    pub(crate) fn end_work(&self) {
        self.set_idle();
    }

    /// Stop starting new units and wait, at most `deadline`, for the loop to be between units.
    /// `true` = idle now and staying idle until `resume`; `false` = still busy (the loop keeps
    /// running the current unit; the owner keeps ownership and should `resume`).
    #[cfg_attr(not(unix), allow(dead_code))] // paused only by the Unix-only live handoff
    pub(crate) fn pause(&self, deadline: Duration) -> bool {
        let started = Instant::now();
        let mut state = self.lock();
        state.paused = true;
        while !state.idle {
            let remaining = deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return false;
            }
            let (guard, _) = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            state = guard;
        }
        true
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn resume(&self) {
        self.lock().paused = false;
        self.changed.notify_all();
    }

    /// Owner is shutting the loop down: release any paused wait so queued work drains and the join
    /// is bounded (a poller starts no further units; a consumer runs what is already queued).
    pub(crate) fn stop(&self) {
        let mut state = self.lock();
        state.stopping = true;
        state.paused = false;
        self.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_is_atomic_with_work_selection() {
        let control = PauseControl::new();
        control.set_idle();
        assert!(control.pause(Duration::from_millis(10)));
        assert!(
            !control.try_begin_work(),
            "no unit may start once pause observed idle"
        );
        control.resume();
        assert!(control.try_begin_work());
        assert!(
            !control.pause(Duration::from_millis(50)),
            "busy inside a unit"
        );
        control.end_work();
        assert!(control.pause(Duration::from_millis(50)));
        assert!(control.pause(Duration::from_millis(10)));
        control.stop();
        control.begin_work(); // a paused consumer is released to drain, not blocked forever
        assert!(
            !control.try_begin_work(),
            "a poller starts nothing once stopping"
        );
    }
}
