//! zynk fork: pause/idle coordination for the App-owned DB workers (live-handoff quiescence,
//! Codex Gate-2 rounds 9-11). A worker loop brackets every unit of work with `begin_work`/`end_work`;
//! a queue-fed worker also accounts for units it has ACCEPTED but not started (`try_enqueue`). The
//! owner can `pause` — a bounded wait until nothing is in flight AND nothing is queued, after which a
//! poller starts no new unit and a queue refuses new submissions — `resume`, or `stop`.

use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

#[derive(Default)]
struct PauseState {
    paused: bool,
    idle: bool,
    stopping: bool,
    /// Units accepted into a queue but not yet started (`try_enqueue` / `begin_work`).
    queued: usize,
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

    /// Producer side of a queue: accept a unit unless paused or stopping (`false` = refuse it; the
    /// caller must not enqueue). A paused queue takes nothing new while what is already queued drains.
    pub(crate) fn try_enqueue(&self) -> bool {
        let mut state = self.lock();
        if state.paused || state.stopping {
            return false;
        }
        state.queued += 1;
        true
    }

    /// The producer could not hand an accepted unit to the queue after all.
    pub(crate) fn unenqueue(&self) {
        let mut state = self.lock();
        state.queued = state.queued.saturating_sub(1);
        self.changed.notify_all();
    }

    /// Queue-consumer variant: start a unit that was accepted earlier. Never waits — units already
    /// queued DRAIN even while paused (a pause is acknowledged only once the queue is empty).
    pub(crate) fn begin_work(&self) {
        let mut state = self.lock();
        state.queued = state.queued.saturating_sub(1);
        state.idle = false;
    }

    pub(crate) fn end_work(&self) {
        self.set_idle();
    }

    /// Stop accepting new units and wait, at most `deadline`, until nothing is in flight AND
    /// nothing is queued. `true` = quiescent now and staying so until `resume`; `false` = still
    /// busy or still draining (the loop keeps running; the owner keeps ownership and should
    /// `resume`).
    pub(crate) fn pause(&self, deadline: Duration) -> bool {
        let started = Instant::now();
        let mut state = self.lock();
        state.paused = true;
        while !(state.idle && state.queued == 0) {
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

    pub(crate) fn resume(&self) {
        self.lock().paused = false;
        self.changed.notify_all();
    }

    /// Owner is shutting the loop down: a poller starts no further units; a queue accepts nothing
    /// new and runs what is already queued (ordinary shutdown drains; a live handoff never reaches
    /// this point with a non-empty queue because `pause` acknowledged only an empty one).
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
        assert!(!control.try_enqueue(), "a paused queue accepts nothing new");
        control.resume();
        assert!(control.try_begin_work());
        assert!(
            !control.pause(Duration::from_millis(50)),
            "busy inside a unit"
        );
        control.end_work();
        assert!(control.pause(Duration::from_millis(50)));
        control.resume();
        assert!(control.try_enqueue(), "accepting again after resume");
        assert!(
            !control.pause(Duration::from_millis(50)),
            "a queued, unstarted unit is not quiescent"
        );
        control.begin_work(); // the queued unit drains even while paused ...
        control.end_work();
        assert!(
            control.pause(Duration::from_millis(50)),
            "... and then the pause is acknowledged"
        );
        control.stop();
        assert!(
            !control.try_begin_work(),
            "a poller starts nothing once stopping"
        );
    }
}
