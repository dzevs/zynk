// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::cell::Cell;
use std::io;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, Ordering},
    Arc, Mutex,
};

use bytes::Bytes;
use portable_pty::CommandBuilder;
#[cfg(test)]
use portable_pty::{native_pty_system, PtySize};
use ratatui::{layout::Rect, Frame};
#[cfg(test)]
use tokio::sync::watch;
use tokio::sync::{mpsc, Notify};
use tracing::{debug, error, info, warn};

use crate::detect::{Agent, AgentState};
use crate::events::AppEvent;
use crate::layout::PaneId;
use crate::pty::actor::{PtyIoActor, PtyIoActorConfig, PtyIoActorHandle, PtyReadResult};
use crate::render_signal::RenderSignal;
use crate::terminal::state::ForegroundProcessObservation;

mod agent_detection;
mod cursor;
mod input;
mod kitty_keyboard;
mod osc;
mod state;
mod terminal;
mod xtgettcap;

use self::agent_detection::{
    decide_detection_screen_read, decide_screen_detection_publish,
    detection_update_for_publish_with_osc, mark_detection_content_changed,
    observe_detection_content_change, DetectionPublishDecision, DetectionScreenReadDecision,
    DetectionScreenReadInput, PendingIdleConfirmation, ScreenDetectionPublishInput,
    AGENT_PENDING_IDLE_RECHECK, AGENT_STARTUP_GRACE_WINDOW,
};
use self::terminal::{GhosttyPaneTerminal, PaneTerminal};
pub(crate) use self::terminal::{
    TerminalDirtyPatch, TerminalDirtyPatchOutcome, TerminalTextMatch, TerminalTextPoint,
    TerminalWordMotion,
};
pub use self::{
    state::PaneState,
    terminal::{InputState, ScrollMetrics, TerminalCursorState},
};

const RELEASE_REACQUIRE_SUPPRESSION: std::time::Duration = std::time::Duration::from_secs(1);
const PANE_TERM: &str = "xterm-256color";
const PANE_COLORTERM: &str = "truecolor";

#[cfg(test)]
thread_local! {
    static AGGREGATE_INPUT_STATE_READS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_aggregate_input_state_reads() {
    AGGREGATE_INPUT_STATE_READS.set(0);
}

#[cfg(test)]
pub(crate) fn aggregate_input_state_reads() -> usize {
    AGGREGATE_INPUT_STATE_READS.get()
}

fn apply_pane_terminal_env(cmd: &mut CommandBuilder) {
    // Each pane is rendered by zynk's own terminal layer, not the outer terminal
    // that launched the app. Advertising the inherited TERM leaks the host terminal
    // identity into shells and across SSH, which breaks redraw and cursor movement
    // when the remote side lacks matching terminfo entries.
    cmd.env("TERM", PANE_TERM);
    cmd.env("COLORTERM", PANE_COLORTERM);
}

/// Codex exports its active thread id to child processes. A pane spawned from
/// inside a codex session would otherwise hand the nested codex the outer
/// session's id, and the nested hook would report it as this pane's identity.
const CODEX_THREAD_ID_ENV_VAR: &str = "CODEX_THREAD_ID";

/// The environment a pane is launched with: caller-supplied `extra` env vars plus
/// an optional `identity` (workspace/tab/pane public ids). Built via
/// [`PaneLaunchEnv::from_extra`] (+ [`PaneLaunchEnv::with_identity`]) and applied
/// to the spawn `CommandBuilder` by [`apply_pane_launch_env`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PaneLaunchEnv {
    extra: Vec<(String, String)>,
    identity: Option<PaneLaunchIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneLaunchIdentity {
    workspace_id: String,
    tab_id: String,
    pane_id: String,
}

impl PaneLaunchEnv {
    pub(crate) fn from_extra(extra: Vec<(String, String)>) -> Self {
        Self {
            extra,
            identity: None,
        }
    }

    pub(crate) fn with_identity(
        mut self,
        workspace_id: String,
        tab_id: String,
        pane_id: String,
    ) -> Self {
        self.identity = Some(PaneLaunchIdentity {
            workspace_id,
            tab_id,
            pane_id,
        });
        self
    }
}

/// Apply a [`PaneLaunchEnv`] to a spawn command: caller-supplied `extra` env,
/// the host-protocol "running inside the multiplexer" flag (`ZYNK_ENV`, ADR
/// 0010), the Zynk base env (`ZYNK_SOCKET_PATH`), and — when the launch env
/// carries an identity — the `ZYNK_WORKSPACE_ID`/`ZYNK_TAB_ID`/`ZYNK_PANE_ID`
/// triple so hooks know which pane they belong to. It also scrubs
/// [`CODEX_THREAD_ID_ENV_VAR`] so a nested codex session cannot inherit the
/// outer session's thread id.
fn apply_pane_launch_env(cmd: &mut CommandBuilder, launch_env: &PaneLaunchEnv) {
    // A codex session nested inside another codex session inherits the outer
    // `CODEX_THREAD_ID`. The codex hook asset compares that env var against the
    // session id in its own hook payload and stays silent when they differ, so a
    // pane spawned from inside a codex session must not carry the outer id in.
    cmd.env_remove(CODEX_THREAD_ID_ENV_VAR);
    for (key, value) in &launch_env.extra {
        cmd.env(key, value);
    }
    cmd.env(crate::ZYNK_ENV_VAR, crate::ZYNK_ENV_VALUE);
    crate::integration::apply_pane_base_env(cmd);
    if let Some(identity) = &launch_env.identity {
        cmd.env(
            crate::integration::ZYNK_WORKSPACE_ID_ENV_VAR,
            &identity.workspace_id,
        );
        cmd.env(crate::integration::ZYNK_TAB_ID_ENV_VAR, &identity.tab_id);
        cmd.env(crate::integration::ZYNK_PANE_ID_ENV_VAR, &identity.pane_id);
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingAgentRelease {
    agent: Agent,
    until: std::time::Instant,
}

#[derive(Clone, Copy, Default)]
struct SpawnInitialState<'a> {
    detected_agent: Option<Agent>,
    history_ansi: Option<&'a str>,
}

fn active_pending_release(
    pending_release: &Mutex<Option<PendingAgentRelease>>,
    now: std::time::Instant,
) -> Option<Agent> {
    let mut pending_release = pending_release.lock().ok()?;
    match *pending_release {
        Some(pending) if now < pending.until => Some(pending.agent),
        Some(_) => {
            *pending_release = None;
            None
        }
        None => None,
    }
}

type PendingProcessExits = Arc<Mutex<Vec<(Option<Agent>, std::time::Instant)>>>;
type ProcessObservationSlot = Arc<Mutex<Option<ForegroundProcessObservation>>>;

fn record_process_observation(
    slot: &Mutex<Option<ForegroundProcessObservation>>,
    agent: Option<Agent>,
    observed_at: std::time::Instant,
) {
    let Ok(mut current) = slot.lock() else {
        return;
    };
    match current.as_mut() {
        Some(old) if old.observed_at > observed_at => {}
        Some(old) if old.observed_at == observed_at => {
            // Equal-time loss or contradictory labels cannot be revived by a
            // positive arriving later. Recovery needs a strictly newer probe.
            if old.agent != agent {
                old.agent = None;
            }
        }
        _ => *current = Some(ForegroundProcessObservation { agent, observed_at }),
    }
}

#[derive(Clone)]
struct DetectionEventSender {
    sender: mpsc::Sender<AppEvent>,
    pending_exits: PendingProcessExits,
    process_observation: ProcessObservationSlot,
}

impl DetectionEventSender {
    fn record_foreground_probe(
        &self,
        native_group: Option<u32>,
        probed_group: Option<u32>,
        agent: Option<Agent>,
        observed_at: std::time::Instant,
    ) {
        let native_agent = agent.filter(|_| native_group.is_some() && native_group == probed_group);
        record_process_observation(&self.process_observation, native_agent, observed_at);
    }
}

async fn publish_state_changed_event(
    state_events: DetectionEventSender,
    pane_id: PaneId,
    agent: Option<Agent>,
    state: AgentState,
    visible_blocker: bool,
    visible_working: bool,
    process_exited: bool,
    observed_at: std::time::Instant,
) {
    // Record before the first queue await. Receipt and handoff readers must see
    // the exit even when the bounded AppEvent channel has no free slot.
    if process_exited {
        record_process_observation(&state_events.process_observation, None, observed_at);
        state_events
            .pending_exits
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .push((agent, observed_at));
    }
    // This runs on the async detector task, not the PTY reader thread.
    // Waiting for queue space here preserves correctness-critical state transitions
    // without blocking pane I/O.
    if let Err(e) = state_events
        .sender
        .send(AppEvent::StateChanged {
            pane_id,
            agent,
            state,
            visible_blocker,
            visible_working,
            process_exited,
            observed_at,
        })
        .await
    {
        warn!(
            pane = pane_id.raw(),
            err = %e,
            "failed to deliver StateChanged event"
        );
    }
}

#[derive(Debug, Clone, Copy)]
struct AgentDetectionPublishUpdate {
    state: AgentState,
    visible_idle: bool,
    visible_blocker: bool,
    visible_working: bool,
    process_exited: bool,
}

async fn skip_screen_detection_under_hook_authority(
    state_events: DetectionEventSender,
    pane_id: PaneId,
    fresh_agent: Option<Agent>,
    observed_at: std::time::Instant,
    lifecycle_authority_active: bool,
    process_exited: bool,
) -> bool {
    if !lifecycle_authority_active || process_exited {
        return false;
    }
    // State remains hook-owned, but a fresh process probe must answer a pending
    // exit even when the agent label did not change. Never use cached presence.
    if fresh_agent.is_some() {
        publish_state_changed_event(
            state_events,
            pane_id,
            fresh_agent,
            AgentState::Idle,
            false,
            false,
            false,
            observed_at,
        )
        .await;
    }
    true
}

async fn apply_agent_detection_publish_update(
    state_events: DetectionEventSender,
    pane_id: PaneId,
    agent: Option<Agent>,
    update: AgentDetectionPublishUpdate,
    observed_at: std::time::Instant,
    state: &mut AgentState,
    last_visible_idle: &mut bool,
    last_visible_blocker: &mut bool,
    last_visible_working: &mut bool,
    last_visible_signal_refresh: &mut Option<std::time::Instant>,
    foreground_shell_exit_reported: &mut bool,
) {
    *state = update.state;
    *last_visible_idle = update.visible_idle;
    *last_visible_blocker = update.visible_blocker;
    *last_visible_working = update.visible_working;
    *last_visible_signal_refresh = if update.visible_blocker || update.visible_working {
        Some(observed_at)
    } else {
        None
    };
    if update.process_exited {
        *foreground_shell_exit_reported = true;
    }
    publish_state_changed_event(
        state_events,
        pane_id,
        agent,
        update.state,
        update.visible_blocker,
        update.visible_working,
        update.process_exited,
        observed_at,
    )
    .await;
}

const AGENT_MISS_CONFIRMATION_ATTEMPTS: u8 = 6;
const PROCESS_RECHECK_IDENTIFIED: std::time::Duration = std::time::Duration::from_secs(5);
const PROCESS_RECHECK_MISSING_FOREGROUND_GROUP: std::time::Duration =
    std::time::Duration::from_secs(30);
const PROCESS_ACQUISITION_WINDOW: std::time::Duration = std::time::Duration::from_secs(8);
const PROCESS_ACQUISITION_FAST_WINDOW: std::time::Duration = std::time::Duration::from_millis(1500);
const PROCESS_ACQUISITION_FAST_RECHECK: std::time::Duration = std::time::Duration::from_millis(500);
const PROCESS_ACQUISITION_SLOW_RECHECK: std::time::Duration = std::time::Duration::from_secs(2);
const PROCESS_ACQUISITION_IDLE_RESET: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, Clone, Copy)]
struct AgentDetectionPresence {
    current_agent: Option<Agent>,
    consecutive_misses: u8,
}

fn absolute_process_cwd(pid: u32) -> Option<std::path::PathBuf> {
    crate::platform::process_cwd(pid).filter(|cwd| cwd.is_absolute())
}

fn foreground_member_cwd_different_from_shell(
    shell_pid: u32,
    shell_cwd: Option<&std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    let job = crate::detect::foreground_job(shell_pid)?;
    for process in job.processes {
        if process.pid == shell_pid {
            continue;
        }
        let Some(cwd) = absolute_process_cwd(process.pid) else {
            continue;
        };
        if shell_cwd != Some(&cwd) {
            return Some(cwd);
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForegroundShellAgentAction {
    ObserveProbe,
    ReportProcessExit,
    ClearAgent,
}

fn foreground_shell_agent_action(
    previous_agent: Option<Agent>,
    new_agent: Option<Agent>,
    foreground_is_pane_shell: bool,
    process_exit_reported: bool,
) -> ForegroundShellAgentAction {
    if previous_agent.is_none() || new_agent.is_some() {
        return ForegroundShellAgentAction::ObserveProbe;
    }

    if process_exit_reported {
        return ForegroundShellAgentAction::ClearAgent;
    }

    if foreground_is_pane_shell {
        // Do not clear identity immediately. First publish an idle process-exit
        // transition for the previous agent so notifications and wait-agent callers
        // observe completion before the pane becomes unknown.
        return ForegroundShellAgentAction::ReportProcessExit;
    }

    ForegroundShellAgentAction::ObserveProbe
}

#[derive(Debug, Clone, Copy)]
struct ProcessProbeInput {
    current_agent: Option<Agent>,
    suppressed_agent: Option<Agent>,
    foreground_pgid: Option<u32>,
    last_foreground_pgid: Option<u32>,
    has_process_probe: bool,
    acquisition_age: Option<std::time::Duration>,
    pending_foreground_shell_clear: bool,
    pending_restore_probe: bool,
    elapsed_since_process_check: std::time::Duration,
}

fn foreground_group_changed(
    foreground_pgid: Option<u32>,
    last_foreground_pgid: Option<u32>,
) -> bool {
    foreground_pgid != last_foreground_pgid
        && (foreground_pgid.is_some() || last_foreground_pgid.is_some())
}

// Only kernel-observed foreground groups drive change detection. Remembering an
// inferred group would look like a change on every tick while the kernel stays silent.
fn process_group_for_change_tracking(
    observed_foreground_pgid: Option<u32>,
    probed_process_group_id: Option<u32>,
) -> Option<u32> {
    observed_foreground_pgid?;
    probed_process_group_id.or(observed_foreground_pgid)
}

fn should_skip_process_probe_for_lifecycle_authority(
    full_lifecycle_authority_active: bool,
    input: ProcessProbeInput,
) -> bool {
    full_lifecycle_authority_active
        && input.foreground_pgid.is_some()
        && !input.pending_foreground_shell_clear
        && input.suppressed_agent.is_none()
        && input.has_process_probe
        && !foreground_group_changed(input.foreground_pgid, input.last_foreground_pgid)
}

fn should_probe_foreground_job(input: ProcessProbeInput) -> bool {
    if input.pending_foreground_shell_clear || input.pending_restore_probe {
        return true;
    }

    let foreground_group_changed =
        foreground_group_changed(input.foreground_pgid, input.last_foreground_pgid);

    if input.suppressed_agent.is_some() {
        return !input.has_process_probe || foreground_group_changed;
    }

    if let Some(acquisition_age) = input.acquisition_age {
        let acquisition_interval = if acquisition_age <= PROCESS_ACQUISITION_FAST_WINDOW {
            PROCESS_ACQUISITION_FAST_RECHECK
        } else {
            PROCESS_ACQUISITION_SLOW_RECHECK
        };
        if acquisition_age <= PROCESS_ACQUISITION_WINDOW
            && input.elapsed_since_process_check >= acquisition_interval
        {
            return true;
        }
    }

    if input.current_agent.is_none() {
        return !input.has_process_probe
            || foreground_group_changed
            || (input.foreground_pgid.is_none()
                && input.elapsed_since_process_check >= PROCESS_RECHECK_MISSING_FOREGROUND_GROUP);
    }

    foreground_group_changed || input.elapsed_since_process_check >= PROCESS_RECHECK_IDENTIFIED
}

fn sync_content_change_acquisition(
    current_agent: Option<Agent>,
    suppressed_agent: Option<Agent>,
    process_group_changed: bool,
    content_changed: bool,
    now: std::time::Instant,
    acquisition_started_at: &mut Option<std::time::Instant>,
    last_content_change_at: &mut Option<std::time::Instant>,
) {
    if current_agent.is_some() || suppressed_agent.is_some() || process_group_changed {
        return;
    }

    if content_changed {
        let should_start = acquisition_started_at.is_none_or(|started| {
            now.duration_since(started) > PROCESS_ACQUISITION_WINDOW
                && last_content_change_at.is_none_or(|last_change| {
                    now.duration_since(last_change) >= PROCESS_ACQUISITION_IDLE_RESET
                })
        });
        if should_start {
            *acquisition_started_at = Some(now);
        }
        *last_content_change_at = Some(now);
        return;
    }

    let Some(acquisition_started) = *acquisition_started_at else {
        return;
    };
    let Some(last_content_change) = *last_content_change_at else {
        return;
    };

    if now.duration_since(acquisition_started) > PROCESS_ACQUISITION_WINDOW
        && now.duration_since(last_content_change) >= PROCESS_ACQUISITION_IDLE_RESET
    {
        *acquisition_started_at = None;
        *last_content_change_at = None;
    }
}

#[derive(Debug, Clone)]
struct ProcessProbeResult {
    process_group_id: Option<u32>,
    foreground_is_pane_shell: bool,
    agent: Option<Agent>,
    process_name: Option<String>,
}

fn agent_hint_for_foreground_job_members(
    job: &crate::platform::ForegroundJob,
    read_hint: impl Fn(u32) -> Option<Agent>,
) -> Option<Agent> {
    read_hint(job.process_group_id)
        .or_else(|| agent_hint_for_non_leader_foreground_job_members(job, read_hint))
}

fn agent_hint_for_non_leader_foreground_job_members(
    job: &crate::platform::ForegroundJob,
    read_hint: impl Fn(u32) -> Option<Agent>,
) -> Option<Agent> {
    job.processes
        .iter()
        .filter(|process| process.pid != job.process_group_id)
        .find_map(|process| read_hint(process.pid))
}

fn identify_process_group_leader_in_job(
    job: &crate::platform::ForegroundJob,
) -> Option<(Agent, String)> {
    let leader = job
        .processes
        .iter()
        .find(|process| process.pid == job.process_group_id)?;
    let leader_job = crate::platform::ForegroundJob {
        process_group_id: job.process_group_id,
        processes: vec![leader.clone()],
    };
    crate::detect::identify_agent_in_job(&leader_job)
}

fn process_probe_result(
    job: &crate::platform::ForegroundJob,
    pid: u32,
    agent: Agent,
    process_name: String,
) -> ProcessProbeResult {
    ProcessProbeResult {
        process_group_id: Some(job.process_group_id),
        foreground_is_pane_shell: job.processes.iter().any(|process| process.pid == pid),
        agent: Some(agent),
        process_name: Some(process_name),
    }
}

fn hinted_process_probe_result(
    job: &crate::platform::ForegroundJob,
    pid: u32,
    read_hint: impl Fn(u32) -> Option<Agent>,
) -> Option<ProcessProbeResult> {
    let agent = agent_hint_for_foreground_job_members(job, read_hint)?;
    Some(process_probe_result(
        job,
        pid,
        agent,
        crate::detect::agent_label(agent).to_string(),
    ))
}

fn probe_foreground_process_from_jobs(
    pid: u32,
    foreground_pgid: Option<u32>,
    leader_job: Option<crate::platform::ForegroundJob>,
    foreground_job: impl FnOnce() -> Option<crate::platform::ForegroundJob>,
    read_hint: impl Fn(u32) -> Option<Agent> + Copy,
) -> ProcessProbeResult {
    if let Some(job) = leader_job.as_ref() {
        if let Some(hinted) = hinted_process_probe_result(job, pid, read_hint) {
            return hinted;
        }
        if let Some((agent, process_name)) = crate::detect::identify_agent_in_job(job) {
            return process_probe_result(job, pid, agent, process_name);
        }
    }

    let foreground_job = foreground_job();
    if let Some(job) = foreground_job.as_ref() {
        if let Some(agent) = read_hint(job.process_group_id) {
            return process_probe_result(
                job,
                pid,
                agent,
                crate::detect::agent_label(agent).to_string(),
            );
        }
        if let Some((agent, process_name)) = identify_process_group_leader_in_job(job) {
            return process_probe_result(job, pid, agent, process_name);
        }
        if let Some(agent) = agent_hint_for_non_leader_foreground_job_members(job, read_hint) {
            return process_probe_result(
                job,
                pid,
                agent,
                crate::detect::agent_label(agent).to_string(),
            );
        }

        let identified = crate::detect::identify_agent_in_job(job);
        return ProcessProbeResult {
            process_group_id: Some(job.process_group_id),
            foreground_is_pane_shell: job.processes.iter().any(|process| process.pid == pid),
            agent: identified.as_ref().map(|(agent, _)| *agent),
            process_name: identified.map(|(_, process_name)| process_name),
        };
    }

    ProcessProbeResult {
        process_group_id: foreground_pgid,
        foreground_is_pane_shell: false,
        agent: None,
        process_name: None,
    }
}

fn probe_foreground_process(pid: u32, foreground_pgid: Option<u32>) -> ProcessProbeResult {
    probe_foreground_process_from_jobs(
        pid,
        foreground_pgid,
        foreground_pgid.and_then(crate::detect::foreground_group_leader_job),
        || crate::detect::foreground_job(pid),
        crate::platform::process_agent_hint,
    )
}

fn spawn_basic_detection_task(
    pane_id: PaneId,
    child_pid: Arc<AtomicU32>,
    terminal: Arc<PaneTerminal>,
    detection_content_seq: Arc<AtomicU64>,
    full_lifecycle_authority_active: Arc<AtomicBool>,
    state_events: DetectionEventSender,
) -> (
    tokio::task::AbortHandle,
    Arc<Notify>,
    Arc<Mutex<Option<PendingAgentRelease>>>,
) {
    let detect_reset_notify = Arc::new(Notify::new());
    let detect_reset = detect_reset_notify.clone();
    let pending_release = Arc::new(Mutex::new(None));
    let pending_release_for_task = pending_release.clone();

    let handle = tokio::spawn(async move {
        let mut agent_presence = AgentDetectionPresence::from_agent(None);
        let mut state = AgentState::Unknown;
        let mut last_visible_idle = false;
        let mut last_visible_blocker = false;
        let mut last_visible_working = false;
        let mut last_visible_signal_refresh = None;
        let mut last_process_check = std::time::Instant::now();
        let mut last_foreground_pgid = None;
        let mut has_process_probe = false;
        let mut acquisition_started_at = None;
        let mut last_content_change_at = None;
        let mut pending_foreground_shell_clear = false;
        let mut foreground_shell_exit_reported = false;
        let mut release_was_active = false;
        let mut last_detection_text = String::new();
        let mut last_screen_scan_detection_content_seq = None;
        let mut agent_startup_grace_until = None;
        let mut pending_idle = PendingIdleConfirmation::default();

        loop {
            let sleep_duration = if pending_idle.active() {
                AGENT_PENDING_IDLE_RECHECK
            } else {
                std::time::Duration::from_millis(300)
            };
            tokio::select! {
                _ = tokio::time::sleep(sleep_duration) => {}
                _ = detect_reset.notified() => {
                    record_process_observation(&state_events.process_observation, None, std::time::Instant::now());
                    agent_presence = AgentDetectionPresence::from_agent(None);
                    state = AgentState::Unknown;
                    last_visible_idle = false;
                    last_visible_blocker = false;
                    last_visible_working = false;
                    last_visible_signal_refresh = None;
                    last_process_check = std::time::Instant::now();
                    last_foreground_pgid = None;
                    has_process_probe = false;
                    acquisition_started_at = None;
                    last_content_change_at = None;
                    pending_foreground_shell_clear = false;
                    foreground_shell_exit_reported = false;
                    release_was_active = false;
                    last_detection_text.clear();
                    last_screen_scan_detection_content_seq = None;
                    agent_startup_grace_until = None;
                    pending_idle.clear();
                }
            }

            let now = std::time::Instant::now();
            let suppressed_agent = active_pending_release(&pending_release_for_task, now);
            if suppressed_agent.is_none() && release_was_active {
                has_process_probe = false;
                acquisition_started_at = None;
                last_content_change_at = None;
            }
            release_was_active = suppressed_agent.is_some();
            let pid = child_pid.load(Ordering::Acquire);
            let mut agent_changed = false;
            let mut fresh_process_agent = None;
            let mut agent = agent_presence.current_agent();
            let lifecycle_authority_active =
                full_lifecycle_authority_active.load(Ordering::Acquire);
            let foreground_pgid = (pid > 0)
                .then(|| crate::detect::foreground_process_group_id(pid))
                .flatten();
            let process_group_changed =
                foreground_group_changed(foreground_pgid, last_foreground_pgid);
            let should_check_process = pid > 0 && {
                let process_probe_input = ProcessProbeInput {
                    current_agent: agent,
                    suppressed_agent,
                    foreground_pgid,
                    last_foreground_pgid,
                    has_process_probe,
                    acquisition_age: acquisition_started_at
                        .map(|started| now.duration_since(started)),
                    pending_foreground_shell_clear,
                    pending_restore_probe: false,
                    elapsed_since_process_check: now.duration_since(last_process_check),
                };
                !should_skip_process_probe_for_lifecycle_authority(
                    lifecycle_authority_active,
                    process_probe_input,
                ) && should_probe_foreground_job(process_probe_input)
            };

            if should_check_process {
                last_process_check = now;
                let had_process_probe = has_process_probe;
                has_process_probe = true;
                let probe = probe_foreground_process(pid, foreground_pgid);
                let process_group_id = probe.process_group_id;
                let tracked_process_group_id =
                    process_group_for_change_tracking(foreground_pgid, process_group_id);
                let foreground_is_pane_shell = probe.foreground_is_pane_shell;
                let mut new_agent = probe.agent;
                if let Some(suppressed_agent) = suppressed_agent {
                    if new_agent == Some(suppressed_agent) {
                        new_agent = None;
                    } else if let Ok(mut pending_release) = pending_release_for_task.lock() {
                        *pending_release = None;
                    }
                }
                state_events.record_foreground_probe(
                    foreground_pgid,
                    process_group_id,
                    new_agent,
                    now,
                );
                let previous_agent = agent_presence.current_agent();
                fresh_process_agent = new_agent;
                let changed = match foreground_shell_agent_action(
                    previous_agent,
                    new_agent,
                    foreground_is_pane_shell,
                    foreground_shell_exit_reported,
                ) {
                    ForegroundShellAgentAction::ReportProcessExit => {
                        pending_foreground_shell_clear = true;
                        false
                    }
                    ForegroundShellAgentAction::ClearAgent => {
                        pending_foreground_shell_clear = false;
                        foreground_shell_exit_reported = false;
                        agent_presence.clear_current_agent()
                    }
                    ForegroundShellAgentAction::ObserveProbe => {
                        pending_foreground_shell_clear = false;
                        foreground_shell_exit_reported = false;
                        agent_presence.observe_process_probe(new_agent)
                    }
                };
                last_foreground_pgid = tracked_process_group_id;
                if new_agent.is_some() {
                    acquisition_started_at = None;
                    last_content_change_at = None;
                } else if agent_presence.current_agent().is_none()
                    && had_process_probe
                    && process_group_changed
                {
                    acquisition_started_at = Some(now);
                }
                if changed {
                    agent = agent_presence.current_agent();
                    agent_changed = previous_agent != agent;
                    if agent_changed {
                        pending_idle.clear();
                        last_screen_scan_detection_content_seq = None;
                        // A new foreground agent must not inherit OSC
                        // title/progress evidence from the previous process.
                        terminal.clear_agent_osc_state();
                        if agent.is_some() {
                            agent_startup_grace_until = Some(now + AGENT_STARTUP_GRACE_WINDOW);
                            state = AgentState::Idle;
                            last_visible_idle = true;
                            last_visible_blocker = false;
                            last_visible_working = false;
                            last_visible_signal_refresh = None;
                            publish_state_changed_event(
                                state_events.clone(),
                                pane_id,
                                agent,
                                AgentState::Idle,
                                false,
                                false,
                                false,
                                now,
                            )
                            .await;
                        } else {
                            agent_startup_grace_until = None;
                        }
                    }
                }
            }

            let process_exited = pending_foreground_shell_clear
                && agent.is_some()
                && !foreground_shell_exit_reported;

            if skip_screen_detection_under_hook_authority(
                state_events.clone(),
                pane_id,
                fresh_process_agent.filter(|_| !agent_changed),
                now,
                lifecycle_authority_active,
                process_exited,
            )
            .await
            {
                pending_idle.clear();
                continue;
            }

            if let Some(until) = agent_startup_grace_until {
                if process_exited {
                    agent_startup_grace_until = None;
                    pending_idle.clear();
                } else {
                    if now < until {
                        pending_idle.clear();
                        continue;
                    }
                    agent_startup_grace_until = None;
                    last_screen_scan_detection_content_seq = None;
                    pending_idle.clear();
                    continue;
                }
            }

            let current_detection_content_seq = if agent.is_some() {
                Some(detection_content_seq.load(Ordering::Relaxed))
            } else {
                None
            };
            match decide_detection_screen_read(DetectionScreenReadInput {
                state,
                agent,
                pending_idle_active: pending_idle.active(),
                agent_changed,
                process_exited,
                current_detection_content_seq,
                last_screen_scan_detection_content_seq,
            }) {
                DetectionScreenReadDecision::Read => {}
                DetectionScreenReadDecision::Skip => continue,
            }

            let content = terminal.detection_text();
            last_screen_scan_detection_content_seq = current_detection_content_seq;
            let content_changed = content != last_detection_text;
            last_detection_text.clone_from(&content);
            if !process_exited && crate::detect::should_skip_state_update(agent, &content) {
                pending_idle.clear();
                continue;
            }
            sync_content_change_acquisition(
                agent_presence.current_agent(),
                suppressed_agent,
                process_group_changed,
                content_changed,
                now,
                &mut acquisition_started_at,
                &mut last_content_change_at,
            );

            let osc_title = terminal.agent_osc_title();
            let osc_progress = terminal.agent_osc_progress();
            let unwrapped_content = terminal.detection_unwrapped_text();
            let Some(screen_detection) = detection_update_for_publish_with_osc(
                agent,
                &content,
                Some(&unwrapped_content),
                &osc_title,
                &osc_progress,
                process_exited,
            ) else {
                pending_idle.clear();
                continue;
            };
            match decide_screen_detection_publish(
                ScreenDetectionPublishInput {
                    screen_detection,
                    current_state: state,
                    last_visible_idle,
                    last_visible_blocker,
                    last_visible_working,
                    last_visible_signal_refresh,
                    process_exited,
                    agent_changed,
                    now,
                },
                &mut pending_idle,
            ) {
                DetectionPublishDecision::NoPublish => {}
                DetectionPublishDecision::Publish {
                    state: new_state,
                    visible_idle,
                    visible_blocker,
                    visible_working,
                    process_exited: publish_process_exited,
                } => {
                    apply_agent_detection_publish_update(
                        state_events.clone(),
                        pane_id,
                        agent,
                        AgentDetectionPublishUpdate {
                            state: new_state,
                            visible_idle,
                            visible_blocker,
                            visible_working,
                            process_exited: publish_process_exited,
                        },
                        now,
                        &mut state,
                        &mut last_visible_idle,
                        &mut last_visible_blocker,
                        &mut last_visible_working,
                        &mut last_visible_signal_refresh,
                        &mut foreground_shell_exit_reported,
                    )
                    .await;
                }
            }
        }
    });

    (handle.abort_handle(), detect_reset_notify, pending_release)
}

impl AgentDetectionPresence {
    fn from_agent(current_agent: Option<Agent>) -> Self {
        Self {
            current_agent,
            consecutive_misses: 0,
        }
    }

    fn current_agent(&self) -> Option<Agent> {
        self.current_agent
    }

    fn clear_current_agent(&mut self) -> bool {
        if self.current_agent.is_none() {
            self.consecutive_misses = 0;
            return false;
        }
        self.current_agent = None;
        self.consecutive_misses = 0;
        true
    }

    fn observe_process_probe(&mut self, identified_agent: Option<Agent>) -> bool {
        match identified_agent {
            Some(agent) => {
                self.consecutive_misses = 0;
                if Some(agent) == self.current_agent {
                    return false;
                }
                self.current_agent = Some(agent);
                true
            }
            None => {
                if self.current_agent.is_none() {
                    self.consecutive_misses = 0;
                    return false;
                }
                self.consecutive_misses = self.consecutive_misses.saturating_add(1);
                if self.consecutive_misses < AGENT_MISS_CONFIRMATION_ATTEMPTS {
                    return false;
                }
                self.current_agent = None;
                self.consecutive_misses = 0;
                true
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PaneRuntime — PTY, parser, channels, background tasks
// ---------------------------------------------------------------------------

/// PTY runtime for a pane. Owns the terminal, I/O channels, and background tasks.
/// Dropping this shuts down all background tasks and closes the PTY.
pub struct PaneRuntime {
    pane_id: PaneId,
    terminal: Arc<PaneTerminal>,
    io: PaneRuntimeIo,
    current_size: Cell<(u16, u16, u32, u32)>,
    child_pid: Arc<AtomicU32>,
    /// The start time of the process at `child_pid`, captured when that pid was
    /// published; `0` means "never captured", which fails closed (ADR 0014).
    ///
    /// Published BEFORE `child_pid` and read after it, so a reader that sees a
    /// pid always sees the start time that belongs to it. That ordering is why
    /// the pair needs no lock on the PTY path.
    child_start_time: Arc<AtomicU64>,
    reported_cwd: Arc<Mutex<Option<std::path::PathBuf>>>,
    child_wait_completed: Option<Arc<AtomicBool>>,
    kitty_keyboard_flags: Arc<AtomicU16>,
    detection_content_seq: Arc<AtomicU64>,
    full_lifecycle_authority_active: Arc<AtomicBool>,
    // A single detector awaits each send, bounding this to queue capacity + one.
    // Entries retire only after AppState has applied that exact observation.
    pending_process_exits: PendingProcessExits,
    process_observation: ProcessObservationSlot,
    detect_reset_notify: Arc<Notify>,
    pending_release: Arc<Mutex<Option<PendingAgentRelease>>>,
    preserve_processes_on_drop: bool,
    // Task handles for deterministic shutdown
    detect_handle: tokio::task::AbortHandle,
}

enum PaneRuntimeIo {
    Actor(PtyIoActorHandle),
    #[cfg(test)]
    TestChannel {
        sender: mpsc::Sender<Bytes>,
        resize_tx: watch::Sender<(u16, u16, u32, u32)>,
    },
}

impl PaneRuntimeIo {
    fn write_terminal_response(&self, response: impl FnOnce() -> Option<Bytes>) {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.write_terminal_response(response),
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { sender, .. } => {
                if let Some(bytes) = response() {
                    let _ = sender.try_send(bytes);
                }
            }
        }
    }

    fn shutdown(&self) {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.shutdown(),
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { .. } => {}
        }
    }

    fn duplicate_handoff_fd(&self) -> std::io::Result<std::os::fd::RawFd> {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.duplicate_for_handoff(),
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { .. } => {
                Err(std::io::Error::other("test runtime has no PTY master fd"))
            }
        }
    }

    fn foreground_process_group_id(&self) -> Option<u32> {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.foreground_process_group_id(),
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { .. } => None,
        }
    }

    fn begin_handoff(&self, timeout: std::time::Duration) -> std::io::Result<()> {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.begin_handoff(timeout),
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { .. } => Ok(()),
        }
    }

    fn set_handoff_paused(&self, paused: bool) -> std::io::Result<()> {
        match self {
            PaneRuntimeIo::Actor(actor) => {
                if paused {
                    actor.begin_handoff(std::time::Duration::from_secs(1))
                } else {
                    actor.rollback_handoff()
                }
            }
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { .. } => Ok(()),
        }
    }

    fn release_after_commit(&self) -> std::io::Result<()> {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.release_after_commit(),
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { .. } => Ok(()),
        }
    }

    fn resize(
        &self,
        rows: u16,
        cols: u16,
        cell_width_px: u32,
        cell_height_px: u32,
        terminal_responses: Vec<Bytes>,
    ) {
        match self {
            PaneRuntimeIo::Actor(actor) => {
                actor.resize(
                    rows,
                    cols,
                    cell_width_px,
                    cell_height_px,
                    terminal_responses,
                );
            }
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { resize_tx, .. } => {
                let _ = resize_tx.send((rows, cols, cell_width_px, cell_height_px));
            }
        }
    }

    fn nudge_child_redraw_after_handoff(
        &self,
        rows: u16,
        cols: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) {
        match self {
            PaneRuntimeIo::Actor(actor) => {
                actor.nudge_child_redraw_after_handoff(rows, cols, cell_width_px, cell_height_px);
            }
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { .. } => {}
        }
    }

    async fn send_bytes(&self, bytes: Bytes) -> Result<(), mpsc::error::SendError<Bytes>> {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.write_user_input(bytes).await,
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { sender, .. } => sender.send(bytes).await,
        }
    }

    fn try_send_bytes(&self, bytes: Bytes) -> Result<(), mpsc::error::TrySendError<Bytes>> {
        match self {
            PaneRuntimeIo::Actor(actor) => actor.try_write_user_input(bytes),
            #[cfg(test)]
            PaneRuntimeIo::TestChannel { sender, .. } => sender.try_send(bytes),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelRouting {
    HostScroll,
    MouseReport,
    AlternateScroll,
}

impl Drop for PaneRuntime {
    fn drop(&mut self) {
        // Abort detection task immediately and terminate the owned session.
        // The PTY actor shuts down before the process/session policy runs.
        self.detect_handle.abort();
        self.io.shutdown();
        if !self.preserve_processes_on_drop {
            shutdown_pane_processes(
                self.pane_id,
                self.child_pid.load(Ordering::Acquire),
                self.child_wait_completed.as_deref(),
            );
        }
    }
}

fn process_alive_for_shutdown(
    pid: u32,
    child_pid: u32,
    child_wait_completed: bool,
    process_exists: impl FnOnce(u32) -> bool,
) -> bool {
    if pid == child_pid && child_wait_completed {
        return false;
    }
    process_exists(pid)
}

fn wait_for_processes_to_exit(
    pids: &[u32],
    child_pid: u32,
    child_wait_completed: Option<&AtomicBool>,
    timeout: std::time::Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let child_wait_completed =
            child_wait_completed.is_some_and(|flag| flag.load(Ordering::Acquire));
        if pids.iter().all(|pid| {
            !process_alive_for_shutdown(
                *pid,
                child_pid,
                child_wait_completed,
                crate::platform::process_exists,
            )
        }) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn shutdown_pane_processes(
    pane_id: PaneId,
    child_pid: u32,
    child_wait_completed: Option<&AtomicBool>,
) {
    if child_pid == 0 {
        return;
    }

    let mut pids = crate::platform::session_processes(child_pid);
    if pids.is_empty() {
        pids.push(child_pid);
    }
    pids.sort_unstable();
    pids.dedup();

    for (signal, grace) in [
        (
            crate::platform::Signal::Hangup,
            std::time::Duration::from_millis(250),
        ),
        (
            crate::platform::Signal::Terminate,
            std::time::Duration::from_millis(250),
        ),
        (
            crate::platform::Signal::Kill,
            std::time::Duration::from_millis(250),
        ),
    ] {
        crate::platform::signal_processes(&pids, signal);
        if wait_for_processes_to_exit(&pids, child_pid, child_wait_completed, grace) {
            info!(
                pane = pane_id.raw(),
                pid = child_pid,
                ?signal,
                "pane session terminated"
            );
            return;
        }
    }

    warn!(
        pane = pane_id.raw(),
        pid = child_pid,
        pids = ?pids,
        "pane session still alive after forced shutdown"
    );
}

fn truncate_handoff_history(history: String, max_bytes: usize) -> String {
    if history.len() <= max_bytes {
        return history;
    }
    let mut start = history.len().saturating_sub(max_bytes);
    while !history.is_char_boundary(start) {
        start += 1;
    }
    let Some(newline_offset) = history[start..].find('\n') else {
        return String::new();
    };
    start += newline_offset + 1;
    history[start..].to_owned()
}

fn pane_shell(configured_shell: &str) -> String {
    pane_shell_from(configured_shell, std::env::var("SHELL").ok())
}

fn pane_shell_from(configured_shell: &str, env_shell: Option<String>) -> String {
    let configured_shell = configured_shell.trim();
    if !configured_shell.is_empty() {
        return configured_shell.to_string();
    }

    env_shell
        .map(|shell| shell.trim().to_string())
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(default_pane_shell)
}

fn default_pane_shell() -> String {
    "/bin/sh".into()
}

#[derive(Clone, Copy)]
pub(crate) struct PaneShellConfig<'a> {
    pub(crate) default_shell: &'a str,
    pub(crate) mode: crate::config::ShellModeConfig,
}

impl<'a> PaneShellConfig<'a> {
    pub(crate) fn new(default_shell: &'a str, mode: crate::config::ShellModeConfig) -> Self {
        Self {
            default_shell,
            mode,
        }
    }
}

fn shell_mode_uses_login_shell(mode: crate::config::ShellModeConfig) -> bool {
    match mode {
        crate::config::ShellModeConfig::Auto | crate::config::ShellModeConfig::NonLogin => false,
        crate::config::ShellModeConfig::Login => true,
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

fn resolve_shell_for_login_mode(shell: &str) -> io::Result<String> {
    if shell.contains(std::path::MAIN_SEPARATOR) {
        let path = Path::new(shell);
        return is_executable_file(path)
            .then(|| shell.to_string())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("login shell {shell:?} is not executable"),
                )
            });
    }

    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(shell))
                .find(|candidate| is_executable_file(candidate))
        })
        .and_then(|path| path.into_os_string().into_string().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("login shell {shell:?} was not found on PATH"),
            )
        })
}

fn pane_shell_command_builder(shell_config: PaneShellConfig<'_>) -> io::Result<CommandBuilder> {
    let shell = pane_shell(shell_config.default_shell);
    if shell_mode_uses_login_shell(shell_config.mode) {
        let mut cmd = CommandBuilder::new_default_prog();
        cmd.env("SHELL", resolve_shell_for_login_mode(&shell)?);
        Ok(cmd)
    } else {
        Ok(CommandBuilder::new(&shell))
    }
}

fn usable_reported_cwd(cwd: std::path::PathBuf) -> Option<std::path::PathBuf> {
    (cwd.is_absolute() && cwd.is_dir()).then_some(cwd)
}

fn publish_reported_cwd(
    pane_id: PaneId,
    cwd: std::path::PathBuf,
    reported_cwd: &Arc<Mutex<Option<std::path::PathBuf>>>,
    events: &mpsc::Sender<AppEvent>,
) {
    let Some(cwd) = usable_reported_cwd(cwd) else {
        return;
    };
    if let Ok(mut current) = reported_cwd.lock() {
        if current.as_ref() == Some(&cwd) {
            return;
        }
        *current = Some(cwd.clone());
    }
    if let Err(err) = events.try_send(AppEvent::TerminalCwdReported { pane_id, cwd }) {
        warn!(
            pane = pane_id.raw(),
            err = %err,
            "failed to send terminal cwd report"
        );
    }
}

impl PaneRuntime {
    pub(crate) fn pending_process_exits(&self) -> Vec<(Option<Agent>, std::time::Instant)> {
        self.pending_process_exits
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    pub(crate) fn acknowledge_process_exit(
        &self,
        agent: Option<Agent>,
        observed_at: std::time::Instant,
    ) {
        let mut pending = self
            .pending_process_exits
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        if let Some(index) = pending
            .iter()
            .position(|entry| *entry == (agent, observed_at))
        {
            pending.remove(index);
        }
    }

    pub fn shutdown(mut self) {
        self.detect_handle.abort();
        self.io.shutdown();
        shutdown_pane_processes(
            self.pane_id,
            self.child_pid.load(Ordering::Acquire),
            self.child_wait_completed.as_deref(),
        );
        self.preserve_processes_on_drop = true;
    }

    pub fn duplicate_handoff_fd(&self) -> std::io::Result<std::os::fd::RawFd> {
        self.io.duplicate_handoff_fd()
    }

    pub fn preserve_for_handoff(mut self) {
        if let Err(err) = self.io.release_after_commit() {
            warn!(
                pane = self.pane_id.raw(),
                err = %err,
                "failed to release PTY actor after handoff commit; dropping runtime will still close the actor handle"
            );
        }
        self.detect_handle.abort();
        self.preserve_processes_on_drop = true;
    }

    pub fn assume_handoff_ownership(&mut self) {
        self.preserve_processes_on_drop = false;
    }

    pub fn set_handoff_reader_paused(&self, paused: bool) {
        if let Err(err) = self.io.set_handoff_paused(paused) {
            warn!(
                pane = self.pane_id.raw(),
                err = %err,
                paused,
                "failed to update PTY actor handoff pause state"
            );
        }
    }

    pub fn pause_handoff_reader(&self, timeout: std::time::Duration) -> std::io::Result<()> {
        self.io.begin_handoff(timeout)
    }

    pub fn handoff_runtime_state(
        &self,
        pane_id: u32,
    ) -> crate::handoff_runtime::HandoffRuntimeState {
        let child_pid = self.child_pid.load(Ordering::Acquire);
        let child_start_time = self.child_start_time.load(Ordering::Acquire);
        let (rows, cols, cell_width_px, cell_height_px) = self.current_size.get();
        crate::handoff_runtime::HandoffRuntimeState {
            pane_id,
            child_pid,
            child_start_time,
            rows,
            cols,
            cell_width_px,
            cell_height_px,
            keyboard_protocol_flags: match self.keyboard_protocol() {
                crate::input::KeyboardProtocol::Legacy => 0,
                crate::input::KeyboardProtocol::Kitty { flags } => flags,
            },
            keyboard_protocol_ansi: self.terminal.kitty_keyboard_state_ansi(),
            input_state: self.input_state(),
            initial_history_ansi: None,
        }
    }

    pub fn handoff_history_ansi(&self) -> Option<String> {
        if self.terminal.alternate_screen_active() {
            return None;
        }
        self.snapshot_history().map(|history| {
            truncate_handoff_history(history, crate::server::handoff::MAX_REPLAY_BYTES_PER_PANE)
        })
    }

    pub fn apply_host_terminal_theme(&self, theme: crate::terminal_theme::TerminalTheme) {
        self.terminal.apply_host_terminal_theme(theme);
    }

    pub fn apply_host_terminal_appearance(
        &self,
        appearance: Option<crate::terminal_theme::HostAppearance>,
    ) {
        self.io
            .write_terminal_response(|| self.terminal.apply_host_terminal_appearance(appearance));
    }

    // Runtime construction threads PTY geometry, host context, launch policy, and render hooks.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: PaneShellConfig<'_>,
        launch_env: &PaneLaunchEnv,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> std::io::Result<Self> {
        Self::spawn_with_initial_history(
            pane_id,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            shell_config,
            launch_env,
            None,
            events,
            render_notify,
            render_dirty,
        )
    }

    // Runtime construction needs to thread PTY size, environment, theme, and render hooks together.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn spawn_with_initial_history(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        shell_config: PaneShellConfig<'_>,
        launch_env: &PaneLaunchEnv,
        initial_history_ansi: Option<&str>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> std::io::Result<Self> {
        let mut cmd = pane_shell_command_builder(shell_config)?;
        cmd.cwd(cwd);
        apply_pane_terminal_env(&mut cmd);
        apply_pane_launch_env(&mut cmd, launch_env);
        Self::spawn_command_builder(
            pane_id,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            events,
            render_notify,
            render_dirty,
            cmd,
            "failed to spawn shell",
            SpawnInitialState {
                detected_agent: None,
                history_ansi: initial_history_ansi,
            },
        )
    }

    // Runtime construction needs to thread PTY size, environment, theme, and render hooks together.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_shell_command(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        command: &str,
        launch_env: &PaneLaunchEnv,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> std::io::Result<Self> {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg(command);
        cmd.cwd(cwd);
        apply_pane_terminal_env(&mut cmd);
        apply_pane_launch_env(&mut cmd, launch_env);
        Self::spawn_command_builder(
            pane_id,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            events,
            render_notify,
            render_dirty,
            cmd,
            "failed to spawn command pane",
            SpawnInitialState::default(),
        )
    }

    // Runtime construction threads PTY geometry, host context, launch policy, and render hooks.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_argv_command(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        argv: &[String],
        launch_env: &PaneLaunchEnv,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> std::io::Result<Self> {
        let Some((program, args)) = argv.split_first() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "argv must not be empty",
            ));
        };
        let mut cmd = CommandBuilder::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        cmd.cwd(cwd);
        apply_pane_terminal_env(&mut cmd);
        apply_pane_launch_env(&mut cmd, launch_env);
        Self::spawn_command_builder(
            pane_id,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            host_terminal_appearance,
            events,
            render_notify,
            render_dirty,
            cmd,
            "failed to spawn argv command pane",
            SpawnInitialState::default(),
        )
    }

    pub fn from_handoff_fd(
        import: crate::handoff_runtime::ImportedHandoffRuntime,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
    ) -> std::io::Result<Self> {
        let crate::handoff_runtime::ImportedHandoffRuntime { master_fd, state } = import;
        let crate::handoff_runtime::HandoffRuntimeState {
            pane_id,
            child_pid,
            child_start_time,
            rows,
            cols,
            cell_width_px,
            cell_height_px,
            keyboard_protocol_flags,
            keyboard_protocol_ansi,
            input_state,
            initial_history_ansi,
        } = state;
        let pane_id = PaneId::from_raw(pane_id);
        use std::os::fd::FromRawFd;

        let master_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(master_fd) };

        let (response_tx, _response_rx) = mpsc::channel::<Bytes>(1);
        let mut terminal = crate::ghostty::Terminal::new(cols, rows, scrollback_limit_bytes)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        terminal
            .resize(cols, rows, cell_width_px, cell_height_px)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if crate::kitty_graphics::is_enabled() {
            terminal
                .enable_kitty_graphics()
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        let pane_terminal = GhosttyPaneTerminal::new(terminal, response_tx.clone())?;
        pane_terminal.apply_host_terminal_theme(host_terminal_theme);
        let _ = pane_terminal.apply_host_terminal_appearance(host_terminal_appearance);
        if let Some(input_state) = input_state {
            pane_terminal.seed_handoff_input_state(input_state);
        }
        if let Some(ansi) = keyboard_protocol_ansi.as_deref() {
            pane_terminal.seed_keyboard_protocol_ansi(ansi);
        } else {
            pane_terminal.seed_keyboard_protocol_flags(keyboard_protocol_flags);
        }
        if let Some(ansi) = initial_history_ansi.as_deref() {
            pane_terminal.seed_history_ansi(ansi);
        }
        let terminal = Arc::new(PaneTerminal::new(pane_terminal));
        // The handed-over pane keeps its child process, so it must keep the
        // start time that identifies it (ADR 0014). The exporting server sends
        // the one it captured; a server that predates this field sends 0, and
        // the pid is still live here, so re-read it rather than lose the pane's
        // binding across the upgrade the handoff exists to perform.
        let child_start_time = Arc::new(AtomicU64::new(if child_start_time > 0 {
            child_start_time
        } else {
            crate::platform::process_start_time(child_pid).unwrap_or(0)
        }));
        let child_pid = Arc::new(AtomicU32::new(child_pid));
        let reported_cwd = Arc::new(Mutex::new(None));
        let kitty_keyboard_flags = Arc::new(AtomicU16::new(keyboard_protocol_flags));
        let detection_content_seq = Arc::new(AtomicU64::new(0));

        let io = {
            let terminal = terminal.clone();
            let response_writer = response_tx.clone();
            let render_notify = render_notify.clone();
            let render_dirty = render_dirty.clone();
            let detection_content_seq = detection_content_seq.clone();
            let child_pid = child_pid.clone();
            let read_events = events.clone();
            let reported_cwd = reported_cwd.clone();
            let rt = tokio::runtime::Handle::current();
            let delay_rt = rt.clone();
            let on_read = Box::new(move |bytes: &[u8]| {
                let shell_pid = child_pid.load(Ordering::Acquire);
                let result =
                    terminal.process_pty_bytes(pane_id, shell_pid, bytes, &response_writer);
                observe_detection_content_change(bytes, &detection_content_seq);
                if result.request_render && render_dirty.request_pty(pane_id) {
                    render_notify.notify_one();
                }
                if let Some(delay) = result.render_delay {
                    let render_notify = render_notify.clone();
                    let render_dirty = render_dirty.clone();
                    delay_rt.spawn(async move {
                        tokio::time::sleep(delay).await;
                        if render_dirty.request_pty(pane_id) {
                            render_notify.notify_one();
                        }
                    });
                }
                if let Some(cwd) = result.reported_cwd.clone() {
                    publish_reported_cwd(pane_id, cwd, &reported_cwd, &read_events);
                }
                for content in result.clipboard_writes {
                    if let Err(err) = read_events.try_send(AppEvent::ClipboardWrite { content }) {
                        warn!(
                            pane = pane_id.raw(),
                            err = %err,
                            "failed to queue OSC 52 clipboard write"
                        );
                    }
                }
                PtyReadResult {
                    terminal_responses: result.terminal_responses,
                }
            });
            let exit_events = events.clone();
            let on_reader_exit = Box::new(move || {
                let _ = rt.block_on(exit_events.send(AppEvent::PaneDied { pane_id }));
                debug!(pane = pane_id.raw(), "handoff PTY actor exiting");
            });
            PaneRuntimeIo::Actor(PtyIoActor::spawn(PtyIoActorConfig {
                pane_id: pane_id.raw(),
                master_fd,
                initially_quiesced: true,
                on_read,
                on_reader_exit: Some(on_reader_exit),
            })?)
        };

        let full_lifecycle_authority_active = Arc::new(AtomicBool::new(false));
        let pending_process_exits = Arc::new(Mutex::new(Vec::new()));
        let process_observation = Arc::new(Mutex::new(None));
        let (detect_handle, detect_reset_notify, pending_release) = spawn_basic_detection_task(
            pane_id,
            child_pid.clone(),
            terminal.clone(),
            detection_content_seq.clone(),
            full_lifecycle_authority_active.clone(),
            DetectionEventSender {
                sender: events,
                pending_exits: pending_process_exits.clone(),
                process_observation: process_observation.clone(),
            },
        );

        Ok(Self {
            pane_id,
            terminal,
            io,
            current_size: Cell::new((rows, cols, cell_width_px, cell_height_px)),
            child_pid,
            child_start_time,
            reported_cwd,
            child_wait_completed: None,
            kitty_keyboard_flags,
            detection_content_seq,
            full_lifecycle_authority_active,
            pending_process_exits,
            process_observation,
            detect_reset_notify,
            pending_release,
            preserve_processes_on_drop: true,
            detect_handle,
        })
    }

    // Runtime construction threads PTY geometry, host context, launch policy, and render hooks.
    #[allow(clippy::too_many_arguments)]
    fn spawn_command_builder(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<RenderSignal>,
        cmd: CommandBuilder,
        spawn_error_message: &'static str,
        initial_state: SpawnInitialState<'_>,
    ) -> std::io::Result<Self> {
        crate::logging::pane_spawn_started(pane_id.raw(), rows, cols, scrollback_limit_bytes);

        let (response_tx, _response_rx) = mpsc::channel::<Bytes>(1);
        let mut terminal = crate::ghostty::Terminal::new(cols, rows, scrollback_limit_bytes)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if crate::kitty_graphics::is_enabled() {
            terminal
                .enable_kitty_graphics()
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        let pane_terminal = GhosttyPaneTerminal::new(terminal, response_tx.clone())?;
        pane_terminal.apply_host_terminal_theme(host_terminal_theme);
        let _ = pane_terminal.apply_host_terminal_appearance(host_terminal_appearance);
        if let Some(ansi) = initial_state.history_ansi {
            pane_terminal.seed_history_ansi(ansi);
        }
        let terminal = Arc::new(PaneTerminal::new(pane_terminal));
        let kitty_keyboard_flags = Arc::new(AtomicU16::new(0));

        let spawned = crate::pty::backend::spawn_with_portable_pty(rows, cols, cmd)
            .inspect_err(|err| error!(pane = pane_id.raw(), err = %err, "{spawn_error_message}"))?;

        // --- Child watcher task ---
        let child_pid = Arc::new(AtomicU32::new(0));
        let child_start_time = Arc::new(AtomicU64::new(0));
        let reported_cwd = Arc::new(Mutex::new(None));
        let child_wait_completed = Arc::new(AtomicBool::new(false));
        let detection_content_seq = Arc::new(AtomicU64::new(0));
        let full_lifecycle_authority_active = Arc::new(AtomicBool::new(false));
        let pending_process_exits = Arc::new(Mutex::new(Vec::new()));
        let process_observation = Arc::new(Mutex::new(None));
        {
            let child_pid = child_pid.clone();
            let child_start_time = child_start_time.clone();
            let child_wait_completed = child_wait_completed.clone();
            let events = events.clone();
            let rt = tokio::runtime::Handle::current();
            let mut child = spawned.child;
            if let Some(pid) = child.process_id() {
                // The start time goes first: ADR 0014's principal is the pair,
                // and a reader that has seen the pid must never find the pane
                // root as a bare pid it could match a reused one against.
                child_start_time.store(
                    crate::platform::process_start_time(pid).unwrap_or(0),
                    Ordering::Release,
                );
                child_pid.store(pid, Ordering::Release);
                crate::logging::pane_spawned(pane_id.raw(), pid);
            }
            tokio::task::spawn_blocking(move || {
                match child.wait() {
                    Ok(status) => {
                        let status_text = format!("{status:?}");
                        crate::logging::pane_exited(pane_id.raw(), &status_text);
                    }
                    Err(e) => crate::logging::pane_exit_failed(pane_id.raw(), &e.to_string()),
                }
                child_wait_completed.store(true, Ordering::Release);
                // Use blocking send — PaneDied is critical, must not be dropped
                if let Err(e) = rt.block_on(events.send(AppEvent::PaneDied { pane_id })) {
                    error!(pane = pane_id.raw(), err = %e, "failed to send PaneDied event");
                }
            });
        }

        let io = {
            let terminal = terminal.clone();
            let response_writer = response_tx.clone();
            let render_notify = render_notify.clone();
            let render_dirty = render_dirty.clone();
            let detection_content_seq = detection_content_seq.clone();
            let child_pid = child_pid.clone();
            let events = events.clone();
            let reported_cwd = reported_cwd.clone();
            let rt = tokio::runtime::Handle::current();
            let on_read = Box::new(move |bytes: &[u8]| {
                let shell_pid = child_pid.load(Ordering::Acquire);
                let result =
                    terminal.process_pty_bytes(pane_id, shell_pid, bytes, &response_writer);
                observe_detection_content_change(bytes, &detection_content_seq);
                if result.request_render && render_dirty.request_pty(pane_id) {
                    render_notify.notify_one();
                }
                if let Some(delay) = result.render_delay {
                    let render_notify = render_notify.clone();
                    let render_dirty = render_dirty.clone();
                    rt.spawn(async move {
                        tokio::time::sleep(delay).await;
                        if render_dirty.request_pty(pane_id) {
                            render_notify.notify_one();
                        }
                    });
                }
                if let Some(cwd) = result.reported_cwd.clone() {
                    publish_reported_cwd(pane_id, cwd, &reported_cwd, &events);
                }
                for content in result.clipboard_writes {
                    if let Err(err) = events.try_send(AppEvent::ClipboardWrite { content }) {
                        warn!(
                            pane = pane_id.raw(),
                            err = %err,
                            "failed to send OSC 52 clipboard write"
                        );
                    }
                }
                PtyReadResult {
                    terminal_responses: result.terminal_responses,
                }
            });
            PaneRuntimeIo::Actor(PtyIoActor::spawn(PtyIoActorConfig {
                pane_id: pane_id.raw(),
                master_fd: spawned.master_fd,
                initially_quiesced: false,
                on_read,
                on_reader_exit: None,
            })?)
        };

        // --- Detection task ---
        let (detect_handle, detect_reset_notify, pending_release) = {
            use crate::detect;
            use std::time::{Duration, Instant};

            const TICK_UNIDENTIFIED: Duration = Duration::from_millis(500);
            const TICK_IDENTIFIED: Duration = Duration::from_millis(300);
            const TICK_PENDING_RELEASE: Duration = Duration::from_millis(50);

            let child_pid = child_pid.clone();
            let terminal = terminal.clone();
            let state_events = DetectionEventSender {
                sender: events.clone(),
                pending_exits: pending_process_exits.clone(),
                process_observation: process_observation.clone(),
            };
            let detection_content_seq = detection_content_seq.clone();
            let full_lifecycle_authority_active_for_task = full_lifecycle_authority_active.clone();
            let render_notify = render_notify.clone();
            let render_dirty = render_dirty.clone();
            let detect_reset_notify = Arc::new(Notify::new());
            let detect_reset = detect_reset_notify.clone();
            let pending_release = Arc::new(Mutex::new(None));
            let pending_release_for_task = pending_release.clone();

            let handle = tokio::spawn(async move {
                let mut agent_presence =
                    AgentDetectionPresence::from_agent(initial_state.detected_agent);
                let mut state = AgentState::Idle;
                let mut last_visible_idle = initial_state.detected_agent.is_some();
                let mut last_process_check = Instant::now();
                let mut last_foreground_pgid = None;
                let mut has_process_probe = false;
                let mut acquisition_started_at = None;
                let mut last_content_change_at = None;
                let mut pending_foreground_shell_clear = false;
                let mut foreground_shell_exit_reported = false;
                let mut release_was_active = false;
                let mut pending_restore_probe = initial_state.detected_agent.is_some();
                let mut last_visible_blocker = false;
                let mut last_visible_working = false;
                let mut last_visible_signal_refresh = None;
                let mut last_detection_text = String::new();
                let mut last_screen_scan_detection_content_seq = None;
                let mut agent_startup_grace_until = None;
                let mut pending_idle = PendingIdleConfirmation::default();

                tokio::time::sleep(Duration::from_millis(50)).await;

                loop {
                    let now_for_tick = Instant::now();
                    let tick = if active_pending_release(&pending_release_for_task, now_for_tick)
                        .is_some()
                        || terminal.has_transient_default_color_override()
                    {
                        TICK_PENDING_RELEASE
                    } else if pending_idle.active() {
                        AGENT_PENDING_IDLE_RECHECK
                    } else if agent_presence.current_agent().is_none() {
                        TICK_UNIDENTIFIED
                    } else {
                        TICK_IDENTIFIED
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(tick) => {}
                        _ = detect_reset.notified() => {
                            record_process_observation(&state_events.process_observation, None, Instant::now());
                            agent_presence = AgentDetectionPresence::from_agent(None);
                            state = AgentState::Unknown;
                            last_visible_idle = false;
                            last_foreground_pgid = None;
                            has_process_probe = false;
                            acquisition_started_at = None;
                            last_content_change_at = None;
                            pending_foreground_shell_clear = false;
                            foreground_shell_exit_reported = false;
                            release_was_active = false;
                            pending_restore_probe = false;
                            last_visible_blocker = false;
                            last_visible_working = false;
                            last_visible_signal_refresh = None;
                            last_detection_text.clear();
                            last_screen_scan_detection_content_seq = None;
                            agent_startup_grace_until = None;
                            pending_idle.clear();
                        }
                    }

                    let now = Instant::now();
                    let suppressed_agent = active_pending_release(&pending_release_for_task, now);
                    if suppressed_agent.is_none() && release_was_active {
                        has_process_probe = false;
                        acquisition_started_at = None;
                        last_content_change_at = None;
                    }
                    release_was_active = suppressed_agent.is_some();
                    let pid = child_pid.load(Ordering::Acquire);
                    let mut agent = agent_presence.current_agent();
                    let lifecycle_authority_active =
                        full_lifecycle_authority_active_for_task.load(Ordering::Acquire);
                    let foreground_pgid = (pid > 0)
                        .then(|| detect::foreground_process_group_id(pid))
                        .flatten();
                    let process_group_changed =
                        foreground_group_changed(foreground_pgid, last_foreground_pgid);
                    let should_check_process = pid > 0 && {
                        let process_probe_input = ProcessProbeInput {
                            current_agent: agent,
                            suppressed_agent,
                            foreground_pgid,
                            last_foreground_pgid,
                            has_process_probe,
                            acquisition_age: acquisition_started_at
                                .map(|started| now.duration_since(started)),
                            pending_foreground_shell_clear,
                            pending_restore_probe,
                            elapsed_since_process_check: now.duration_since(last_process_check),
                        };
                        !should_skip_process_probe_for_lifecycle_authority(
                            lifecycle_authority_active,
                            process_probe_input,
                        ) && should_probe_foreground_job(process_probe_input)
                    };

                    let mut agent_changed = false;
                    let mut fresh_process_agent = None;
                    if should_check_process {
                        last_process_check = now;
                        let had_process_probe = has_process_probe;
                        has_process_probe = true;
                        if pid > 0 {
                            let probe = probe_foreground_process(pid, foreground_pgid);
                            let process_name = probe.process_name;
                            let process_group_id = probe.process_group_id;
                            let tracked_process_group_id = process_group_for_change_tracking(
                                foreground_pgid,
                                process_group_id,
                            );
                            let foreground_is_pane_shell = probe.foreground_is_pane_shell;
                            let mut new_agent = probe.agent;

                            if let Some(suppressed_agent) = suppressed_agent {
                                if new_agent == Some(suppressed_agent) {
                                    new_agent = None;
                                } else if let Ok(mut pending_release) =
                                    pending_release_for_task.lock()
                                {
                                    *pending_release = None;
                                }
                            }

                            state_events.record_foreground_probe(
                                foreground_pgid,
                                process_group_id,
                                new_agent,
                                now,
                            );
                            let previous_agent = agent_presence.current_agent();
                            fresh_process_agent = new_agent;
                            let changed = match foreground_shell_agent_action(
                                previous_agent,
                                new_agent,
                                foreground_is_pane_shell,
                                foreground_shell_exit_reported,
                            ) {
                                ForegroundShellAgentAction::ReportProcessExit => {
                                    pending_foreground_shell_clear = true;
                                    false
                                }
                                ForegroundShellAgentAction::ClearAgent => {
                                    pending_foreground_shell_clear = false;
                                    foreground_shell_exit_reported = false;
                                    agent_presence.clear_current_agent()
                                }
                                ForegroundShellAgentAction::ObserveProbe => {
                                    pending_foreground_shell_clear = false;
                                    foreground_shell_exit_reported = false;
                                    agent_presence.observe_process_probe(new_agent)
                                }
                            };
                            last_foreground_pgid = tracked_process_group_id;
                            if new_agent.is_some() {
                                acquisition_started_at = None;
                                last_content_change_at = None;
                            } else if agent_presence.current_agent().is_none()
                                && had_process_probe
                                && process_group_changed
                            {
                                acquisition_started_at = Some(now);
                            }
                            pending_restore_probe = false;
                            if changed {
                                agent = agent_presence.current_agent();
                                if agent != previous_agent {
                                    pending_idle.clear();
                                    last_screen_scan_detection_content_seq = None;
                                    // A new foreground agent must not inherit OSC
                                    // title/progress evidence from the previous process.
                                    terminal.clear_agent_osc_state();
                                    if agent.is_some() {
                                        agent_startup_grace_until =
                                            Some(now + AGENT_STARTUP_GRACE_WINDOW);
                                        state = AgentState::Idle;
                                        last_visible_idle = true;
                                        last_visible_blocker = false;
                                        last_visible_working = false;
                                        last_visible_signal_refresh = None;
                                        publish_state_changed_event(
                                            state_events.clone(),
                                            pane_id,
                                            agent,
                                            AgentState::Idle,
                                            false,
                                            false,
                                            false,
                                            now,
                                        )
                                        .await;
                                    } else {
                                        agent_startup_grace_until = None;
                                    }
                                }
                                if let Some(process_name) = process_name {
                                    info!(
                                        pane = pane_id.raw(),
                                        previous_agent = ?previous_agent,
                                        ?agent,
                                        process = %process_name,
                                        pgid = ?process_group_id,
                                        "agent changed"
                                    );
                                } else {
                                    info!(
                                        pane = pane_id.raw(),
                                        previous_agent = ?previous_agent,
                                        ?agent,
                                        pgid = ?process_group_id,
                                        "agent changed"
                                    );
                                }
                                agent_changed = true;
                            }
                        }
                    }

                    let pid = child_pid.load(Ordering::Acquire);
                    // Keep the terminal restore side effect separate from render notification state.
                    #[allow(clippy::collapsible_if)]
                    if pid > 0 && terminal.maybe_restore_host_terminal_theme(pane_id, pid) {
                        if render_dirty.request_pty(pane_id) {
                            render_notify.notify_one();
                        }
                    }

                    let process_exited = pending_foreground_shell_clear
                        && agent.is_some()
                        && !foreground_shell_exit_reported;

                    if skip_screen_detection_under_hook_authority(
                        state_events.clone(),
                        pane_id,
                        fresh_process_agent.filter(|_| !agent_changed),
                        now,
                        lifecycle_authority_active,
                        process_exited,
                    )
                    .await
                    {
                        pending_idle.clear();
                        continue;
                    }

                    if let Some(until) = agent_startup_grace_until {
                        if process_exited {
                            agent_startup_grace_until = None;
                            last_screen_scan_detection_content_seq = None;
                            pending_idle.clear();
                        } else {
                            if now < until {
                                pending_idle.clear();
                                continue;
                            }
                            agent_startup_grace_until = None;
                            pending_idle.clear();
                            continue;
                        }
                    }

                    let current_detection_content_seq = if agent.is_some() {
                        Some(detection_content_seq.load(Ordering::Relaxed))
                    } else {
                        None
                    };
                    match decide_detection_screen_read(DetectionScreenReadInput {
                        state,
                        agent,
                        pending_idle_active: pending_idle.active(),
                        agent_changed,
                        process_exited,
                        current_detection_content_seq,
                        last_screen_scan_detection_content_seq,
                    }) {
                        DetectionScreenReadDecision::Read => {}
                        DetectionScreenReadDecision::Skip => continue,
                    }

                    let content = terminal.detection_text();
                    last_screen_scan_detection_content_seq = current_detection_content_seq;
                    let content_changed = content != last_detection_text;
                    last_detection_text.clone_from(&content);
                    if detect::should_skip_state_update(agent, &content) {
                        pending_idle.clear();
                        continue;
                    }
                    sync_content_change_acquisition(
                        agent_presence.current_agent(),
                        suppressed_agent,
                        process_group_changed,
                        content_changed,
                        now,
                        &mut acquisition_started_at,
                        &mut last_content_change_at,
                    );

                    let osc_title = terminal.agent_osc_title();
                    let osc_progress = terminal.agent_osc_progress();
                    let unwrapped_content = terminal.detection_unwrapped_text();
                    let Some(screen_detection) = detection_update_for_publish_with_osc(
                        agent,
                        &content,
                        Some(&unwrapped_content),
                        &osc_title,
                        &osc_progress,
                        process_exited,
                    ) else {
                        pending_idle.clear();
                        continue;
                    };
                    match decide_screen_detection_publish(
                        ScreenDetectionPublishInput {
                            screen_detection,
                            current_state: state,
                            last_visible_idle,
                            last_visible_blocker,
                            last_visible_working,
                            last_visible_signal_refresh,
                            process_exited,
                            agent_changed,
                            now,
                        },
                        &mut pending_idle,
                    ) {
                        DetectionPublishDecision::NoPublish => {}
                        DetectionPublishDecision::Publish {
                            state: new_state,
                            visible_idle,
                            visible_blocker,
                            visible_working,
                            process_exited: publish_process_exited,
                        } => {
                            apply_agent_detection_publish_update(
                                state_events.clone(),
                                pane_id,
                                agent,
                                AgentDetectionPublishUpdate {
                                    state: new_state,
                                    visible_idle,
                                    visible_blocker,
                                    visible_working,
                                    process_exited: publish_process_exited,
                                },
                                now,
                                &mut state,
                                &mut last_visible_idle,
                                &mut last_visible_blocker,
                                &mut last_visible_working,
                                &mut last_visible_signal_refresh,
                                &mut foreground_shell_exit_reported,
                            )
                            .await;
                        }
                    }
                }
            });
            (handle.abort_handle(), detect_reset_notify, pending_release)
        };

        Ok(Self {
            pane_id,
            terminal,
            io,
            current_size: Cell::new((rows, cols, 0, 0)),
            child_pid,
            child_start_time,
            reported_cwd,
            child_wait_completed: Some(child_wait_completed),
            kitty_keyboard_flags,
            detection_content_seq,
            full_lifecycle_authority_active,
            pending_process_exits,
            process_observation,
            detect_reset_notify,
            pending_release,
            preserve_processes_on_drop: false,
            detect_handle,
        })
    }

    pub fn begin_graceful_release(&self, agent: Agent) {
        record_process_observation(&self.process_observation, None, std::time::Instant::now());
        if let Ok(mut pending_release) = self.pending_release.lock() {
            *pending_release = Some(PendingAgentRelease {
                agent,
                until: std::time::Instant::now() + RELEASE_REACQUIRE_SUPPRESSION,
            });
        }
        self.detect_reset_notify.notify_one();
    }

    pub fn reset_agent_detection(&self) {
        record_process_observation(&self.process_observation, None, std::time::Instant::now());
        self.detect_reset_notify.notify_one();
    }

    pub(crate) fn foreground_process_observation(&self) -> Option<ForegroundProcessObservation> {
        if self
            .child_wait_completed
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            return None;
        }
        *self.process_observation.lock().ok()?
    }

    #[cfg(test)]
    pub(crate) fn agent_detection_reset_notify_for_test(&self) -> Arc<Notify> {
        self.detect_reset_notify.clone()
    }

    pub fn set_full_lifecycle_authority_active(&self, active: bool) {
        let previous = self
            .full_lifecycle_authority_active
            .swap(active, Ordering::AcqRel);
        if active && !previous {
            self.detect_reset_notify.notify_one();
        }
    }

    pub(crate) fn current_size(&self) -> (u16, u16) {
        let (rows, cols, _, _) = self.current_size.get();
        (rows, cols)
    }

    /// Resize if the dimensions actually changed.
    pub fn resize(&self, rows: u16, cols: u16, cell_width_px: u32, cell_height_px: u32) {
        let rows = rows.max(2);
        let cols = cols.max(4);
        let size = (rows, cols, cell_width_px, cell_height_px);
        if self.current_size.get() == size {
            return;
        }
        self.current_size.set(size);
        let terminal_responses = self
            .terminal
            .resize(rows, cols, cell_width_px, cell_height_px);
        mark_detection_content_changed(&self.detection_content_seq);
        self.io.resize(
            rows,
            cols,
            cell_width_px,
            cell_height_px,
            terminal_responses,
        );
    }

    pub fn nudge_child_redraw_after_handoff(&self) {
        let (rows, cols, cell_width_px, cell_height_px) = self.current_size.get();
        self.io
            .nudge_child_redraw_after_handoff(rows, cols, cell_width_px, cell_height_px);
    }

    /// Scroll up by N lines (into scrollback history).
    pub fn scroll_up(&self, lines: usize) {
        self.terminal.scroll_up(lines);
    }

    /// Scroll down by N lines (toward live output).
    pub fn scroll_down(&self, lines: usize) {
        self.terminal.scroll_down(lines);
    }

    /// Reset scroll to live view (offset = 0).
    pub fn scroll_reset(&self) {
        self.terminal.scroll_reset();
    }

    /// Set scrollback offset measured from the live bottom of the terminal.
    pub fn set_scroll_offset_from_bottom(&self, lines: usize) {
        self.terminal.set_scroll_offset_from_bottom(lines);
    }

    pub fn scroll_metrics(&self) -> Option<ScrollMetrics> {
        self.terminal.scroll_metrics()
    }

    pub(crate) fn search_text_matches(
        &self,
        query: &str,
        case_sensitive: bool,
    ) -> Vec<crate::pane::TerminalTextMatch> {
        self.terminal.search_text_matches(query, case_sensitive)
    }

    pub(crate) fn text_match_is_current(&self, text_match: crate::pane::TerminalTextMatch) -> bool {
        self.terminal.text_match_is_current(text_match)
    }

    pub(crate) fn text_matches_are_current(
        &self,
        text_matches: &[crate::pane::TerminalTextMatch],
    ) -> Vec<bool> {
        self.terminal.text_matches_are_current(text_matches)
    }

    pub(crate) fn word_motion_target(
        &self,
        row: u32,
        col: u16,
        motion: crate::pane::TerminalWordMotion,
    ) -> Option<crate::pane::TerminalTextPoint> {
        self.terminal.word_motion_target(row, col, motion)
    }

    pub fn input_state(&self) -> Option<InputState> {
        #[cfg(test)]
        AGGREGATE_INPUT_STATE_READS.set(AGGREGATE_INPUT_STATE_READS.get() + 1);
        self.terminal.input_state()
    }

    pub fn bracketed_paste_enabled(&self) -> bool {
        self.terminal.bracketed_paste_enabled()
    }

    pub fn focus_reporting_enabled(&self) -> bool {
        self.terminal.focus_reporting_enabled()
    }

    pub fn mouse_reporting_enabled(&self) -> bool {
        self.terminal.mouse_reporting_enabled()
    }

    pub fn plain_page_keys_use_host_scrollback(&self) -> Option<bool> {
        self.terminal.plain_page_keys_use_host_scrollback()
    }

    pub fn alternate_screen_active(&self) -> bool {
        self.terminal.alternate_screen_active()
    }

    pub fn cursor_state(&self, area: Rect, show_cursor: bool) -> Option<TerminalCursorState> {
        if !show_cursor {
            return None;
        }
        let cursor = self.terminal.cursor_state()?;
        if cursor.x >= area.width || cursor.y >= area.height {
            return None;
        }
        Some(TerminalCursorState {
            x: area.x + cursor.x,
            y: area.y + cursor.y,
            visible: cursor.visible,
            shape: cursor.shape,
        })
    }

    pub fn synchronized_output_active(&self) -> bool {
        self.terminal.synchronized_output_active()
    }

    pub fn visible_text(&self) -> String {
        self.terminal.visible_text()
    }

    pub fn visible_ansi(&self) -> String {
        self.terminal.visible_ansi()
    }

    pub fn detection_text(&self) -> String {
        self.terminal.detection_text()
    }

    pub fn detection_unwrapped_text(&self) -> String {
        self.terminal.detection_unwrapped_text()
    }

    pub fn agent_osc_title(&self) -> String {
        self.terminal.agent_osc_title()
    }

    pub fn agent_osc_progress(&self) -> String {
        self.terminal.agent_osc_progress()
    }

    pub fn recent_text(&self, lines: usize) -> String {
        self.terminal.recent_text(lines)
    }

    pub fn recent_ansi(&self, lines: usize) -> String {
        self.terminal.recent_ansi(lines)
    }

    pub fn recent_unwrapped_text(&self, lines: usize) -> String {
        self.terminal.recent_unwrapped_text(lines)
    }

    pub fn recent_unwrapped_ansi(&self, lines: usize) -> String {
        self.terminal.recent_unwrapped_ansi(lines)
    }

    pub fn snapshot_history(&self) -> Option<String> {
        let ansi = self.recent_unwrapped_ansi(usize::MAX);
        (!ansi.trim().is_empty()).then_some(ansi)
    }

    pub fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        self.terminal.extract_selection(selection)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, show_cursor: bool) {
        self.terminal.render(frame, area, show_cursor);
    }

    pub(crate) fn collect_dirty_patch(
        &self,
        area_width: u16,
        area_height: u16,
    ) -> TerminalDirtyPatchOutcome {
        self.terminal.collect_dirty_patch(area_width, area_height)
    }

    pub fn visible_hyperlinks(&self, area: Rect) -> Vec<((u16, u16), String, String)> {
        self.terminal.visible_hyperlinks(area)
    }

    pub fn kitty_image_placements_with_data_filter<F>(
        &self,
        needs_data: F,
    ) -> Vec<crate::ghostty::KittyImagePlacement>
    where
        F: FnMut(crate::ghostty::KittyImageDescriptor) -> bool,
    {
        self.terminal
            .kitty_image_placements_with_data_filter(needs_data)
    }

    pub fn keyboard_protocol(&self) -> crate::input::KeyboardProtocol {
        let fallback = crate::input::KeyboardProtocol::from_kitty_flags(
            self.kitty_keyboard_flags.load(Ordering::Relaxed),
        );
        self.terminal.keyboard_protocol(fallback)
    }

    pub fn encode_terminal_key(&self, key: crate::input::TerminalKey) -> Vec<u8> {
        self.terminal
            .encode_terminal_key(key, self.keyboard_protocol())
    }

    pub async fn send_bytes(&self, bytes: Bytes) -> Result<(), mpsc::error::SendError<Bytes>> {
        self.io.send_bytes(bytes).await
    }

    pub fn try_send_bytes(&self, bytes: Bytes) -> Result<(), mpsc::error::TrySendError<Bytes>> {
        self.io.try_send_bytes(bytes)
    }

    pub async fn send_paste(&self, text: String) -> Result<(), mpsc::error::SendError<Bytes>> {
        let bracketed = self.bracketed_paste_enabled();
        let payload = if bracketed {
            format!("\x1b[200~{text}\x1b[201~")
        } else {
            text
        };
        self.send_bytes(Bytes::from(payload)).await
    }

    pub fn try_send_focus_event(&self, event: crate::ghostty::FocusEvent) -> bool {
        if !self.focus_reporting_enabled() {
            return false;
        }

        let Ok(bytes) = crate::ghostty::encode_focus(event) else {
            return false;
        };
        if let Err(err) = self.try_send_bytes(Bytes::from(bytes)) {
            warn!(err = %err, ?event, "failed to forward pane focus event");
        }
        true
    }

    pub fn wheel_routing(&self) -> Option<WheelRouting> {
        self.terminal.wheel_routing()
    }

    pub fn encode_mouse_button(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        if !self.mouse_reporting_enabled() {
            return None;
        }
        self.terminal
            .encode_mouse_button(kind, column, row, modifiers)
    }

    pub fn encode_mouse_motion(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        self.terminal
            .encode_mouse_motion(kind, column, row, modifiers)
    }

    pub fn encode_mouse_wheel(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        if self.wheel_routing()? != WheelRouting::MouseReport {
            return None;
        }
        self.terminal
            .encode_mouse_wheel(kind, column, row, modifiers)
    }

    pub fn encode_alternate_scroll(
        &self,
        kind: crossterm::event::MouseEventKind,
    ) -> Option<Vec<u8>> {
        if self.wheel_routing()? != WheelRouting::AlternateScroll {
            return None;
        }
        let key = match kind {
            crossterm::event::MouseEventKind::ScrollUp => crossterm::event::KeyCode::Up,
            crossterm::event::MouseEventKind::ScrollDown => crossterm::event::KeyCode::Down,
            _ => return None,
        };
        Some(self.encode_terminal_key(crate::input::TerminalKey::new(
            key,
            crossterm::event::KeyModifiers::empty(),
        )))
    }

    /// Get the current working directory of the child shell process.
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        if let Some(cwd) = self
            .reported_cwd
            .lock()
            .ok()
            .and_then(|reported_cwd| reported_cwd.clone())
        {
            return Some(cwd);
        }
        let pid = self.child_pid.load(Ordering::Relaxed);
        crate::platform::process_cwd(pid)
    }

    /// PID of the child shell process driving this pane, if it is still running.
    pub fn child_pid(&self) -> Option<u32> {
        let pid = self.child_pid.load(Ordering::Acquire);
        (pid > 0).then_some(pid)
    }

    /// The start time captured for `child_pid()`, which together with the pid is
    /// ADR 0014's pane-root principal.
    ///
    /// Load the pid FIRST: the writer publishes this value before the pid, so a
    /// reader that has seen a pid is guaranteed to see the start time belonging
    /// to it. `None` means no start time was ever captured, and the pane-tree
    /// check refuses rather than fall back to matching a bare pid.
    pub fn child_start_time(&self) -> Option<u64> {
        let start_time = self.child_start_time.load(Ordering::Acquire);
        (start_time > 0).then_some(start_time)
    }

    /// Get the current working directory of the process group controlling the pane PTY.
    pub fn foreground_cwd(&self) -> Option<std::path::PathBuf> {
        let pid = self.child_pid.load(Ordering::Acquire);
        let shell_cwd = absolute_process_cwd(pid);
        let foreground_pgid = self
            .io
            .foreground_process_group_id()
            .or_else(|| crate::platform::foreground_process_group_id(pid));
        let leader_cwd = foreground_pgid.and_then(absolute_process_cwd);

        if leader_cwd.as_ref() == shell_cwd.as_ref() {
            foreground_member_cwd_different_from_shell(pid, shell_cwd.as_ref()).or(leader_cwd)
        } else {
            leader_cwd
                .or_else(|| foreground_member_cwd_different_from_shell(pid, shell_cwd.as_ref()))
        }
    }
}

#[cfg(test)]
impl PaneRuntime {
    pub(crate) fn test_publish_reported_cwd(&self, cwd: std::path::PathBuf) {
        let (events, _rx) = mpsc::channel(1);
        publish_reported_cwd(self.pane_id, cwd, &self.reported_cwd, &events);
    }

    pub(crate) fn test_record_foreground_probe(
        &self,
        native_group: Option<u32>,
        probed_group: Option<u32>,
        agent: Option<Agent>,
        observed_at: std::time::Instant,
    ) {
        let (sender, _rx) = mpsc::channel(1);
        DetectionEventSender {
            sender,
            pending_exits: self.pending_process_exits.clone(),
            process_observation: self.process_observation.clone(),
        }
        .record_foreground_probe(native_group, probed_group, agent, observed_at);
    }

    pub(crate) async fn test_publish_process_exit(
        &self,
        tx: mpsc::Sender<AppEvent>,
        pane_id: PaneId,
        agent: Agent,
        observed_at: std::time::Instant,
    ) {
        publish_state_changed_event(
            DetectionEventSender {
                sender: tx,
                pending_exits: self.pending_process_exits.clone(),
                process_observation: self.process_observation.clone(),
            },
            pane_id,
            Some(agent),
            AgentState::Idle,
            false,
            false,
            true,
            observed_at,
        )
        .await;
    }

    pub(crate) fn test_with_channel(cols: u16, rows: u16) -> (Self, mpsc::Receiver<Bytes>) {
        Self::test_with_channel_and_scrollback_bytes(cols, rows, 0, &[], 4)
    }

    pub(crate) fn test_with_channel_capacity(
        cols: u16,
        rows: u16,
        capacity: usize,
    ) -> (Self, mpsc::Receiver<Bytes>) {
        Self::test_with_channel_and_scrollback_bytes(cols, rows, 0, &[], capacity)
    }

    pub(crate) fn test_with_screen_bytes(cols: u16, rows: u16, bytes: &[u8]) -> Self {
        Self::test_with_scrollback_bytes(cols, rows, 0, bytes)
    }

    pub(crate) fn test_process_pty_bytes(&self, bytes: &[u8]) {
        let (tx, _rx) = mpsc::channel(1);
        let _ = self.terminal.process_pty_bytes(self.pane_id, 0, bytes, &tx);
    }

    pub(crate) fn test_with_scrollback_bytes(
        cols: u16,
        rows: u16,
        scrollback_limit_bytes: usize,
        bytes: &[u8],
    ) -> Self {
        Self::test_with_channel_and_scrollback_bytes(cols, rows, scrollback_limit_bytes, bytes, 4).0
    }

    pub(crate) fn test_with_channel_and_scrollback_bytes(
        cols: u16,
        rows: u16,
        scrollback_limit_bytes: usize,
        bytes: &[u8],
        channel_capacity: usize,
    ) -> (Self, mpsc::Receiver<Bytes>) {
        let (tx, rx) = mpsc::channel(channel_capacity);
        let (resize_tx, _resize_rx) = watch::channel((rows, cols, 0, 0));
        let mut terminal =
            crate::ghostty::Terminal::new(cols, rows, scrollback_limit_bytes).unwrap();
        terminal.write(bytes);

        (
            Self {
                pane_id: PaneId::from_raw(0),
                terminal: Arc::new(PaneTerminal::new(
                    GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
                )),
                io: PaneRuntimeIo::TestChannel {
                    sender: tx,
                    resize_tx,
                },
                current_size: Cell::new((rows, cols, 0, 0)),
                child_pid: Arc::new(AtomicU32::new(0)),
                child_start_time: Arc::new(AtomicU64::new(0)),
                reported_cwd: Arc::new(Mutex::new(None)),
                child_wait_completed: None,
                kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
                detection_content_seq: Arc::new(AtomicU64::new(0)),
                full_lifecycle_authority_active: Arc::new(AtomicBool::new(false)),
                pending_process_exits: Arc::new(Mutex::new(Vec::new())),
                process_observation: Arc::new(Mutex::new(None)),
                detect_reset_notify: Arc::new(Notify::new()),
                pending_release: Arc::new(Mutex::new(None)),
                preserve_processes_on_drop: true,
                detect_handle: tokio::spawn(async {}).abort_handle(),
            },
            rx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_process_slot_orders_capture_time_and_equal_time_loss() {
        let now = std::time::Instant::now();
        for reverse in [false, true] {
            for other in [None, Some(Agent::Pi)] {
                let slot = Mutex::new(None);
                let mut agents = [Some(Agent::Claude), other];
                if reverse {
                    agents.reverse();
                }
                for agent in agents {
                    record_process_observation(&slot, agent, now);
                }
                assert_eq!(slot.lock().unwrap().unwrap().agent, None);
                record_process_observation(&slot, Some(Agent::Claude), now);
                assert_eq!(
                    slot.lock().unwrap().unwrap().agent,
                    None,
                    "equal proof revived loss"
                );
                record_process_observation(
                    &slot,
                    Some(Agent::Claude),
                    now - std::time::Duration::from_millis(1),
                );
                assert_eq!(slot.lock().unwrap().unwrap().observed_at, now);
                let newer = now + std::time::Duration::from_millis(1);
                record_process_observation(&slot, Some(Agent::Claude), newer);
                record_process_observation(&slot, None, now);
                assert_eq!(
                    *slot.lock().unwrap(),
                    Some(ForegroundProcessObservation {
                        agent: Some(Agent::Claude),
                        observed_at: newer,
                    })
                );
            }
        }
    }

    #[tokio::test]
    async fn owner_proof_requires_a_matching_native_group_not_an_inferred_group() {
        for (native, probed, agent, expected) in [
            (Some(7), Some(7), Some(Agent::Claude), Some(Agent::Claude)),
            (Some(7), Some(8), Some(Agent::Claude), None),
            (None, Some(7), Some(Agent::Claude), None),
            (None, None, Some(Agent::Claude), None),
            (Some(7), None, Some(Agent::Claude), None),
            (Some(7), Some(7), None, None),
        ] {
            let (sender, _rx) = mpsc::channel(1);
            let slot = Arc::new(Mutex::new(None));
            let events = DetectionEventSender {
                sender,
                pending_exits: Arc::new(Mutex::new(Vec::new())),
                process_observation: slot.clone(),
            };
            let capture = std::time::Instant::now();
            events.record_foreground_probe(native, probed, agent, capture);
            assert_eq!(
                *slot.lock().unwrap(),
                Some(ForegroundProcessObservation {
                    agent: expected,
                    observed_at: capture,
                }),
                "{native:?}/{probed:?}/{agent:?}"
            );
        }
    }

    #[tokio::test]
    async fn cached_screen_publication_cannot_supply_owner_process_proof() {
        let (sender, mut rx) = mpsc::channel(1);
        let slot = Arc::new(Mutex::new(None));
        let events = DetectionEventSender {
            sender,
            pending_exits: Arc::new(Mutex::new(Vec::new())),
            process_observation: slot.clone(),
        };
        let capture = std::time::Instant::now();
        for has_probe in [false, true] {
            if has_probe {
                events.record_foreground_probe(Some(7), Some(7), Some(Agent::Claude), capture);
            }
            let before = *slot.lock().unwrap();
            publish_state_changed_event(
                events.clone(),
                PaneId::alloc(),
                Some(Agent::Claude),
                AgentState::Idle,
                false,
                false,
                false,
                capture + std::time::Duration::from_secs(1),
            )
            .await;
            assert_eq!(
                *slot.lock().unwrap(),
                before,
                "screen publish refreshed proof"
            );
            rx.recv().await.unwrap();
        }
    }

    #[tokio::test]
    async fn process_exit_invalidates_owner_proof_before_waiting_for_queue_space() {
        let runtime = PaneRuntime::test_with_screen_bytes(80, 24, b"");
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(AppEvent::PaneDied {
            pane_id: PaneId::alloc(),
        })
        .unwrap();
        let capture = std::time::Instant::now();
        runtime.test_record_foreground_probe(Some(7), Some(7), Some(Agent::Claude), capture);
        assert_eq!(
            runtime.foreground_process_observation().unwrap().agent,
            Some(Agent::Claude)
        );
        let exit_at = capture + std::time::Duration::from_millis(1);
        let publish =
            runtime.test_publish_process_exit(tx, PaneId::alloc(), Agent::Claude, exit_at);
        tokio::pin!(publish);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut publish)
                .await
                .is_err()
        );
        assert_eq!(
            runtime.foreground_process_observation(),
            Some(ForegroundProcessObservation {
                agent: None,
                observed_at: exit_at,
            }),
            "exit was hidden behind a full event queue"
        );
        rx.recv().await.unwrap();
        publish.await;
    }

    #[tokio::test]
    async fn owner_process_proof_is_invalidated_by_release_reset_wait_and_poison() {
        let (mut runtime, _rx) = PaneRuntime::test_with_channel(80, 24);
        assert_eq!(runtime.foreground_process_observation(), None);
        for release in [false, true] {
            let now = std::time::Instant::now();
            runtime.test_record_foreground_probe(Some(7), Some(7), Some(Agent::Claude), now);
            assert_eq!(
                runtime.foreground_process_observation().unwrap().agent,
                Some(Agent::Claude)
            );
            if release {
                runtime.begin_graceful_release(Agent::Claude);
            } else {
                runtime.reset_agent_detection();
            }
            assert_eq!(
                runtime.foreground_process_observation().unwrap().agent,
                None
            );
            runtime.test_record_foreground_probe(Some(7), Some(7), Some(Agent::Claude), now);
            assert_eq!(
                runtime.foreground_process_observation().unwrap().agent,
                None,
                "in-flight pre-reset probe revived"
            );
        }
        runtime.test_record_foreground_probe(
            Some(7),
            Some(7),
            Some(Agent::Claude),
            std::time::Instant::now(),
        );
        let wait = Arc::new(AtomicBool::new(false));
        runtime.child_wait_completed = Some(wait.clone());
        assert_eq!(
            runtime.foreground_process_observation().unwrap().agent,
            Some(Agent::Claude)
        );
        wait.store(true, Ordering::Release);
        assert_eq!(runtime.foreground_process_observation(), None);
        wait.store(false, Ordering::Release);
        let slot = runtime.process_observation.clone();
        assert!(std::thread::spawn(move || {
            let _guard = slot.lock().unwrap();
            panic!("poison owner observation for refusal control");
        })
        .join()
        .is_err());
        assert_eq!(runtime.foreground_process_observation(), None);
        runtime.test_record_foreground_probe(
            Some(7),
            Some(7),
            Some(Agent::Claude),
            std::time::Instant::now(),
        );
        assert_eq!(runtime.foreground_process_observation(), None);
        let (fresh_runtime, _rx) = PaneRuntime::test_with_channel(80, 24);
        assert_eq!(fresh_runtime.foreground_process_observation(), None);
    }

    async fn runtime_with_native_probe_child() -> (PaneRuntime, mpsc::Receiver<AppEvent>) {
        let (events, rx) = mpsc::channel(16);
        let runtime = PaneRuntime::spawn_argv_command(
            PaneId::alloc(),
            24,
            80,
            std::env::current_dir().unwrap(),
            &["/usr/bin/sleep".into(), "30".into()],
            &PaneLaunchEnv::from_extra(vec![("ZYNK_AGENT".into(), "claude".into())]),
            0,
            crate::terminal_theme::TerminalTheme::default(),
            None,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        )
        .unwrap();
        (runtime, rx)
    }

    async fn wait_for_native_owner_proof(runtime: &PaneRuntime) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if runtime
                .foreground_process_observation()
                .is_some_and(|o| o.agent == Some(Agent::Claude))
            {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "real detector never published native owner evidence"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    async fn detector_reset_invalidates_without_a_new_probe(runtime: &PaneRuntime) {
        struct RestorePid<'a>(&'a AtomicU32, u32);
        impl Drop for RestorePid<'_> {
            fn drop(&mut self) {
                self.0.store(self.1, Ordering::Release);
            }
        }
        let first = runtime.foreground_process_observation().unwrap();
        // Stop probe eligibility, not the real child. Restore even on panic so
        // runtime teardown still owns and reaps that exact child.
        let restore = RestorePid(
            &runtime.child_pid,
            runtime.child_pid.swap(0, Ordering::AcqRel),
        );
        runtime.detect_reset_notify.notify_one();
        let reset = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if runtime
                    .foreground_process_observation()
                    .is_some_and(|o| o.agent.is_none() && o.observed_at > first.observed_at)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        drop(restore);
        reset.expect("detector reset left positive proof while no new probe was possible");
    }

    #[tokio::test]
    async fn spawn_detector_records_native_owner_process_proof() {
        let (runtime, _rx) = runtime_with_native_probe_child().await;
        wait_for_native_owner_proof(&runtime).await;
        let pid = runtime.child_pid.load(Ordering::Acquire);
        assert_eq!(
            crate::platform::process_agent_hint(pid),
            Some(Agent::Claude)
        );
        assert_eq!(crate::platform::foreground_process_group_id(pid), Some(pid));
        detector_reset_invalidates_without_a_new_probe(&runtime).await;
        runtime.shutdown();
    }

    #[tokio::test]
    async fn handoff_detector_records_native_owner_process_proof() {
        let (mut runtime, _rx) = runtime_with_native_probe_child().await;
        runtime.detect_handle.abort();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !runtime.detect_handle.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let (sender, _rx) = mpsc::channel(16);
        runtime.process_observation = Arc::new(Mutex::new(None));
        let (handle, reset, pending_release) = spawn_basic_detection_task(
            runtime.pane_id,
            runtime.child_pid.clone(),
            runtime.terminal.clone(),
            runtime.detection_content_seq.clone(),
            runtime.full_lifecycle_authority_active.clone(),
            DetectionEventSender {
                sender,
                pending_exits: runtime.pending_process_exits.clone(),
                process_observation: runtime.process_observation.clone(),
            },
        );
        runtime.detect_handle = handle;
        runtime.detect_reset_notify = reset;
        runtime.pending_release = pending_release;
        wait_for_native_owner_proof(&runtime).await;
        detector_reset_invalidates_without_a_new_probe(&runtime).await;
        runtime.shutdown();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn imported_runtime_reports_size_and_ignores_empty_clipboard_before_resize() {
        use std::{
            io::{Read, Write},
            os::{fd::IntoRawFd, unix::net::UnixStream},
        };

        let (socket, mut peer) = UnixStream::pair().unwrap();
        peer.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let pid = std::process::id();
        let import = crate::handoff_runtime::ImportedHandoffRuntime {
            master_fd: socket.into_raw_fd(),
            state: crate::handoff_runtime::HandoffRuntimeState {
                pane_id: 1,
                child_pid: pid,
                child_start_time: crate::platform::process_start_time(pid).unwrap(),
                rows: 24,
                cols: 80,
                cell_width_px: 9,
                cell_height_px: 18,
                keyboard_protocol_flags: 0,
                keyboard_protocol_ansi: None,
                input_state: None,
                initial_history_ansi: None,
            },
        };
        let (events, mut rx) = mpsc::channel(32);
        let runtime = PaneRuntime::from_handoff_fd(
            import,
            0,
            crate::terminal_theme::TerminalTheme::default(),
            None,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        )
        .unwrap();
        assert!(
            runtime.preserve_processes_on_drop,
            "fixture must never own the test process"
        );
        runtime.io.set_handoff_paused(false).unwrap();
        peer.write_all(b"\x1b]52;c;\x07\x1b]52;c;eA==\x1b\\\x1b[14t\x1b[16t\x1b[18t\x1b[5n")
            .unwrap();
        let expected = b"\x1b[4;432;720t\x1b[6;18;9t\x1b[8;24;80t\x1b[0n";
        let mut replies = vec![0; expected.len()];
        let result = peer.read_exact(&mut replies);
        let mut clipboard = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let AppEvent::ClipboardWrite { content } = event {
                clipboard.push(content);
            }
        }
        drop(runtime);
        drop(peer);

        result.expect("imported geometry is available before any later resize");
        assert_eq!(replies, expected);
        assert_eq!(
            clipboard,
            vec![b"x".to_vec()],
            "only the nonempty write may produce an event"
        );
    }

    struct CwdTestProbe {
        base: std::path::PathBuf,
        restricted: Option<(std::path::PathBuf, std::fs::Permissions)>,
        child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    }

    impl CwdTestProbe {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::var_os("ZYNK_TEST_ROOT")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            let base = root.join(format!("cwd-probe-{}-{stamp}", std::process::id()));
            std::fs::create_dir(&base).unwrap();
            Self {
                base,
                restricted: None,
                child: None,
            }
        }
    }

    impl Drop for CwdTestProbe {
        fn drop(&mut self) {
            if let Some((path, permissions)) = self.restricted.take() {
                if let Err(err) = std::fs::set_permissions(&path, permissions) {
                    eprintln!("failed to restore CWD probe permissions: {err}");
                }
            }
            if let Some(child) = self.child.as_mut() {
                let _ = child.kill();
                if let Err(err) = child.wait() {
                    eprintln!("failed to reap CWD probe child: {err}");
                }
            }
            if let Err(err) = std::fs::remove_dir_all(&self.base) {
                eprintln!("failed to remove CWD probe: {err}");
            }
        }
    }

    #[tokio::test]
    async fn cwd_returns_accepted_report_without_rechecking_filesystem() {
        let probe = CwdTestProbe::new();
        let cwd = probe.base.join("accepted");
        std::fs::create_dir(&cwd).unwrap();
        let (runtime, _rx) = PaneRuntime::test_with_channel(80, 24);
        let (events, mut event_rx) = mpsc::channel(1);
        publish_reported_cwd(runtime.pane_id, cwd.clone(), &runtime.reported_cwd, &events);
        assert_eq!(runtime.reported_cwd.lock().unwrap().as_ref(), Some(&cwd));
        assert!(matches!(
            event_rx.try_recv(),
            Ok(AppEvent::TerminalCwdReported { .. })
        ));
        std::fs::remove_dir(&cwd).unwrap();

        assert_eq!(runtime.cwd(), Some(cwd));
    }

    #[tokio::test]
    async fn reported_cwd_admission_still_rejects_relative_missing_and_file_paths() {
        let probe = CwdTestProbe::new();
        let file = probe.base.join("file");
        std::fs::write(&file, b"not a directory").unwrap();
        let (runtime, _rx) = PaneRuntime::test_with_channel(80, 24);
        let (events, mut event_rx) = mpsc::channel(4);
        for path in [
            std::path::PathBuf::from("."),
            probe.base.join("missing"),
            file,
        ] {
            publish_reported_cwd(runtime.pane_id, path, &runtime.reported_cwd, &events);
            assert!(runtime.reported_cwd.lock().unwrap().is_none());
            assert!(event_rx.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn foreground_cwd_does_not_require_traversing_the_directory_path() {
        use std::os::unix::fs::PermissionsExt;

        let mut probe = CwdTestProbe::new();
        let private = probe.base.join("private");
        let cwd = private.join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new("/usr/bin/sleep");
        command.arg("30");
        command.cwd(&cwd);
        probe.child = Some(pair.slave.spawn_command(command).unwrap());
        let pid = probe.child.as_ref().unwrap().process_id().unwrap();
        assert_eq!(crate::platform::foreground_process_group_id(pid), Some(pid));
        let expected = crate::platform::process_cwd(pid).unwrap();
        assert_eq!(expected, cwd);
        let (runtime, _rx) = PaneRuntime::test_with_channel(80, 24);
        runtime.child_pid.store(pid, Ordering::Release);
        assert_eq!(runtime.foreground_cwd(), Some(expected.clone()));
        let permissions = std::fs::metadata(&private).unwrap().permissions();
        probe.restricted = Some((private.clone(), permissions));
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o000)).unwrap();
        if cwd.is_dir() {
            eprintln!("UNEXERCISED: privileged process can traverse the restricted CWD");
            return;
        }

        assert_eq!(runtime.foreground_cwd(), Some(expected));
    }

    #[test]
    fn pane_launch_env_removes_outer_codex_thread_id() {
        // A pane spawned from inside a codex session must not inherit that
        // session's thread id: the nested codex hook keys its "am I nested?"
        // check off `CODEX_THREAD_ID` vs the session id in its hook payload.
        let mut cmd = CommandBuilder::new("shell");
        cmd.env(CODEX_THREAD_ID_ENV_VAR, "outer-session");

        apply_pane_launch_env(&mut cmd, &PaneLaunchEnv::default());

        assert!(cmd.get_env(CODEX_THREAD_ID_ENV_VAR).is_none());
    }

    #[test]
    fn pane_launch_env_exports_zynk_env_and_socket_without_identity() {
        // ADR 0010: a spawned pane gets the Zynk-branded `ZYNK_ENV` flag and the
        // `ZYNK_SOCKET_PATH` base env even when it carries no pane identity.
        use std::ffi::OsStr;
        let mut cmd = CommandBuilder::new("/bin/sh");
        apply_pane_launch_env(&mut cmd, &PaneLaunchEnv::default());
        assert_eq!(
            cmd.get_env(crate::ZYNK_ENV_VAR),
            Some(OsStr::new(crate::ZYNK_ENV_VALUE)),
            "spawned pane must export ZYNK_ENV"
        );
        let socket = crate::api::socket_path();
        assert_eq!(
            cmd.get_env(crate::api::ZYNK_SOCKET_PATH_ENV_VAR),
            Some(socket.as_os_str()),
            "spawned pane must export ZYNK_SOCKET_PATH"
        );
        // An identity-less launch env must not ADD the pane/tab/workspace triple.
        // Compare against a baseline command that inherits the same ambient env
        // (the test process may itself run inside a zynk pane) but had no launch
        // env applied, so only env keys `apply_pane_launch_env` injects differ.
        let baseline = CommandBuilder::new("/bin/sh");
        for var in [
            crate::integration::ZYNK_PANE_ID_ENV_VAR,
            crate::integration::ZYNK_TAB_ID_ENV_VAR,
            crate::integration::ZYNK_WORKSPACE_ID_ENV_VAR,
        ] {
            assert_eq!(
                cmd.get_env(var),
                baseline.get_env(var),
                "identity-less launch env must not inject {var}"
            );
        }
    }

    #[test]
    fn pane_launch_env_exports_the_running_binary_path() {
        // Agent panes spawn through `spawn`/`spawn_with_initial_history`,
        // `spawn_shell_command` and `spawn_argv_command`, and all four apply
        // `apply_pane_launch_env`. Hook assets that shell out to the CLI
        // (qodercli, hermes) read `ZYNK_BIN_PATH` from that env, so it has to
        // survive the composition, not just `apply_pane_base_env` in isolation.
        let mut cmd = CommandBuilder::new("/bin/sh");
        apply_pane_launch_env(&mut cmd, &PaneLaunchEnv::default());

        let executable = std::env::current_exe().expect("current_exe");
        assert_eq!(
            cmd.get_env("ZYNK_BIN_PATH"),
            Some(executable.as_os_str()),
            "an agent pane must export ZYNK_BIN_PATH"
        );
    }

    #[test]
    fn pane_launch_env_exports_identity_triple_and_extra() {
        // A launch env carrying an identity exports the Zynk-branded
        // `ZYNK_WORKSPACE_ID`/`ZYNK_TAB_ID`/`ZYNK_PANE_ID` triple plus caller extra env.
        use std::ffi::OsStr;
        let mut cmd = CommandBuilder::new("/bin/sh");
        let launch_env = PaneLaunchEnv::from_extra(vec![("FOO".to_string(), "bar".to_string())])
            .with_identity("w1".to_string(), "w1:t1".to_string(), "w1:p1".to_string());
        apply_pane_launch_env(&mut cmd, &launch_env);
        assert_eq!(cmd.get_env("FOO"), Some(OsStr::new("bar")));
        assert_eq!(
            cmd.get_env(crate::ZYNK_ENV_VAR),
            Some(OsStr::new(crate::ZYNK_ENV_VALUE))
        );
        assert_eq!(
            cmd.get_env(crate::integration::ZYNK_WORKSPACE_ID_ENV_VAR),
            Some(OsStr::new("w1")),
            "identity pane must export ZYNK_WORKSPACE_ID"
        );
        assert_eq!(
            cmd.get_env(crate::integration::ZYNK_TAB_ID_ENV_VAR),
            Some(OsStr::new("w1:t1")),
            "identity pane must export ZYNK_TAB_ID"
        );
        assert_eq!(
            cmd.get_env(crate::integration::ZYNK_PANE_ID_ENV_VAR),
            Some(OsStr::new("w1:p1")),
            "identity pane must export ZYNK_PANE_ID"
        );
    }

    #[test]
    fn shutdown_liveness_treats_reaped_direct_child_as_gone() {
        assert!(!process_alive_for_shutdown(42, 42, true, |_| true));
    }

    #[test]
    fn shutdown_liveness_keeps_unreaped_direct_child_alive() {
        assert!(process_alive_for_shutdown(42, 42, false, |_| true));
    }

    #[test]
    fn shutdown_liveness_keeps_other_session_processes_alive() {
        assert!(process_alive_for_shutdown(43, 42, true, |_| true));
    }

    #[test]
    fn shutdown_liveness_treats_missing_process_as_gone() {
        assert!(!process_alive_for_shutdown(43, 42, false, |_| false));
    }

    fn capture_shell_output(command: &str, extra_env: &[(&str, &str)]) -> String {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let output_path = std::env::temp_dir().join(format!(
            "zynk-pane-term-test-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg(format!("{command} > '{}'", output_path.display()));
        cmd.cwd(std::env::current_dir().unwrap());
        cmd.env("TERM", "xterm-ghostty");
        cmd.env("COLORTERM", "falsecolor");
        apply_pane_terminal_env(&mut cmd);
        for (key, value) in extra_env {
            cmd.env(key, value);
        }

        let mut child = pair.slave.spawn_command(cmd).unwrap();
        let status = child.wait().unwrap();
        assert!(status.success(), "shell command failed: {status:?}");

        let output = std::fs::read_to_string(&output_path).unwrap();
        let _ = std::fs::remove_file(output_path);
        output
    }

    #[test]
    fn pane_shell_prefers_configured_shell() {
        assert_eq!(
            pane_shell_from("/usr/bin/nu", Some("/bin/bash".to_string())),
            "/usr/bin/nu"
        );
    }

    #[test]
    fn pane_shell_falls_back_to_shell_env() {
        assert_eq!(
            pane_shell_from("", Some("/bin/bash".to_string())),
            "/bin/bash"
        );
    }

    #[test]
    fn pane_shell_ignores_empty_values() {
        assert_eq!(
            pane_shell_from("   ", Some("  ".to_string())),
            default_pane_shell()
        );
        assert_eq!(pane_shell_from("", None), default_pane_shell());
    }

    #[test]
    fn only_login_shell_mode_uses_a_login_shell() {
        assert!(shell_mode_uses_login_shell(
            crate::config::ShellModeConfig::Login
        ));
        assert!(!shell_mode_uses_login_shell(
            crate::config::ShellModeConfig::Auto
        ));
        assert!(!shell_mode_uses_login_shell(
            crate::config::ShellModeConfig::NonLogin
        ));
    }

    #[test]
    fn login_shell_builder_uses_default_prog_with_resolved_shell_env() {
        let cmd = pane_shell_command_builder(PaneShellConfig::new(
            "/bin/sh",
            crate::config::ShellModeConfig::Login,
        ))
        .unwrap();
        assert!(cmd.is_default_prog());
        assert_eq!(
            cmd.get_env("SHELL").and_then(std::ffi::OsStr::to_str),
            Some("/bin/sh")
        );
    }

    #[test]
    fn auto_shell_builder_keeps_direct_shell() {
        let cmd = pane_shell_command_builder(PaneShellConfig::new(
            "/bin/sh",
            crate::config::ShellModeConfig::Auto,
        ))
        .unwrap();
        assert!(!cmd.is_default_prog());
        assert_eq!(cmd.get_argv(), &[std::ffi::OsString::from("/bin/sh")]);
    }

    #[test]
    fn login_shell_builder_rejects_missing_shell_instead_of_falling_back() {
        let err = pane_shell_command_builder(PaneShellConfig::new(
            "/__zynk_missing_shell__",
            crate::config::ShellModeConfig::Login,
        ))
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn login_shell_builder_resolves_bare_shell_names_from_path() {
        let _lock = crate::integration::integration_env_lock();
        let base = std::env::temp_dir().join(format!(
            "zynk-login-shell-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let bin = base.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let shell = bin.join("fake-shell");
        std::fs::write(&shell, "#!/bin/sh\nexit 0\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let original_path = std::env::var_os("PATH");
        std::env::set_var("PATH", &bin);

        let cmd = pane_shell_command_builder(PaneShellConfig::new(
            "fake-shell",
            crate::config::ShellModeConfig::Login,
        ))
        .unwrap();

        assert!(cmd.is_default_prog());
        assert_eq!(
            cmd.get_env("SHELL").and_then(std::ffi::OsStr::to_str),
            shell.to_str()
        );
        match original_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn login_shell_resolution_preserves_shell_paths() {
        assert_eq!(resolve_shell_for_login_mode("/bin/sh").unwrap(), "/bin/sh");
    }

    #[test]
    fn non_login_shell_builder_execs_resolved_shell_directly() {
        let cmd = pane_shell_command_builder(PaneShellConfig::new(
            "/bin/sh",
            crate::config::ShellModeConfig::NonLogin,
        ))
        .unwrap();
        assert!(!cmd.is_default_prog());
        assert_eq!(cmd.get_argv(), &[std::ffi::OsString::from("/bin/sh")]);
    }

    #[test]
    fn pane_terminal_identity_overrides_outer_terminal_env() {
        let output = capture_shell_output("printf '%s\\n%s\\n' \"$TERM\" \"$COLORTERM\"", &[]);
        assert_eq!(output, "xterm-256color\ntruecolor\n");
    }

    #[test]
    fn pane_terminal_identity_allows_explicit_override() {
        let output = capture_shell_output(
            "printf '%s\\n%s\\n' \"$TERM\" \"$COLORTERM\"",
            &[("TERM", "vt100"), ("COLORTERM", "24bit")],
        );
        assert_eq!(output, "vt100\n24bit\n");
    }

    #[tokio::test]
    async fn handoff_history_ansi_captures_primary_screen() {
        let runtime =
            PaneRuntime::test_with_scrollback_bytes(40, 5, 4096, b"handoff-primary-history\r\n");

        let history = runtime.handoff_history_ansi().unwrap();

        assert!(history.contains("handoff-primary-history"));
    }

    #[tokio::test]
    async fn handoff_history_ansi_skips_alternate_screen() {
        let runtime = PaneRuntime::test_with_scrollback_bytes(
            40,
            5,
            4096,
            b"primary\r\n\x1b[?1049halt-screen",
        );

        assert!(runtime.handoff_history_ansi().is_none());
    }

    #[tokio::test]
    async fn handoff_runtime_state_captures_terminal_input_state() {
        let runtime = PaneRuntime::test_with_screen_bytes(
            80,
            24,
            b"\x1b[>5u\x1b[>4;2m\x1b[?1h\x1b[?2004h\x1b[?1004h\x1b[?1002h\x1b[?1006h\x1b[?2031h",
        );

        let pane = runtime.handoff_runtime_state(12);

        assert_eq!(pane.keyboard_protocol_flags, 5);
        assert_eq!(
            pane.input_state,
            Some(InputState {
                alternate_screen: false,
                application_cursor: true,
                bracketed_paste: true,
                focus_reporting: true,
                mouse_protocol_mode: crate::input::MouseProtocolMode::ButtonMotion,
                mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Sgr,
                mouse_alternate_scroll: true,
                modify_other_keys: true,
                color_scheme_reporting: true,
            })
        );
    }

    #[test]
    fn truncate_handoff_history_keeps_recent_utf8_boundary() {
        let history = format!("old\n{}\nrecent\n", "é".repeat(8));

        let truncated = truncate_handoff_history(history, 20);

        assert_eq!(truncated, "recent\n");
        assert!(truncated.is_char_boundary(0));
    }

    #[test]
    fn truncate_handoff_history_drops_partial_long_line() {
        let history = format!("old\n{}", "x".repeat(64));

        let truncated = truncate_handoff_history(history, 12);

        assert!(truncated.is_empty());
    }

    #[tokio::test]
    async fn focus_events_are_forwarded_when_enabled() {
        let (tx, mut rx) = mpsc::channel(4);
        let (resize_tx, _resize_rx) = watch::channel((80, 24, 0, 0));
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal
            .mode_set(crate::ghostty::MODE_FOCUS_EVENT, true)
            .unwrap();
        let runtime = PaneRuntime {
            pane_id: PaneId::from_raw(0),
            terminal: Arc::new(PaneTerminal::new(
                GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
            )),
            io: PaneRuntimeIo::TestChannel {
                sender: tx,
                resize_tx,
            },
            current_size: Cell::new((80, 24, 0, 0)),
            child_pid: Arc::new(AtomicU32::new(0)),
            child_start_time: Arc::new(AtomicU64::new(0)),
            reported_cwd: Arc::new(Mutex::new(None)),
            child_wait_completed: None,
            kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
            detection_content_seq: Arc::new(AtomicU64::new(0)),
            full_lifecycle_authority_active: Arc::new(AtomicBool::new(false)),
            pending_process_exits: Arc::new(Mutex::new(Vec::new())),
            process_observation: Arc::new(Mutex::new(None)),
            detect_reset_notify: Arc::new(Notify::new()),
            pending_release: Arc::new(Mutex::new(None)),
            preserve_processes_on_drop: true,
            detect_handle: tokio::spawn(async {}).abort_handle(),
        };

        assert!(runtime.try_send_focus_event(crate::ghostty::FocusEvent::Gained));
        assert_eq!(rx.recv().await.unwrap(), Bytes::from_static(b"\x1b[I"));
    }

    #[tokio::test]
    async fn focus_events_are_suppressed_when_disabled() {
        let (tx, mut rx) = mpsc::channel(4);
        let (resize_tx, _resize_rx) = watch::channel((80, 24, 0, 0));
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let runtime = PaneRuntime {
            pane_id: PaneId::from_raw(0),
            terminal: Arc::new(PaneTerminal::new(
                GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
            )),
            io: PaneRuntimeIo::TestChannel {
                sender: tx,
                resize_tx,
            },
            current_size: Cell::new((80, 24, 0, 0)),
            child_pid: Arc::new(AtomicU32::new(0)),
            child_start_time: Arc::new(AtomicU64::new(0)),
            reported_cwd: Arc::new(Mutex::new(None)),
            child_wait_completed: None,
            kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
            detection_content_seq: Arc::new(AtomicU64::new(0)),
            full_lifecycle_authority_active: Arc::new(AtomicBool::new(false)),
            pending_process_exits: Arc::new(Mutex::new(Vec::new())),
            process_observation: Arc::new(Mutex::new(None)),
            detect_reset_notify: Arc::new(Notify::new()),
            pending_release: Arc::new(Mutex::new(None)),
            preserve_processes_on_drop: true,
            detect_handle: tokio::spawn(async {}).abort_handle(),
        };

        assert!(!runtime.try_send_focus_event(crate::ghostty::FocusEvent::Gained));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), rx.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn subscribed_idle_child_receives_color_scheme_transition() {
        let (runtime, mut rx) = PaneRuntime::test_with_channel(80, 24);
        runtime.apply_host_terminal_appearance(Some(crate::terminal_theme::HostAppearance::Dark));
        runtime.test_process_pty_bytes(b"\x1b[?2031h");

        runtime.apply_host_terminal_appearance(Some(crate::terminal_theme::HostAppearance::Light));

        assert_eq!(rx.recv().await, Some(Bytes::from_static(b"\x1b[?997;2n")));
    }

    #[test]
    fn foreground_shell_reports_process_exit_before_clearing_agent() {
        assert_eq!(
            foreground_shell_agent_action(Some(Agent::Codex), None, true, false),
            ForegroundShellAgentAction::ReportProcessExit
        );
        assert_eq!(
            foreground_shell_agent_action(Some(Agent::Codex), None, true, true),
            ForegroundShellAgentAction::ClearAgent
        );
    }

    #[test]
    fn unknown_non_shell_foreground_job_is_not_immediate_clear_signal() {
        assert_eq!(
            foreground_shell_agent_action(Some(Agent::Claude), None, false, false),
            ForegroundShellAgentAction::ObserveProbe
        );
    }

    #[test]
    fn reported_process_exit_clears_before_unknown_foreground_probe() {
        assert_eq!(
            foreground_shell_agent_action(Some(Agent::Claude), None, false, true),
            ForegroundShellAgentAction::ClearAgent
        );
    }

    #[test]
    fn foreground_agent_job_is_not_clear_signal() {
        assert_eq!(
            foreground_shell_agent_action(Some(Agent::Claude), Some(Agent::OpenCode), true, false),
            ForegroundShellAgentAction::ObserveProbe
        );
    }

    fn foreground_process(pid: u32, name: &str) -> crate::platform::ForegroundProcess {
        crate::platform::ForegroundProcess {
            pid,
            name: name.to_string(),
            argv0: None,
            argv: None,
            cmdline: None,
        }
    }

    #[test]
    fn foreground_agent_hint_accepts_pane_shell_environment() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 42,
            processes: vec![foreground_process(42, "bash")],
        };

        assert_eq!(
            agent_hint_for_foreground_job_members(&job, |pid| {
                (pid == 42).then_some(Agent::Claude)
            }),
            Some(Agent::Claude)
        );
    }

    #[test]
    fn foreground_agent_hint_accepts_non_leader_foreground_process_environment() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 99,
            processes: vec![
                foreground_process(99, "fence"),
                foreground_process(100, "pi"),
            ],
        };

        assert_eq!(
            agent_hint_for_foreground_job_members(&job, |pid| {
                (pid == 100).then_some(Agent::Codex)
            }),
            Some(Agent::Codex)
        );
    }

    #[test]
    fn foreground_agent_hint_wins_over_process_name_detection() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 99,
            processes: vec![foreground_process(99, "codex")],
        };

        let result = probe_foreground_process_from_jobs(
            42,
            Some(99),
            Some(job),
            || None,
            |pid| (pid == 99).then_some(Agent::Claude),
        );

        assert_eq!(result.agent, Some(Agent::Claude));
        assert_eq!(result.process_name.as_deref(), Some("claude"));
    }

    #[test]
    fn foreground_agent_hint_on_inherited_child_environment_is_authoritative() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 99,
            processes: vec![foreground_process(99, "vim")],
        };

        let result = probe_foreground_process_from_jobs(
            42,
            Some(99),
            None,
            || Some(job),
            |pid| (pid == 99).then_some(Agent::Claude),
        );

        assert_eq!(result.agent, Some(Agent::Claude));
        assert_eq!(result.process_name.as_deref(), Some("claude"));
    }

    #[test]
    fn non_leader_agent_hint_does_not_override_identifiable_leader() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 99,
            processes: vec![
                foreground_process(99, "codex"),
                foreground_process(100, "vim"),
            ],
        };

        let result = probe_foreground_process_from_jobs(
            42,
            Some(99),
            None,
            || Some(job),
            |pid| (pid == 100).then_some(Agent::Claude),
        );

        assert_eq!(result.agent, Some(Agent::Codex));
        assert_eq!(result.process_name.as_deref(), Some("codex"));
    }

    #[test]
    fn non_leader_agent_hint_wins_when_leader_is_unidentified() {
        let job = crate::platform::ForegroundJob {
            process_group_id: 99,
            processes: vec![
                foreground_process(99, "some_vm"),
                foreground_process(100, "vim"),
            ],
        };

        let result = probe_foreground_process_from_jobs(
            42,
            Some(99),
            None,
            || Some(job),
            |pid| (pid == 100).then_some(Agent::Claude),
        );

        assert_eq!(result.agent, Some(Agent::Claude));
        assert_eq!(result.process_name.as_deref(), Some("claude"));
    }

    fn process_probe_input() -> ProcessProbeInput {
        ProcessProbeInput {
            current_agent: None,
            suppressed_agent: None,
            foreground_pgid: Some(42),
            last_foreground_pgid: Some(42),
            has_process_probe: true,
            acquisition_age: None,
            pending_foreground_shell_clear: false,
            pending_restore_probe: false,
            elapsed_since_process_check: std::time::Duration::from_secs(1),
        }
    }

    #[test]
    fn unchanged_unidentified_foreground_group_skips_full_process_probe() {
        assert!(!should_probe_foreground_job(process_probe_input()));
    }

    #[test]
    fn unidentified_foreground_group_change_runs_full_process_probe() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            foreground_pgid: Some(43),
            ..process_probe_input()
        }));
    }

    #[test]
    fn unidentified_pane_gets_initial_process_probe() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            has_process_probe: false,
            ..process_probe_input()
        }));
    }

    #[test]
    fn stable_unidentified_foreground_group_has_no_safety_process_probe() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            elapsed_since_process_check: PROCESS_RECHECK_MISSING_FOREGROUND_GROUP,
            ..process_probe_input()
        }));
    }

    #[test]
    fn unidentified_pane_without_foreground_group_uses_safety_process_probe() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            foreground_pgid: None,
            last_foreground_pgid: None,
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            foreground_pgid: None,
            last_foreground_pgid: None,
            elapsed_since_process_check: PROCESS_RECHECK_MISSING_FOREGROUND_GROUP,
            ..process_probe_input()
        }));
    }

    #[test]
    fn unidentified_pane_probes_when_foreground_group_disappears() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            foreground_pgid: None,
            last_foreground_pgid: Some(42),
            ..process_probe_input()
        }));
    }

    #[test]
    fn pending_shell_clear_and_restore_force_process_probes() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            pending_foreground_shell_clear: true,
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            pending_restore_probe: true,
            ..process_probe_input()
        }));
    }

    #[test]
    fn inferred_group_does_not_trigger_a_probe_on_every_tick() {
        let tracked = process_group_for_change_tracking(None, Some(300));
        assert_eq!(tracked, None);
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Claude),
            foreground_pgid: None,
            last_foreground_pgid: tracked,
            elapsed_since_process_check: std::time::Duration::from_millis(300),
            ..process_probe_input()
        }));
        assert_eq!(process_group_for_change_tracking(Some(42), None), Some(42));
        assert_eq!(
            process_group_for_change_tracking(Some(42), Some(300)),
            Some(300)
        );
    }

    #[test]
    fn lifecycle_authority_skips_stable_routine_process_probe() {
        assert!(should_skip_process_probe_for_lifecycle_authority(
            true,
            ProcessProbeInput {
                current_agent: Some(Agent::Pi),
                elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED,
                ..process_probe_input()
            }
        ));
        assert!(!should_skip_process_probe_for_lifecycle_authority(
            false,
            ProcessProbeInput {
                current_agent: Some(Agent::Pi),
                elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED,
                ..process_probe_input()
            }
        ));
    }

    #[test]
    fn lifecycle_authority_keeps_periodic_probes_without_an_observed_group() {
        let input = ProcessProbeInput {
            current_agent: Some(Agent::Pi),
            foreground_pgid: None,
            last_foreground_pgid: None,
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED,
            ..process_probe_input()
        };
        assert!(!should_skip_process_probe_for_lifecycle_authority(
            true, input
        ));
        assert!(should_probe_foreground_job(input));
    }

    #[test]
    fn lifecycle_authority_preserves_process_exit_and_release_probes() {
        assert!(!should_skip_process_probe_for_lifecycle_authority(
            true,
            ProcessProbeInput {
                current_agent: Some(Agent::Pi),
                pending_foreground_shell_clear: true,
                ..process_probe_input()
            }
        ));
        assert!(!should_skip_process_probe_for_lifecycle_authority(
            true,
            ProcessProbeInput {
                current_agent: Some(Agent::Pi),
                suppressed_agent: Some(Agent::Pi),
                ..process_probe_input()
            }
        ));
    }

    #[test]
    fn lifecycle_authority_preserves_initial_and_foreground_group_change_probes() {
        assert!(!should_skip_process_probe_for_lifecycle_authority(
            true,
            ProcessProbeInput {
                current_agent: None,
                has_process_probe: false,
                ..process_probe_input()
            }
        ));
        assert!(!should_skip_process_probe_for_lifecycle_authority(
            true,
            ProcessProbeInput {
                current_agent: Some(Agent::Pi),
                foreground_pgid: Some(43),
                ..process_probe_input()
            }
        ));
    }

    #[test]
    fn pending_release_forces_initial_process_probe() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            suppressed_agent: Some(Agent::Codex),
            has_process_probe: false,
            ..process_probe_input()
        }));
    }

    #[test]
    fn pending_release_forces_process_probe_after_runtime_identity_clears() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            suppressed_agent: Some(Agent::Codex),
            has_process_probe: false,
            ..process_probe_input()
        }));
    }

    #[test]
    fn pending_release_skips_repeated_probe_when_foreground_group_is_stable() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            suppressed_agent: Some(Agent::Codex),
            ..process_probe_input()
        }));
    }

    #[test]
    fn pending_release_probes_when_foreground_group_changes() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            suppressed_agent: Some(Agent::Codex),
            foreground_pgid: Some(43),
            ..process_probe_input()
        }));
    }

    #[test]
    fn acquisition_window_catches_delayed_same_group_wrapper_startup() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            acquisition_age: Some(std::time::Duration::from_millis(1250)),
            elapsed_since_process_check: PROCESS_ACQUISITION_FAST_RECHECK
                - std::time::Duration::from_millis(1),
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            acquisition_age: Some(std::time::Duration::from_millis(1250)),
            elapsed_since_process_check: PROCESS_ACQUISITION_FAST_RECHECK,
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            acquisition_age: Some(std::time::Duration::from_secs(5)),
            elapsed_since_process_check: PROCESS_ACQUISITION_SLOW_RECHECK,
            ..process_probe_input()
        }));
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            acquisition_age: Some(PROCESS_ACQUISITION_WINDOW + std::time::Duration::from_millis(1),),
            elapsed_since_process_check: PROCESS_ACQUISITION_SLOW_RECHECK,
            ..process_probe_input()
        }));
    }

    #[test]
    fn content_change_starts_bounded_unidentified_acquisition_window() {
        let now = std::time::Instant::now();
        let mut acquisition_started_at = None;
        let mut last_content_change_at = None;

        sync_content_change_acquisition(
            None,
            None,
            false,
            true,
            now,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );
        assert_eq!(acquisition_started_at, Some(now));
        assert_eq!(last_content_change_at, Some(now));

        let later = now + std::time::Duration::from_secs(1);
        sync_content_change_acquisition(
            None,
            None,
            false,
            true,
            later,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );
        assert_eq!(
            acquisition_started_at,
            Some(now),
            "changed frames should not refresh the acquisition window"
        );
        assert_eq!(last_content_change_at, Some(later));

        let quiet_after_window =
            later + PROCESS_ACQUISITION_WINDOW + PROCESS_ACQUISITION_IDLE_RESET;
        sync_content_change_acquisition(
            None,
            None,
            false,
            false,
            quiet_after_window,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );
        assert_eq!(acquisition_started_at, None);
        assert_eq!(last_content_change_at, None);

        let next_burst = quiet_after_window + std::time::Duration::from_secs(1);
        sync_content_change_acquisition(
            None,
            None,
            false,
            true,
            next_burst,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );
        assert_eq!(acquisition_started_at, Some(next_burst));
        assert_eq!(last_content_change_at, Some(next_burst));
    }

    #[test]
    fn content_change_does_not_start_acquisition_when_process_probe_has_other_signal() {
        let now = std::time::Instant::now();
        let mut acquisition_started_at = None;
        let mut last_content_change_at = None;

        sync_content_change_acquisition(
            Some(Agent::Codex),
            None,
            false,
            true,
            now,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );
        assert_eq!(acquisition_started_at, None);
        assert_eq!(last_content_change_at, None);

        sync_content_change_acquisition(
            None,
            Some(Agent::Codex),
            false,
            true,
            now,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );
        assert_eq!(acquisition_started_at, None);
        assert_eq!(last_content_change_at, None);

        sync_content_change_acquisition(
            None,
            None,
            true,
            true,
            now,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );
        assert_eq!(acquisition_started_at, None);
        assert_eq!(last_content_change_at, None);
    }

    #[test]
    fn content_change_restarts_stale_process_group_acquisition_window() {
        let now = std::time::Instant::now();
        let stale_start = now - PROCESS_ACQUISITION_WINDOW - std::time::Duration::from_millis(1);
        let mut acquisition_started_at = Some(stale_start);
        let mut last_content_change_at = None;

        sync_content_change_acquisition(
            None,
            None,
            false,
            true,
            now,
            &mut acquisition_started_at,
            &mut last_content_change_at,
        );

        assert_eq!(acquisition_started_at, Some(now));
        assert_eq!(last_content_change_at, Some(now));
    }

    #[test]
    fn release_expiry_can_force_reacquire_probe_by_resetting_probe_state() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            has_process_probe: false,
            ..process_probe_input()
        }));
    }

    #[test]
    fn identified_agent_uses_shorter_safety_process_probe() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED
                - std::time::Duration::from_millis(1),
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED,
            ..process_probe_input()
        }));
    }

    #[test]
    fn identified_agent_probes_when_foreground_group_disappears() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            foreground_pgid: None,
            last_foreground_pgid: Some(42),
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED
                - std::time::Duration::from_millis(1),
            ..process_probe_input()
        }));
    }

    #[test]
    fn stable_missing_foreground_group_uses_safety_process_probe() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            foreground_pgid: None,
            last_foreground_pgid: None,
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED
                - std::time::Duration::from_millis(1),
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            foreground_pgid: None,
            last_foreground_pgid: None,
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED,
            ..process_probe_input()
        }));
    }

    #[test]
    fn transient_process_miss_keeps_current_agent_detected() {
        let mut presence = AgentDetectionPresence::from_agent(Some(Agent::Pi));

        let changed = presence.observe_process_probe(None);

        assert!(!changed, "one miss should not clear the detected agent");
        assert_eq!(presence.current_agent(), Some(Agent::Pi));
    }

    #[test]
    fn agent_only_clears_after_confirmation_misses() {
        let mut presence = AgentDetectionPresence::from_agent(Some(Agent::Pi));

        for attempt in 1..AGENT_MISS_CONFIRMATION_ATTEMPTS {
            let changed = presence.observe_process_probe(None);
            assert!(
                !changed,
                "miss {attempt} should stay in the confirmation window"
            );
            assert_eq!(presence.current_agent(), Some(Agent::Pi));
        }

        let changed = presence.observe_process_probe(None);
        assert!(changed, "last confirmation miss should clear the agent");
        assert_eq!(presence.current_agent(), None);
    }

    #[tokio::test]
    async fn set_full_lifecycle_authority_active_notifies_only_on_activation_transitions() {
        let runtime = PaneRuntime::test_with_screen_bytes(80, 24, b"");
        let reset_notify = runtime.agent_detection_reset_notify_for_test();

        runtime.set_full_lifecycle_authority_active(true);
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            reset_notify.notified(),
        )
        .await
        .expect("false-to-true transition should notify detection reset");

        runtime.set_full_lifecycle_authority_active(true);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                reset_notify.notified()
            )
            .await
            .is_err(),
            "repeated true-to-true sync should not notify detection reset"
        );

        runtime.set_full_lifecycle_authority_active(false);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                reset_notify.notified()
            )
            .await
            .is_err(),
            "true-to-false transition should not notify detection reset"
        );

        runtime.set_full_lifecycle_authority_active(true);
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            reset_notify.notified(),
        )
        .await
        .expect("re-entering active authority should notify detection reset");
    }

    #[tokio::test]
    async fn state_changed_event_waits_for_queue_space_instead_of_dropping() {
        let (tx, mut rx) = mpsc::channel(1);
        let pane_id = PaneId::from_raw(42);

        tx.try_send(AppEvent::UpdateReady {
            version: "9.9.9".into(),
            install_command: "zynk update".into(),
        })
        .unwrap();

        let publish = publish_state_changed_event(
            DetectionEventSender {
                sender: tx.clone(),
                pending_exits: Arc::new(Mutex::new(Vec::new())),
                process_observation: Arc::new(Mutex::new(None)),
            },
            pane_id,
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            std::time::Instant::now(),
        );
        tokio::pin!(publish);

        let blocked = tokio::time::timeout(std::time::Duration::from_millis(20), async {
            (&mut publish).await;
        })
        .await;
        assert!(
            blocked.is_err(),
            "publisher should wait for queue space instead of dropping StateChanged"
        );

        let first = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
            .await
            .expect("queue should yield first event")
            .expect("sender still alive");
        assert!(matches!(first, AppEvent::UpdateReady { .. }));

        tokio::time::timeout(std::time::Duration::from_millis(50), async {
            (&mut publish).await;
        })
        .await
        .expect("publisher should complete once queue space is available");

        let second = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
            .await
            .expect("queue should yield second event")
            .expect("sender still alive");
        assert!(matches!(
            second,
            AppEvent::StateChanged {
                pane_id: delivered_pane,
                agent: Some(Agent::Pi),
                state: AgentState::Idle,
                visible_blocker: false,
                visible_working: false,
                process_exited: false,
                observed_at: _,
            } if delivered_pane == pane_id
        ));
    }

    #[tokio::test]
    async fn a_fresh_same_agent_probe_confirms_a_provisional_lifecycle_owner() {
        use std::time::{Duration, Instant};
        let mut terminal =
            crate::terminal::TerminalState::new(crate::terminal::TerminalId::alloc(), "/".into());
        let exit_at = Instant::now() - Duration::from_secs(1);
        terminal
            .set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::id("restarted"),
                Some(10),
            )
            .expect("new hook owner");
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            exit_at,
        );
        assert!(terminal.confirmed_hook_owner().is_none());
        let mut presence = AgentDetectionPresence::from_agent(Some(Agent::Pi));
        assert!(
            !presence.observe_process_probe(Some(Agent::Pi)),
            "same label is not a state change"
        );
        let (tx, mut rx) = mpsc::channel(2);
        let state_events = DetectionEventSender {
            sender: tx,
            pending_exits: Arc::new(Mutex::new(Vec::new())),
            process_observation: Arc::new(Mutex::new(None)),
        };
        assert!(
            skip_screen_detection_under_hook_authority(
                state_events.clone(),
                PaneId::from_raw(42),
                None,
                Instant::now(),
                true,
                false,
            )
            .await
        );
        assert!(
            rx.try_recv().is_err(),
            "cached presence or a missed probe cannot confirm an owner"
        );
        assert!(
            skip_screen_detection_under_hook_authority(
                state_events,
                PaneId::from_raw(42),
                Some(Agent::Pi),
                Instant::now(),
                true,
                false,
            )
            .await
        );
        let event = rx
            .try_recv()
            .expect("a real same-agent probe must reach the state machine");
        let AppEvent::StateChanged {
            agent,
            state,
            visible_blocker,
            visible_working,
            process_exited,
            observed_at,
            ..
        } = event
        else {
            panic!("expected process observation");
        };
        terminal.set_detected_state_with_screen_signals_at(
            agent,
            state,
            visible_blocker,
            false,
            visible_working,
            process_exited,
            observed_at,
        );
        assert!(terminal.confirmed_hook_owner().is_some());
        assert_eq!(
            terminal.state,
            AgentState::Working,
            "the hook still owns lifecycle state"
        );
    }

    #[tokio::test]
    async fn acknowledging_one_exit_keeps_other_captured_exits_pending() {
        let runtime = PaneRuntime::test_with_screen_bytes(80, 24, b"");
        let pane = PaneId::from_raw(42);
        let now = std::time::Instant::now();
        let later = now + std::time::Duration::from_millis(1);
        let (tx, _rx) = mpsc::channel(2);
        runtime
            .test_publish_process_exit(tx.clone(), pane, Agent::Pi, now)
            .await;
        runtime
            .test_publish_process_exit(tx, pane, Agent::Hermes, later)
            .await;
        runtime.acknowledge_process_exit(Some(Agent::Hermes), now);
        assert_eq!(
            runtime.pending_process_exits().len(),
            2,
            "another owner's event is not an acknowledgement"
        );
        runtime.acknowledge_process_exit(Some(Agent::Pi), now);
        assert_eq!(
            runtime.pending_process_exits(),
            vec![(Some(Agent::Hermes), later)]
        );
        runtime.acknowledge_process_exit(Some(Agent::Hermes), later);
        assert!(runtime.pending_process_exits().is_empty());
    }
}
