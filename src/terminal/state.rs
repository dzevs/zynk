// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::collections::HashMap;
use std::path::PathBuf;
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

// Effective state arbitration is intentionally centralized here. Full lifecycle
// Zynk hook integrations are hook-authoritative while live; screen recovery
// remains only for session-only/custom hook paths and fallback detection.
// Process-exit updates clear matching hook authority before recomputing state.

use crate::detect::{Agent, AgentState};
use crate::terminal::TerminalId;

#[path = "metadata.rs"]
mod metadata;
pub use metadata::{AgentMetadata, AgentMetadataReport, EffectivePresentation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookAuthority {
    pub source: String,
    pub agent_label: String,
    pub state: AgentState,
    pub message: Option<String>,
    pub custom_status: Option<String>,
    pub reported_at: Instant,
    pub session_ref: Option<crate::agent_resume::AgentSessionRef>,
    /// The capture instant of an observed process EXIT this owner has not yet been
    /// proven to postdate, or `None` when the detector has confirmed its process since.
    ///
    /// Hook reports carry no capture time of their own, so `reported_at` is stamped when
    /// the report ARRIVED. An exit the detector captured BEFORE that arrival but the App
    /// handled after it therefore looks older than the report and retires nothing, while
    /// the detector has in fact seen the process gone and not seen it since. Retiring
    /// outright would be wrong the other way: that same window is where a genuinely
    /// restarted agent's first session-start report lands, and retiring it would strand
    /// the new process. The detector, not the hook, is the process oracle here, so the
    /// identity is held PROVISIONAL instead — kept and still visible, but barred from
    /// anchoring a receipt (`TerminalState::confirmed_hook_owner`) until a running
    /// observation captured after this instant confirms it, or a later exit retires it.
    pub unconfirmed_since: Option<Instant>,
}

/// Hook-reported IDENTITY for a `crate::detect::session_identity_only_integration`.
///
/// These integrations name their own agent (and, when they report one, their
/// session) over the hook, but hold NO lifecycle authority — their state stays
/// screen-detected. Identity is still hook-derived here, so unlike a
/// detection-only label it may anchor receipts and awareness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookIdentity {
    pub source: String,
    pub agent_label: String,
    /// When the hook reported it, so a detection observation captured EARLIER
    /// cannot erase it — the freshness `HookAuthority::reported_at` gives the
    /// full-lifecycle path, carried across the identity/lifecycle split.
    pub reported_at: Instant,
    /// The unanswered exit this identity must be proven to postdate, exactly as
    /// [`HookAuthority::unconfirmed_since`].
    pub unconfirmed_since: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuppressedHookReport {
    agent_label: String,
    session_ref: Option<crate::agent_resume::AgentSessionRef>,
    /// WHEN the observation or operation that retired this owner was CAPTURED, never
    /// when the App got round to handling it. The detector stamps every screen
    /// observation at capture time and `publish_state_changed_event` carries that stamp
    /// through the queue, but the queue holds 256 events and each drain is bounded at
    /// 64, so handling routinely lags capture. Were this handler time, an observation
    /// the detector genuinely took AFTER the exit could still look older than the
    /// retirement, and the running process it proves would decide nothing
    /// (`detected_state_observed_before_release_suppression`). API clear and release
    /// carry no observation of their own, so those pass their own handling instant.
    observed_at: Instant,
    /// WHEN this owner's process was last OBSERVED GONE while it was still only
    /// suppressed — its own exit, or a different agent detected in its place. A
    /// retirement spends a window here before any running observation converts it into
    /// a `StaleHookSession`, and a loss seen during that window is recorded nowhere
    /// else; without it the converted session starts with no boundary at all and a
    /// running observation captured BEFORE that loss re-arms a process already seen
    /// gone. It is carried into `StaleHookSession::last_loss_observed_at` on conversion.
    last_loss_observed_at: Option<Instant>,
    reason: HookSuppressionReason,
}

impl SuppressedHookReport {
    /// Advance the loss boundary this suppression will hand to the stale session it
    /// becomes. Only the NEWEST loss is kept, for the same reason
    /// `StaleHookSession::observe_process_loss` keeps it: a reordered older observation
    /// must not roll the boundary back over a newer one.
    fn observe_process_loss(&mut self, observed_at: Instant) {
        if self
            .last_loss_observed_at
            .is_none_or(|lost_at| lost_at < observed_at)
        {
            self.last_loss_observed_at = Some(observed_at);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookSuppressionReason {
    HookClear,
    ProcessExit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StaleHookSession {
    agent_label: String,
    session_ref: crate::agent_resume::AgentSessionRef,
    /// WHEN this owner's process was OBSERVED AGAIN after the retirement that made
    /// this session stale, if it has been. It is the same "fresh process" evidence the
    /// restart path already requires, and it is what separates a genuine resume from a
    /// late callback: only with it may an explicit session-start report reclaim this
    /// session id (`explicit_session_start_reclaims_stale_session`). Every new
    /// retirement clears it, so only an observation after the LATEST retirement counts.
    ///
    /// The timestamp is what bounds its LIFETIME. The evidence asserts a process that is
    /// still RUNNING, so a later observation showing that process gone expires it again
    /// (`observe_process_loss_for_retired_owners`), while an observation captured
    /// before it decides nothing.
    fresh_process_evidence: Option<Instant>,
    /// WHEN this owner's process was last OBSERVED GONE: its own exit, or a different
    /// agent detected in its place. This is the boundary the evidence above has to
    /// beat, and it is kept in its OWN field because expiry empties that evidence —
    /// were the boundary only the expired timestamp, the latest loss would be forgotten
    /// the moment it did its work, every further loss would decide nothing while
    /// evidence is absent, and a delayed running observation captured BEFORE that loss
    /// would re-arm a session whose process is gone. It advances on every newer loss
    /// whether or not evidence is held, and only a running observation strictly newer
    /// than it re-arms (`StaleHookSession::record_fresh_process_evidence`).
    last_loss_observed_at: Option<Instant>,
    /// WHEN the retirement that made this session stale was stamped, carried from the
    /// suppression it was converted out of.
    ///
    /// It is a boundary in its OWN right, and for an API clear or release it is the
    /// ONLY one: those observe no process at all, so they seed no loss, and a session
    /// converted out of one would otherwise start life with no boundary — a running
    /// observation captured BEFORE the release would then re-arm a session the agent
    /// was released from. Keeping it separate from the loss is what keeps the two
    /// facts independent: a retirement is not an observation of a process gone, so it
    /// must not be recorded as one, yet a resume must still beat it. It advances on
    /// every further retirement, so evidence is always scoped to the LATEST one.
    retired_at: Instant,
}

impl StaleHookSession {
    /// The boundary a running observation must beat to be evidence this session's
    /// process is alive NOW: the LATEST of the retirement that made it stale and the
    /// last loss observed since. Neither alone covers both retirements — a clear or
    /// release observes no process, and a loss may never have been seen — so the bar
    /// is the later of the two, and it is STRICT on both
    /// (`has_reclaimable_process_evidence`).
    fn evidence_boundary(&self) -> Instant {
        self.last_loss_observed_at
            .map_or(self.retired_at, |lost_at| lost_at.max(self.retired_at))
    }

    /// Take a retirement of this owner: advance the boundary, and drop only the evidence
    /// that retirement OUTDATES. Only the NEWEST retirement is kept, for the same
    /// reason `observe_process_loss` keeps only the newest loss — a reordered older
    /// retirement must not roll the boundary back — and the boundary advances whether
    /// or not there is evidence left to drop, because emptying the evidence must not
    /// also erase the fact that the session was retired again.
    ///
    /// The expiry is scoped to the RESULTING boundary, exactly the way
    /// `observe_process_loss` scopes its own: a retirement re-scopes evidence captured
    /// before or at it, never evidence strictly newer than every boundary it leaves
    /// behind. Scoping matters because this runs on the ALREADY-STALE sessions of an
    /// owner whose REPLACEMENT identity is being retired
    /// (`suppress_hook_report_with_session_ref`), so emptying unconditionally let a
    /// reordered older retirement erase newer proof that the owner's own process is
    /// alive — and made the capture-time rule turn on whether a replacement identity
    /// happened to be installed at all, since with none the loss path preserves that
    /// very same evidence (Codex B1 `msg_2f7b62540bcb70d5`).
    fn observe_retirement(&mut self, observed_at: Instant) {
        if self.retired_at < observed_at {
            self.retired_at = observed_at;
        }
        if self
            .fresh_process_evidence
            .is_some_and(|recorded_at| recorded_at <= self.evidence_boundary())
        {
            self.fresh_process_evidence = None;
        }
    }

    /// Record a fresh-process observation, keeping the NEWEST and never crossing the
    /// boundary: a replayed or reordered older observation must not roll the evidence
    /// back over a newer one, nor assert a process the latest loss has already shown
    /// gone, nor speak for a session retired after it was captured.
    fn record_fresh_process_evidence(&mut self, observed_at: Instant) {
        if observed_at <= self.evidence_boundary() {
            return;
        }
        if self
            .fresh_process_evidence
            .is_none_or(|recorded_at| recorded_at < observed_at)
        {
            self.fresh_process_evidence = Some(observed_at);
        }
    }

    /// Take an observation that shows this owner's process gone: advance the loss
    /// boundary, and drop any evidence the loss outdates. The boundary moves even when
    /// there is no evidence left to expire — that is the whole point of keeping it
    /// separately — while an observation older than the boundary decides nothing.
    fn observe_process_loss(&mut self, observed_at: Instant) {
        if self
            .last_loss_observed_at
            .is_none_or(|lost_at| lost_at < observed_at)
        {
            self.last_loss_observed_at = Some(observed_at);
        }
        if self
            .fresh_process_evidence
            .is_some_and(|recorded_at| recorded_at <= observed_at)
        {
            self.fresh_process_evidence = None;
        }
    }

    /// Whether this session currently holds evidence a resume may reclaim it on.
    ///
    /// The rule is the documented one and it is STRICT: evidence has to be newer than
    /// the boundary — the latest retirement AND the latest loss — never merely as new.
    /// `record_fresh_process_evidence`, `observe_process_loss` and `observe_retirement`
    /// already enforce exactly that on the way in, from every side; re-checking it at
    /// the reclaim keeps those sides from silently drifting apart, so no future path
    /// that touches a field directly can leave evidence armed against a loss or a
    /// retirement that already outdates it.
    fn has_reclaimable_process_evidence(&self) -> bool {
        self.fresh_process_evidence
            .is_some_and(|recorded_at| self.evidence_boundary() < recorded_at)
    }
}

/// The hook RETIREMENT bookkeeping of one terminal, in a form that survives a live
/// handoff to a replacement server process.
///
/// Only the fences travel — never the live `hook_authority`, which the next report
/// re-establishes on its own. Without them every retirement guarantee the fork makes
/// was void across `server.live_handoff`: a released session could be re-anchored on
/// the new server by an ORDINARY same-session report with no session-start reason, and
/// the message addressed to the released session then read `received` (Gate-3 arbiter
/// `msg_24fa384d30dfa9af`).
///
/// Instants cannot cross a process boundary, so every one is carried as an AGE in
/// milliseconds at capture time and re-based against the restoring server's own clock.
/// The granularity is deliberate: boundaries a fraction of a millisecond apart collapse
/// into a tie, and every comparison in the retirement machine is STRICTLY newer, so a
/// tie always decides against the reclaim — the fail-closed direction.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HookRetirementSnapshot {
    /// The per-source hook-report replay fence (`hook_report_sequences`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sequences: Vec<(String, u64)>,
    /// The per-source agent-METADATA replay fence (`metadata_report_sequences`). It is
    /// the same `seq <= last` rule, so it travels for the same reason: dropping it would
    /// open a replay window on the new server that does not exist on the old one, while
    /// keeping it refuses only genuinely replayed reports — a live agent's sequence
    /// counter is monotonic across a handoff, which is the whole point of a handoff.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metadata_sequences: Vec<(String, u64)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suppressed: Vec<SuppressedHookReportSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale: Vec<StaleHookSessionSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_identity: Option<HookIdentitySnapshot>,
    /// The provisional state of the previous commit: an observed exit no running
    /// observation has answered yet, with the agent whose process it was observed for.
    /// It covers the live authority's `unconfirmed_since` as well, because the restore
    /// deliberately re-installs no authority for a bare age to live on; carried this
    /// way it taints the next identity the new server records, exactly as it would have
    /// in the old process, and only a running observation of that same agent clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unanswered_exit: Option<UnansweredExitSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UnansweredExitSnapshot {
    /// The hook SOURCE the exit was observed for, so the fence lands on the same full owner it
    /// was recorded against rather than on any owner that happens to share the agent label.
    ///
    /// Absent in a snapshot written by a server that keyed this fence by label alone. Such a
    /// fence restores with no source and then matches its label under ANY source — exactly the
    /// reach it had on the server that wrote it — because narrowing it to a source that server
    /// never recorded would invent evidence, and dropping it would open a window that server
    /// did not have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub agent_label: String,
    pub age_ms: u64,
}

/// The full owner an unanswered process exit belongs to: the hook SOURCE that reported it and the
/// agent label that source named.
///
/// Two owners can share an agent label under different sources — `zynk:hermes` and some other
/// source both reporting `hermes` — and a fence keyed by the label alone lets one owner's pending
/// exit gate the other's identity. That is the class of bug fix #20 closed one level up, so the
/// fence is keyed by the pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookOwner {
    /// `None` only for a fence restored from a snapshot that predates the source travelling; see
    /// `UnansweredExitSnapshot::source`.
    pub source: Option<String>,
    pub agent_label: String,
}

impl HookOwner {
    fn new(source: &str, agent_label: &str) -> Self {
        Self {
            source: Some(source.to_string()),
            agent_label: agent_label.to_string(),
        }
    }

    /// Whether this fence is the one `(source, agent_label)` has to answer. A fence with no
    /// recorded source matches its label alone, as the server that wrote it did.
    fn matches(&self, source: &str, agent_label: &str) -> bool {
        self.agent_label == agent_label
            && self
                .source
                .as_deref()
                .is_none_or(|fenced_source| fenced_source == source)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentSessionRefSnapshot {
    pub kind: crate::agent_resume::AgentSessionRefKind,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SuppressedHookReportSnapshot {
    pub source: String,
    pub agent_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<AgentSessionRefSnapshot>,
    pub reason: HookSuppressionReason,
    pub observed_age_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_loss_age_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StaleHookSessionSnapshot {
    pub source: String,
    pub agent_label: String,
    pub session_ref: AgentSessionRefSnapshot,
    pub retired_age_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_loss_age_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_age_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HookIdentitySnapshot {
    pub source: String,
    pub agent_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_ref: Option<AgentSessionRefSnapshot>,
    pub reported_age_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unconfirmed_age_ms: Option<u64>,
}

impl AgentSessionRefSnapshot {
    fn capture(session_ref: &crate::agent_resume::AgentSessionRef) -> Self {
        Self {
            kind: session_ref.kind,
            value: session_ref.value.clone(),
        }
    }

    fn restore(self) -> crate::agent_resume::AgentSessionRef {
        crate::agent_resume::AgentSessionRef {
            kind: self.kind,
            value: self.value,
        }
    }
}

/// The age of `instant` at `now`, in whole milliseconds. An instant the caller's clock
/// has not reached yet (only reachable from a test that stamps ahead) saturates to 0,
/// so it re-bases onto `now` itself rather than into the future.
fn age_ms(now: Instant, instant: Instant) -> u64 {
    u64::try_from(now.saturating_duration_since(instant).as_millis()).unwrap_or(u64::MAX)
}

/// The inverse of [`age_ms`] against the restoring process's own clock, saturating at
/// `now` for an age no monotonic clock can reach back to.
fn instant_from_age_ms(now: Instant, age_ms: u64) -> Instant {
    now.checked_sub(std::time::Duration::from_millis(age_ms))
        .unwrap_or(now)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveStateChange {
    pub previous_agent_label: Option<String>,
    pub previous_known_agent: Option<Agent>,
    pub previous_state: AgentState,
    pub previous_presentation: EffectivePresentation,
    pub agent_label: Option<String>,
    pub known_agent: Option<Agent>,
    pub state: AgentState,
    pub presentation: EffectivePresentation,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TerminalStateMutation {
    pub effective_state_change: Option<EffectiveStateChange>,
    pub session_ref_changed: bool,
}

/// Pure state for a server-owned terminal.
///
/// During the migration this is still one-to-one with a pane-backed PTY, but
/// pane/view state no longer owns terminal identity, cwd, labels, or agent
/// metadata.
#[derive(Clone)]
pub struct TerminalState {
    pub id: TerminalId,
    pub cwd: PathBuf,
    pub detected_agent: Option<Agent>,
    pub fallback_state: AgentState,
    fallback_visible_blocker: bool,
    fallback_observed_at: Option<Instant>,
    pub hook_authority: Option<HookAuthority>,
    pub hook_identity: Option<HookIdentity>,
    pub agent_metadata: HashMap<String, AgentMetadata>,
    pub persisted_agent_session: Option<crate::agent_resume::PersistedAgentSession>,
    pub manual_label: Option<String>,
    pub agent_name: Option<String>,
    hook_report_sequences: HashMap<String, u64>,
    /// An observed process EXIT that no running observation has answered yet, held at
    /// the TERMINAL so it outlives the owner that recorded it: `(full owner, capture
    /// instant of the oldest unanswered exit)`.
    ///
    /// Keyed by the FULL owner — source and agent label — because two owners can report the
    /// same agent label under different sources, and a label-only key lets one gate the other.
    /// A `HookOwner` rather than a bare tuple: the pair is stored, snapshotted and matched
    /// as one thing, and the legacy no-source case needs a name and a rule of its own.
    ///
    /// In one process the per-owner `unconfirmed_since` is enough, because every path
    /// that drops an owner also SUPPRESSES it. A live handoff is the exception — the
    /// restore deliberately re-installs no `hook_authority`, so without this the next
    /// report on the new server would record a confirmed identity for a process the
    /// detector last saw exiting.
    unanswered_hook_exit: Option<(HookOwner, Instant)>,
    /// A transferred identity is not liveness evidence after the snapshot. Only a
    /// fresh observation by this server's detector may confirm this imported owner.
    handoff_confirmation: Option<(HookOwner, Instant)>,
    suppressed_hook_reports: HashMap<String, SuppressedHookReport>,
    stale_hook_sessions: HashMap<String, Vec<StaleHookSession>>,
    metadata_report_sequences: HashMap<String, u64>,
    pub state: AgentState,
    pub last_agent_state_change_seq: Option<u64>,
    pub revision: u64,
    pub launch_argv: Option<Vec<String>>,
    pub respawn_shell_on_exit: bool,
    pub pending_agent_resume_plan: Option<crate::agent_resume::AgentResumePlan>,
}

impl TerminalState {
    pub fn new(id: TerminalId, cwd: PathBuf) -> Self {
        Self {
            id,
            cwd,
            detected_agent: None,
            fallback_state: AgentState::Unknown,
            fallback_visible_blocker: false,
            fallback_observed_at: None,
            hook_authority: None,
            hook_identity: None,
            agent_metadata: HashMap::new(),
            persisted_agent_session: None,
            manual_label: None,
            agent_name: None,
            hook_report_sequences: HashMap::new(),
            unanswered_hook_exit: None,
            handoff_confirmation: None,
            suppressed_hook_reports: HashMap::new(),
            stale_hook_sessions: HashMap::new(),
            metadata_report_sequences: HashMap::new(),
            state: AgentState::Unknown,
            last_agent_state_change_seq: None,
            revision: 0,
            launch_argv: None,
            respawn_shell_on_exit: false,
            pending_agent_resume_plan: None,
        }
    }

    pub fn with_launch_argv(mut self, argv: Vec<String>) -> Self {
        self.launch_argv = Some(argv);
        self
    }

    pub fn with_respawn_shell_on_exit(mut self) -> Self {
        self.respawn_shell_on_exit = true;
        self
    }

    pub fn with_pending_agent_resume_plan(
        mut self,
        plan: crate::agent_resume::AgentResumePlan,
    ) -> Self {
        self.pending_agent_resume_plan = Some(plan);
        self
    }

    #[cfg(test)]
    pub fn set_detected_state(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
    ) -> Option<EffectiveStateChange> {
        self.set_detected_state_with_visible_blocker(agent, fallback_state, false, false, false)
    }

    #[cfg(test)]
    pub fn set_detected_state_with_mutation(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
    ) -> TerminalStateMutation {
        self.set_detected_state_with_screen_signals_at(
            agent,
            fallback_state,
            false,
            false,
            false,
            false,
            Instant::now(),
        )
    }

    #[cfg(test)]
    pub fn set_detected_state_with_visible_blocker(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
        visible_blocker: bool,
        _ignored_screen_idle: bool,
        process_exited: bool,
    ) -> Option<EffectiveStateChange> {
        self.set_detected_state_with_screen_signals_at(
            agent,
            fallback_state,
            visible_blocker,
            false,
            false,
            process_exited,
            Instant::now(),
        )
        .effective_state_change
    }

    pub fn set_detected_state_with_screen_signals_at(
        &mut self,
        agent: Option<Agent>,
        fallback_state: AgentState,
        visible_blocker: bool,
        _visible_idle: bool,
        _visible_working: bool,
        process_exited: bool,
        now: Instant,
    ) -> TerminalStateMutation {
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_detected_agent = self.detected_agent;
        let previous_session = self.current_session_identity_for_persistence();
        // The detector is the process oracle, so a RUNNING observation is what answers
        // an exit a hook report was accepted across. It is settled first, before any
        // early return: a live full-lifecycle authority makes this function ignore the
        // observation for state, and a capture older than a release suppression makes it
        // ignore it entirely, but in both cases the observation still proves the process
        // this owner's identity is waiting on (`confirm_pending_hook_owner`).
        if !process_exited {
            self.confirm_pending_hook_owner(agent, now);
        }
        if self.should_ignore_detected_state_under_full_lifecycle_hook(agent, process_exited) {
            if self
                .hook_authority
                .as_ref()
                .and_then(|authority| crate::detect::parse_agent_label(&authority.agent_label))
                == agent
            {
                self.detected_agent = agent;
            }
            return TerminalStateMutation {
                effective_state_change: self.recompute_effective_state(
                    previous_agent_label,
                    previous_known_agent,
                    previous_state,
                    previous_presentation,
                    now,
                ),
                session_ref_changed: previous_session
                    != self.current_session_identity_for_persistence(),
            };
        }
        if !process_exited && self.detected_state_observed_before_release_suppression(agent, now) {
            return TerminalStateMutation {
                effective_state_change: self.recompute_effective_state(
                    previous_agent_label,
                    previous_known_agent,
                    previous_state,
                    previous_presentation,
                    now,
                ),
                session_ref_changed: previous_session
                    != self.current_session_identity_for_persistence(),
            };
        }
        self.detected_agent = agent;
        if !process_exited {
            self.clear_hook_suppression_for_detected_agent(agent, now);
        }
        self.fallback_state = fallback_state;
        self.fallback_visible_blocker = visible_blocker && fallback_state == AgentState::Blocked;
        self.fallback_observed_at = Some(now);
        // An exit of this owner's own process decides one of three things. It is OLD
        // enough to be ordered against the report (`reported_at <= now`), so it retires
        // as it always has. Or it is a SECOND unanswered exit — the owner already holds
        // one this exit postdates, so the detector never saw the process alive between
        // them — and it retires the identity the first one only held provisional,
        // stamped with its OWN capture time so late callbacks are fenced as usual. Or it
        // falls in the reorder window (`reported_at > now`, an arrival stamp that says
        // nothing about capture), where retiring would strand a genuinely restarted
        // agent whose first session-start report lands exactly there: the owner is held
        // PROVISIONAL instead, and answers no receipt until the detector confirms it.
        let exited_authority_owner = process_exited
            .then(|| {
                self.hook_authority.as_ref().and_then(|authority| {
                    (crate::detect::parse_agent_label(&authority.agent_label) == agent)
                        .then(|| (authority.source.clone(), authority.agent_label.clone()))
                })
            })
            .flatten();
        if let Some((owner_source, owner_label)) = exited_authority_owner {
            if self.hook_authority_not_newer_than(now)
                || self.hook_owner_has_unanswered_exit_older_than(&owner_source, &owner_label, now)
            {
                let cleared_source = self
                    .hook_authority
                    .as_ref()
                    .map(|authority| authority.source.clone());
                self.suppress_current_hook_authority(HookSuppressionReason::ProcessExit, now);
                if let Some(source) = cleared_source {
                    self.hook_report_sequences.remove(&source);
                }
                self.hook_authority = None;
            } else {
                self.hold_hook_owner_unconfirmed(&owner_source, &owner_label, now);
            }
        }
        // A session-identity-only integration lives and dies with its process: it
        // holds no lifecycle authority to arbitrate, so its identity is dropped on
        // the same evidence that drops its session, and whenever the detected agent
        // contradicts the label the hook reported. `hook_identity_not_newer_than` is
        // the same freshness comparison the authority path applies: an observation
        // captured BEFORE the report it would erase decides nothing. Retiring the
        // identity also suppresses its owner, so a late callback cannot undo this.
        // The exit limb takes the three-way rule above; the CONFLICT limb — a different
        // agent detected in this one's place — keeps the ordering rule unchanged,
        // because a contradicting label is not an unanswered question about a process.
        let exited_identity_owner = process_exited
            .then(|| {
                self.hook_identity.as_ref().and_then(|identity| {
                    (crate::detect::parse_agent_label(&identity.agent_label) == agent)
                        .then(|| (identity.source.clone(), identity.agent_label.clone()))
                })
            })
            .flatten();
        if let Some((owner_source, owner_label)) = exited_identity_owner {
            if self.hook_identity_not_newer_than(now)
                || self.hook_owner_has_unanswered_exit_older_than(&owner_source, &owner_label, now)
            {
                self.retire_hook_identity(HookSuppressionReason::ProcessExit, now);
            } else {
                self.hold_hook_owner_unconfirmed(&owner_source, &owner_label, now);
            }
        } else if self.hook_identity_not_newer_than(now)
            && self.hook_identity_conflicts_with_detected_agent(agent)
        {
            self.retire_hook_identity(
                if process_exited {
                    HookSuppressionReason::ProcessExit
                } else {
                    HookSuppressionReason::HookClear
                },
                now,
            );
        }
        // Pending reclaim evidence OUTLIVES the identity it was recorded against, so
        // its lifetime cannot be bounded by the block above: that block runs only while
        // an identity is installed, and the window a delayed resume arrives in has none.
        self.observe_process_loss_for_retired_owners(agent, process_exited, now);
        // Identity and its session go together: the session survives only while a
        // still-live identity (one this observation was too old to retire) anchors it.
        if process_exited
            && !self.persisted_session_is_anchored_by_hook_identity()
            && self
                .persisted_agent_session
                .as_ref()
                .is_some_and(|session| crate::detect::parse_agent_label(&session.agent) == agent)
        {
            self.persisted_agent_session = None;
        }
        if self.hook_authority_not_newer_than(now)
            && (self.hook_authority_conflicts_with_detected_agent(agent)
                || (previous_detected_agent.is_some()
                    && agent != previous_detected_agent
                    && self.hook_authority.as_ref().is_some_and(|authority| {
                        crate::detect::parse_agent_label(&authority.agent_label)
                            == previous_detected_agent
                    })))
        {
            let durable_session = self.hook_authority.as_ref().and_then(|authority| {
                authority.session_ref.as_ref().map(|session_ref| {
                    crate::agent_resume::PersistedAgentSession {
                        source: authority.source.clone(),
                        agent: authority.agent_label.clone(),
                        session_ref: session_ref.clone(),
                    }
                })
            });
            self.suppress_current_hook_authority(HookSuppressionReason::HookClear, now);
            self.hook_authority = None;
            self.persisted_agent_session = durable_session;
        }
        TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session
                != self.current_session_identity_for_persistence(),
        }
    }

    #[cfg(test)]
    pub fn set_hook_authority(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        self.set_hook_authority_with_custom_status(source, agent_label, state, message, None, seq)
    }

    #[cfg(test)]
    pub fn set_hook_authority_with_custom_status(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        custom_status: Option<String>,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        self.set_hook_authority_with_custom_status_at(
            source,
            agent_label,
            state,
            message,
            custom_status,
            None,
            seq,
            Instant::now(),
        )
        .and_then(|mutation| mutation.effective_state_change)
    }

    pub fn set_hook_authority_with_session_ref(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        custom_status: Option<String>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        self.set_hook_authority_with_custom_status_at(
            source,
            agent_label,
            state,
            message,
            custom_status,
            session_ref,
            seq,
            Instant::now(),
        )
    }

    pub fn set_hook_authority_with_custom_status_at(
        &mut self,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        custom_status: Option<String>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
        now: Instant,
    ) -> Option<TerminalStateMutation> {
        if crate::detect::session_identity_only_integration(&source, &agent_label) {
            return self.record_identity_only_hook_report_at(
                source,
                agent_label,
                session_ref,
                seq,
                now,
            );
        }
        if self.opencode_state_report_is_cross_talk(&source, &agent_label, &session_ref) {
            return None;
        }
        if !self.hook_report_survives_retirement(&source, &agent_label, &session_ref, None) {
            return None;
        }
        if !self.accept_hook_report(&source, seq) {
            return None;
        }

        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        if self.known_agent_label_conflicts_with_detected_agent(&agent_label)
            || self.current_session_owner_conflicts(&source, &agent_label)
        {
            return None;
        }
        let session_ref = session_ref.map(|session_ref| {
            self.conflicting_same_owner_session_ref(&source, &agent_label, &session_ref, None)
                .unwrap_or(session_ref)
        });
        if self.live_full_lifecycle_hook_authority_conflicts_with_session(
            &source,
            &agent_label,
            &session_ref,
        ) {
            return None;
        }
        if session_ref.is_some() {
            self.retire_suppressed_session_after_accepting(
                &source,
                &agent_label,
                session_ref.as_ref(),
            );
        }
        // Owner coherence: the two identity representations must never name different
        // owners at once. An accepted full-lifecycle owner supersedes any identity-only
        // identity another owner left behind, so the obsolete identity cannot later
        // veto this owner's own release.
        if self.hook_identity.as_ref().is_some_and(|identity| {
            identity.source != source || identity.agent_label != agent_label
        }) {
            self.hook_identity = None;
        }
        self.persisted_agent_session = None;
        let unconfirmed_since = self.unanswered_exit_to_inherit(&source, &agent_label);
        self.hook_authority = Some(HookAuthority {
            source,
            agent_label,
            state,
            message,
            custom_status,
            reported_at: now,
            session_ref,
            unconfirmed_since,
        });
        let current_session = self.current_session_identity_for_persistence();
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session != current_session,
        })
    }

    /// Record a hook report from a `crate::detect::session_identity_only_integration`.
    ///
    /// Identity and lifecycle authority are SPLIT here. The reported `source`,
    /// `agent_label` and `session_ref` are hook-derived IDENTITY and are kept, so
    /// `pane.get` still surfaces the session and a receipt can anchor on it. The
    /// reported `state`/`message`/`custom_status` are dropped: screen detection
    /// stays the only lifecycle authority for these integrations, and the report
    /// never takes `hook_authority`, so no lifecycle arbitration runs.
    ///
    /// The App-side `HookStateReported` dispatch calls this directly so the routing
    /// is visible where the report arrives; the funnel above keeps the same guard so
    /// no other caller can route around the split.
    pub fn record_identity_only_hook_report(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        self.record_identity_only_hook_report_at(
            source,
            agent_label,
            session_ref,
            seq,
            Instant::now(),
        )
    }

    /// `record_identity_only_hook_report` with the report's own observation time.
    pub fn record_identity_only_hook_report_at(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
        now: Instant,
    ) -> Option<TerminalStateMutation> {
        // Retirement is shared with the full-lifecycle path: a clear, a release or a
        // process exit retires this owner until a genuinely NEW session or fresh
        // process evidence arrives. A higher sequence alone is not a new session, and
        // this shape carries no session-start reason, so it can never reclaim one.
        if !self.hook_report_survives_retirement(&source, &agent_label, &session_ref, None) {
            return None;
        }
        if !self.accept_hook_report(&source, seq) {
            return None;
        }
        if self.known_agent_label_conflicts_with_detected_agent(&agent_label)
            || self.current_session_owner_conflicts(&source, &agent_label)
        {
            return None;
        }
        // The same clamp the full-lifecycle path applies: a same-owner report that
        // repoints an established anchor without a session-start reason keeps the
        // anchor it already has (the M3-11 replacement guard).
        let session_ref = session_ref.map(|session_ref| {
            self.conflicting_same_owner_session_ref(&source, &agent_label, &session_ref, None)
                .unwrap_or(session_ref)
        });
        if session_ref.is_some() {
            self.retire_suppressed_session_after_accepting(
                &source,
                &agent_label,
                session_ref.as_ref(),
            );
        }
        let previous_session = self.current_session_identity_for_persistence();
        let identity_changed = self
            .hook_identity
            .as_ref()
            .is_none_or(|current| current.source != source || current.agent_label != agent_label);
        self.hook_identity = Some(HookIdentity {
            source: source.clone(),
            agent_label: agent_label.clone(),
            reported_at: now,
            unconfirmed_since: self.unanswered_exit_to_inherit(&source, &agent_label),
        });
        if let Some(session_ref) = session_ref {
            self.persisted_agent_session = Some(crate::agent_resume::PersistedAgentSession {
                source,
                agent: agent_label,
                session_ref,
            });
        }
        let session_ref_changed =
            previous_session != self.current_session_identity_for_persistence();
        (identity_changed || session_ref_changed).then_some(TerminalStateMutation {
            effective_state_change: None,
            session_ref_changed,
        })
    }

    fn hook_authority_not_newer_than(&self, observed_at: Instant) -> bool {
        self.hook_authority
            .as_ref()
            .is_none_or(|authority| authority.reported_at <= observed_at)
    }

    fn hook_identity_not_newer_than(&self, observed_at: Instant) -> bool {
        self.hook_identity
            .as_ref()
            .is_none_or(|identity| identity.reported_at <= observed_at)
    }

    /// The hook owner that may anchor a RECEIPT or awareness: `(source, agent_label)`,
    /// and only while its identity is CONFIRMED.
    ///
    /// The precedence is the receipt path's own — a full-lifecycle owner's
    /// `hook_authority` first, an identity-only owner's `hook_identity` only when no
    /// authority is installed — with one addition: an owner still holding an unanswered
    /// exit (`unconfirmed_since`) yields nothing at all, and never falls through to the
    /// other representation, because the pending exit is a fact about this terminal's
    /// process, not about which shape reported it. The caller then answers
    /// `receiver_identity_unverified`, exactly as for a pane no hook ever named.
    /// An imported identity likewise waits for a post-import process observation.
    pub fn confirmed_hook_owner(&self) -> Option<(&str, &str)> {
        let (source, agent_label, pending_exit) = if let Some(authority) = &self.hook_authority {
            (
                &authority.source,
                &authority.agent_label,
                authority.unconfirmed_since,
            )
        } else {
            let identity = self.hook_identity.as_ref()?;
            (
                &identity.source,
                &identity.agent_label,
                identity.unconfirmed_since,
            )
        };
        if pending_exit.is_some()
            || self
                .handoff_confirmation
                .as_ref()
                .is_some_and(|(owner, _)| owner.matches(source, agent_label))
        {
            return None;
        }
        Some((source, agent_label))
    }

    pub fn require_imported_hook_identity_confirmation(&mut self, imported_at: Instant) {
        self.handoff_confirmation = self.hook_identity.as_ref().map(|identity| {
            (
                HookOwner::new(&identity.source, &identity.agent_label),
                imported_at,
            )
        });
    }

    /// The unanswered exit an owner recorded NOW has to inherit, for the INCOMING
    /// `agent_label`.
    ///
    /// A report is not process evidence — only the detector is — so an exit that no
    /// running observation has answered yet taints the identity that replaces the one
    /// holding it, including a genuinely new session start from the same owner. The
    /// OLDEST pending exit wins, the same keep-the-oldest rule the exit path applies,
    /// so the confirmation bar never moves forward on its own.
    ///
    /// The fence is OWNER-SCOPED, and the owner is the FULL pair `(source, agent_label)`:
    /// only a pending exit recorded for THIS owner carries over, whether it sits on the
    /// outgoing authority, on the outgoing identity or in the terminal's own copy. A
    /// different owner starts unfenced, because the exit asked an unanswered question about
    /// the previous owner's process and the detector's observation of THAT agent is the only
    /// thing that answers it. Unscoped, a retired owner's pending exit was inherited by every
    /// successor while `confirm_pending_hook_owner` could only clear it on an observation of
    /// the RETIRED label, so an already-confirmed new owner lost its receipt authority on its
    /// next ordinary report and could never regain it (Codex Gate-2 `msg_3e339000b75278a4`).
    /// Scoping it to the label alone left the same bug one level finer: two owners can share
    /// an agent label under different sources, and one would still gate the other (warden R15,
    /// `msg_e60980952feed121`).
    ///
    /// The accepted consequence: a fence for an owner that never returns simply lingers. It
    /// gates that owner alone, and it rides the handoff snapshot as it already did —
    /// `UnansweredExitSnapshot` carries the label, and now the source with it.
    fn unanswered_exit_to_inherit(&self, source: &str, agent_label: &str) -> Option<Instant> {
        [
            self.hook_authority
                .as_ref()
                .filter(|authority| {
                    authority.source == source && authority.agent_label == agent_label
                })
                .and_then(|authority| authority.unconfirmed_since),
            self.hook_identity
                .as_ref()
                .filter(|identity| identity.source == source && identity.agent_label == agent_label)
                .and_then(|identity| identity.unconfirmed_since),
            self.unanswered_hook_exit
                .as_ref()
                .filter(|(owner, _)| owner.matches(source, agent_label))
                .map(|(_, at)| *at),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Hold the live hook owner `(source, agent_label)` PROVISIONAL against an exit captured
    /// at `observed_at`, keeping the OLDEST unanswered exit when one is already pending FOR
    /// THAT OWNER: a newer exit must not move the bar the detector has to clear, and
    /// another owner's older exit is not this owner's question to answer.
    fn hold_hook_owner_unconfirmed(
        &mut self,
        source: &str,
        agent_label: &str,
        observed_at: Instant,
    ) {
        // The terminal's own copy, so the exit outlives the owner that recorded it
        // across a live handoff. Same keep-the-oldest rule, same owner — the one whose
        // process the exit was observed for. A pending exit stored under a DIFFERENT
        // owner is replaced rather than merged: it belonged to an owner this one
        // succeeded, and re-keying its instant would fence this owner behind a bar set
        // for someone else's process.
        let pending = match self.unanswered_hook_exit.as_ref() {
            Some((owner, at)) if owner.matches(source, agent_label) => (*at).min(observed_at),
            _ => observed_at,
        };
        self.unanswered_hook_exit = Some((HookOwner::new(source, agent_label), pending));
        if let Some(authority) = self
            .hook_authority
            .as_mut()
            .filter(|authority| authority.source == source && authority.agent_label == agent_label)
        {
            authority.unconfirmed_since = Some(
                authority
                    .unconfirmed_since
                    .map_or(observed_at, |pending| pending.min(observed_at)),
            );
        }
        if let Some(identity) = self
            .hook_identity
            .as_mut()
            .filter(|identity| identity.source == source && identity.agent_label == agent_label)
        {
            identity.unconfirmed_since = Some(
                identity
                    .unconfirmed_since
                    .map_or(observed_at, |pending| pending.min(observed_at)),
            );
        }
    }

    /// Whether the live owner holds an unanswered exit that `observed_at` — this exit's
    /// own capture time — is strictly newer than. That means the detector never saw the
    /// process alive between the two exits, so the second one retires the identity the
    /// first one only held provisional.
    fn hook_owner_has_unanswered_exit_older_than(
        &self,
        source: &str,
        agent_label: &str,
        observed_at: Instant,
    ) -> bool {
        self.unanswered_exit_to_inherit(source, agent_label)
            .is_some_and(|pending| pending < observed_at)
    }

    /// A RUNNING observation of the pending owner's own agent, captured strictly after
    /// the unanswered exit, is the detector confirming the process the hook claimed.
    /// Nothing else clears the flag: a hook report proves only that a reporter is alive,
    /// which is precisely what the reorder window makes untrustworthy.
    fn confirm_pending_hook_owner(&mut self, detected_agent: Option<Agent>, observed_at: Instant) {
        let Some(detected_agent) = detected_agent else {
            return;
        };
        let confirms = |agent_label: &str, pending: Option<Instant>| {
            pending.is_some_and(|pending| {
                pending < observed_at
                    && crate::detect::parse_agent_label(agent_label) == Some(detected_agent)
            })
        };
        if self
            .handoff_confirmation
            .as_ref()
            .is_some_and(|(owner, at)| confirms(&owner.agent_label, Some(*at)))
        {
            self.handoff_confirmation = None;
        }
        if let Some(authority) = self.hook_authority.as_mut() {
            if confirms(&authority.agent_label, authority.unconfirmed_since) {
                authority.unconfirmed_since = None;
            }
        }
        if let Some(identity) = self.hook_identity.as_mut() {
            if confirms(&identity.agent_label, identity.unconfirmed_since) {
                identity.unconfirmed_since = None;
            }
        }
        // The detector observes a PROCESS, not a source, so an owner's fence is answered by a
        // running observation of the agent it named — the same rule as for the two live
        // representations above, applied to the owner this fence belongs to.
        if self
            .unanswered_hook_exit
            .as_ref()
            .is_some_and(|(owner, at)| confirms(&owner.agent_label, Some(*at)))
        {
            self.unanswered_hook_exit = None;
        }
    }

    /// Export this terminal's hook RETIREMENT fences so they survive a live handoff.
    ///
    /// `None` when nothing is retired, fenced or pending, so an ordinary pane adds no
    /// bytes to the snapshot. Everything is sorted, so a round trip is byte-stable.
    pub fn export_hook_retirement(&self, now: Instant) -> Option<HookRetirementSnapshot> {
        let mut sequences: Vec<(String, u64)> = self
            .hook_report_sequences
            .iter()
            .map(|(source, seq)| (source.clone(), *seq))
            .collect();
        sequences.sort();
        let mut metadata_sequences: Vec<(String, u64)> = self
            .metadata_report_sequences
            .iter()
            .map(|(source, seq)| (source.clone(), *seq))
            .collect();
        metadata_sequences.sort();
        let mut suppressed: Vec<SuppressedHookReportSnapshot> = self
            .suppressed_hook_reports
            .iter()
            .map(|(source, suppressed)| SuppressedHookReportSnapshot {
                source: source.clone(),
                agent_label: suppressed.agent_label.clone(),
                session_ref: suppressed
                    .session_ref
                    .as_ref()
                    .map(AgentSessionRefSnapshot::capture),
                reason: suppressed.reason,
                observed_age_ms: age_ms(now, suppressed.observed_at),
                last_loss_age_ms: suppressed
                    .last_loss_observed_at
                    .map(|lost_at| age_ms(now, lost_at)),
            })
            .collect();
        suppressed.sort_by(|left, right| {
            (&left.source, &left.agent_label).cmp(&(&right.source, &right.agent_label))
        });
        let mut stale: Vec<StaleHookSessionSnapshot> = self
            .stale_hook_sessions
            .iter()
            .flat_map(|(source, sessions)| {
                sessions.iter().map(move |stale| StaleHookSessionSnapshot {
                    source: source.clone(),
                    agent_label: stale.agent_label.clone(),
                    session_ref: AgentSessionRefSnapshot::capture(&stale.session_ref),
                    retired_age_ms: age_ms(now, stale.retired_at),
                    last_loss_age_ms: stale
                        .last_loss_observed_at
                        .map(|lost_at| age_ms(now, lost_at)),
                    evidence_age_ms: stale
                        .fresh_process_evidence
                        .map(|recorded_at| age_ms(now, recorded_at)),
                })
            })
            .collect();
        stale.sort_by(|left, right| {
            (&left.source, &left.agent_label, &left.session_ref.value).cmp(&(
                &right.source,
                &right.agent_label,
                &right.session_ref.value,
            ))
        });
        let hook_identity = self
            .hook_identity
            .as_ref()
            .map(|identity| HookIdentitySnapshot {
                source: identity.source.clone(),
                agent_label: identity.agent_label.clone(),
                session_ref: self
                    .owned_session_ref(&identity.source, &identity.agent_label)
                    .as_ref()
                    .map(AgentSessionRefSnapshot::capture),
                reported_age_ms: age_ms(now, identity.reported_at),
                unconfirmed_age_ms: identity
                    .unconfirmed_since
                    .map(|pending| age_ms(now, pending)),
            });
        let unanswered_exit =
            self.unanswered_hook_exit
                .as_ref()
                .map(|(owner, pending)| UnansweredExitSnapshot {
                    source: owner.source.clone(),
                    agent_label: owner.agent_label.clone(),
                    age_ms: age_ms(now, *pending),
                });
        let empty = sequences.is_empty()
            && metadata_sequences.is_empty()
            && suppressed.is_empty()
            && stale.is_empty()
            && hook_identity.is_none()
            && unanswered_exit.is_none();
        (!empty).then_some(HookRetirementSnapshot {
            sequences,
            metadata_sequences,
            suppressed,
            stale,
            hook_identity,
            unanswered_exit,
        })
    }

    /// Re-apply exported fences on the replacement server, re-basing every age against
    /// this process's own clock.
    ///
    /// Everything goes in through the SAME accessors the live paths use — the
    /// suppression funnel, then `remember_stale_hook_session` with retirement, loss and
    /// evidence in that order — so no invariant can be sidestepped by a restore. The
    /// live `hook_authority` is deliberately NOT re-installed: the owner's next report
    /// re-establishes it, and it is the FENCE, not the authority, that must survive.
    pub fn restore_hook_retirement(&mut self, snapshot: HookRetirementSnapshot, now: Instant) {
        for (source, seq) in snapshot.sequences {
            self.hook_report_sequences.insert(source, seq);
        }
        for (source, seq) in snapshot.metadata_sequences {
            self.metadata_report_sequences.insert(source, seq);
        }
        for suppressed in snapshot.suppressed {
            let source = suppressed.source.clone();
            self.suppress_hook_report_with_session_ref(
                suppressed.source,
                suppressed.agent_label,
                suppressed.session_ref.map(AgentSessionRefSnapshot::restore),
                suppressed.reason,
                instant_from_age_ms(now, suppressed.observed_age_ms),
            );
            // The funnel seeds a loss only for a `ProcessExit`; a loss observed WHILE
            // this owner was merely suppressed is a fact of its own and must survive too.
            if let Some(last_loss_age_ms) = suppressed.last_loss_age_ms {
                if let Some(restored) = self.suppressed_hook_reports.get_mut(&source) {
                    restored.observe_process_loss(instant_from_age_ms(now, last_loss_age_ms));
                }
            }
        }
        for stale in snapshot.stale {
            self.remember_stale_hook_session(
                stale.source,
                stale.agent_label,
                stale.session_ref.restore(),
                instant_from_age_ms(now, stale.retired_age_ms),
                stale
                    .evidence_age_ms
                    .map(|age_ms| instant_from_age_ms(now, age_ms)),
                stale
                    .last_loss_age_ms
                    .map(|age_ms| instant_from_age_ms(now, age_ms)),
            );
        }
        if let Some(identity) = snapshot.hook_identity {
            // Owner coherence the way the report path keeps it: identity and its session
            // name the same owner. The pane snapshot is authoritative for the session and
            // the caller applies it first, so this only fills a gap it left.
            if let Some(session_ref) = identity.session_ref {
                if self.persisted_agent_session.is_none() {
                    self.persisted_agent_session =
                        Some(crate::agent_resume::PersistedAgentSession {
                            source: identity.source.clone(),
                            agent: identity.agent_label.clone(),
                            session_ref: session_ref.restore(),
                        });
                }
            }
            self.hook_identity = Some(HookIdentity {
                source: identity.source,
                agent_label: identity.agent_label,
                reported_at: instant_from_age_ms(now, identity.reported_age_ms),
                unconfirmed_since: identity
                    .unconfirmed_age_ms
                    .map(|age_ms| instant_from_age_ms(now, age_ms)),
            });
        }
        if let Some(unanswered) = snapshot.unanswered_exit {
            self.unanswered_hook_exit = Some((
                HookOwner {
                    source: unanswered.source,
                    agent_label: unanswered.agent_label,
                },
                instant_from_age_ms(now, unanswered.age_ms),
            ));
        }
    }

    /// Owners whose hook reports are RETIRED by a clear, a release or a process exit.
    ///
    /// Both hook-owned shapes qualify. A full-lifecycle owner holds `hook_authority`;
    /// a session-identity-only owner holds `hook_identity` and no lifecycle authority,
    /// but its identity anchors receipts just the same, so once retired it must not
    /// come back on a late callback carrying a higher sequence alone.
    fn hook_report_retirement_applies(source: &str, agent_label: &str) -> bool {
        crate::detect::full_lifecycle_hook_authority(source, agent_label)
            || crate::detect::session_identity_only_integration(source, agent_label)
    }

    /// The retirement gate EVERY hook report passes, in both representations.
    ///
    /// `false` means this owner is still retired. A genuinely new session, or fresh
    /// process evidence, re-anchors the sequence instead of banning the owner.
    ///
    /// `session_start_source` is the reason the agent gave for starting a session, when
    /// the report carries one (`pane.report_agent_session`). It is what admits the one
    /// legitimate report that names an already-retired session — an explicit resume of
    /// the SAME session, which the Hermes resume command deliberately produces by
    /// reusing `session_ref.value`. Callers that carry no reason pass `None`, so an
    /// ordinary late callback is refused exactly as before.
    fn hook_report_survives_retirement(
        &mut self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
        session_start_source: Option<&str>,
    ) -> bool {
        if self.hook_report_is_suppressed(source, agent_label, session_ref) {
            return false;
        }
        if self.hook_report_matches_stale_session(source, agent_label, session_ref) {
            if !self.explicit_session_start_reclaims_stale_session(
                source,
                agent_label,
                session_ref,
                session_start_source,
            ) {
                return false;
            }
            // A reclaim opens a new generation of the same session, so the sequence
            // anchor is dropped the way it is for a brand-new session. The stale entry
            // itself is only dropped once the report is ACCEPTED
            // (`retire_suppressed_session_after_accepting`): a report this gate lets
            // through but a later check refuses must leave the session retired.
            self.hook_report_sequences.remove(source);
            return true;
        }
        if self.hook_report_has_fresh_session_after_suppression(source, agent_label, session_ref)
            || self.hook_report_has_fresh_session_after_stale_session(
                source,
                agent_label,
                session_ref,
            )
        {
            self.hook_report_sequences.remove(source);
        }
        true
    }

    /// Consume this owner's retirement bookkeeping once a report carrying a session is
    /// accepted: the suppressed session becomes a STALE session, so the retired session
    /// stays retired while the new one anchors, and the accepted session — which the
    /// gate only admits as an explicit reclaim — stops being stale, so this owner's
    /// ordinary reports for it are no longer refused as retired.
    fn retire_suppressed_session_after_accepting(
        &mut self,
        source: &str,
        agent_label: &str,
        accepted_session_ref: Option<&crate::agent_resume::AgentSessionRef>,
    ) {
        if let Some(suppressed) = self.suppressed_hook_reports.remove(source) {
            // The other conversion out of the suppression window, and it carries the
            // same boundaries for the same reason: BOTH the retirement itself and any
            // loss seen while this owner was only suppressed must survive into the
            // stale session it becomes (Gate-3 B1). The retirement is the load-bearing
            // half here — an API clear or release observes no process and seeds no
            // loss, so dropping it left the converted session with no boundary at all.
            let retired_at = suppressed.observed_at;
            let last_loss_observed_at = suppressed.last_loss_observed_at;
            if let Some(suppressed_ref) = suppressed.session_ref {
                self.remember_stale_hook_session(
                    source.to_string(),
                    suppressed.agent_label,
                    suppressed_ref,
                    retired_at,
                    None,
                    last_loss_observed_at,
                );
            }
        }
        if let Some(accepted_ref) = accepted_session_ref {
            self.forget_stale_hook_session(source, agent_label, accepted_ref);
        }
    }

    /// Drop a reclaimed session from this owner's stale list: it anchors identity
    /// again, so refusing its later reports would retire a session that is live.
    fn forget_stale_hook_session(
        &mut self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
    ) {
        let Some(stale_sessions) = self.stale_hook_sessions.get_mut(source) else {
            return;
        };
        stale_sessions
            .retain(|stale| stale.agent_label != agent_label || &stale.session_ref != session_ref);
        if stale_sessions.is_empty() {
            self.stale_hook_sessions.remove(source);
        }
    }

    /// The session anchor `source`/`agent_label` currently owns, from either
    /// representation. Retirement has to remember it so a late report cannot bring
    /// the same session back.
    fn owned_session_ref(
        &self,
        source: &str,
        agent_label: &str,
    ) -> Option<crate::agent_resume::AgentSessionRef> {
        let authority_session = self.hook_authority.as_ref().and_then(|authority| {
            (authority.source == source && authority.agent_label == agent_label)
                .then(|| authority.session_ref.clone())
                .flatten()
        });
        authority_session.or_else(|| {
            self.persisted_agent_session
                .as_ref()
                .filter(|session| session.source == source && session.agent == agent_label)
                .map(|session| session.session_ref.clone())
        })
    }

    fn persisted_session_is_anchored_by_hook_identity(&self) -> bool {
        self.hook_identity.as_ref().is_some_and(|identity| {
            self.persisted_agent_session_matches(&identity.source, &identity.agent_label)
        })
    }

    /// Retire the current hook identity the way the authority path retires
    /// `hook_authority`: the owner is suppressed WITH the session it anchored, so a
    /// late callback cannot bring the same session back on a higher sequence alone.
    fn retire_hook_identity(
        &mut self,
        reason: HookSuppressionReason,
        observed_at: Instant,
    ) -> Option<HookIdentity> {
        let identity = self.hook_identity.take()?;
        if reason == HookSuppressionReason::ProcessExit {
            self.hook_report_sequences.remove(&identity.source);
        }
        self.suppress_hook_report(&identity.source, &identity.agent_label, reason, observed_at);
        Some(identity)
    }

    fn fallback_not_older_than_hook(&self) -> bool {
        self.hook_authority.as_ref().is_none_or(|authority| {
            self.fallback_observed_at
                .is_some_and(|observed_at| authority.reported_at <= observed_at)
        })
    }

    fn hook_authority_conflicts_with_detected_agent(&self, detected_agent: Option<Agent>) -> bool {
        let Some(detected_agent) = detected_agent else {
            return false;
        };
        self.hook_authority.as_ref().is_some_and(|authority| {
            crate::detect::parse_agent_label(&authority.agent_label)
                .is_some_and(|hook_agent| hook_agent != detected_agent)
        })
    }

    fn hook_identity_conflicts_with_detected_agent(&self, detected_agent: Option<Agent>) -> bool {
        let Some(detected_agent) = detected_agent else {
            return false;
        };
        self.hook_identity.as_ref().is_some_and(|identity| {
            crate::detect::parse_agent_label(&identity.agent_label)
                .is_some_and(|hook_agent| hook_agent != detected_agent)
        })
    }

    fn should_ignore_detected_state_under_full_lifecycle_hook(
        &self,
        detected_agent: Option<Agent>,
        process_exited: bool,
    ) -> bool {
        self.live_full_lifecycle_hook_authority()
            && !process_exited
            && !self.hook_authority_conflicts_with_detected_agent(detected_agent)
    }

    fn persisted_agent_session_matches(&self, source: &str, agent: &str) -> bool {
        self.persisted_agent_session
            .as_ref()
            .is_some_and(|session| session.source == source && session.agent == agent)
    }

    fn suppress_current_hook_authority(
        &mut self,
        reason: HookSuppressionReason,
        observed_at: Instant,
    ) {
        if let Some((source, agent_label, session_ref)) =
            self.hook_authority.as_ref().and_then(|authority| {
                crate::detect::full_lifecycle_hook_authority(
                    &authority.source,
                    &authority.agent_label,
                )
                .then(|| {
                    (
                        authority.source.clone(),
                        authority.agent_label.clone(),
                        authority.session_ref.clone(),
                    )
                })
            })
        {
            self.suppress_hook_report_with_session_ref(
                source,
                agent_label,
                session_ref,
                reason,
                observed_at,
            );
        }
    }

    fn suppress_hook_report(
        &mut self,
        source: &str,
        agent_label: &str,
        reason: HookSuppressionReason,
        observed_at: Instant,
    ) {
        if Self::hook_report_retirement_applies(source, agent_label) {
            let session_ref = self.owned_session_ref(source, agent_label);
            self.suppress_hook_report_with_session_ref(
                source.to_string(),
                agent_label.to_string(),
                session_ref,
                reason,
                observed_at,
            );
        }
    }

    /// Record a retirement, stamped with WHEN the thing that caused it was observed.
    ///
    /// `observed_at` is the detector's capture time on every path that has one, and the
    /// operation's own instant only for an API clear or release, which observes no
    /// process at all. That distinction is the whole of Gate-3 B1
    /// WARDEN-R13-OBSERVED-AT-001: handler time turned a legitimate post-exit
    /// observation into a pre-retirement one whenever the App drained the queue late,
    /// and the resumed owner then never regained its session.
    fn suppress_hook_report_with_session_ref(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        reason: HookSuppressionReason,
        observed_at: Instant,
    ) {
        // Evidence is scoped to the LATEST retirement: a process observed before this
        // one proves nothing about a session retired now, so a resume must wait for a
        // new observation. The retirement is RECORDED on the session, not merely used
        // to empty the flag — otherwise the next observation would have only the
        // FIRST retirement to beat and a report from before this one could re-arm it
        // (`StaleHookSession::retired_at`).
        if let Some(stale_sessions) = self.stale_hook_sessions.get_mut(&source) {
            for stale in stale_sessions
                .iter_mut()
                .filter(|stale| stale.agent_label == agent_label)
            {
                stale.observe_retirement(observed_at);
            }
        }
        // A retirement on an observed process EXIT is itself a loss observation, so it
        // seeds the boundary the converted stale session will have to beat. An API
        // clear or release observes no process and seeds none.
        let last_loss_observed_at =
            (reason == HookSuppressionReason::ProcessExit).then_some(observed_at);
        self.suppressed_hook_reports.insert(
            source,
            SuppressedHookReport {
                agent_label,
                session_ref,
                observed_at,
                last_loss_observed_at,
                reason,
            },
        );
    }

    fn hook_report_is_suppressed(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
    ) -> bool {
        if !Self::hook_report_retirement_applies(source, agent_label) {
            return false;
        }
        self.suppressed_hook_reports
            .get(source)
            .is_some_and(|suppressed| {
                if suppressed.agent_label != agent_label {
                    return false;
                }
                if suppressed.reason == HookSuppressionReason::ProcessExit {
                    return true;
                }
                match (&suppressed.session_ref, session_ref) {
                    (Some(suppressed_ref), Some(incoming_ref)) => incoming_ref == suppressed_ref,
                    (Some(_), None) => true,
                    (None, Some(_)) => false,
                    (None, None) => true,
                }
            })
    }

    fn hook_report_has_fresh_session_after_suppression(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
    ) -> bool {
        if !Self::hook_report_retirement_applies(source, agent_label) {
            return false;
        }
        self.suppressed_hook_reports
            .get(source)
            .is_some_and(|suppressed| {
                suppressed.agent_label == agent_label
                    && suppressed.reason != HookSuppressionReason::ProcessExit
                    && matches!(
                        (&suppressed.session_ref, session_ref),
                        (Some(suppressed_ref), Some(incoming_ref))
                            if incoming_ref != suppressed_ref
                    )
            })
    }

    fn hook_report_matches_stale_session(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
    ) -> bool {
        if !Self::hook_report_retirement_applies(source, agent_label) {
            return false;
        }
        self.stale_hook_sessions
            .get(source)
            .is_some_and(|stale_sessions| {
                session_ref.as_ref().is_some_and(|incoming_ref| {
                    stale_sessions.iter().any(|stale| {
                        stale.agent_label == agent_label && incoming_ref == &stale.session_ref
                    })
                })
            })
    }

    fn hook_report_has_fresh_session_after_stale_session(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
    ) -> bool {
        if !Self::hook_report_retirement_applies(source, agent_label) {
            return false;
        }
        self.stale_hook_sessions
            .get(source)
            .is_some_and(|stale_sessions| {
                stale_sessions
                    .iter()
                    .any(|stale| stale.agent_label == agent_label)
                    && session_ref.as_ref().is_some_and(|incoming_ref| {
                        stale_sessions.iter().all(|stale| {
                            stale.agent_label != agent_label || incoming_ref != &stale.session_ref
                        })
                    })
            })
    }

    /// The ONE way back into a session this owner retired.
    ///
    /// Two things must hold. The report must be an explicit session-start report whose
    /// reason this owner actually starts on — the same set
    /// `session_start_source_allows_session_replacement` already trusts to repoint an
    /// established anchor — which is the shape `pane.report_agent_session` carries and
    /// an ordinary state callback never does. And the named session must carry fresh
    /// process evidence: the owner's process was observed again AFTER the retirement,
    /// the same bar the restart path applies to a brand-new session id. A resume that
    /// arrives with no process seen since the retirement is still suppressed, and a
    /// late callback with no session-start reason is still stale, however fresh the
    /// process is. Evidence a later observation has since expired counts as none
    /// (`observe_process_loss_for_retired_owners`). WHY this exists at all: the
    /// Hermes resume command reuses
    /// `session_ref.value` (`src/agent_resume.rs`), so a new id would be a NEW session
    /// — a real resume can only ever name the retired one.
    fn explicit_session_start_reclaims_stale_session(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
        session_start_source: Option<&str>,
    ) -> bool {
        if !Self::session_start_source_allows_session_replacement(
            source,
            agent_label,
            session_start_source,
        ) {
            return false;
        }
        let Some(incoming_ref) = session_ref.as_ref() else {
            return false;
        };
        self.stale_hook_sessions
            .get(source)
            .is_some_and(|stale_sessions| {
                stale_sessions.iter().any(|stale| {
                    stale.agent_label == agent_label
                        && &stale.session_ref == incoming_ref
                        && stale.has_reclaimable_process_evidence()
                })
            })
    }

    fn live_full_lifecycle_hook_authority_conflicts_with_session(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
    ) -> bool {
        let Some(authority) = self.hook_authority.as_ref() else {
            return false;
        };
        if !crate::detect::full_lifecycle_hook_authority(&authority.source, &authority.agent_label)
        {
            return false;
        }
        if authority.source != source || authority.agent_label != agent_label {
            return false;
        }
        authority
            .session_ref
            .as_ref()
            .zip(session_ref.as_ref())
            .is_some_and(|(current, incoming)| current != incoming)
    }

    /// Upstream's `opencode_cross_talk` gate, in this fork's arbitration order.
    ///
    /// One opencode server is shared by every client attached to it, and the bundled
    /// server plugin reports activity for EVERY root session that server holds. A state
    /// report naming a session other than the one THIS pane anchored is therefore another
    /// client's activity, and the pane must not adopt it: clamping the session id (what
    /// `conflicting_same_owner_session_ref` does) would still let the other client's state
    /// drive this pane, so the report is dropped whole. The pane's own selection arrives
    /// on the session-start path from the TUI plugin instead.
    ///
    /// The anchor is the stored session triple through `owned_session_ref` —
    /// `hook_authority` first, then the persisted session, which is what a local TUI
    /// selection leaves behind. Identity stays hook-authoritative throughout: `source`,
    /// `agent_label` and `session_ref` all come off the hook report, and `detected_agent`
    /// is read only as upstream's `process_present` evidence that the agent's own process
    /// is in the foreground — the same use the session-start path already makes of it,
    /// never as a source of the pane's identity.
    fn opencode_state_report_is_cross_talk(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &Option<crate::agent_resume::AgentSessionRef>,
    ) -> bool {
        if (source, agent_label) != ("zynk:opencode", "opencode") {
            return false;
        }
        let process_present = crate::detect::parse_agent_label(agent_label)
            .is_some_and(|known_agent| self.detected_agent == Some(known_agent));
        if !process_present {
            return false;
        }
        self.owned_session_ref(source, agent_label)
            .zip(session_ref.as_ref())
            .is_some_and(|(anchored, incoming)| &anchored != incoming)
    }

    fn same_owner_full_lifecycle_hook_authority_session_ref(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
    ) -> Option<crate::agent_resume::AgentSessionRef> {
        let authority = self.hook_authority.as_ref()?;
        if !crate::detect::full_lifecycle_hook_authority(&authority.source, &authority.agent_label)
            || authority.source != source
            || authority.agent_label != agent_label
        {
            return None;
        }
        authority
            .session_ref
            .as_ref()
            .filter(|current| *current != session_ref)
            .cloned()
    }

    /// Convert this owner's retirement into a stale session, and re-arm the sessions it
    /// already holds, on a RUNNING observation of its own process.
    ///
    /// The trigger is the observation itself, never a CHANGE of detected agent. An agent
    /// restarted in place is observed under the SAME label across its own exit — the
    /// screen still shows it, and the detector's miss-confirmation window may never
    /// report the agent gone — so keying the conversion on a label change left exactly
    /// that retirement with no way back and the agent's own explicit resume refused
    /// forever (Gate-3 B1 arbiter `msg_fbcdf59a01d6f70d`).
    ///
    /// What makes an observation evidence is TIME, and the gate is applied HERE so it
    /// holds however the caller reaches this: a suppression is lifted only by an
    /// observation captured strictly after BOTH the retirement itself and any loss seen
    /// while the owner was only suppressed — the same bar `StaleHookSession` applies to
    /// evidence, so a queued observation captured before the exit cannot lift the
    /// retirement it predates. An older or equal capture leaves the suppression
    /// untouched.
    fn clear_hook_suppression_for_detected_agent(
        &mut self,
        detected_agent: Option<Agent>,
        observed_at: Instant,
    ) {
        let Some(detected_agent) = detected_agent else {
            return;
        };
        let mut cleared_a_suppression = false;
        let mut stale_sessions = Vec::new();
        self.suppressed_hook_reports.retain(|source, suppressed| {
            let should_clear = crate::detect::parse_agent_label(&suppressed.agent_label)
                == Some(detected_agent)
                && suppressed.observed_at < observed_at
                && suppressed
                    .last_loss_observed_at
                    .is_none_or(|lost_at| lost_at < observed_at);
            if should_clear {
                cleared_a_suppression = true;
                if let Some(session_ref) = suppressed.session_ref.clone() {
                    stale_sessions.push((
                        source.clone(),
                        suppressed.agent_label.clone(),
                        session_ref,
                        suppressed.observed_at,
                        suppressed.last_loss_observed_at,
                    ));
                }
            }
            !should_clear
        });
        // Every entry collected above IS a fresh process observation: this owner was
        // retired, its agent is the detected process again, and the observation is
        // newer than every boundary the retirement left. The session it anchored stays stale
        // — a late callback must not resurrect it — but the observation is recorded on
        // it, so the agent's own explicit resume of that session can reclaim it.
        // The retirement itself, and any loss seen while this owner was only
        // suppressed, come WITH it: the session it becomes must not start life with a
        // clean boundary, or this very observation could arm it against a process a
        // newer loss already showed gone, or against a release that happened after it
        // was captured.
        for (source, agent_label, session_ref, retired_at, last_loss_observed_at) in stale_sessions
        {
            self.remember_stale_hook_session(
                source,
                agent_label,
                session_ref,
                retired_at,
                Some(observed_at),
                last_loss_observed_at,
            );
        }
        // The same observation re-arms sessions that were ALREADY stale, including one
        // whose earlier evidence a process exit has since expired: seeing this owner's
        // process again is the whole of the evidence, and it is no weaker the second
        // time. Without this, a session could be reclaimed only on the first fresh
        // process after its retirement and never again once an exit voided that one.
        for stale in self.stale_hook_sessions.values_mut().flatten() {
            if crate::detect::parse_agent_label(&stale.agent_label) == Some(detected_agent) {
                stale.record_fresh_process_evidence(observed_at);
            }
        }
        // A restarted process gets a fresh sequence window EXACTLY ONCE, at the
        // conversion. Now that this runs on every running observation, resetting the
        // fence unconditionally would leave the window open for as long as the agent is
        // detected at all, and any older report could replay through it.
        if cleared_a_suppression {
            let detected_label = crate::detect::agent_label(detected_agent);
            self.hook_report_sequences
                .retain(|source, _| !Self::hook_report_retirement_applies(source, detected_label));
        }
    }

    /// Record a retired session, applying BOTH boundaries — the retirement that made it
    /// stale and any loss seen since — BEFORE the evidence, so they can refuse it: an
    /// observation is only evidence of a process alive now if it is newer than both, and
    /// that holds just as much for the very observation that converts a suppression into
    /// a stale session as for every later one. Every argument is applied through
    /// `StaleHookSession`'s own accessors, so a session already present keeps whichever
    /// of each pair is newer.
    fn remember_stale_hook_session(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: crate::agent_resume::AgentSessionRef,
        retired_at: Instant,
        fresh_process_evidence: Option<Instant>,
        last_loss_observed_at: Option<Instant>,
    ) {
        let source_stale_sessions = self.stale_hook_sessions.entry(source).or_default();
        if !source_stale_sessions.iter().any(|existing| {
            existing.agent_label == agent_label && existing.session_ref == session_ref
        }) {
            source_stale_sessions.push(StaleHookSession {
                agent_label: agent_label.clone(),
                session_ref: session_ref.clone(),
                fresh_process_evidence: None,
                last_loss_observed_at: None,
                retired_at,
            });
        }
        for stale in source_stale_sessions.iter_mut().filter(|existing| {
            existing.agent_label == agent_label && existing.session_ref == session_ref
        }) {
            stale.observe_retirement(retired_at);
            if let Some(lost_at) = last_loss_observed_at {
                stale.observe_process_loss(lost_at);
            }
            if let Some(observed_at) = fresh_process_evidence {
                stale.record_fresh_process_evidence(observed_at);
            }
        }
    }

    /// Record that a LATER observation shows this owner's process gone, expiring any
    /// pending reclaim evidence it outdates.
    ///
    /// It runs over BOTH representations a retired owner can be in. A retirement lands
    /// first in `suppressed_hook_reports` and only becomes a `StaleHookSession` when a
    /// running observation converts it, so a loss seen during that window has no stale
    /// session to land on; recorded only on the sessions, it would be forgotten, and the
    /// session the conversion then creates would carry no boundary for a delayed running
    /// observation captured before that loss to fail against (Gate-3 B1).
    ///
    /// The evidence asserts a process that is still running, so it dies on the same
    /// observation that retires an identity for that owner: this owner's process seen
    /// exiting, or a different agent detected in its place — the two limbs of the
    /// identity rule (`hook_identity_conflicts_with_detected_agent` likewise decides
    /// nothing when no agent is detected at all). It cannot live INSIDE that path,
    /// because that path runs only while an identity is installed, and between observing
    /// the replacement process and accepting its session report there is deliberately
    /// none — exactly the window a delayed resume arrives in. Ordering is preserved the
    /// way `hook_identity_not_newer_than` preserves it for the identity: an observation
    /// captured before the evidence it would erase decides nothing. Expiry only voids
    /// the reclaim; a later fresh process observation arms it again.
    ///
    /// Every such observation is recorded on the session as the loss boundary, whether
    /// or not there is evidence left for it to expire: emptying the evidence must not
    /// also erase the fact that a loss was seen, or losses after the first would decide
    /// nothing and a running observation captured before the latest exit could re-arm
    /// a retired session (`StaleHookSession::last_loss_observed_at`).
    fn observe_process_loss_for_retired_owners(
        &mut self,
        detected_agent: Option<Agent>,
        process_exited: bool,
        observed_at: Instant,
    ) {
        let shows_loss_of = |agent_label: &str| {
            crate::detect::parse_agent_label(agent_label).is_some_and(|owner_agent| {
                (process_exited && detected_agent == Some(owner_agent))
                    || detected_agent.is_some_and(|detected_agent| detected_agent != owner_agent)
            })
        };
        for suppressed in self.suppressed_hook_reports.values_mut() {
            if shows_loss_of(&suppressed.agent_label) {
                suppressed.observe_process_loss(observed_at);
            }
        }
        for stale in self.stale_hook_sessions.values_mut().flatten() {
            if shows_loss_of(&stale.agent_label) {
                stale.observe_process_loss(observed_at);
            }
        }
    }

    fn detected_state_observed_before_release_suppression(
        &self,
        detected_agent: Option<Agent>,
        observed_at: Instant,
    ) -> bool {
        let Some(detected_agent) = detected_agent else {
            return false;
        };
        self.suppressed_hook_reports.values().any(|suppressed| {
            crate::detect::parse_agent_label(&suppressed.agent_label) == Some(detected_agent)
                && observed_at <= suppressed.observed_at
        })
    }

    fn current_session_identity_for_persistence(
        &self,
    ) -> Option<(
        String,
        String,
        crate::agent_resume::AgentSessionRefKind,
        String,
    )> {
        if let Some(authority) = self.hook_authority.as_ref() {
            if let Some(session_ref) = authority.session_ref.as_ref() {
                return Some((
                    authority.source.clone(),
                    authority.agent_label.clone(),
                    session_ref.kind,
                    session_ref.value.clone(),
                ));
            }
        }
        self.persisted_agent_session.as_ref().map(|session| {
            (
                session.source.clone(),
                session.agent.clone(),
                session.session_ref.kind,
                session.session_ref.value.clone(),
            )
        })
    }

    fn current_session_owner_conflicts(&self, source: &str, agent_label: &str) -> bool {
        self.current_session_identity_for_persistence().is_some_and(
            |(current_source, current_agent, _, _)| {
                current_source != source || current_agent != agent_label
            },
        )
    }

    fn conflicting_same_owner_session_ref(
        &self,
        source: &str,
        agent_label: &str,
        session_ref: &crate::agent_resume::AgentSessionRef,
        session_start_source: Option<&str>,
    ) -> Option<crate::agent_resume::AgentSessionRef> {
        self.current_session_identity_for_persistence().and_then(
            |(current_source, current_agent, current_kind, current_value)| {
                (current_source == source
                    && current_agent == agent_label
                    && current_kind == crate::agent_resume::AgentSessionRefKind::Id
                    && session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
                    && current_value != session_ref.value
                    && !Self::session_start_source_allows_session_replacement(
                        source,
                        agent_label,
                        session_start_source,
                    ))
                .then_some(crate::agent_resume::AgentSessionRef {
                    kind: current_kind,
                    value: current_value,
                })
            },
        )
    }

    fn session_start_source_allows_session_replacement(
        source: &str,
        agent_label: &str,
        session_start_source: Option<&str>,
    ) -> bool {
        matches!(
            (source, agent_label, session_start_source),
            (
                "zynk:claude",
                "claude",
                Some("clear" | "resume" | "compact")
            ) | (
                "zynk:codex",
                "codex",
                Some("startup" | "clear" | "resume" | "compact")
            ) | ("zynk:hermes", "hermes", Some("startup" | "new" | "resume"))
                // The antigravity-cli hook fires on PreInvocation and carries no start
                // source, so a conversation switch is the ONLY signal that its identity
                // moved; the identity-only `process_present` gate below still requires
                // the agent to be the detected foreground process before it may repoint.
                | ("zynk:antigravity_cli", "agy", None)
                // `select` is reported ONLY by the opencode TUI plugin, for the root
                // session this pane's own TUI has selected. `new`/`resume` come from the
                // shared server and may name an attached client's session, so neither
                // may repoint this pane.
                | ("zynk:opencode", "opencode", Some("select"))
                | ("zynk:pi", "pi", Some("new" | "resume" | "fork"))
                | (
                    "zynk:omp",
                    "omp",
                    Some("startup" | "new" | "resume" | "fork")
                )
                // Qwen Code names the reason it started a session, and every one of
                // these five reasons IS a new conversation in the same pane. The
                // identity-only `process_present` gate below still requires qwen to be
                // the detected foreground process before any of them may repoint.
                | (
                    "zynk:qwen",
                    "qwen",
                    Some("startup" | "clear" | "resume" | "compact" | "branch")
                )
        )
    }

    /// A `select` report from the opencode TUI plugin, which carries no sequence.
    ///
    /// The TUI plugin and the server plugin share the `zynk:opencode` source, and only
    /// the server plugin numbers its reports, so the shared sequence fence would refuse
    /// every unsequenced selection once the server has reported once. A report with no
    /// sequence carries no ordering the fence could ever use, so this shape is exempted
    /// from it rather than silently dropped.
    ///
    /// It is a pure function of the four values the hook report itself carries — never
    /// of `detected_agent`, `hook_authority` or `hook_identity` — and is pinned to the
    /// one `(source, agent_label)` pair `crate::detect::full_lifecycle_hook_authority`
    /// already owns, so it cannot widen any other owner.
    fn is_unsequenced_opencode_selection(
        source: &str,
        agent_label: &str,
        session_start_source: Option<&str>,
        seq: Option<u64>,
    ) -> bool {
        (source, agent_label, session_start_source, seq)
            == ("zynk:opencode", "opencode", Some("select"), None)
    }

    pub fn set_persisted_agent_session(
        &mut self,
        session: crate::agent_resume::PersistedAgentSession,
    ) {
        self.persisted_agent_session = Some(session);
    }

    pub fn set_agent_session_ref(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        self.set_agent_session_ref_for_session_start(source, agent_label, session_ref, seq, None)
    }

    pub fn set_agent_session_ref_for_session_start(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        seq: Option<u64>,
        session_start_source: Option<String>,
    ) -> Option<TerminalStateMutation> {
        let session_ref = session_ref?;
        // A session-identity-only owner reports its IDENTITY through this path too, so
        // the same retirement gate applies here as on the state-report path.
        if crate::detect::session_identity_only_integration(&source, &agent_label)
            && !self.hook_report_survives_retirement(
                &source,
                &agent_label,
                &Some(session_ref.clone()),
                session_start_source.as_deref(),
            )
        {
            return None;
        }
        let unsequenced_selection = Self::is_unsequenced_opencode_selection(
            &source,
            &agent_label,
            session_start_source.as_deref(),
            seq,
        );
        if !unsequenced_selection && !self.accept_hook_report(&source, seq) {
            return None;
        }
        if self.known_agent_label_conflicts_with_detected_agent(&agent_label) {
            return None;
        }
        // A session-identity-only integration holds no lifecycle authority, so a
        // claim that repoints an established anchor is only trustworthy while its
        // process is the detected agent. Otherwise a stale or background report
        // would silently rewrite the pane's session identity.
        let replacing_identity_only_session =
            crate::detect::session_identity_only_integration(&source, &agent_label)
                && Self::session_start_source_allows_session_replacement(
                    &source,
                    &agent_label,
                    session_start_source.as_deref(),
                )
                && self.current_session_identity_for_persistence().is_some_and(
                    |(current_source, current_agent, current_kind, current_value)| {
                        current_source == source
                            && current_agent == agent_label
                            && current_kind == crate::agent_resume::AgentSessionRefKind::Id
                            && session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
                            && current_value != session_ref.value
                    },
                );
        let process_present = crate::detect::parse_agent_label(&agent_label)
            .is_some_and(|known_agent| self.detected_agent == Some(known_agent));
        if replacing_identity_only_session && !process_present {
            return None;
        }
        let session_replacement_allowed = Self::session_start_source_allows_session_replacement(
            &source,
            &agent_label,
            session_start_source.as_deref(),
        );
        if self.current_session_owner_conflicts(&source, &agent_label)
            || self
                .conflicting_same_owner_session_ref(
                    &source,
                    &agent_label,
                    &session_ref,
                    session_start_source.as_deref(),
                )
                .is_some()
        {
            return None;
        }
        // A full-lifecycle owner that reports a NEW session for the anchor it already
        // holds is re-anchoring: its authority names the session it has just replaced,
        // so keeping it would report the stale session forever. Retire that session and
        // drop the authority, but only when the reported start source is one this owner
        // may legitimately replace on.
        let replaced_hook_session = self.same_owner_full_lifecycle_hook_authority_session_ref(
            &source,
            &agent_label,
            &session_ref,
        );
        if replaced_hook_session.is_some() && !session_replacement_allowed {
            return None;
        }

        let now = Instant::now();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        // A start source this owner may replace on RECLAIMS the session it names:
        // an earlier replacement may have retired it, and leaving it retired would
        // refuse every later report for the session that is now live again. Upstream
        // also admits a foreground takeover here; this fork has no takeover path, so
        // only the replacement gate reaches it.
        if session_replacement_allowed {
            self.forget_stale_hook_session(&source, &agent_label, &session_ref);
        }
        if let Some(replaced_hook_session) = replaced_hook_session {
            self.remember_stale_hook_session(
                source.clone(),
                agent_label.clone(),
                replaced_hook_session,
                now,
                None,
                None,
            );
            self.hook_authority = None;
        }
        if crate::detect::session_identity_only_integration(&source, &agent_label) {
            // For these integrations the session report IS the identity report: the
            // agent named it over its own hook, so it anchors identity (never state).
            self.retire_suppressed_session_after_accepting(
                &source,
                &agent_label,
                Some(&session_ref),
            );
            self.hook_identity = Some(HookIdentity {
                source: source.clone(),
                agent_label: agent_label.clone(),
                reported_at: Instant::now(),
                unconfirmed_since: self.unanswered_exit_to_inherit(&source, &agent_label),
            });
        }
        self.persisted_agent_session = Some(crate::agent_resume::PersistedAgentSession {
            source,
            agent: agent_label,
            session_ref,
        });
        let current_session = self.current_session_identity_for_persistence();
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session != current_session,
        })
    }

    fn known_agent_label_conflicts_with_detected_agent(&self, agent_label: &str) -> bool {
        let Some(detected_agent) = self.detected_agent else {
            return false;
        };
        crate::detect::parse_agent_label(agent_label)
            .is_some_and(|hook_agent| hook_agent != detected_agent)
    }

    /// Whether `seq` is behind (or repeats) the last sequence this source anchored.
    /// The non-mutating half of `accept_hook_report`, so a clear can fence against
    /// several owners before committing to any of them.
    fn hook_report_is_stale(&self, source: &str, seq: Option<u64>) -> bool {
        match seq {
            None => self.hook_report_sequences.contains_key(source),
            Some(seq) => self
                .hook_report_sequences
                .get(source)
                .is_some_and(|last_seq| seq <= *last_seq),
        }
    }

    fn accept_hook_report(&mut self, source: &str, seq: Option<u64>) -> bool {
        if self.hook_report_is_stale(source, seq) {
            return false;
        }
        if let Some(seq) = seq {
            self.hook_report_sequences.insert(source.to_string(), seq);
        }
        true
    }

    #[cfg(test)]
    pub fn clear_hook_authority(
        &mut self,
        source: Option<&str>,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        self.clear_hook_authority_with_mutation(source, seq)
            .and_then(|mutation| mutation.effective_state_change)
    }

    pub fn clear_hook_authority_with_mutation(
        &mut self,
        source: Option<&str>,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        // `pane.clear_agent_authority` permits `source = None`. The sequence fence then
        // has to resolve EVERY owner the clear would drop — the identity-only owner
        // holds no `hook_authority`, so resolving from that alone let a stale clear
        // erase a newer identity report.
        let sequence_sources: Vec<String> = match source {
            Some(source) => vec![source.to_string()],
            None => {
                let mut sources: Vec<String> = Vec::new();
                if let Some(authority) = self.hook_authority.as_ref() {
                    sources.push(authority.source.clone());
                }
                if let Some(identity) = self.hook_identity.as_ref() {
                    if !sources.iter().any(|known| known == &identity.source) {
                        sources.push(identity.source.clone());
                    }
                }
                sources
            }
        };
        if sequence_sources
            .iter()
            .any(|source| self.hook_report_is_stale(source, seq))
        {
            return None;
        }
        for source in &sequence_sources {
            self.accept_hook_report(source, seq);
        }

        let now = Instant::now();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        let should_clear_authority = self
            .hook_authority
            .as_ref()
            .is_some_and(|authority| source.is_none_or(|source| authority.source == source));
        // A session-identity-only integration never holds `hook_authority`, so its
        // own clear has to be able to drop the identity it DID record.
        let should_clear_identity = self
            .hook_identity
            .as_ref()
            .is_some_and(|identity| source.is_none_or(|source| identity.source == source));
        if !should_clear_authority && !should_clear_identity {
            return None;
        }
        // Scope each suppression to the owner actually being cleared: an obsolete
        // identity's own clear must not suppress a DIFFERENT owner's live authority.
        if should_clear_authority {
            self.suppress_current_hook_authority(HookSuppressionReason::HookClear, now);
        }
        let cleared_identity = if should_clear_identity {
            self.retire_hook_identity(HookSuppressionReason::HookClear, now)
        } else {
            None
        };
        if should_clear_authority {
            self.hook_authority = None;
            self.persisted_agent_session = None;
        } else if let Some(identity) = cleared_identity {
            // Only the session this identity anchored goes; another owner's stays.
            if self.persisted_agent_session_matches(&identity.source, &identity.agent_label) {
                self.persisted_agent_session = None;
            }
        }
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session
                != self.current_session_identity_for_persistence(),
        })
    }

    #[cfg(test)]
    pub fn release_agent(
        &mut self,
        source: &str,
        agent_label: &str,
        seq: Option<u64>,
    ) -> Option<EffectiveStateChange> {
        self.release_agent_with_mutation(source, agent_label, seq)
            .and_then(|mutation| mutation.effective_state_change)
    }

    pub fn release_agent_with_mutation(
        &mut self,
        source: &str,
        agent_label: &str,
        seq: Option<u64>,
    ) -> Option<TerminalStateMutation> {
        if !self.accept_hook_report(source, seq) {
            return None;
        }

        if self.hook_authority.as_ref().is_some_and(|authority| {
            authority.agent_label != agent_label || authority.source != source
        }) {
            return None;
        }
        // Symmetric guard for a session-identity-only integration: a release from
        // another owner must not drop the identity this one recorded. It is SCOPED,
        // not a veto: an obsolete identity must not block the owner of the live
        // authority from releasing, so that release simply leaves the identity alone.
        let releaser_owns_authority = self.hook_authority.as_ref().is_some_and(|authority| {
            authority.source == source && authority.agent_label == agent_label
        });
        let preserve_foreign_hook_identity = self.hook_identity.as_ref().is_some_and(|identity| {
            identity.agent_label != agent_label || identity.source != source
        });
        if preserve_foreign_hook_identity && !releaser_owns_authority {
            return None;
        }

        let matches_current_agent = self.effective_agent_label() == Some(agent_label);
        let matches_persisted_session = self.persisted_agent_session_matches(source, agent_label);
        // An identity-only integration can hold identity with no session and no
        // detected process; its own release still applies to it.
        let matches_hook_identity = self.hook_identity.as_ref().is_some_and(|identity| {
            identity.source == source && identity.agent_label == agent_label
        });
        if !matches_current_agent && !matches_persisted_session && !matches_hook_identity {
            return None;
        }
        let preserve_foreign_persisted_session = self
            .persisted_agent_session
            .as_ref()
            .is_some_and(|session| session.source != source || session.agent != agent_label);

        let now = Instant::now();
        let previous_agent_label = self.effective_agent_label().map(str::to_string);
        let previous_known_agent = self.effective_known_agent();
        let previous_state = self.state;
        let previous_presentation = self.effective_presentation_for_state_at(previous_state, now);
        let previous_session = self.current_session_identity_for_persistence();
        self.suppress_hook_report(source, agent_label, HookSuppressionReason::HookClear, now);
        self.detected_agent = None;
        self.fallback_state = AgentState::Unknown;
        self.fallback_visible_blocker = false;
        self.fallback_observed_at = None;
        self.hook_authority = None;
        if !preserve_foreign_hook_identity {
            self.hook_identity = None;
        }
        if !preserve_foreign_persisted_session {
            self.persisted_agent_session = None;
        }
        let current_session = self.current_session_identity_for_persistence();
        Some(TerminalStateMutation {
            effective_state_change: self.recompute_effective_state(
                previous_agent_label,
                previous_known_agent,
                previous_state,
                previous_presentation,
                now,
            ),
            session_ref_changed: previous_session != current_session,
        })
    }

    pub fn effective_agent_label(&self) -> Option<&str> {
        self.hook_authority
            .as_ref()
            .map(|authority| authority.agent_label.as_str())
            .or_else(|| self.detected_agent.map(crate::detect::agent_label))
    }

    pub fn effective_known_agent(&self) -> Option<Agent> {
        if let Some(authority) = &self.hook_authority {
            return crate::detect::parse_agent_label(&authority.agent_label);
        }
        self.detected_agent
    }

    pub fn full_lifecycle_hook_authority_active(&self) -> bool {
        self.live_full_lifecycle_hook_authority()
    }

    fn visible_blocker_overrides_hook(&self) -> bool {
        if self.live_full_lifecycle_hook_authority() {
            return false;
        }
        self.fallback_visible_blocker
            && self.fallback_not_older_than_hook()
            && self.hook_authority.as_ref().is_some_and(|authority| {
                authority.state != AgentState::Blocked
                    && crate::detect::parse_agent_label(&authority.agent_label)
                        == self.detected_agent
            })
    }

    fn live_full_lifecycle_hook_authority(&self) -> bool {
        self.hook_authority.as_ref().is_some_and(|authority| {
            crate::detect::full_lifecycle_hook_authority(&authority.source, &authority.agent_label)
        })
    }

    pub fn set_manual_label(&mut self, label: String) {
        let label = label.trim().to_string();
        self.manual_label = (!label.is_empty()).then_some(label);
    }

    pub fn clear_manual_label(&mut self) {
        self.manual_label = None;
    }

    pub fn set_agent_name(&mut self, name: String) {
        let name = name.trim().to_string();
        self.agent_name = (!name.is_empty()).then_some(name);
    }

    pub fn clear_agent_name(&mut self) {
        self.agent_name = None;
    }

    pub fn clear_agent_runtime_identity_after_respawn(&mut self) {
        self.detected_agent = None;
        self.fallback_state = AgentState::Unknown;
        self.fallback_visible_blocker = false;
        self.fallback_observed_at = None;
        self.hook_authority = None;
        self.hook_identity = None;
        self.persisted_agent_session = None;
        self.agent_metadata.clear();
        self.suppressed_hook_reports.clear();
        self.stale_hook_sessions.clear();
        self.state = AgentState::Unknown;
        self.last_agent_state_change_seq = None;
        self.launch_argv = None;
        self.respawn_shell_on_exit = false;
        self.pending_agent_resume_plan = None;
        self.clear_agent_name();
    }

    pub fn is_agent_terminal(&self) -> bool {
        self.agent_name.is_some()
            || self.effective_agent_label().is_some()
            || self.launch_argv.is_some()
    }

    pub fn border_label(&self, show_agent_labels: bool) -> Option<String> {
        self.effective_title().or_else(|| {
            self.manual_label.clone().or_else(|| {
                show_agent_labels
                    .then(|| {
                        self.effective_display_agent()
                            .or_else(|| self.effective_agent_label().map(str::to_string))
                    })
                    .flatten()
            })
        })
    }

    fn recompute_effective_state(
        &mut self,
        previous_agent_label: Option<String>,
        previous_known_agent: Option<Agent>,
        previous_state: AgentState,
        previous_presentation: EffectivePresentation,
        now: Instant,
    ) -> Option<EffectiveStateChange> {
        let state = if self.visible_blocker_overrides_hook() {
            AgentState::Blocked
        } else {
            self.hook_authority
                .as_ref()
                .map(|authority| authority.state)
                .unwrap_or(self.fallback_state)
        };
        let agent_label = self.effective_agent_label().map(str::to_string);
        let known_agent = self.effective_known_agent();

        let presentation = self.effective_presentation_for_state_at(state, now);
        self.clear_expiry_pending_for_hidden_metadata();

        if previous_agent_label == agent_label
            && previous_state == state
            && previous_presentation == presentation
        {
            return None;
        }

        self.state = state;
        Some(EffectiveStateChange {
            previous_agent_label,
            previous_known_agent,
            previous_state,
            previous_presentation,
            agent_label,
            known_agent,
            state,
            presentation,
        })
    }
}

pub(crate) fn stabilize_agent_detection(detection: crate::detect::AgentDetection) -> AgentState {
    detection.state
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::AgentDetection;

    fn test_terminal() -> TerminalState {
        TerminalState::new(TerminalId::alloc(), "/tmp".into())
    }

    fn test_session_path(name: &str) -> String {
        std::env::current_dir()
            .unwrap()
            .join(name)
            .display()
            .to_string()
    }

    /// The instant the production code ACTUALLY stamped on the hook authority it just
    /// recorded.
    ///
    /// Observations derive from this, never from a baseline captured before the setup
    /// ran. The ordering rules compare an observation against the RECORDED stamp
    /// (`hook_authority_not_newer_than`, and every retirement boundary built on it), and
    /// the setter stamps `Instant::now()` itself, so a pre-setup baseline plus a
    /// millisecond offset silently becomes an OLDER observation whenever the setup takes
    /// longer than the offset — a scheduling delay, a saturated test runner or a slow
    /// `test_session_path` is enough — and the decision under test flips.
    fn authority_reported_at(terminal: &TerminalState) -> Instant {
        terminal
            .hook_authority
            .as_ref()
            .expect("hook authority")
            .reported_at
    }

    /// `authority_reported_at` for the session-identity-only shape, which records
    /// `hook_identity` and never takes lifecycle authority.
    fn identity_reported_at(terminal: &TerminalState) -> Instant {
        terminal
            .hook_identity
            .as_ref()
            .expect("hook identity")
            .reported_at
    }

    #[test]
    fn stabilization_uses_raw_policy_state() {
        let detection = AgentDetection {
            state: AgentState::Idle,
            skip_state_update: false,
            visible_idle: false,
            visible_blocker: false,
            visible_working: false,
        };

        assert_eq!(stabilize_agent_detection(detection), AgentState::Idle);
    }

    #[test]
    fn hook_authority_overrides_fallback_for_same_agent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
        );

        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.effective_agent_label(), Some("pi"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn hook_authority_can_override_with_unknown_agent_label() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "zynk:custom".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            None,
        );

        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.effective_agent_label(), Some("custom-agent"));
        assert_eq!(terminal.effective_known_agent(), None);
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn omp_hook_authority_works_without_detected_agent_variant() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "zynk:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            None,
        );

        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.effective_agent_label(), Some("omp"));
        assert_eq!(terminal.effective_known_agent(), None);
        assert_eq!(terminal.state, AgentState::Working);

        let change = terminal.set_detected_state_with_visible_blocker(
            None,
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Unknown);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.is_none());
    }

    #[test]
    fn session_only_report_does_not_create_hook_authority() {
        for (agent, source, label, session_id) in [
            (Agent::Codex, "zynk:codex", "codex", "codex-session"),
            (Agent::Devin, "zynk:devin", "devin", "devin-session"),
        ] {
            let mut terminal = test_terminal();
            terminal.set_detected_state(Some(agent), AgentState::Idle);

            let mutation = terminal.set_agent_session_ref(
                source.into(),
                label.into(),
                crate::agent_resume::AgentSessionRef::id(session_id),
                Some(1),
            );

            assert!(mutation.is_some());
            assert!(terminal.hook_authority.is_none());
            assert!(!terminal.full_lifecycle_hook_authority_active());
            assert_eq!(terminal.state, AgentState::Idle);

            terminal.set_detected_state_with_screen_signals_at(
                Some(agent),
                AgentState::Working,
                false,
                false,
                false,
                false,
                Instant::now(),
            );

            assert_eq!(terminal.state, AgentState::Working);
        }
    }

    #[test]
    fn pi_session_replacement_reports_reanchor_full_lifecycle_authority() {
        for reason in ["new", "resume", "fork"] {
            let mut terminal = test_terminal();
            let old_session = test_session_path(&format!("pi-{reason}-old.jsonl"));
            let new_session = test_session_path(&format!("pi-{reason}-new.jsonl"));
            terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
            terminal.set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Idle,
                None,
                None,
                crate::agent_resume::AgentSessionRef::path(old_session),
                Some(10),
            );

            let session_report = terminal.set_agent_session_ref_for_session_start(
                "zynk:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::path(new_session.clone()),
                Some(11),
                Some(reason.into()),
            );

            assert!(
                session_report.is_some(),
                "{reason} should replace the previous Pi session"
            );
            assert!(terminal.hook_authority.is_none());

            let working = terminal.set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::path(new_session.clone()),
                Some(12),
            );

            assert!(
                working.is_some(),
                "{reason} should accept working for the replacement session"
            );
            assert_eq!(terminal.state, AgentState::Working);
            assert_eq!(
                terminal.hook_authority.as_ref().unwrap().session_ref,
                crate::agent_resume::AgentSessionRef::path(new_session)
            );
        }
    }

    #[test]
    fn pi_resume_reactivates_a_previously_stale_session() {
        let mut terminal = test_terminal();
        let session_a = test_session_path("pi-session-a.jsonl");
        let session_b = test_session_path("pi-session-b.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_a.clone()),
            Some(10),
        );

        terminal.set_agent_session_ref_for_session_start(
            "zynk:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(session_b.clone()),
            Some(11),
            Some("new".into()),
        );
        terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_b.clone()),
            Some(12),
        );

        let resumed = terminal.set_agent_session_ref_for_session_start(
            "zynk:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(session_a.clone()),
            Some(13),
            Some("resume".into()),
        );
        let working = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_a.clone()),
            Some(14),
        );

        assert!(resumed.is_some());
        assert!(working.is_some());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().session_ref,
            crate::agent_resume::AgentSessionRef::path(session_a)
        );

        let late_session_b = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_b),
            Some(15),
        );
        assert!(late_session_b.is_none());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn pi_startup_adopts_persisted_session_without_live_authority() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("pi-startup-old.jsonl");
        let new_session = test_session_path("pi-startup-new.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "zynk:pi".into(),
            agent: "pi".into(),
            session_ref: crate::agent_resume::AgentSessionRef::path(old_session)
                .expect("test session path should be valid"),
        });

        let startup = terminal.set_agent_session_ref_for_session_start(
            "zynk:pi".into(),
            "pi".into(),
            crate::agent_resume::AgentSessionRef::path(new_session.clone()),
            Some(11),
            Some("startup".into()),
        );

        assert!(startup.is_some());
        assert_eq!(
            terminal.current_session_identity_for_persistence(),
            Some((
                "zynk:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRefKind::Path,
                new_session,
            ))
        );
    }

    #[test]
    fn pi_non_replacement_reports_preserve_full_lifecycle_authority() {
        for reason in [None, Some("reload"), Some("startup")] {
            let mut terminal = test_terminal();
            let old_session = test_session_path("pi-current.jsonl");
            let new_session = test_session_path("pi-unexpected.jsonl");
            terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
            terminal.set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Idle,
                None,
                None,
                crate::agent_resume::AgentSessionRef::path(old_session.clone()),
                Some(10),
            );

            let session_report = terminal.set_agent_session_ref_for_session_start(
                "zynk:pi".into(),
                "pi".into(),
                crate::agent_resume::AgentSessionRef::path(new_session.clone()),
                Some(11),
                reason.map(str::to_string),
            );
            let working = terminal.set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::path(new_session),
                Some(12),
            );

            assert!(session_report.is_none());
            assert!(working.is_none());
            assert_eq!(terminal.state, AgentState::Idle);
            assert_eq!(
                terminal.hook_authority.as_ref().unwrap().session_ref,
                crate::agent_resume::AgentSessionRef::path(old_session),
                "{reason:?} must not replace the current Pi session"
            );
        }
    }

    #[test]
    fn omp_resume_session_report_reanchors_full_lifecycle_authority() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("omp-old.jsonl");
        let new_session = test_session_path("omp-new.jsonl");
        // This fork has no `Agent::Omp` variant, so omp is a hook-only identity and
        // nothing is detected on screen for it. Upstream seeds a matching detected
        // agent here purely to clear the known-agent conflict gate, which an
        // unparseable label clears anyway.
        terminal.set_hook_authority_with_session_ref(
            "zynk:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session.clone()),
            Some(10),
        );

        let session_report = terminal.set_agent_session_ref_for_session_start(
            "zynk:omp".into(),
            "omp".into(),
            crate::agent_resume::AgentSessionRef::path(new_session.clone()),
            Some(11),
            Some("resume".into()),
        );

        assert!(session_report.is_some());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .unwrap()
                .session_ref,
            crate::agent_resume::AgentSessionRef::path(new_session.clone()).unwrap()
        );

        let blocked = terminal.set_hook_authority_with_session_ref(
            "zynk:omp".into(),
            "omp".into(),
            AgentState::Blocked,
            Some("waiting".into()),
            None,
            crate::agent_resume::AgentSessionRef::path(new_session.clone()),
            Some(12),
        );

        assert!(blocked.is_some());
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().session_ref,
            crate::agent_resume::AgentSessionRef::path(new_session)
        );

        let stale = terminal.set_hook_authority_with_session_ref(
            "zynk:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session),
            Some(13),
        );

        assert!(stale.is_none());
        assert_eq!(terminal.state, AgentState::Blocked);
    }

    #[test]
    fn process_exit_clears_matching_full_lifecycle_hook_authority() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            None,
            Some(10),
            now,
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(1),
        );

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(
            change.effective_state_change.unwrap().previous_state,
            AgentState::Working
        );

        let stale = terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            None,
            Some(9),
            now + Duration::from_millis(2),
        );

        assert!(stale.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn process_exit_clears_omp_full_lifecycle_hook_authority_without_known_agent() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            None,
            None,
            Some(10),
            now,
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(1),
        );

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(
            change.effective_state_change.unwrap().previous_state,
            AgentState::Working
        );
    }

    #[test]
    fn late_full_lifecycle_hook_after_process_exit_does_not_reacquire_authority() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            None,
            Some(20),
            now,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(1),
        );
        let late = terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some("late".into()),
            None,
            Some(21),
            now + Duration::from_millis(2),
        );

        assert!(late.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn late_full_lifecycle_hook_with_same_session_after_process_exit_does_not_reacquire_authority()
    {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);
        terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(20),
        );
        let reported_at = authority_reported_at(&terminal);

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            reported_at + Duration::from_millis(1),
        );
        let late = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some("late".into()),
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(21),
        );

        assert!(late.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn late_full_lifecycle_hook_after_release_does_not_reacquire_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        terminal.release_agent("zynk:pi", "pi", Some(21));
        let late = terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(22),
        );

        assert!(late.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn late_full_lifecycle_hook_with_same_session_after_release_does_not_reacquire_authority() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(20),
        );

        terminal.release_agent("zynk:pi", "pi", Some(21));
        let late = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(22),
        );

        assert!(late.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn changed_session_ref_allows_full_lifecycle_hook_after_suppression() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(test_session_path("old.jsonl")),
            Some(20),
        );
        terminal.release_agent("zynk:pi", "pi", Some(21));

        let fresh = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(test_session_path("new.jsonl")),
            Some(22),
        );

        assert!(fresh.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn live_full_lifecycle_hook_rejects_different_session_ref_for_same_source() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(test_session_path("one.jsonl")),
            Some(20),
        );

        let mutation = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(test_session_path("two.jsonl")),
            Some(21),
        );

        assert!(mutation.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref())
                .map(|session_ref| session_ref.value.as_str()),
            Some(test_session_path("one.jsonl").as_str())
        );
    }

    #[test]
    fn fresh_detected_process_allows_full_lifecycle_hook_after_suppression() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );
        terminal.release_agent("zynk:pi", "pi", Some(21));
        let now = Instant::now();

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now,
        );
        let fresh = terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(22),
        );

        assert!(fresh.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn changed_session_ref_reanchors_hook_sequence_after_release() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(test_session_path("old.jsonl")),
            Some(1000),
        );
        terminal.release_agent("zynk:pi", "pi", Some(3000));

        let fresh = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(test_session_path("new.jsonl")),
            Some(1500),
        );

        assert!(fresh.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn stale_session_suppression_survives_multiple_release_generations() {
        let mut terminal = test_terminal();
        let session_a = test_session_path("release-generation-a.jsonl");
        let session_b = test_session_path("release-generation-b.jsonl");
        let session_c = test_session_path("release-generation-c.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_a.clone()),
            Some(1000),
        );
        terminal.release_agent("zynk:pi", "pi", Some(2000));

        let generation_b = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_b),
            Some(1500),
        );
        assert!(generation_b.is_some());
        terminal.release_agent("zynk:pi", "pi", Some(3000));

        let late_generation_a = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some("late".into()),
            crate::agent_resume::AgentSessionRef::path(session_a),
            Some(2500),
        );
        let generation_c = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_c),
            Some(2500),
        );

        assert!(late_generation_a.is_none());
        assert!(generation_c.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn fresh_detected_process_reanchors_hook_sequence_after_process_exit() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(1000),
        );
        let process_exit_seen_at = Instant::now() + Duration::from_millis(1);
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            process_exit_seen_at,
        );

        let fresh_process_seen_at = process_exit_seen_at + Duration::from_millis(1);
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            fresh_process_seen_at,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            fresh_process_seen_at + Duration::from_millis(1),
        );
        let fresh = terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(500),
        );

        assert!(fresh.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn fresh_detected_process_keeps_old_session_suppressed_after_process_exit() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("old-process-exit.jsonl");
        let new_session = test_session_path("new-process-exit.jsonl");
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session.clone()),
            Some(1000),
        );
        let process_exit_seen_at = Instant::now() + Duration::from_secs(1);
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            process_exit_seen_at,
        );

        let fresh_process_seen_at = process_exit_seen_at + Duration::from_millis(1);
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            fresh_process_seen_at,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            fresh_process_seen_at + Duration::from_millis(1),
        );

        let late_old = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some("late".into()),
            crate::agent_resume::AgentSessionRef::path(old_session),
            Some(500),
        );
        let fresh_new = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(new_session),
            Some(500),
        );

        assert!(late_old.is_none());
        assert!(fresh_new.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn different_session_after_process_exit_waits_for_fresh_process_evidence() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("old-before-process-exit.jsonl");
        let new_session = test_session_path("new-after-process-exit.jsonl");
        let now = Instant::now();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session),
            Some(1000),
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(1),
        );

        let early_new = terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(new_session.clone()),
            Some(500),
            now + Duration::from_millis(2),
        );

        assert!(early_new.is_none());
        assert!(terminal.hook_authority.is_none());

        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(3),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(4),
        );
        let fresh_new = terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(new_session),
            Some(500),
            now + Duration::from_millis(5),
        );

        assert!(fresh_new.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn missing_session_after_process_exit_waits_for_fresh_process_evidence() {
        let mut terminal = test_terminal();
        let old_session = test_session_path("old-before-nosession-process-exit.jsonl");
        let now = Instant::now();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(old_session),
            Some(1000),
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(1),
        );

        let early_without_session = terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            None,
            Some(500),
            now + Duration::from_millis(2),
        );

        assert!(early_without_session.is_none());
        assert!(terminal.hook_authority.is_none());

        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(3),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(4),
        );
        let fresh_without_session = terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            None,
            Some(500),
            now + Duration::from_millis(5),
        );

        assert!(fresh_without_session.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn stale_session_suppression_survives_multiple_process_generations() {
        let mut terminal = test_terminal();
        let session_a = test_session_path("generation-a.jsonl");
        let session_b = test_session_path("generation-b.jsonl");
        let session_c = test_session_path("generation-c.jsonl");
        let now = Instant::now();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_a.clone()),
            Some(1000),
            now,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(1),
        );
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(2),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(3),
        );
        let generation_b = terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_b),
            Some(500),
            now + Duration::from_millis(4),
        );
        assert!(generation_b.is_some());

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(5),
        );
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(6),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(7),
        );

        let late_generation_a = terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some("late".into()),
            crate::agent_resume::AgentSessionRef::path(session_a),
            Some(250),
            now + Duration::from_millis(8),
        );
        let generation_c = terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_c),
            Some(250),
            now + Duration::from_millis(9),
        );

        assert!(late_generation_a.is_none());
        assert!(generation_c.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn release_suppression_ignores_same_agent_idle_publish() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );
        terminal.release_agent("zynk:pi", "pi", Some(21));
        let retired_at = terminal.suppressed_hook_reports["zynk:pi"].observed_at;

        // Captured BEFORE the release that retired this owner, derived from the
        // retirement the release actually stamped: the publish has to lose to it.
        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            retired_at - Duration::from_millis(1),
        );
        let late = terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(22),
        );

        assert!(change.effective_state_change.is_none());
        assert!(late.is_none());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn fresh_session_ref_allows_full_lifecycle_hook_after_suppression() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );
        terminal.release_agent("zynk:pi", "pi", Some(21));

        let fresh = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("fresh-session"),
            Some(22),
        );

        assert!(fresh.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    // omp has no Agent identity (the process is pi), so its release suppression
    // can only be cleared by a fresh session ref, never by process detection.
    // Regression for upstream #614: a same-pane restart must reacquire lifecycle authority.
    #[test]
    fn omp_reacquires_full_lifecycle_hook_after_release_with_fresh_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_session_ref(
            "zynk:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("omp-old"),
            Some(20),
        );
        terminal.release_agent("zynk:omp", "omp", Some(21));

        // A late report from the released run keeps its old session ref and stays
        // suppressed, so a just-exited omp cannot resurrect the pane.
        let stale = terminal.set_hook_authority_with_session_ref(
            "zynk:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("omp-old"),
            Some(22),
        );
        assert!(stale.is_none());
        assert!(terminal.hook_authority.is_none());

        // A fresh omp run carries a new session ref and reacquires authority.
        let fresh = terminal.set_hook_authority_with_session_ref(
            "zynk:omp".into(),
            "omp".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("omp-new"),
            Some(23),
        );
        assert!(fresh.is_some());
        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn accepted_hook_report_marks_changed_when_same_owner_session_identity_changes() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "zynk:pi".into(),
            agent: "pi".into(),
            session_ref: crate::agent_resume::AgentSessionRef::path(test_session_path("old.jsonl"))
                .unwrap(),
        });

        let mutation = terminal
            .set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::path(test_session_path("new.jsonl")),
                Some(20),
            )
            .expect("accepted report");

        assert!(mutation.session_ref_changed);
    }

    #[test]
    fn codex_lifecycle_session_ref_replaces_existing_session_ref() {
        for session_start_source in ["startup", "clear", "resume", "compact"] {
            let mut terminal = test_terminal();
            terminal
                .set_agent_session_ref(
                    "zynk:codex".into(),
                    "codex".into(),
                    crate::agent_resume::AgentSessionRef::id("codex-session"),
                    Some(20),
                )
                .expect("initial session should be accepted");

            let next_session = format!("codex-{session_start_source}-session");
            let mutation = terminal
                .set_agent_session_ref_for_session_start(
                    "zynk:codex".into(),
                    "codex".into(),
                    crate::agent_resume::AgentSessionRef::id(&next_session),
                    Some(21),
                    Some(session_start_source.into()),
                )
                .unwrap_or_else(|| panic!("{session_start_source} should replace the session"));

            assert!(mutation.session_ref_changed);
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| session.session_ref.value.as_str()),
                Some(next_session.as_str())
            );
        }
    }

    #[test]
    fn opencode_tui_selection_replaces_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "zynk:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-old"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal
            .set_agent_session_ref_for_session_start(
                "zynk:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-selected"),
                None,
                Some("select".into()),
            )
            .expect("a local TUI selection should replace the session");

        assert!(mutation.session_ref_changed);
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-selected")
        );
    }

    #[test]
    fn qwen_lifecycle_session_ref_replaces_existing_session_ref() {
        // CARRY-IN #1 (upstream `a4d52ab6`): every start reason qwen reports names a
        // NEW conversation in the same pane, so each one may repoint the anchor while
        // qwen is the detected foreground process.
        for session_start_source in ["startup", "clear", "resume", "compact", "branch"] {
            let mut terminal = test_terminal();
            terminal.set_detected_state(Some(Agent::Qwen), AgentState::Idle);
            terminal
                .set_agent_session_ref(
                    "zynk:qwen".into(),
                    "qwen".into(),
                    crate::agent_resume::AgentSessionRef::id("qwen-session"),
                    Some(20),
                )
                .expect("initial session should be accepted");

            let next_session = format!("qwen-{session_start_source}-session");
            let mutation = terminal
                .set_agent_session_ref_for_session_start(
                    "zynk:qwen".into(),
                    "qwen".into(),
                    crate::agent_resume::AgentSessionRef::id(&next_session),
                    Some(21),
                    Some(session_start_source.into()),
                )
                .unwrap_or_else(|| panic!("{session_start_source} should replace the session"));

            assert!(mutation.session_ref_changed);
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| session.session_ref.value.as_str()),
                Some(next_session.as_str())
            );
        }
    }

    #[test]
    fn qwen_session_ref_does_not_replace_without_foreground_qwen() {
        // The identity-only `process_present` gate: qwen holds no lifecycle authority,
        // so a report that repoints an established anchor is only trusted while qwen is
        // the detected process. Without it the parent session is retained.
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "zynk:qwen".into(),
                "qwen".into(),
                crate::agent_resume::AgentSessionRef::id("qwen-parent"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "zynk:qwen".into(),
            "qwen".into(),
            crate::agent_resume::AgentSessionRef::id("qwen-branch"),
            Some(21),
            Some("branch".into()),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("qwen-parent")
        );
    }

    #[test]
    fn opencode_server_new_does_not_replace_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        terminal
            .set_agent_session_ref_for_session_start(
                "zynk:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-visible"),
                None,
                Some("select".into()),
            )
            .expect("local selection should be accepted");

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "zynk:opencode".into(),
            "opencode".into(),
            crate::agent_resume::AgentSessionRef::id("opencode-attached-client"),
            Some(21),
            Some("new".into()),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-visible")
        );
    }

    #[test]
    fn opencode_server_resume_does_not_replace_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        terminal
            .set_agent_session_ref_for_session_start(
                "zynk:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-visible"),
                None,
                Some("select".into()),
            )
            .expect("local selection should be accepted");

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "zynk:opencode".into(),
            "opencode".into(),
            crate::agent_resume::AgentSessionRef::id("opencode-attached-client"),
            Some(21),
            Some("resume".into()),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-visible")
        );
    }

    #[test]
    fn opencode_tui_selection_is_exempt_from_the_shared_sequence_fence() {
        // The TUI plugin sends no `seq` and shares `zynk:opencode` with the numbered
        // server plugin, so without the exemption the fence would refuse every
        // selection once the server had reported once.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        terminal
            .set_hook_authority_with_session_ref(
                "zynk:opencode".into(),
                "opencode".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::id("opencode-server-session"),
                Some(4_100),
            )
            .expect("the server plugin should anchor the first session it reports");
        assert!(terminal
            .set_agent_session_ref(
                "zynk:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-unsequenced"),
                None,
            )
            .is_none());

        let selected = terminal
            .set_agent_session_ref_for_session_start(
                "zynk:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-selected"),
                None,
                Some("select".into()),
            )
            .expect("an unsequenced local selection should still be accepted");

        assert!(selected.session_ref_changed);
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-selected")
        );
        // The exemption bypasses the fence, it does not rewrite it: the server
        // plugin's own numbering is left exactly where it was.
        assert_eq!(
            terminal.hook_report_sequences.get("zynk:opencode"),
            Some(&4_100)
        );
    }

    #[test]
    fn opencode_tui_selection_anchors_before_the_process_is_detected() {
        // Upstream stashes a selection reported before its process is detected and
        // replays it on detection. This fork has no such process gate on the
        // session-start path, so the selection anchors immediately and detection
        // then finds the session already in place — the same end state.
        let mut terminal = test_terminal();
        let startup_selection = terminal
            .set_agent_session_ref_for_session_start(
                "zynk:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-startup-selection"),
                None,
                Some("select".into()),
            )
            .expect("a selection reported before detection should still anchor");
        assert!(startup_selection.session_ref_changed);

        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);

        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-startup-selection")
        );
    }

    #[test]
    fn opencode_tui_selection_reanchors_full_lifecycle_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        let old_session = crate::agent_resume::AgentSessionRef::id("opencode-newer").unwrap();
        let selected_session =
            crate::agent_resume::AgentSessionRef::id("opencode-selected-older").unwrap();
        let attached_session =
            crate::agent_resume::AgentSessionRef::id("opencode-attached-client").unwrap();
        terminal
            .set_hook_authority_with_session_ref(
                "zynk:opencode".into(),
                "opencode".into(),
                AgentState::Idle,
                None,
                None,
                Some(old_session.clone()),
                Some(20),
            )
            .expect("initial session should own lifecycle state");
        let attached = terminal.set_hook_authority_with_session_ref(
            "zynk:opencode".into(),
            "opencode".into(),
            AgentState::Working,
            None,
            None,
            Some(attached_session.clone()),
            Some(21),
        );
        assert!(attached.is_none());
        assert_eq!(terminal.state, AgentState::Idle);

        let before_selection = Instant::now();
        let selected = terminal
            .set_agent_session_ref_for_session_start(
                "zynk:opencode".into(),
                "opencode".into(),
                Some(selected_session.clone()),
                None,
                Some("select".into()),
            )
            .expect("the selected session should replace the previous session");

        assert!(selected.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
        let retired = terminal.stale_hook_sessions["zynk:opencode"]
            .iter()
            .find(|stale| stale.session_ref == old_session)
            .expect("the replaced session must retain its retirement boundary");
        assert!(retired.retired_at >= before_selection);
        assert!(retired.retired_at <= Instant::now());
        assert!(retired.fresh_process_evidence.is_none());
        assert!(retired.last_loss_observed_at.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref),
            Some(&selected_session)
        );

        terminal
            .set_hook_authority_with_session_ref(
                "zynk:opencode".into(),
                "opencode".into(),
                AgentState::Working,
                None,
                None,
                Some(selected_session.clone()),
                Some(22),
            )
            .expect("the selected session should regain lifecycle authority");
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref()),
            Some(&selected_session)
        );

        let late_old_session = terminal.set_hook_authority_with_session_ref(
            "zynk:opencode".into(),
            "opencode".into(),
            AgentState::Idle,
            None,
            None,
            Some(old_session),
            Some(23),
        );
        assert!(late_old_session.is_none());
        assert_eq!(terminal.state, AgentState::Working);

        let late_attached_session = terminal.set_hook_authority_with_session_ref(
            "zynk:opencode".into(),
            "opencode".into(),
            AgentState::Blocked,
            None,
            None,
            Some(attached_session),
            Some(24),
        );
        assert!(late_attached_session.is_none());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn opencode_state_report_for_another_root_session_is_ignored() {
        // The shared opencode server reports activity for every root session it holds,
        // so a report naming a session this pane never selected is an attached client's
        // and must move neither the pane's state nor its session.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Idle);
        terminal
            .set_agent_session_ref_for_session_start(
                "zynk:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-visible"),
                None,
                Some("select".into()),
            )
            .expect("local selection should be accepted");

        let cross_talk = terminal.set_hook_authority_with_session_ref(
            "zynk:opencode".into(),
            "opencode".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("opencode-attached-client"),
            Some(31),
        );

        assert!(cross_talk.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-visible")
        );
        // The ignored report must not consume the shared sequence either.
        assert!(!terminal.hook_report_sequences.contains_key("zynk:opencode"));

        let visible = terminal
            .set_hook_authority_with_session_ref(
                "zynk:opencode".into(),
                "opencode".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::id("opencode-visible"),
                Some(32),
            )
            .expect("the anchored session's own report should still be accepted");

        assert!(visible.effective_state_change.is_some());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn opencode_session_ref_without_start_source_does_not_replace_existing() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "zynk:opencode".into(),
                "opencode".into(),
                crate::agent_resume::AgentSessionRef::id("opencode-old"),
                Some(20),
            )
            .expect("initial session should be accepted");

        // session.updated reports carry no session_start_source, so a different
        // id must not displace the established session (cross-talk guard).
        let mutation = terminal.set_agent_session_ref_for_session_start(
            "zynk:opencode".into(),
            "opencode".into(),
            crate::agent_resume::AgentSessionRef::id("opencode-other"),
            Some(21),
            None,
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-old")
        );
    }

    #[test]
    fn hermes_session_claim_leaves_state_to_detection() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("hermes-root").unwrap();

        let session = terminal.set_agent_session_ref_for_session_start(
            "zynk:hermes".into(),
            "hermes".into(),
            Some(session_ref.clone()),
            Some(10),
            Some("startup".into()),
        );

        assert!(session.is_some());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref),
            Some(&session_ref)
        );

        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Working);

        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_none());

        let replacement_ref =
            crate::agent_resume::AgentSessionRef::id("hermes-replacement").unwrap();
        let replacement = terminal.set_agent_session_ref_for_session_start(
            "zynk:hermes".into(),
            "hermes".into(),
            Some(replacement_ref.clone()),
            Some(11),
            Some("startup".into()),
        );

        assert!(replacement.is_some());
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref),
            Some(&replacement_ref)
        );

        let legacy_state = terminal.set_hook_authority_with_session_ref(
            "zynk:hermes".into(),
            "hermes".into(),
            AgentState::Blocked,
            None,
            None,
            Some(replacement_ref.clone()),
            Some(12),
        );
        assert!(legacy_state.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_none());

        terminal.set_detected_state(None, AgentState::Unknown);
        let background_replacement = terminal.set_agent_session_ref_for_session_start(
            "zynk:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id("hermes-background"),
            Some(13),
            Some("resume".into()),
        );
        assert!(background_replacement.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref),
            Some(&replacement_ref)
        );

        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        let retried_ref = crate::agent_resume::AgentSessionRef::id("hermes-background").unwrap();
        let retried_replacement = terminal.set_agent_session_ref_for_session_start(
            "zynk:hermes".into(),
            "hermes".into(),
            Some(retried_ref.clone()),
            Some(14),
            Some("resume".into()),
        );
        assert!(retried_replacement.is_some());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref),
            Some(&retried_ref)
        );
    }

    #[test]
    fn antigravity_cli_session_claim_leaves_state_to_detection() {
        // The antigravity-cli twin of `hermes_session_claim_leaves_state_to_detection`,
        // kept as its own test rather than folded into a table so the hermes
        // characterization stays byte-identical. The one behavioural difference is the
        // start source: the antigravity-cli hook fires on `PreInvocation` and reports no
        // session-start reason, so its replacement arm keys on `None`.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Antigravity), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("agy-root").unwrap();

        let session = terminal.set_agent_session_ref_for_session_start(
            "zynk:antigravity_cli".into(),
            "agy".into(),
            Some(session_ref.clone()),
            Some(10),
            None,
        );

        assert!(session.is_some());
        // Identity, never lifecycle authority.
        assert!(terminal.hook_authority.is_none());
        assert!(terminal.hook_identity.is_some());
        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref),
            Some(&session_ref)
        );

        terminal.set_detected_state(Some(Agent::Antigravity), AgentState::Working);

        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_none());

        // A conversation switch repoints the session while the agent is the detected
        // foreground process.
        let replacement_ref = crate::agent_resume::AgentSessionRef::id("agy-replacement").unwrap();
        let replacement = terminal.set_agent_session_ref_for_session_start(
            "zynk:antigravity_cli".into(),
            "agy".into(),
            Some(replacement_ref.clone()),
            Some(11),
            None,
        );

        assert!(replacement.is_some_and(|mutation| mutation.session_ref_changed));
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref),
            Some(&replacement_ref)
        );

        // A stateful report from the same owner is refused outright: an
        // identity-only integration never takes lifecycle authority.
        let legacy_state = terminal.set_hook_authority_with_session_ref(
            "zynk:antigravity_cli".into(),
            "agy".into(),
            AgentState::Blocked,
            None,
            None,
            Some(replacement_ref.clone()),
            Some(12),
        );
        assert!(legacy_state.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_none());

        // With the process gone the claim is background noise and must not rewrite
        // the pane's session identity.
        terminal.set_detected_state(None, AgentState::Unknown);
        let background_ref = crate::agent_resume::AgentSessionRef::id("agy-background").unwrap();
        let background_replacement = terminal.set_agent_session_ref_for_session_start(
            "zynk:antigravity_cli".into(),
            "agy".into(),
            Some(background_ref.clone()),
            Some(13),
            None,
        );
        assert!(background_replacement.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref),
            Some(&replacement_ref)
        );

        terminal.set_detected_state(Some(Agent::Antigravity), AgentState::Idle);
        let retried_replacement = terminal.set_agent_session_ref_for_session_start(
            "zynk:antigravity_cli".into(),
            "agy".into(),
            Some(background_ref.clone()),
            Some(14),
            None,
        );
        assert!(retried_replacement.is_some_and(|mutation| mutation.session_ref_changed));
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref),
            Some(&background_ref)
        );
    }

    #[test]
    fn identity_only_hook_report_keeps_identity_without_taking_lifecycle_authority() {
        // Codex Gate-2 M3 finding (msg_34f2e9b655927aaf): the single report a
        // session-identity-only integration sends carries BOTH a lifecycle state and
        // its session id. Only the LIFECYCLE half is dropped.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        let session_ref = crate::agent_resume::AgentSessionRef::id("hermes-1").unwrap();

        let mutation = terminal
            .set_hook_authority_with_session_ref(
                "zynk:hermes".into(),
                "hermes".into(),
                AgentState::Blocked,
                Some("ignored message".into()),
                Some("ignored status".into()),
                Some(session_ref.clone()),
                Some(1),
            )
            .expect("an identity report that records a session is a mutation");

        // Identity is kept: hook-reported source + label + session.
        assert_eq!(
            terminal
                .hook_identity
                .as_ref()
                .map(|identity| (identity.source.as_str(), identity.agent_label.as_str())),
            Some(("zynk:hermes", "hermes"))
        );
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref),
            Some(&session_ref)
        );
        assert!(mutation.session_ref_changed);

        // Lifecycle is not: no authority, no state change, no custom status, and the
        // screen keeps arbitrating afterwards.
        assert!(terminal.hook_authority.is_none());
        assert!(mutation.effective_state_change.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(!terminal.full_lifecycle_hook_authority_active());
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Working);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_none());
        assert!(terminal.hook_identity.is_some());
    }

    #[test]
    fn identity_only_hook_report_without_a_session_still_records_identity() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);

        let mutation = terminal
            .set_hook_authority_with_session_ref(
                "zynk:hermes".into(),
                "hermes".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some(1),
            )
            .expect("the first identity report is a mutation");

        assert!(!mutation.session_ref_changed);
        assert!(mutation.effective_state_change.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal
                .hook_identity
                .as_ref()
                .map(|identity| identity.agent_label.as_str()),
            Some("hermes")
        );
        assert!(terminal.persisted_agent_session.is_none());
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn identity_only_report_does_not_repoint_an_established_session() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        let first = crate::agent_resume::AgentSessionRef::id("hermes-1").unwrap();
        terminal.set_hook_authority_with_session_ref(
            "zynk:hermes".into(),
            "hermes".into(),
            AgentState::Idle,
            None,
            None,
            Some(first.clone()),
            Some(1),
        );

        // `pane.report_agent` carries no session-start reason, so a different id keeps
        // the anchor the pane already has (the M3-11 replacement guard).
        terminal.set_hook_authority_with_session_ref(
            "zynk:hermes".into(),
            "hermes".into(),
            AgentState::Idle,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("hermes-2"),
            Some(2),
        );

        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref),
            Some(&first)
        );
    }

    #[test]
    fn identity_only_hook_identity_ends_with_its_process() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "zynk:hermes".into(),
            "hermes".into(),
            AgentState::Idle,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("hermes-1"),
            Some(1),
        );
        assert!(terminal.hook_identity.is_some());

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Hermes),
            AgentState::Idle,
            false,
            false,
            true,
        );

        assert!(terminal.hook_identity.is_none());
        assert!(terminal.persisted_agent_session.is_none());
    }

    #[test]
    fn a_conflicting_detected_agent_drops_the_identity_only_hook_identity() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_session_ref(
            "zynk:hermes".into(),
            "hermes".into(),
            AgentState::Idle,
            None,
            None,
            None,
            Some(1),
        );
        assert!(terminal.hook_identity.is_some());

        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);

        assert!(terminal.hook_identity.is_none());
    }

    #[test]
    fn only_its_own_owner_clears_or_releases_an_identity_only_identity() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "zynk:hermes".into(),
            "hermes".into(),
            AgentState::Idle,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("hermes-1"),
            Some(1),
        );

        terminal.clear_hook_authority_with_mutation(Some("zynk:pi"), Some(2));
        terminal.release_agent_with_mutation("zynk:pi", "pi", Some(3));
        assert!(terminal.hook_identity.is_some());
        assert!(terminal.persisted_agent_session.is_some());

        assert!(terminal
            .clear_hook_authority_with_mutation(Some("zynk:hermes"), Some(4))
            .is_some());
        assert!(terminal.hook_identity.is_none());
        assert!(terminal.persisted_agent_session.is_none());
    }

    #[test]
    fn releasing_an_identity_only_integration_drops_its_identity() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "zynk:hermes".into(),
            "hermes".into(),
            AgentState::Idle,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("hermes-1"),
            Some(1),
        );

        terminal.release_agent_with_mutation("zynk:hermes", "hermes", Some(2));

        assert!(terminal.hook_identity.is_none());
        assert!(terminal.persisted_agent_session.is_none());
    }

    /// Report through the shipped session-identity-only shape: one `pane.report_agent`
    /// carrying both a lifecycle state and the session id.
    fn identity_report(terminal: &mut TerminalState, seq: u64) {
        terminal.set_hook_authority_with_session_ref(
            "zynk:hermes".into(),
            "hermes".into(),
            AgentState::Idle,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("hermes-1"),
            Some(seq),
        );
    }

    #[test]
    fn identity_retirement_rejects_a_late_state_report() {
        // Codex Gate-2 M3 extension finding (msg_fd6336c8e9038990): a released
        // identity-only owner must not come back on its own late state callback. A
        // higher sequence alone is NOT a new session.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_report(&mut terminal, 20);
        terminal.release_agent_with_mutation("zynk:hermes", "hermes", Some(21));
        assert!(terminal.hook_identity.is_none());

        identity_report(&mut terminal, 22);

        assert!(
            terminal.hook_identity.is_none(),
            "a retired Hermes identity was resurrected by its late state callback"
        );
    }

    #[test]
    fn identity_retirement_rejects_a_late_session_report() {
        // Same retirement, reached through `pane.report_agent_session` instead.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_report(&mut terminal, 20);
        terminal.release_agent_with_mutation("zynk:hermes", "hermes", Some(21));

        terminal.set_agent_session_ref_for_session_start(
            "zynk:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id("hermes-1"),
            Some(22),
            Some("resume".into()),
        );

        assert!(
            terminal.hook_identity.is_none(),
            "a retired Hermes identity was resurrected by its late session callback"
        );
    }

    #[test]
    fn identity_newer_than_exit_observation_is_preserved() {
        // The freshness protection the full-lifecycle path already applies
        // (`stale_process_exit_does_not_clear_newer_same_agent_hook_authority`), carried
        // across the identity/lifecycle split.
        let mut terminal = test_terminal();
        let observed = Instant::now();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:hermes".into(),
            "hermes".into(),
            AgentState::Idle,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("hermes-new"),
            Some(20),
            observed + Duration::from_secs(1),
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            observed,
        );

        assert!(
            terminal.hook_identity.is_some(),
            "an old exit observation erased a newer hook identity"
        );
        assert!(terminal.persisted_agent_session.is_some());
    }

    #[test]
    fn identity_stale_clear_all_keeps_the_newer_report() {
        // `pane.clear_agent_authority` permits `source = None`; the sequence fence then
        // has to resolve the ACTIVE identity's source, not only `hook_authority`'s.
        let mut terminal = test_terminal();
        identity_report(&mut terminal, 20);

        terminal.clear_hook_authority_with_mutation(None, Some(19));

        assert!(
            terminal.hook_identity.is_some(),
            "clear without a source bypassed the identity sequence fence"
        );
    }

    #[test]
    fn identity_does_not_prevent_a_new_full_owner_from_releasing() {
        // Owner coherence: accepting a full-lifecycle owner retires the identity-only
        // identity, so the obsolete identity cannot veto the accepted owner's cleanup.
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_session_ref(
            "zynk:hermes".into(),
            "hermes".into(),
            AgentState::Idle,
            None,
            None,
            None,
            Some(1),
        );
        terminal
            .set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::id("pi-new"),
                Some(1),
            )
            .expect("the new full-lifecycle owner was accepted");

        terminal.release_agent_with_mutation("zynk:pi", "pi", Some(2));

        assert!(
            terminal.hook_authority.is_none(),
            "the previous identity-only owner prevents the accepted full owner from releasing"
        );
    }

    #[test]
    fn identity_reacquires_after_release_with_a_fresh_session() {
        // Retirement must not permanently ban a restart: a genuinely NEW session
        // re-anchors the identity, while the retired one stays retired.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_report(&mut terminal, 20);
        terminal.release_agent_with_mutation("zynk:hermes", "hermes", Some(21));

        let restarted = terminal.set_hook_authority_with_session_ref(
            "zynk:hermes".into(),
            "hermes".into(),
            AgentState::Idle,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("hermes-2"),
            Some(22),
        );

        assert!(restarted.is_some());
        assert_eq!(
            terminal
                .hook_identity
                .as_ref()
                .map(|identity| identity.agent_label.as_str()),
            Some("hermes")
        );
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("hermes-2")
        );

        // The session the release retired stays retired across the next generation.
        terminal.release_agent_with_mutation("zynk:hermes", "hermes", Some(23));
        identity_report(&mut terminal, 24);
        assert!(terminal.hook_identity.is_none());
    }

    #[test]
    fn identity_session_start_reacquires_after_release_with_a_fresh_session() {
        // The same positive path through `pane.report_agent_session`.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_report(&mut terminal, 20);
        terminal.release_agent_with_mutation("zynk:hermes", "hermes", Some(21));

        let restarted = terminal.set_agent_session_ref_for_session_start(
            "zynk:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id("hermes-2"),
            Some(22),
            Some("startup".into()),
        );

        assert!(restarted.is_some());
        assert!(terminal.hook_identity.is_some());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("hermes-2")
        );
    }

    #[test]
    fn identity_resumes_after_process_exit_with_fresh_process_evidence() {
        // A process exit retires the identity until a FRESH process is observed —
        // the same evidence bar the full-lifecycle path applies.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_report(&mut terminal, 20);
        let observed = identity_reported_at(&terminal);

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            observed + Duration::from_secs(1),
        );
        assert!(terminal.hook_identity.is_none());

        // A late callback alone does not bring it back.
        identity_report(&mut terminal, 21);
        assert!(terminal.hook_identity.is_none());

        // A fresh process does.
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            observed + Duration::from_secs(2),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            observed + Duration::from_secs(3),
        );
        let resumed = terminal.set_hook_authority_with_session_ref(
            "zynk:hermes".into(),
            "hermes".into(),
            AgentState::Idle,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("hermes-2"),
            Some(5),
        );

        assert!(resumed.is_some());
        assert!(terminal.hook_identity.is_some());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("hermes-2")
        );
    }

    /// Report the identity-only owner's session the way `pane.report_agent_session`
    /// does, with an explicit session-start reason.
    fn identity_session_start(
        terminal: &mut TerminalState,
        session: &str,
        seq: u64,
        session_start_source: &str,
    ) -> Option<TerminalStateMutation> {
        terminal.set_agent_session_ref_for_session_start(
            "zynk:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id(session),
            Some(seq),
            Some(session_start_source.into()),
        )
    }

    /// Retire the identity-only owner's session, either by its own release or by an
    /// observed process exit, then observe its process AGAIN — the fresh
    /// post-retirement process evidence the restart path already relies on.
    fn retire_then_observe_a_fresh_process(
        terminal: &mut TerminalState,
        observed: Instant,
        process_exit: bool,
    ) {
        if process_exit {
            terminal.set_detected_state_with_screen_signals_at(
                Some(Agent::Hermes),
                AgentState::Idle,
                false,
                false,
                false,
                true,
                observed + Duration::from_secs(1),
            );
        } else {
            terminal.release_agent_with_mutation("zynk:hermes", "hermes", Some(21));
        }
        assert!(
            terminal.hook_identity.is_none(),
            "retirement dropped nothing"
        );
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            observed + Duration::from_secs(2),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            observed + Duration::from_secs(3),
        );
    }

    fn same_session_resume_after_retirement(process_exit: bool) {
        // Codex Gate-2 M3 extension finding (msg_5fe4c9a3eff5f1a8): the Hermes resume
        // command deliberately REUSES `session_ref.value` (`src/agent_resume.rs`), so a
        // new id would be a new session, not a resume. An explicit `resume` naming the
        // retired session, made after the agent's process was observed again, is the
        // legitimate reclaim — the same evidence bar the restart path applies.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
        let observed = identity_reported_at(&terminal);

        retire_then_observe_a_fresh_process(&mut terminal, observed, process_exit);

        let resumed = identity_session_start(&mut terminal, "existing-session", 30, "resume");

        assert!(
            resumed.is_some(),
            "an explicit same-session resume backed by fresh process evidence was refused"
        );
        assert!(terminal.hook_identity.is_some());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("existing-session")
        );
        // Reclaimed, not merely waved through once: the session is live again, so this
        // owner's ordinary reports for it are no longer refused as stale.
        assert!(
            !terminal.hook_report_matches_stale_session(
                "zynk:hermes",
                "hermes",
                &crate::agent_resume::AgentSessionRef::id("existing-session"),
            ),
            "the reclaimed session stayed retired, so its ordinary reports would still be refused"
        );
    }

    #[test]
    fn identity_same_session_resume_after_release_with_fresh_process_evidence() {
        same_session_resume_after_retirement(false);
    }

    #[test]
    fn identity_same_session_resume_after_process_exit_with_fresh_process_evidence() {
        same_session_resume_after_retirement(true);
    }

    fn reordered_running_observation_after_exit(repeated_exit: bool) {
        // Codex Gate-2 M3 extension finding (msg_d2c0941e71e39abe): expiring pending
        // reclaim evidence throws away the only record of WHEN the process was last
        // seen gone, so once evidence is absent every later loss observation decides
        // nothing and a delayed running observation captured BEFORE the latest exit
        // re-arms the retired session. The latest loss is a boundary in its own right:
        // only a running observation strictly newer than it is evidence of a process
        // alive now.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
        let observed = identity_reported_at(&terminal);
        retire_then_observe_a_fresh_process(&mut terminal, observed, false);

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            observed + Duration::from_secs(4),
        );
        if repeated_exit {
            // Loss observations must advance the retirement boundary even when
            // the first loss already emptied the pending evidence.
            terminal.set_detected_state_with_screen_signals_at(
                Some(Agent::Hermes),
                AgentState::Idle,
                false,
                false,
                false,
                true,
                observed + Duration::from_secs(8),
            );
        }
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            observed + Duration::from_secs(9),
        );
        assert!(
            identity_session_start(&mut terminal, "existing-session", 30, "resume").is_none(),
            "the unreplayed loss should retire the session"
        );

        // Delivered after the exit, but captured BEFORE the latest exit.
        let stale_observation = if repeated_exit { 6000 } else { 3500 };
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            observed + Duration::from_millis(stale_observation),
        );
        assert!(
            identity_session_start(&mut terminal, "existing-session", 31, "resume").is_none(),
            "a running observation older than the latest exit re-armed the retired session"
        );
        assert!(terminal.hook_identity.is_none());
        assert!(terminal.persisted_agent_session.is_none());

        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            observed + Duration::from_secs(10),
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            observed + Duration::from_secs(11),
        );
        assert!(
            identity_session_start(&mut terminal, "existing-session", 32, "resume").is_some(),
            "evidence genuinely newer than the latest loss must still allow a resume"
        );
    }

    /// Drive the identity-only owner through a retirement and a following running
    /// observation at CAPTURE timestamps the caller chooses, so a test can reproduce
    /// the order the detector saw independently of when the app handled it.
    fn observe_at(
        terminal: &mut TerminalState,
        agent: Option<Agent>,
        process_exited: bool,
        observed_at: Instant,
    ) {
        terminal.set_detected_state_with_screen_signals_at(
            agent,
            if agent.is_some() {
                AgentState::Idle
            } else {
                AgentState::Unknown
            },
            false,
            false,
            false,
            process_exited,
            observed_at,
        );
    }

    /// Drive a release, a new session that consumes its suppression, and a delayed
    /// running observation, with `fresh` picking the two halves apart: an observation
    /// captured AFTER the release is the legitimate resume evidence, one captured
    /// BEFORE it proves nothing about a process released since.
    fn reclaim_after_new_session_consumes_suppression(fresh: bool) {
        // Codex B1 finding (msg_872d0d03dd6e4a49): an API release observes no process,
        // so it seeds NO loss boundary and the only boundary it leaves is the
        // retirement itself. Accepting a new session consumes the suppression and
        // converts the retired one into a stale session; forwarding just the loss
        // boundary handed that session no boundary at all, so a running observation
        // captured BEFORE the release re-armed it through the already-stale re-arm
        // loop and an explicit resume repointed identity off the live session back
        // onto the retired one.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "retired-session", 20, "startup")
            .expect("initial session");
        let before_release = Instant::now();
        terminal
            .release_agent_with_mutation("zynk:hermes", "hermes", Some(21))
            .expect("release");
        assert!(terminal.suppressed_hook_reports["zynk:hermes"].observed_at >= before_release);
        identity_session_start(&mut terminal, "new-session", 30, "startup")
            .expect("a new session is allowed after release");
        assert!(!terminal.suppressed_hook_reports.contains_key("zynk:hermes"));
        let running_at = if fresh {
            Instant::now()
        } else {
            before_release
        };
        observe_at(&mut terminal, Some(Agent::Hermes), false, running_at);
        let result = identity_session_start(&mut terminal, "retired-session", 40, "resume");
        if fresh {
            assert!(
                result.is_some(),
                "fresh post-retirement process evidence must allow an explicit resume"
            );
        } else {
            assert!(
                result.is_none(),
                "a pre-release running observation re-armed a retired session after a \
                 new-session report consumed its suppression"
            );
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| session.session_ref.value.as_str()),
                Some("new-session")
            );
        }
    }

    #[test]
    fn pre_release_observation_stays_stale_after_new_session_report() {
        reclaim_after_new_session_consumes_suppression(false);
    }

    #[test]
    fn post_release_observation_allows_resume_after_new_session_report() {
        reclaim_after_new_session_consumes_suppression(true);
    }

    #[test]
    fn observation_at_the_release_instant_stays_stale_after_new_session_report() {
        // The boundary is STRICT, the way it already is for a loss: an observation
        // captured at the very instant of the release shows a process that was still
        // running WHEN it was released, never one running after it.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "retired-session", 20, "startup")
            .expect("initial session");
        terminal
            .release_agent_with_mutation("zynk:hermes", "hermes", Some(21))
            .expect("release");
        let retired_at = terminal.suppressed_hook_reports["zynk:hermes"].observed_at;
        identity_session_start(&mut terminal, "new-session", 30, "startup")
            .expect("a new session is allowed after release");
        observe_at(&mut terminal, Some(Agent::Hermes), false, retired_at);
        assert!(
            identity_session_start(&mut terminal, "retired-session", 40, "resume").is_none(),
            "an observation captured AT the retirement instant re-armed the retired session"
        );
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("new-session")
        );
    }

    #[test]
    fn repeated_releases_keep_the_latest_retirement_boundary() {
        // Evidence is scoped to the LATEST retirement, so the boundary a stale session
        // carries has to advance with every further one: an observation from between
        // two releases is older than the retirement it must beat, however much newer
        // it is than the first.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "retired-session", 20, "startup")
            .expect("initial session");
        terminal
            .release_agent_with_mutation("zynk:hermes", "hermes", Some(21))
            .expect("first release");
        let first_retirement = terminal.suppressed_hook_reports["zynk:hermes"].observed_at;
        identity_session_start(&mut terminal, "second-session", 30, "startup")
            .expect("a new session is allowed after the first release");
        terminal
            .release_agent_with_mutation("zynk:hermes", "hermes", Some(31))
            .expect("second release");
        let second_retirement = terminal.suppressed_hook_reports["zynk:hermes"].observed_at;
        assert!(first_retirement < second_retirement);
        identity_session_start(&mut terminal, "third-session", 40, "startup")
            .expect("a new session is allowed after the second release");
        assert!(!terminal.suppressed_hook_reports.contains_key("zynk:hermes"));

        // The work above puts a real monotonic gap between the two releases; assert the
        // instant lands strictly inside it rather than letting the case degenerate.
        let between_releases = first_retirement + (second_retirement - first_retirement) / 2;
        assert!(first_retirement < between_releases && between_releases < second_retirement);
        observe_at(&mut terminal, Some(Agent::Hermes), false, between_releases);
        assert!(
            identity_session_start(&mut terminal, "retired-session", 50, "resume").is_none(),
            "an observation older than the LATEST retirement re-armed the session the first \
             release retired"
        );
        assert!(
            identity_session_start(&mut terminal, "second-session", 51, "resume").is_none(),
            "an observation older than the LATEST retirement re-armed the session the second \
             release retired"
        );
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("third-session")
        );

        // A gap in the detected agent first — the shape this case was written against.
        // The conversion is keyed on the observation's CAPTURE TIME, so the gap decides
        // nothing here; what admits the resume below is a capture newer than every
        // retirement.
        observe_at(&mut terminal, None, false, Instant::now());
        observe_at(&mut terminal, Some(Agent::Hermes), false, Instant::now());
        assert!(
            identity_session_start(&mut terminal, "retired-session", 52, "resume").is_some(),
            "evidence newer than every retirement must still allow an explicit resume"
        );
    }

    #[test]
    fn a_release_advances_the_boundary_of_a_session_already_stale_from_an_exit() {
        // Codex B1 precision (msg_ec7ace170dd16799): the retirement boundary has to be
        // recorded on sessions that are ALREADY stale, not only on the one a conversion
        // is creating. Here the two halves of the boundary are deliberately pulled
        // apart: the ONLY loss this owner ever shows is the exit that retires the first
        // session, and the second retirement is a release, which observes no process
        // and so seeds no loss at all. The running observation replayed at the end is
        // newer than that single loss, so nothing but the release recorded on the
        // already-stale session can refuse it.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "retired-session", 20, "startup")
            .expect("initial session");

        let exited_at = Instant::now();
        observe_at(&mut terminal, Some(Agent::Hermes), true, exited_at);
        assert!(
            terminal.hook_identity.is_none(),
            "the observed exit retired nothing"
        );

        // The process is seen again, which converts the suppression into a stale
        // session and arms it. The gap in the detected agent is incidental: the
        // conversion turns on the observation being captured after the exit.
        observe_at(&mut terminal, None, false, Instant::now());
        let seen_running_at = Instant::now();
        observe_at(&mut terminal, Some(Agent::Hermes), false, seen_running_at);

        identity_session_start(&mut terminal, "second-session", 30, "startup")
            .expect("a new session is allowed once the process is running again");
        terminal
            .release_agent_with_mutation("zynk:hermes", "hermes", Some(31))
            .expect("release");
        let released_at = terminal.suppressed_hook_reports["zynk:hermes"].observed_at;
        assert!(exited_at < seen_running_at && seen_running_at < released_at);
        identity_session_start(&mut terminal, "third-session", 40, "startup")
            .expect("a new session is allowed after the release");
        assert!(!terminal.suppressed_hook_reports.contains_key("zynk:hermes"));

        // Replay the very observation that armed the first session BEFORE the release.
        // It is newer than the only loss this owner ever showed, so the release is its
        // one fence.
        observe_at(&mut terminal, Some(Agent::Hermes), false, seen_running_at);
        assert!(
            identity_session_start(&mut terminal, "retired-session", 50, "resume").is_none(),
            "a release did not advance the boundary of a session that was already stale"
        );
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("third-session")
        );

        observe_at(&mut terminal, None, false, Instant::now());
        observe_at(&mut terminal, Some(Agent::Hermes), false, Instant::now());
        assert!(
            identity_session_start(&mut terminal, "retired-session", 51, "resume").is_some(),
            "evidence newer than the release must still allow an explicit resume"
        );
    }

    /// Replay an exit that RETIRES a REPLACEMENT identity while the owner's already
    /// stale session holds newer running evidence, with `exit_is_newer` picking the two
    /// halves apart: an exit captured AFTER that evidence genuinely outdates it, one
    /// captured BEFORE it proves nothing about a process seen running since.
    ///
    /// Codex B1 finding (`msg_2f7b62540bcb70d5`, P2): `StaleHookSession::observe_retirement`
    /// advanced `retired_at` with the keep-the-newest rule but emptied the evidence
    /// UNCONDITIONALLY. Its already-stale caller — the loop in
    /// `suppress_hook_report_with_session_ref` — runs on every retirement of a
    /// replacement identity of the SAME owner, so a reordered older exit erased evidence
    /// strictly newer than both boundaries that retirement left behind, and merely
    /// having a replacement identity installed changed the capture-time rule: with no
    /// replacement to retire, `observe_process_loss` preserves that very same evidence.
    ///
    /// The case asserts the invariant at the session, not through an explicit resume:
    /// the exit that retires the replacement identity also opens a `ProcessExit`
    /// suppression, which refuses every report from this owner regardless of capture
    /// times, and the comment on the closing assertion explains why no arrangement of
    /// observations can discriminate the two behaviours through that wall.
    fn replayed_exit_with_a_replacement_identity(exit_is_newer: bool) {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "old-session", 20, "startup")
            .expect("initial session");
        terminal
            .release_agent_with_mutation("zynk:hermes", "hermes", Some(21))
            .expect("release");
        identity_session_start(&mut terminal, "replacement-session", 30, "startup")
            .expect("replacement session");

        // Every instant below is stamped from `base` and then slept PAST, so each one is
        // already in the PAST when the state machine handles it. That is the queue delay
        // this boundary exists for, reproduced without any future stand-in timestamp.
        let base = Instant::now();
        std::thread::sleep(Duration::from_millis(20));

        // The next observation is captured after the release, which is what converts
        // the release's suppression into a stale session for `old-session` and arms it
        // with post-retirement evidence; the agent gap before it decides nothing.
        observe_at(&mut terminal, None, false, base + Duration::from_millis(1));
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            false,
            base + Duration::from_millis(5),
        );
        assert!(
            terminal.stale_hook_sessions["zynk:hermes"][0].has_reclaimable_process_evidence(),
            "the post-release running observation did not arm the retired session"
        );

        // The exit retires the REPLACEMENT identity, and that retirement walks the
        // already-stale loop onto `old-session` as well.
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            true,
            base + Duration::from_millis(if exit_is_newer { 6 } else { 3 }),
        );
        let stale = &terminal.stale_hook_sessions["zynk:hermes"][0];
        assert_eq!(stale.session_ref.value, "old-session");
        assert_eq!(
            stale.fresh_process_evidence.is_some(),
            !exit_is_newer,
            "a replayed older exit must not erase newer running evidence merely because a \
             replacement identity is installed"
        );
        assert_eq!(
            stale.has_reclaimable_process_evidence(),
            !exit_is_newer,
            "evidence strictly newer than every boundary the retirement left behind must \
             stay reclaimable, and evidence the retirement outdates must not"
        );

        // The explicit resume itself stays refused in BOTH branches, and NOT for the
        // freshness reason: retiring the replacement identity on an observed exit also
        // opens a `ProcessExit` suppression for this source, and
        // `hook_report_is_suppressed` refuses every report from a suppressed owner on
        // that reason alone, before the stale-session reclaim is consulted at all.
        // Nothing can be arranged around that wall either, which is why the reclaimable
        // evidence above is the whole observable difference here:
        // `detected_state_observed_before_release_suppression` ignores any observation
        // captured at or before a suppression, so the only observation that could lift
        // the wall is strictly newer than the retirement instant — and an observation
        // strictly newer than the retirement is exactly one that re-arms the evidence on
        // its own, leaving nothing to tell the two behaviours apart. This assertion pins
        // the wall so that a later change to it has to be deliberate.
        assert!(
            identity_session_start(&mut terminal, "old-session", 40, "resume").is_none(),
            "the ProcessExit suppression window, not the freshness rule, is what refuses \
             this resume"
        );
    }

    #[test]
    fn older_exit_retiring_a_replacement_identity_preserves_newer_evidence() {
        replayed_exit_with_a_replacement_identity(false);
    }

    #[test]
    fn newer_exit_retiring_a_replacement_identity_expires_older_evidence() {
        replayed_exit_with_a_replacement_identity(true);
    }

    #[test]
    fn clear_and_release_retirements_keep_strict_boundaries() {
        // The companion control for Codex B1 `msg_2f7b62540bcb70d5`: scoping the expiry
        // to the evidence a retirement OUTDATES must not relax the strict rule itself.
        // Both retirements that observe no process at all — an API clear and a release —
        // taken once and twice, are held against a running observation captured EARLIER
        // than, exactly AT, and NEWER than the last of them. An explicit resume is
        // admitted if and only if the observation is strictly newer than that boundary.
        for clear in [false, true] {
            for retirements in [1u64, 2] {
                for timing in [-1i8, 0, 1] {
                    let mut terminal = test_terminal();
                    terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
                    identity_session_start(&mut terminal, "old-session", 20, "startup")
                        .expect("initial session");
                    let mut last_retired_at = Instant::now();
                    for i in 0..retirements {
                        let seq = 30 + i * 10;
                        if clear {
                            terminal
                                .clear_hook_authority_with_mutation(Some("zynk:hermes"), Some(seq))
                                .expect("clear");
                        } else {
                            terminal
                                .release_agent_with_mutation("zynk:hermes", "hermes", Some(seq))
                                .expect("release");
                        }
                        last_retired_at =
                            terminal.suppressed_hook_reports["zynk:hermes"].observed_at;
                        identity_session_start(
                            &mut terminal,
                            &format!("new-session-{i}"),
                            seq + 1,
                            "startup",
                        )
                        .expect("replacement session");
                    }

                    // The agent gap first, as the original case was written; the
                    // conversion below turns on the observation's capture time alone.
                    observe_at(&mut terminal, None, false, Instant::now());
                    let running_at = match timing {
                        -1 => last_retired_at - Duration::from_nanos(1),
                        0 => last_retired_at,
                        _ => Instant::now(),
                    };
                    observe_at(&mut terminal, Some(Agent::Hermes), false, running_at);
                    let result =
                        identity_session_start(&mut terminal, "old-session", 100, "resume");
                    assert_eq!(
                        result.is_some(),
                        timing > 0,
                        "only evidence strictly newer than the LATEST retirement may reclaim a \
                         retired session (clear={clear}, retirements={retirements}, \
                         timing={timing})"
                    );
                }
            }
        }
    }

    #[test]
    fn queued_running_observation_newer_than_its_exit_outlives_handler_delay() {
        // Gate-3 B1 WARDEN-R13-OBSERVED-AT-001: the retirement boundary was the moment
        // the app HANDLED the exit, not the moment the detector OBSERVED it. The
        // 256-event channel and the 64-event bounded drain make handler delay an
        // ordinary execution mode, so a running observation captured AFTER the exit but
        // handled after that later moment was discarded as pre-retirement. Its session
        // then never gained fresh evidence and the agent's explicit same-session resume
        // was refused — an availability failure that withholds receipt authority from a
        // legitimately resumed owner.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");

        // Captured in causal order by the detector, none of them handled yet.
        let exit_at = Instant::now();
        let absent_at = exit_at + Duration::from_millis(1);
        let running_at = exit_at + Duration::from_millis(2);
        // The app drains the queue later than every one of those captures.
        std::thread::sleep(Duration::from_millis(20));

        observe_at(&mut terminal, Some(Agent::Hermes), true, exit_at);
        observe_at(&mut terminal, None, false, absent_at);
        observe_at(&mut terminal, Some(Agent::Hermes), false, running_at);

        assert!(
            identity_session_start(&mut terminal, "existing-session", 30, "resume").is_some(),
            "a running observation captured after the exit was discarded solely because the app handled the exit later"
        );
        assert!(terminal.hook_identity.is_some());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("existing-session")
        );
    }

    #[test]
    fn queued_running_observation_older_than_its_exit_stays_retired() {
        // The negative half of the same rule, and why the fix is a CAPTURE-time
        // boundary rather than no boundary at all: an observation the detector took
        // BEFORE the exit proves nothing about a process the exit showed gone, however
        // late it is delivered. A genuinely newer capture still re-arms.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");

        let exit_at = Instant::now() + Duration::from_secs(4);
        std::thread::sleep(Duration::from_millis(20));

        observe_at(&mut terminal, Some(Agent::Hermes), true, exit_at);
        observe_at(&mut terminal, None, false, exit_at + Duration::from_secs(1));
        // Delivered after the exit, captured a second before it.
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            false,
            exit_at - Duration::from_secs(1),
        );

        assert!(
            identity_session_start(&mut terminal, "existing-session", 30, "resume").is_none(),
            "a running observation captured before the exit re-armed the retired session"
        );
        assert!(terminal.hook_identity.is_none());
        assert!(terminal.persisted_agent_session.is_none());

        // Equal capture times decide nothing either: the documented rule is
        // strictly-newer, so an observation taken at the exit's own instant is not
        // evidence of a process alive after it.
        observe_at(&mut terminal, None, false, exit_at + Duration::from_secs(2));
        observe_at(&mut terminal, Some(Agent::Hermes), false, exit_at);

        assert!(
            identity_session_start(&mut terminal, "existing-session", 31, "resume").is_none(),
            "a running observation captured at the exit's own instant re-armed the retired session"
        );
        assert!(terminal.hook_identity.is_none());

        // A genuinely newer capture is still the evidence the resume needs.
        observe_at(&mut terminal, None, false, exit_at + Duration::from_secs(3));
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            false,
            exit_at + Duration::from_secs(4),
        );

        assert!(
            identity_session_start(&mut terminal, "existing-session", 32, "resume").is_some(),
            "evidence genuinely newer than the exit was refused"
        );
        assert!(terminal.hook_identity.is_some());
    }

    /// Establish an identity-only session whose hook report ARRIVED after the exit
    /// instant it is about to be compared against, the reorder window this rule exists
    /// for: `exit_at` is captured first, the process actually handling the report sleeps
    /// past it, and `reported_at` is therefore strictly newer than the exit the detector
    /// took earlier.
    fn identity_reported_after(exit_at: Instant, session: &str) -> TerminalState {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        std::thread::sleep(Duration::from_millis(20));
        identity_session_start(&mut terminal, session, 20, "startup").expect("initial session");
        assert!(
            identity_reported_at(&terminal) > exit_at,
            "the setup did not reproduce the reorder window"
        );
        terminal
    }

    #[test]
    fn hook_retirement_export_is_none_when_nothing_is_retired() {
        // An ordinary pane adds no bytes to the snapshot.
        let mut terminal = test_terminal();
        assert!(terminal.export_hook_retirement(Instant::now()).is_none());
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        assert!(
            terminal.export_hook_retirement(Instant::now()).is_none(),
            "a merely detected agent is not a retirement"
        );
    }

    #[test]
    fn hook_retirement_round_trip_preserves_every_fence() {
        // Gate-3 arbiter finding (msg_24fa384d30dfa9af): the fences, not the live
        // authority, are what has to survive a live handoff. Instants cannot cross a
        // process, so they travel as AGES and are re-based against the new server's own
        // clock — and the restored terminal has to DECIDE the same way the old one did.
        let mut source_terminal = test_terminal();
        // A ProcessExit retirement of the identity owner, with a loss seen while it was
        // only suppressed, plus a stale session armed with post-retirement evidence.
        source_terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut source_terminal, "retired-session", 20, "startup")
            .expect("initial session");
        // Every phase is separated by a real gap, so the boundaries are millisecond-
        // distinct: ages carry whole milliseconds, and a snapshot whose boundaries all
        // collapsed into one instant would prove nothing about ordering.
        let gap = || std::thread::sleep(Duration::from_millis(10));
        gap();
        let exit_at = Instant::now();
        observe_at(&mut source_terminal, Some(Agent::Hermes), true, exit_at);
        gap();
        observe_at(
            &mut source_terminal,
            Some(Agent::Hermes),
            false,
            Instant::now(),
        );
        // A second owner retired by a release, which observes no process at all: its
        // suppression carries a retirement boundary and no loss. Its label has to stop
        // contradicting the detected agent first, the ordinary hook-report rule.
        gap();
        source_terminal.set_detected_state(None, AgentState::Unknown);
        source_terminal
            .set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::id("pi-1"),
                Some(30),
            )
            .expect("pi authority");
        gap();
        source_terminal
            .release_agent_with_mutation("zynk:pi", "pi", Some(31))
            .expect("release");
        // And a live identity for the first owner again — under a NEW session, so the
        // retired one stays stale and the identity half travels alongside it.
        gap();
        source_terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut source_terminal, "second-session", 40, "startup")
            .expect("a new session is allowed after the retirement");

        gap();
        let captured_at = Instant::now();
        let snapshot = source_terminal
            .export_hook_retirement(captured_at)
            .expect("something is retired");
        assert!(!snapshot.sequences.is_empty());
        assert!(!snapshot.suppressed.is_empty());
        assert!(!snapshot.stale.is_empty());
        assert!(snapshot.hook_identity.is_some());
        // The gaps are real, so the boundaries are genuinely ordered rather than a pile
        // of zeroes that would pass any re-basing.
        let stale = &snapshot.stale[0];
        let evidence_age_ms = stale.evidence_age_ms.expect("the session is armed");
        assert!(
            evidence_age_ms < stale.retired_age_ms,
            "the evidence must be NEWER than the retirement it beats: {stale:?}"
        );
        assert!(
            evidence_age_ms > 0 && stale.retired_age_ms > 0,
            "the phases collapsed into one instant: {stale:?}"
        );

        // The replacement server restores 250 ms later, against its own clock.
        let restored_at = captured_at + Duration::from_millis(250);
        let mut restored_terminal = test_terminal();
        restored_terminal.restore_hook_retirement(snapshot.clone(), restored_at);

        // Re-exporting at the SAME offset reproduces the snapshot: every age, and so
        // every gap and every ordering between the boundaries, survived the round trip.
        assert_eq!(
            restored_terminal
                .export_hook_retirement(restored_at)
                .expect("the restored terminal is retired too"),
            snapshot,
            "a boundary lost its age across the round trip"
        );
        // The live authority is deliberately NOT restored — the next report re-takes it.
        assert!(restored_terminal.hook_authority.is_none());

        // The two terminals DECIDE identically for the same probes.
        for (source, agent_label, session) in [
            ("zynk:hermes", "hermes", "retired-session"),
            ("zynk:pi", "pi", "pi-1"),
        ] {
            let session_ref = crate::agent_resume::AgentSessionRef::id(session);
            assert_eq!(
                source_terminal.hook_report_is_suppressed(source, agent_label, &session_ref),
                restored_terminal.hook_report_is_suppressed(source, agent_label, &session_ref),
                "{source} decides suppression differently after a round trip"
            );
            for session_start_source in [None, Some("resume")] {
                assert_eq!(
                    source_terminal.hook_report_survives_retirement(
                        source,
                        agent_label,
                        &session_ref,
                        session_start_source,
                    ),
                    restored_terminal.hook_report_survives_retirement(
                        source,
                        agent_label,
                        &session_ref,
                        session_start_source,
                    ),
                    "{source} decides retirement differently after a round trip \
                     (session_start_source={session_start_source:?})"
                );
            }
        }

        // And a running observation captured after the restore re-arms the restored
        // fences exactly as it would have re-armed the originals.
        let running_at = restored_at + Duration::from_millis(100);
        observe_at(&mut restored_terminal, Some(Agent::Pi), false, running_at);
        assert!(
            !restored_terminal
                .suppressed_hook_reports
                .contains_key("zynk:pi"),
            "the restored release suppression did not convert on a newer observation"
        );
        assert!(
            restored_terminal.stale_hook_sessions["zynk:pi"]
                .iter()
                .any(|stale| stale.has_reclaimable_process_evidence()),
            "the converted session carries no reclaimable evidence"
        );

        // An UNANSWERED exit is a fence too, and since Codex Gate-2 `msg_3e339000b75278a4`
        // it is owner-scoped, so the round trip has to carry its label as well as its age.
        // The exit is captured BEFORE the report it would erase, which is the shape that
        // holds an owner provisional instead of retiring it.
        gap();
        let fence_at = Instant::now();
        // Accepted, but it names the identity and session already installed, so it reports
        // no mutation — what matters is that it re-stamps `reported_at` past the exit.
        source_terminal.record_identity_only_hook_report_at(
            "zynk:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id("second-session"),
            Some(41),
            fence_at + Duration::from_millis(5),
        );
        assert!(
            source_terminal
                .hook_identity
                .as_ref()
                .is_some_and(|identity| identity.reported_at > fence_at),
            "the later Hermes report was refused, so the exit would retire instead of fence"
        );
        observe_at(&mut source_terminal, Some(Agent::Hermes), true, fence_at);
        assert_eq!(
            source_terminal
                .unanswered_hook_exit
                .as_ref()
                .map(|(owner, _)| (owner.source.as_deref(), owner.agent_label.as_str())),
            Some((Some("zynk:hermes"), "hermes")),
            "the reorder-window exit recorded no owner-keyed fence"
        );
        assert!(source_terminal.confirmed_hook_owner().is_none());

        gap();
        let fence_captured_at = Instant::now();
        let fence_snapshot = source_terminal
            .export_hook_retirement(fence_captured_at)
            .expect("the fence is a retirement fact of its own");
        assert_eq!(
            fence_snapshot
                .unanswered_exit
                .as_ref()
                .map(|exit| exit.agent_label.as_str()),
            Some("hermes"),
            "the exported fence lost its owner"
        );
        let fence_restored_at = fence_captured_at + Duration::from_millis(250);
        let mut fenced_terminal = test_terminal();
        fenced_terminal.restore_hook_retirement(fence_snapshot.clone(), fence_restored_at);
        assert_eq!(
            fenced_terminal
                .export_hook_retirement(fence_restored_at)
                .expect("the restored terminal carries the fence"),
            fence_snapshot,
            "the labelled fence lost a boundary across the round trip"
        );
        // It still gates the label it names, and only the detector's own observation of
        // THAT agent, captured after the restored fence, answers it.
        assert!(
            fenced_terminal.confirmed_hook_owner().is_none(),
            "the restored fence stopped gating its own owner"
        );
        observe_at(
            &mut fenced_terminal,
            Some(Agent::Hermes),
            false,
            fence_restored_at + Duration::from_millis(10),
        );
        assert_eq!(
            fenced_terminal.confirmed_hook_owner(),
            Some(("zynk:hermes", "hermes")),
            "a running observation newer than the restored fence did not answer it"
        );
        assert!(fenced_terminal.unanswered_hook_exit.is_none());
    }

    #[test]
    fn hook_retirement_restore_refuses_evidence_older_than_a_restored_boundary() {
        // The negative half, and the reason the ages travel at all: a restored boundary
        // has to REFUSE a running observation captured before it, exactly as the
        // original did. Without the re-based instants the fence would be a clean slate.
        let mut source_terminal = test_terminal();
        source_terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut source_terminal, "retired-session", 20, "startup")
            .expect("initial session");
        source_terminal
            .release_agent_with_mutation("zynk:hermes", "hermes", Some(21))
            .expect("release");
        let released_at = source_terminal.suppressed_hook_reports["zynk:hermes"].observed_at;

        let captured_at = released_at + Duration::from_millis(100);
        let snapshot = source_terminal
            .export_hook_retirement(captured_at)
            .expect("the release is a retirement");
        let restored_at = captured_at + Duration::from_millis(250);
        let mut restored_terminal = test_terminal();
        restored_terminal.restore_hook_retirement(snapshot, restored_at);

        // Captured 50 ms BEFORE the release, delivered on the new server.
        observe_at(
            &mut restored_terminal,
            Some(Agent::Hermes),
            false,
            restored_at - Duration::from_millis(150),
        );
        assert!(
            identity_session_start(&mut restored_terminal, "retired-session", 30, "resume")
                .is_none(),
            "an observation captured before the restored retirement re-armed the session"
        );

        // And the case that only a re-BASED boundary can decide, the handoff window
        // itself: an observation captured 50 ms before the restore is still NEWER than
        // the retirement, which the restored fence dates 100 ms before it. A restore
        // that dropped the ages and stamped every boundary at the restore instant would
        // refuse this one, so the assertion is what makes the ages load-bearing.
        observe_at(
            &mut restored_terminal,
            Some(Agent::Hermes),
            false,
            restored_at - Duration::from_millis(50),
        );
        assert!(
            identity_session_start(&mut restored_terminal, "retired-session", 31, "resume")
                .is_some(),
            "a restored fence banned the session instead of dating it"
        );
    }

    #[test]
    fn exit_captured_before_a_delayed_hook_report_holds_the_identity_provisional() {
        // Gate-3 B1 arbiter (msg_c76820d29bbb759b), the architect's causal-order gap.
        // Hook reports carry no capture time, so `reported_at` is stamped at ARRIVAL.
        // An exit the detector captured BEFORE that arrival but the App handled after it
        // looks older than the report, so it retired nothing and the identity — with its
        // persisted session — stayed receipt-capable although the detector had seen the
        // process gone and had not seen it since. Retiring outright is wrong the other
        // way: the same window is where a genuinely restarted agent's first session-start
        // report lands. The detector is the process oracle, so the identity is held
        // PROVISIONAL until a running observation confirms it.
        let exit_at = Instant::now();
        let mut terminal = identity_reported_after(exit_at, "existing-session");

        observe_at(&mut terminal, Some(Agent::Hermes), true, exit_at);

        assert!(
            terminal.hook_identity.is_some(),
            "the exit retired an identity it was too old to retire"
        );
        assert_eq!(
            terminal
                .hook_identity
                .as_ref()
                .and_then(|identity| identity.unconfirmed_since),
            Some(exit_at),
            "the unhandled exit was not recorded on the identity it could not retire"
        );
        assert!(
            terminal.confirmed_hook_owner().is_none(),
            "a provisional identity anchored a receipt"
        );
        // The session is part of the identity, so it stays while the identity does.
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("existing-session")
        );

        // The detector sees the process AFTER the exit: the claim is confirmed.
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            false,
            exit_at + Duration::from_millis(5),
        );

        assert!(terminal
            .hook_identity
            .as_ref()
            .is_some_and(|identity| identity.unconfirmed_since.is_none()));
        assert_eq!(
            terminal.confirmed_hook_owner(),
            Some(("zynk:hermes", "hermes")),
            "a confirmed identity was still refused"
        );
    }

    #[test]
    fn a_second_exit_while_provisional_retires_the_identity() {
        // The provisional state is a question, not an amnesty: if the detector never
        // sees the process alive between the two exits, the second one answers it, and
        // the identity is retired through the ordinary suppression funnel stamped with
        // THIS exit's capture time, so late callbacks are fenced exactly as usual.
        let exit_at = Instant::now();
        let mut terminal = identity_reported_after(exit_at, "existing-session");

        observe_at(&mut terminal, Some(Agent::Hermes), true, exit_at);
        let second_exit_at = exit_at + Duration::from_millis(10);
        observe_at(&mut terminal, Some(Agent::Hermes), true, second_exit_at);

        assert!(terminal.hook_identity.is_none(), "the identity survived");
        assert!(terminal.persisted_agent_session.is_none());
        assert!(terminal.confirmed_hook_owner().is_none());
        let suppressed = &terminal.suppressed_hook_reports["zynk:hermes"];
        assert_eq!(suppressed.reason, HookSuppressionReason::ProcessExit);
        assert_eq!(suppressed.observed_at, second_exit_at);

        // An ordinary same-session report is refused, as after any retirement.
        assert!(
            terminal
                .record_identity_only_hook_report(
                    "zynk:hermes".into(),
                    "hermes".into(),
                    crate::agent_resume::AgentSessionRef::id("existing-session"),
                    Some(30),
                )
                .is_none(),
            "a late callback reclaimed a retired session"
        );

        // A running observation newer than the second exit re-arms it, and the explicit
        // resume is admitted under the existing rules.
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            false,
            second_exit_at + Duration::from_millis(10),
        );
        assert!(
            identity_session_start(&mut terminal, "existing-session", 31, "resume").is_some(),
            "an explicit resume backed by fresh process evidence was refused"
        );
        assert_eq!(
            terminal.confirmed_hook_owner(),
            Some(("zynk:hermes", "hermes"))
        );
    }

    #[test]
    fn an_old_owners_pending_exit_does_not_retaint_a_confirmed_new_owner() {
        // Codex Gate-2 P2 2 (`msg_3e339000b75278a4`). The terminal-wide pending exit was
        // folded into the inherited minimum WITHOUT its label, while the field itself was
        // cleared only by a running observation of the label that RECORDED it. So once
        // Hermes retired, every ordinary Pi report took the Hermes timestamp again and
        // `confirmed_hook_owner()` fell back from the confirmed Pi owner to `None`,
        // repeatedly and permanently. Ordered detector captures and past instants make the
        // sequence deterministic.
        let base = Instant::now() - Duration::from_secs(10);
        let mut terminal = test_terminal();
        observe_at(&mut terminal, Some(Agent::Hermes), false, base);
        terminal
            .record_identity_only_hook_report_at(
                "zynk:hermes".into(),
                "hermes".into(),
                crate::agent_resume::AgentSessionRef::id("hermes-before-pi"),
                Some(1),
                base + Duration::from_secs(2),
            )
            .expect("initial Hermes identity");

        // An exit CAPTURED before that report arrived: it retires nothing and holds the
        // Hermes owner provisional, recording the terminal-wide fence under its label.
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            true,
            base + Duration::from_secs(1),
        );
        assert!(terminal.confirmed_hook_owner().is_none());

        // The next exit answers the question the other way: Hermes is retired, and the
        // fence outlives it — still naming Hermes.
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            true,
            base + Duration::from_millis(2500),
        );
        assert!(
            terminal.hook_identity.is_none(),
            "the next exit retires Hermes"
        );
        assert!(terminal.persisted_agent_session.is_none());

        // Pi takes the pane, is accepted, and a newer running observation confirms it.
        observe_at(
            &mut terminal,
            Some(Agent::Pi),
            false,
            base + Duration::from_secs(3),
        );
        terminal
            .set_hook_authority_with_custom_status_at(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::id("pi-after-hermes"),
                Some(1),
                base + Duration::from_secs(4),
            )
            .expect("the new Pi owner is accepted");
        observe_at(
            &mut terminal,
            Some(Agent::Pi),
            false,
            base + Duration::from_secs(5),
        );
        assert_eq!(terminal.confirmed_hook_owner(), Some(("zynk:pi", "pi")));

        // The regression: an ordinary same-session Pi lifecycle follow-up.
        terminal
            .set_hook_authority_with_custom_status_at(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Idle,
                None,
                None,
                crate::agent_resume::AgentSessionRef::id("pi-after-hermes"),
                Some(2),
                base + Duration::from_secs(6),
            )
            .expect("ordinary Pi lifecycle follow-up");
        assert_eq!(
            terminal.confirmed_hook_owner(),
            Some(("zynk:pi", "pi")),
            "Hermes's pending exit made an already-confirmed Pi owner non-receipt-capable again"
        );
        // The fence is not silently dropped either: it still stands, for Hermes alone.
        assert_eq!(
            terminal
                .unanswered_hook_exit
                .as_ref()
                .map(|(owner, _)| (owner.source.as_deref(), owner.agent_label.as_str())),
            Some((Some("zynk:hermes"), "hermes")),
            "scoping the fence must not discard it"
        );
    }

    #[test]
    fn a_pending_exit_still_fences_a_new_session_from_the_same_owner() {
        // The control for the scope above: it must not become an escape hatch. A pending
        // exit is a question about THIS owner's process, so a genuinely new session start
        // from the same owner still inherits it — a report is not process evidence — and
        // only a running observation of that owner, captured after the exit, answers it.
        let base = Instant::now() - Duration::from_secs(10);
        let mut terminal = test_terminal();
        observe_at(&mut terminal, Some(Agent::Hermes), false, base);
        terminal
            .record_identity_only_hook_report_at(
                "zynk:hermes".into(),
                "hermes".into(),
                crate::agent_resume::AgentSessionRef::id("hermes-one"),
                Some(1),
                base + Duration::from_secs(2),
            )
            .expect("initial Hermes identity");
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            true,
            base + Duration::from_secs(1),
        );
        assert_eq!(
            terminal.unanswered_hook_exit,
            Some((
                HookOwner::new("zynk:hermes", "hermes"),
                base + Duration::from_secs(1)
            )),
            "the reorder-window exit recorded no owner-keyed fence"
        );
        assert!(terminal.confirmed_hook_owner().is_none());

        identity_session_start(&mut terminal, "hermes-two", 2, "startup")
            .expect("a new session start from the same owner");
        assert!(
            terminal.confirmed_hook_owner().is_none(),
            "a new session from the fenced owner escaped its own pending exit"
        );
        assert_eq!(
            terminal
                .hook_identity
                .as_ref()
                .and_then(|identity| identity.unconfirmed_since),
            Some(base + Duration::from_secs(1)),
            "the OLDEST pending exit must survive the new session"
        );

        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            false,
            base + Duration::from_secs(3),
        );
        assert_eq!(
            terminal.confirmed_hook_owner(),
            Some(("zynk:hermes", "hermes")),
            "the detector's own observation did not answer the fence"
        );
        assert!(terminal.unanswered_hook_exit.is_none());
    }

    #[test]
    fn a_fence_recorded_for_one_source_never_gates_another_source_with_the_same_label() {
        // The refinement fix #20 could not reach: the fence was scoped to the agent LABEL, and two
        // owners can report the same label under different sources. The first source's unanswered
        // exit is a question about ITS process; it says nothing about the second source's.
        let base = Instant::now() - Duration::from_secs(10);
        let mut terminal = test_terminal();
        observe_at(&mut terminal, Some(Agent::Pi), false, base);
        terminal
            .set_hook_authority_with_custom_status_at(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some(1),
                base + Duration::from_secs(2),
            )
            .expect("the first Pi owner is accepted");
        // A reorder-window exit: captured BEFORE the report it would retire, so the owner is held
        // provisional and the terminal records the fence against it.
        observe_at(
            &mut terminal,
            Some(Agent::Pi),
            true,
            base + Duration::from_secs(1),
        );
        assert!(
            terminal.confirmed_hook_owner().is_none(),
            "the reorder-window exit must hold the first owner provisional"
        );
        assert_eq!(
            terminal
                .unanswered_hook_exit
                .as_ref()
                .map(|(owner, _)| (owner.source.as_deref(), owner.agent_label.as_str())),
            Some((Some("zynk:pi"), "pi")),
            "the fence must name the full owner it was recorded for"
        );

        // A DIFFERENT source reporting the same agent label takes the pane.
        terminal
            .set_hook_authority_with_custom_status_at(
                "other:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some(1),
                base + Duration::from_secs(4),
            )
            .expect("the second source is accepted");
        assert_eq!(
            terminal.confirmed_hook_owner(),
            Some(("other:pi", "pi")),
            "one source's pending exit gated another source with the same agent label"
        );
        // And the fence is not discarded: it still stands, for the source it names.
        assert_eq!(
            terminal
                .unanswered_hook_exit
                .as_ref()
                .map(|(owner, _)| (owner.source.as_deref(), owner.agent_label.as_str())),
            Some((Some("zynk:pi"), "pi")),
            "scoping the fence to the full owner must not discard it"
        );
    }

    #[test]
    fn a_delayed_pi_exit_does_not_gate_a_confirmed_codex_owner() {
        // The reviewer's own probe shape (warden R15, msg_e60980952feed121): a delayed Pi exit, a
        // Codex replacement, a fresh Codex observation that confirms the new owner, and then a
        // normal Codex lifecycle report, which must NOT return to None.
        let base = Instant::now() - Duration::from_secs(10);
        let mut terminal = test_terminal();
        observe_at(&mut terminal, Some(Agent::Pi), false, base);
        terminal
            .set_hook_authority_with_custom_status_at(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some(1),
                base + Duration::from_secs(2),
            )
            .expect("the Pi owner is accepted");
        observe_at(
            &mut terminal,
            Some(Agent::Pi),
            true,
            base + Duration::from_secs(1),
        );
        assert!(terminal.confirmed_hook_owner().is_none());

        observe_at(
            &mut terminal,
            Some(Agent::Codex),
            false,
            base + Duration::from_secs(3),
        );
        terminal
            .set_hook_authority_with_custom_status_at(
                "zynk:codex".into(),
                "codex".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some(1),
                base + Duration::from_secs(4),
            )
            .expect("the Codex owner is accepted");
        observe_at(
            &mut terminal,
            Some(Agent::Codex),
            false,
            base + Duration::from_secs(5),
        );
        assert_eq!(
            terminal.confirmed_hook_owner(),
            Some(("zynk:codex", "codex"))
        );

        terminal
            .set_hook_authority_with_custom_status_at(
                "zynk:codex".into(),
                "codex".into(),
                AgentState::Idle,
                None,
                None,
                None,
                Some(2),
                base + Duration::from_secs(6),
            )
            .expect("ordinary Codex lifecycle follow-up");
        assert_eq!(
            terminal.confirmed_hook_owner(),
            Some(("zynk:codex", "codex")),
            "Pi's pending exit made an already-confirmed Codex owner non-receipt-capable again"
        );
    }

    #[test]
    fn a_handoff_snapshot_round_trips_the_fence_source_with_its_label() {
        let base = Instant::now() - Duration::from_secs(10);
        let mut terminal = test_terminal();
        observe_at(&mut terminal, Some(Agent::Pi), false, base);
        terminal
            .set_hook_authority_with_custom_status_at(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some(1),
                base + Duration::from_secs(2),
            )
            .expect("the Pi owner is accepted");
        observe_at(
            &mut terminal,
            Some(Agent::Pi),
            true,
            base + Duration::from_secs(1),
        );

        let captured_at = base + Duration::from_secs(2);
        let snapshot = terminal
            .export_hook_retirement(captured_at)
            .expect("the fence is a retirement fact of its own");
        assert_eq!(
            snapshot
                .unanswered_exit
                .as_ref()
                .map(|exit| (exit.source.as_deref(), exit.agent_label.as_str())),
            Some((Some("zynk:pi"), "pi")),
            "the snapshot must carry the source with the label"
        );

        let restored_at = Instant::now();
        let mut restored = test_terminal();
        restored.restore_hook_retirement(snapshot, restored_at);
        assert_eq!(
            restored
                .unanswered_hook_exit
                .as_ref()
                .map(|(owner, _)| (owner.source.as_deref(), owner.agent_label.as_str())),
            Some((Some("zynk:pi"), "pi"))
        );
        // The restored fence gates its own owner and nobody else's.
        assert!(restored.hook_owner_has_unanswered_exit_older_than(
            "zynk:pi",
            "pi",
            restored_at + Duration::from_secs(1)
        ));
        assert!(!restored.hook_owner_has_unanswered_exit_older_than(
            "other:pi",
            "pi",
            restored_at + Duration::from_secs(1)
        ));
    }

    #[test]
    fn a_fence_from_a_snapshot_without_a_source_still_gates_its_label() {
        // Backward compatibility: a snapshot written by a server that keyed the fence by label
        // alone deserializes with no source (serde default) and keeps exactly the reach it had
        // there — its label under any source — rather than being narrowed or dropped.
        let decoded: UnansweredExitSnapshot =
            serde_json::from_str(r#"{"agent_label":"hermes","age_ms":1500}"#)
                .expect("an older snapshot still loads");
        assert_eq!(decoded.source, None);

        let restored_at = Instant::now();
        let mut restored = test_terminal();
        restored.restore_hook_retirement(
            HookRetirementSnapshot {
                sequences: Vec::new(),
                metadata_sequences: Vec::new(),
                suppressed: Vec::new(),
                stale: Vec::new(),
                hook_identity: None,
                unanswered_exit: Some(decoded),
            },
            restored_at,
        );
        for source in ["zynk:hermes", "some-other-source"] {
            assert!(
                restored.hook_owner_has_unanswered_exit_older_than(
                    source,
                    "hermes",
                    restored_at + Duration::from_secs(1)
                ),
                "a sourceless fence must gate {source} as the server that wrote it did"
            );
        }
        assert!(
            !restored.hook_owner_has_unanswered_exit_older_than(
                "zynk:pi",
                "pi",
                restored_at + Duration::from_secs(1)
            ),
            "a sourceless fence must still gate its own label only"
        );
        // And it re-exports without inventing a source it never had.
        assert_eq!(
            restored
                .export_hook_retirement(restored_at)
                .and_then(|snapshot| snapshot.unanswered_exit)
                .map(|exit| (exit.source, exit.agent_label)),
            Some((None, "hermes".to_string()))
        );
    }

    #[test]
    fn a_fence_recorded_for_one_owner_never_gates_another_live_or_after_a_handoff() {
        // The cross-owner control in the other direction, with the two representations
        // swapped: the fence sits on a full-lifecycle authority (`pi`) and the successor is
        // an identity-only owner (`hermes`). The successor's evidence is the detector's
        // observation of ITS process; the retired owner's unanswered exit says nothing
        // about it, live or across a live handoff.
        let base = Instant::now() - Duration::from_secs(10);
        let mut terminal = test_terminal();
        observe_at(&mut terminal, Some(Agent::Pi), false, base);
        terminal
            .set_hook_authority_with_custom_status_at(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::id("pi-one"),
                Some(1),
                base + Duration::from_secs(2),
            )
            .expect("the Pi owner is accepted");
        observe_at(
            &mut terminal,
            Some(Agent::Pi),
            true,
            base + Duration::from_secs(1),
        );
        assert!(terminal.confirmed_hook_owner().is_none());
        observe_at(
            &mut terminal,
            Some(Agent::Pi),
            true,
            base + Duration::from_millis(2500),
        );
        assert!(
            terminal.hook_authority.is_none(),
            "the second exit retires Pi"
        );
        assert!(terminal.persisted_agent_session.is_none());
        assert_eq!(
            terminal
                .unanswered_hook_exit
                .as_ref()
                .map(|(owner, _)| (owner.source.as_deref(), owner.agent_label.as_str())),
            Some((Some("zynk:pi"), "pi")),
            "the fence must name the owner it was recorded for"
        );

        // Live: Hermes takes the pane and its own identity report is receipt-capable at
        // once — nothing about Pi's process is evidence about this one.
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            false,
            base + Duration::from_secs(3),
        );
        terminal
            .record_identity_only_hook_report_at(
                "zynk:hermes".into(),
                "hermes".into(),
                crate::agent_resume::AgentSessionRef::id("hermes-after-pi"),
                Some(1),
                base + Duration::from_secs(4),
            )
            .expect("the new Hermes identity is accepted");
        assert_eq!(
            terminal.confirmed_hook_owner(),
            Some(("zynk:hermes", "hermes")),
            "Pi's pending exit gated an owner whose process it never observed"
        );

        // And across a live handoff: the exported fence still names Pi, and the restored
        // terminal decides the same way for a fresh Hermes owner.
        let mut fenced = test_terminal();
        observe_at(&mut fenced, Some(Agent::Pi), false, base);
        fenced
            .set_hook_authority_with_custom_status_at(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::id("pi-one"),
                Some(1),
                base + Duration::from_secs(2),
            )
            .expect("the Pi owner is accepted");
        observe_at(
            &mut fenced,
            Some(Agent::Pi),
            true,
            base + Duration::from_secs(1),
        );
        observe_at(
            &mut fenced,
            Some(Agent::Pi),
            true,
            base + Duration::from_millis(2500),
        );
        let captured_at = base + Duration::from_secs(3);
        let snapshot = fenced
            .export_hook_retirement(captured_at)
            .expect("a retired owner and its fence are exportable");
        assert_eq!(
            snapshot
                .unanswered_exit
                .as_ref()
                .map(|exit| exit.agent_label.as_str()),
            Some("pi"),
            "the exported fence lost its owner"
        );
        let restored_at = captured_at + Duration::from_millis(250);
        let mut restored = test_terminal();
        restored.restore_hook_retirement(snapshot, restored_at);
        restored
            .record_identity_only_hook_report_at(
                "zynk:hermes".into(),
                "hermes".into(),
                crate::agent_resume::AgentSessionRef::id("hermes-after-handoff"),
                Some(1),
                restored_at + Duration::from_millis(10),
            )
            .expect("the new Hermes identity is accepted on the replacement server");
        assert_eq!(
            restored.confirmed_hook_owner(),
            Some(("zynk:hermes", "hermes")),
            "a restored fence gated an owner it was never recorded for"
        );
    }

    #[test]
    fn a_hook_report_not_newer_than_the_exit_is_retired_as_before() {
        // The control: outside the reorder window nothing changes. A report that arrived
        // at or before the exit's capture is retired on the spot, exactly as today.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session");
        let reported_at = identity_reported_at(&terminal);
        std::thread::sleep(Duration::from_millis(20));
        let exit_at = Instant::now();
        assert!(reported_at <= exit_at);

        observe_at(&mut terminal, Some(Agent::Hermes), true, exit_at);

        assert!(terminal.hook_identity.is_none());
        assert!(terminal.persisted_agent_session.is_none());
        assert!(terminal.confirmed_hook_owner().is_none());
        assert_eq!(
            terminal.suppressed_hook_reports["zynk:hermes"].reason,
            HookSuppressionReason::ProcessExit
        );
    }

    #[test]
    fn handoff_confirmation_requires_strictly_newer_matching_running_evidence() {
        let imported_at = Instant::now();
        let mut terminal = test_terminal();
        terminal.record_identity_only_hook_report_at(
            "zynk:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id("same-session"),
            Some(1),
            imported_at - Duration::from_secs(1),
        );
        terminal.require_imported_hook_identity_confirmation(imported_at);
        for (agent, exited, offset, allowed) in [
            (Some(Agent::Hermes), false, 0, false),
            (Some(Agent::Hermes), true, 1, false),
            (Some(Agent::Pi), false, 1, false),
            (None, false, 1, false),
            (Some(Agent::Hermes), false, 1, true),
        ] {
            let mut observed = terminal.clone();
            observe_at(
                &mut observed,
                agent,
                exited,
                imported_at + Duration::from_millis(offset),
            );
            assert_eq!(
                observed.confirmed_hook_owner().is_some(),
                allowed,
                "{agent:?}/{exited}/{offset}"
            );
        }
        let mut replacement = terminal.clone();
        assert!(
            identity_session_start(&mut replacement, "new-session", 2, "new").is_none(),
            "replacement needs a detected process"
        );
        // Presence satisfies the existing session-replacement clamp, but evidence
        // captured at the import boundary is still too old to confirm a receipt.
        observe_at(&mut replacement, Some(Agent::Hermes), false, imported_at);
        identity_session_start(&mut replacement, "new-session", 3, "new")
            .expect("a genuinely new session can replace the imported session");
        assert_eq!(
            replacement
                .persisted_agent_session
                .as_ref()
                .unwrap()
                .session_ref
                .value,
            "new-session"
        );
        assert!(
            replacement.confirmed_hook_owner().is_none(),
            "a new-session report is still not process evidence"
        );
        observe_at(
            &mut replacement,
            Some(Agent::Hermes),
            false,
            imported_at + Duration::from_millis(1),
        );
        assert_eq!(
            replacement.confirmed_hook_owner(),
            Some(("zynk:hermes", "hermes"))
        );
        let captured_at = imported_at + Duration::from_millis(2);
        let snapshot = terminal.export_hook_retirement(captured_at).unwrap();
        let second_import = captured_at + Duration::from_secs(1);
        let mut second = test_terminal();
        second.restore_hook_retirement(snapshot, second_import);
        second.require_imported_hook_identity_confirmation(second_import);
        observe_at(&mut second, Some(Agent::Hermes), false, captured_at);
        assert!(
            second.confirmed_hook_owner().is_none(),
            "another handoff cannot reuse prior-server evidence"
        );
        observe_at(
            &mut second,
            Some(Agent::Hermes),
            false,
            second_import + Duration::from_millis(1),
        );
        assert_eq!(
            second.confirmed_hook_owner(),
            Some(("zynk:hermes", "hermes"))
        );
    }

    #[test]
    fn same_label_running_observation_after_an_exit_re_arms_the_retired_session() {
        // Gate-3 B1 arbiter (msg_fbcdf59a01d6f70d): the conversion out of a retirement
        // was keyed on a CHANGE of detected agent, so it never ran for the shape
        // production actually produces. An agent restarted in place is observed under
        // the SAME label across its own exit — the screen still shows it, and the
        // detector's miss-confirmation window may never report the agent gone — so the
        // retirement had no way back and the agent's own explicit resume was refused
        // forever, withholding receipt authority from a live owner.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");

        let exit_at = Instant::now();
        observe_at(&mut terminal, Some(Agent::Hermes), true, exit_at);
        assert!(
            terminal.hook_identity.is_none(),
            "the observed exit retired nothing"
        );

        // The restarted process, seen under the same label with NO `None` in between.
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            false,
            exit_at + Duration::from_millis(2),
        );

        assert!(
            !terminal.suppressed_hook_reports.contains_key("zynk:hermes"),
            "a running observation newer than the exit left the owner suppressed"
        );
        assert!(
            terminal.stale_hook_sessions["zynk:hermes"]
                .iter()
                .any(|stale| stale.session_ref.value == "existing-session"
                    && stale.has_reclaimable_process_evidence()),
            "the converted session carries no reclaimable process evidence"
        );

        assert!(
            identity_session_start(&mut terminal, "existing-session", 30, "resume").is_some(),
            "an explicit same-session resume was refused after the agent restarted in place"
        );
        assert!(terminal.hook_identity.is_some());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("existing-session")
        );
    }

    #[test]
    fn same_label_running_observation_not_newer_than_the_exit_does_not_lift_it() {
        // The negative half: dropping the label-change trigger must not drop the
        // boundary. A same-label observation the detector captured AT or BEFORE the
        // exit proves nothing about a process that exit showed gone, however late it is
        // delivered, so the retirement stands until a genuinely newer capture arrives.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");

        let exit_at = Instant::now() + Duration::from_secs(1);
        observe_at(&mut terminal, Some(Agent::Hermes), true, exit_at);

        for (label, running_at) in [
            ("at the exit's own instant", exit_at),
            (
                "a millisecond before it",
                exit_at - Duration::from_millis(1),
            ),
        ] {
            observe_at(&mut terminal, Some(Agent::Hermes), false, running_at);
            assert!(
                terminal.suppressed_hook_reports.contains_key("zynk:hermes"),
                "a running observation captured {label} lifted the retirement"
            );
            assert!(
                identity_session_start(&mut terminal, "existing-session", 30, "resume").is_none(),
                "a running observation captured {label} re-armed the retired session"
            );
            assert!(terminal.hook_identity.is_none());
        }

        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            false,
            exit_at + Duration::from_millis(1),
        );
        assert!(
            identity_session_start(&mut terminal, "existing-session", 31, "resume").is_some(),
            "a running observation genuinely newer than the exit was refused"
        );
        assert!(terminal.hook_identity.is_some());
    }

    #[test]
    fn sequence_fence_is_reset_only_when_a_suppression_is_cleared() {
        // The replay fence a restarted process needs exactly once. Running the
        // conversion on EVERY observation must not run the sequence reset on every
        // observation too: a restarted process re-anchors its sequence at the moment
        // its retirement is converted, and from then on the fence has to hold, or an
        // owner whose process is simply detected keeps an open replay window forever.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
        assert_eq!(terminal.hook_report_sequences.get("zynk:hermes"), Some(&20));

        // Nothing is retired, so this observation converts nothing and the fence stays.
        observe_at(&mut terminal, Some(Agent::Hermes), false, Instant::now());
        assert_eq!(
            terminal.hook_report_sequences.get("zynk:hermes"),
            Some(&20),
            "a running observation reopened the sequence window with nothing retired"
        );

        let exit_at = Instant::now();
        observe_at(&mut terminal, Some(Agent::Hermes), true, exit_at);
        // The exit retires the identity and drops its sequence anchor with it, so the
        // restarted process starts a fresh window and may re-anchor LOWER than before.
        assert!(!terminal.hook_report_sequences.contains_key("zynk:hermes"));
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            false,
            exit_at + Duration::from_millis(2),
        );
        assert!(
            identity_session_start(&mut terminal, "existing-session", 5, "resume").is_some(),
            "the restarted process could not re-anchor after its retirement was converted"
        );
        assert_eq!(terminal.hook_report_sequences.get("zynk:hermes"), Some(&5));

        // Every FURTHER running observation finds no suppression to clear, so it must
        // leave the fence where the re-anchored owner put it.
        observe_at(&mut terminal, Some(Agent::Hermes), false, Instant::now());
        assert_eq!(
            terminal.hook_report_sequences.get("zynk:hermes"),
            Some(&5),
            "a running observation with no suppression to clear reopened the sequence window"
        );
        terminal.record_identity_only_hook_report(
            "zynk:hermes".into(),
            "hermes".into(),
            crate::agent_resume::AgentSessionRef::id("existing-session"),
            Some(4),
        );
        assert_eq!(
            terminal.hook_report_sequences.get("zynk:hermes"),
            Some(&5),
            "a replayed report behind the fence was accepted after a plain running observation"
        );
    }

    #[test]
    fn equal_time_process_loss_invalidates_resume_evidence() {
        // Gate-3 B1 (arbiter preliminary msg_e524c27dd0759d82): `observe_process_loss`
        // advanced the loss boundary at equality but expired evidence only strictly
        // older than the loss, so evidence recorded at the loss's own instant survived
        // it. The two sides must agree — the boundary the loss sets is exactly the
        // boundary evidence has to beat.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
        terminal.release_agent_with_mutation("zynk:hermes", "hermes", Some(21));

        let observed = Instant::now() + Duration::from_secs(1);
        observe_at(&mut terminal, Some(Agent::Hermes), false, observed);
        observe_at(&mut terminal, Some(Agent::Hermes), true, observed);
        observe_at(
            &mut terminal,
            None,
            false,
            observed + Duration::from_secs(1),
        );

        assert!(
            identity_session_start(&mut terminal, "existing-session", 30, "resume").is_none(),
            "process-loss evidence at the same timestamp left stale resume evidence armed"
        );
        assert!(terminal.hook_identity.is_none());
        assert!(terminal.persisted_agent_session.is_none());
    }

    #[test]
    fn process_loss_while_suppressed_fences_an_older_running_observation() {
        // Gate-3 B1 (arbiter preliminary msg_e524c27dd0759d82): a retired owner spends a
        // window in `suppressed_hook_reports` before any running observation converts it
        // into a `StaleHookSession`. A process loss seen during THAT window was recorded
        // nowhere, so the session it later became carried no loss boundary and a running
        // observation captured before the loss re-armed it. The boundary has to survive
        // the conversion.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
        terminal.release_agent_with_mutation("zynk:hermes", "hermes", Some(21));
        assert!(terminal.hook_identity.is_none());

        let retired = Instant::now();
        // Observed gone while the owner is still only suppressed: nothing has converted
        // it into a stale session yet, so this is the loss that must not be forgotten.
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            true,
            retired + Duration::from_secs(5),
        );
        observe_at(&mut terminal, None, false, retired + Duration::from_secs(6));

        // Delivered later, captured before that loss.
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            false,
            retired + Duration::from_secs(3),
        );

        assert!(
            identity_session_start(&mut terminal, "existing-session", 30, "resume").is_none(),
            "a running observation older than a loss seen while suppression was pending re-armed the retired session"
        );
        assert!(terminal.hook_identity.is_none());
        assert!(terminal.persisted_agent_session.is_none());

        // The carried boundary voids the reclaim, it does not ban the session: a
        // capture newer than that loss is evidence in its own right.
        observe_at(&mut terminal, None, false, retired + Duration::from_secs(7));
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            false,
            retired + Duration::from_secs(8),
        );

        assert!(
            identity_session_start(&mut terminal, "existing-session", 31, "resume").is_some(),
            "a resume backed by a capture newer than the carried loss boundary was refused"
        );
        assert!(terminal.hook_identity.is_some());
    }

    #[test]
    fn a_different_agent_seen_while_suppressed_fences_an_older_running_observation() {
        // The second limb of a process loss — a DIFFERENT agent detected in this
        // owner's place — has to be remembered across the same suppression window.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
        terminal.release_agent_with_mutation("zynk:hermes", "hermes", Some(21));

        let retired = Instant::now();
        observe_at(
            &mut terminal,
            Some(Agent::Codex),
            false,
            retired + Duration::from_secs(5),
        );
        observe_at(
            &mut terminal,
            Some(Agent::Hermes),
            false,
            retired + Duration::from_secs(3),
        );

        assert!(
            identity_session_start(&mut terminal, "existing-session", 30, "resume").is_none(),
            "a running observation older than a replacement agent seen while suppression was pending re-armed the retired session"
        );
        assert!(terminal.hook_identity.is_none());
    }

    #[test]
    fn reordered_running_observation_cannot_undo_a_later_exit() {
        reordered_running_observation_after_exit(false);
    }

    #[test]
    fn reordered_running_observation_cannot_undo_a_second_exit() {
        reordered_running_observation_after_exit(true);
    }

    #[test]
    fn identity_same_session_resume_cannot_use_process_evidence_invalidated_by_a_later_exit() {
        // Codex Gate-2 M3 extension finding (msg_f23d4dbab48a4c35): pending reclaim
        // evidence is a claim about a process that is STILL RUNNING. Between observing
        // the replacement process and accepting its session report there is deliberately
        // no installed identity, so the identity-retirement path cannot expire it — yet a
        // newer exit observation proves that process is gone, and a resume arriving after
        // it is the late callback the retirement exists to refuse.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
        let observed = identity_reported_at(&terminal);
        retire_then_observe_a_fresh_process(&mut terminal, observed, false);
        assert!(terminal.hook_identity.is_none());

        // The new process exited before its queued resume report was handled.
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            observed + Duration::from_secs(4),
        );
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            observed + Duration::from_secs(5),
        );
        assert!(terminal.detected_agent.is_none());

        let resumed = identity_session_start(&mut terminal, "existing-session", 30, "resume");

        assert!(
            resumed.is_none(),
            "a late resume reused process evidence invalidated by a newer exit observation"
        );
        assert!(terminal.hook_identity.is_none());
        assert!(terminal.persisted_agent_session.is_none());
    }

    #[test]
    fn identity_same_session_resume_after_an_invalidating_exit_needs_new_process_evidence() {
        // Expiry voids the reclaim, it does not ban the session: the owner's process
        // observed AGAIN after the invalidating exit is fresh evidence in its own right,
        // and the explicit same-session resume it backs is admitted exactly as the first
        // one was.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
        let observed = identity_reported_at(&terminal);
        retire_then_observe_a_fresh_process(&mut terminal, observed, false);

        // The replacement process exits, expiring the evidence it had left behind.
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            observed + Duration::from_secs(4),
        );
        terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            observed + Duration::from_secs(5),
        );

        // A resume in this window is refused: nothing has been seen running since.
        assert!(
            identity_session_start(&mut terminal, "existing-session", 30, "resume").is_none(),
            "a resume was admitted with no process observed since the invalidating exit"
        );

        // The agent starts again, and THIS observation is the fresh evidence.
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            observed + Duration::from_secs(6),
        );

        assert!(
            identity_session_start(&mut terminal, "existing-session", 31, "resume").is_some(),
            "a resume backed by process evidence newer than the exit was still refused"
        );
        assert!(terminal.hook_identity.is_some());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("existing-session")
        );
    }

    #[test]
    fn identity_same_session_resume_keeps_process_evidence_after_an_older_exit_observation() {
        // The ordering half of the same rule, and the reason expiry is not simply "any
        // exit clears it": an observation captured BEFORE the fresh process it would
        // erase decides nothing, exactly as `hook_identity_not_newer_than` already holds
        // for the identity itself.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
        let observed = identity_reported_at(&terminal);
        retire_then_observe_a_fresh_process(&mut terminal, observed, false);

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Hermes),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            observed + Duration::from_millis(2500),
        );

        assert!(
            identity_session_start(&mut terminal, "existing-session", 30, "resume").is_some(),
            "an exit observed before the fresh process cannot invalidate its resume evidence"
        );
        assert!(terminal.hook_identity.is_some());
    }

    fn same_session_resume_without_fresh_process_evidence(process_exit: bool) {
        // The other half of the rule: the explicit reason alone proves nothing. Without
        // a process observation AFTER the retirement, a `resume` naming the retired
        // session is exactly the late callback the retirement exists to refuse.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
        let observed = identity_reported_at(&terminal);
        if process_exit {
            terminal.set_detected_state_with_screen_signals_at(
                Some(Agent::Hermes),
                AgentState::Idle,
                false,
                false,
                false,
                true,
                observed + Duration::from_secs(1),
            );
        } else {
            terminal.release_agent_with_mutation("zynk:hermes", "hermes", Some(21));
        }
        assert!(terminal.hook_identity.is_none());

        assert!(
            identity_session_start(&mut terminal, "existing-session", 30, "resume").is_none(),
            "a bare resume with no fresh process evidence restored a retired session"
        );
        assert!(terminal.hook_identity.is_none());
        assert!(terminal.persisted_agent_session.is_none());
    }

    #[test]
    fn identity_same_session_resume_after_release_without_fresh_process_evidence_stays_retired() {
        same_session_resume_without_fresh_process_evidence(false);
    }

    #[test]
    fn identity_same_session_resume_after_process_exit_without_fresh_process_evidence_stays_retired(
    ) {
        same_session_resume_without_fresh_process_evidence(true);
    }

    #[test]
    fn identity_late_callback_after_fresh_process_evidence_stays_retired() {
        // Fresh process evidence admits ONLY the explicit session-start shape. An
        // ordinary late callback naming the retired session — the shipped state report,
        // a session report with no reason, or a reason this owner never starts on —
        // stays retired however fresh the process is.
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
        let observed = identity_reported_at(&terminal);

        retire_then_observe_a_fresh_process(&mut terminal, observed, false);

        // The shipped `pane.report_agent` shape: lifecycle state plus the session.
        assert!(
            terminal
                .record_identity_only_hook_report(
                    "zynk:hermes".into(),
                    "hermes".into(),
                    crate::agent_resume::AgentSessionRef::id("existing-session"),
                    Some(30),
                )
                .is_none(),
            "a late state callback reclaimed a retired session"
        );
        assert!(terminal.hook_identity.is_none());

        // `pane.report_agent_session` carrying no session-start reason at all.
        assert!(
            terminal
                .set_agent_session_ref(
                    "zynk:hermes".into(),
                    "hermes".into(),
                    crate::agent_resume::AgentSessionRef::id("existing-session"),
                    Some(31),
                )
                .is_none(),
            "a reason-less session report reclaimed a retired session"
        );
        assert!(terminal.hook_identity.is_none());

        // A reason that is real for another owner but not a session start for this one.
        assert!(
            identity_session_start(&mut terminal, "existing-session", 32, "compact").is_none(),
            "a non-session-start reason reclaimed a retired session"
        );
        assert!(terminal.hook_identity.is_none());
        assert!(terminal.persisted_agent_session.is_none());
    }

    #[test]
    fn an_identity_only_clear_does_not_suppress_a_different_live_owner() {
        // The two representations can coexist while the full owner holds no session.
        // The identity owner's clear then scopes to its own identity and leaves the
        // live authority — and that owner's later reports — alone.
        let mut terminal = test_terminal();
        terminal
            .set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some(1),
            )
            .expect("the full-lifecycle owner takes authority");
        terminal.set_hook_authority_with_session_ref(
            "zynk:hermes".into(),
            "hermes".into(),
            AgentState::Idle,
            None,
            None,
            None,
            Some(1),
        );
        assert!(terminal.hook_identity.is_some());

        terminal.clear_hook_authority_with_mutation(Some("zynk:hermes"), Some(2));

        assert!(terminal.hook_identity.is_none());
        assert!(
            terminal.hook_authority.is_some(),
            "another owner's clear dropped the live authority"
        );

        let next = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            None,
            None,
            Some(2),
        );

        assert!(
            next.is_some(),
            "an obsolete identity's clear suppressed the live owner's reports"
        );
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn an_obsolete_identity_does_not_veto_a_coexisting_owners_release() {
        let mut terminal = test_terminal();
        terminal
            .set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some(1),
            )
            .expect("the full-lifecycle owner takes authority");
        terminal.set_hook_authority_with_session_ref(
            "zynk:hermes".into(),
            "hermes".into(),
            AgentState::Idle,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("hermes-1"),
            Some(1),
        );

        terminal.release_agent_with_mutation("zynk:pi", "pi", Some(2));

        assert!(
            terminal.hook_authority.is_none(),
            "an obsolete identity vetoed the live owner's release"
        );
        // The other owner's identity and session are left alone.
        assert!(terminal.hook_identity.is_some());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("hermes-1")
        );
    }

    #[test]
    fn a_full_lifecycle_hook_report_records_authority_not_identity_only_identity() {
        let mut terminal = test_terminal();
        terminal
            .set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::id("pi-1"),
                Some(1),
            )
            .expect("a full-lifecycle report takes authority");

        assert!(terminal.hook_identity.is_none());
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .map(|authority| authority.agent_label.as_str()),
            Some("pi")
        );
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.full_lifecycle_hook_authority_active());
    }

    #[test]
    fn different_owner_session_ref_does_not_replace_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "zynk:droid".into(),
                "droid".into(),
                crate::agent_resume::AgentSessionRef::id("droid-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "zynk:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("claude-session"),
            Some(21),
            Some("resume".into()),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal.persisted_agent_session.as_ref().map(|session| (
                session.source.as_str(),
                session.agent.as_str(),
                session.session_ref.value.as_str()
            )),
            Some(("zynk:droid", "droid", "droid-session"))
        );
    }

    #[test]
    fn different_owner_full_lifecycle_hook_does_not_replace_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "zynk:droid".into(),
                "droid".into(),
                crate::agent_resume::AgentSessionRef::id("droid-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path("/tmp/pi-session.jsonl"),
            Some(21),
        );

        assert!(mutation.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal.persisted_agent_session.as_ref().map(|session| (
                session.source.as_str(),
                session.agent.as_str(),
                session.session_ref.value.as_str()
            )),
            Some(("zynk:droid", "droid", "droid-session"))
        );
    }

    #[test]
    fn detected_agent_clear_does_not_clear_current_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal
            .set_agent_session_ref(
                "zynk:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let clear = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);
        assert!(!clear.session_ref_changed);

        let mutation = terminal.set_agent_session_ref(
            "zynk:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("new-session"),
            Some(21),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("claude-session")
        );
    }

    #[test]
    fn release_agent_preserves_foreign_persisted_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "zynk:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("claude-session").unwrap(),
        });
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);

        let mutation = terminal
            .release_agent_with_mutation("zynk:pi", "pi", Some(21))
            .expect("visible agent release should be accepted");

        assert!(!mutation.session_ref_changed);
        assert_eq!(
            terminal.persisted_agent_session.as_ref().map(|session| (
                session.source.as_str(),
                session.agent.as_str(),
                session.session_ref.value.as_str()
            )),
            Some(("zynk:claude", "claude", "claude-session"))
        );
    }

    #[test]
    fn process_exit_clears_matching_persisted_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "zynk:pi".into(),
            agent: "pi".into(),
            session_ref: crate::agent_resume::AgentSessionRef::path(test_session_path("pi.jsonl"))
                .unwrap(),
        });
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);

        let mutation = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            std::time::Instant::now(),
        );

        assert!(mutation.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_none());
    }

    #[test]
    fn process_exit_preserves_foreign_persisted_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "zynk:claude".into(),
            agent: "claude".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("claude-session").unwrap(),
        });
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Working);

        let mutation = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            std::time::Instant::now(),
        );

        assert!(!mutation.session_ref_changed);
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("claude-session")
        );
    }

    #[test]
    fn detected_conflict_clears_live_hook_but_preserves_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority_with_session_ref(
            "zynk:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("claude-session"),
            Some(20),
        );

        let mutation =
            terminal.set_detected_state_with_mutation(Some(Agent::Grok), AgentState::Idle);

        assert!(!mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
        assert_eq!(
            terminal.persisted_agent_session.as_ref().map(|session| (
                session.source.as_str(),
                session.agent.as_str(),
                session.session_ref.value.as_str()
            )),
            Some(("zynk:claude", "claude", "claude-session"))
        );
    }

    #[test]
    fn detected_agent_disappearance_preserves_matching_persisted_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "zynk:opencode".into(),
            agent: "opencode".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("opencode-session").unwrap(),
        });

        let first =
            terminal.set_detected_state_with_mutation(Some(Agent::OpenCode), AgentState::Idle);
        assert!(!first.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_some());

        let second = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);
        assert!(!second.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_some());
    }

    #[test]
    fn visible_blocker_overrides_non_blocked_hook_for_same_agent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "zynk:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Blocked);
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(change.unwrap().previous_state, AgentState::Working);
    }

    #[test]
    fn visible_blocker_does_not_override_full_lifecycle_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Pi),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.is_none());
    }

    #[test]
    fn weak_blocked_fallback_does_not_override_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "zynk:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            false,
            false,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Blocked);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.is_none());
    }

    #[test]
    fn hook_blocked_wins_over_visible_blocker() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "zynk:codex".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            None,
        );

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.state, AgentState::Blocked);
        assert!(terminal.hook_authority.is_some());
    }

    #[test]
    fn visible_blocker_does_not_override_different_agent_hook() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(None, AgentState::Unknown);
        terminal.set_hook_authority(
            "custom:agent".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.effective_agent_label(), Some("custom-agent"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn visible_blocker_suppresses_stale_hook_custom_status() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority_with_custom_status(
            "zynk:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            Some("planning".into()),
            None,
        );

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Blocked,
            true,
            false,
            false,
        );

        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(terminal.effective_custom_status(), None);
    }

    #[test]
    fn fallback_idle_does_not_override_hook_working() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            Some("thinking".into()),
            None,
            None,
            now,
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now + Duration::from_secs(10),
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal.effective_custom_status().as_deref(),
            Some("thinking")
        );
    }

    #[test]
    fn fallback_idle_does_not_override_full_lifecycle_hook_working() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:opencode".into(),
            "opencode".into(),
            AgentState::Working,
            None,
            Some("thinking".into()),
            None,
            None,
            now,
        );
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::OpenCode),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now + Duration::from_secs(10),
        );

        assert_eq!(terminal.fallback_state, AgentState::Working);
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal.effective_custom_status().as_deref(),
            Some("thinking")
        );
    }

    #[test]
    fn visible_working_does_not_override_hook_idle_for_same_agent() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:claude".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            None,
            None,
            now,
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.fallback_state, AgentState::Working);
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn visible_working_does_not_override_full_lifecycle_hook_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:kimi".into(),
            "kimi".into(),
            AgentState::Idle,
            None,
            None,
            None,
            None,
            now,
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Kimi),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn detected_working_fallback_is_ignored_under_full_lifecycle_hook_authority() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kilo), AgentState::Idle);
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:kilo".into(),
            "kilo".into(),
            AgentState::Idle,
            None,
            None,
            None,
            None,
            now,
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Kilo),
            AgentState::Working,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(1),
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Idle);
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn visible_working_does_not_hold_against_newer_claude_hook_idle() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now,
        );

        let change = terminal.set_hook_authority_with_custom_status_at(
            "zynk:claude".into(),
            "claude".into(),
            AgentState::Idle,
            None,
            None,
            None,
            None,
            now + Duration::from_millis(100),
        );

        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(
            change
                .unwrap()
                .effective_state_change
                .unwrap()
                .previous_state,
            AgentState::Working
        );
    }

    #[test]
    fn refreshed_visible_working_does_not_override_newer_hook_blocked() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now,
        );
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:codex".into(),
            "codex".into(),
            AgentState::Blocked,
            None,
            Some("permission".into()),
            None,
            None,
            now + Duration::from_millis(1201),
        );

        assert_eq!(terminal.state, AgentState::Blocked);

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            true,
            false,
            now + Duration::from_millis(2000),
        );

        assert_eq!(terminal.fallback_state, AgentState::Working);
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(
            terminal.effective_custom_status().as_deref(),
            Some("permission")
        );
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn fallback_idle_does_not_override_other_agent_hook_working() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "zynk:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        let change = terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            true,
            false,
        );

        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.is_none());
    }

    #[test]
    fn known_hook_authority_does_not_override_different_detected_agent() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Grok), AgentState::Working);
        let change = terminal.set_hook_authority(
            "zynk:claude".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
        );

        assert!(change.is_none());
        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Grok));
        assert_eq!(terminal.effective_agent_label(), Some("grok"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn detected_agent_clears_conflicting_known_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "zynk:claude".into(),
            "claude".into(),
            AgentState::Blocked,
            None,
            None,
        );

        terminal.set_detected_state(Some(Agent::Grok), AgentState::Working);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Grok));
        assert_eq!(terminal.effective_agent_label(), Some("grok"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn border_label_prefers_manual_label_over_agent_label() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);

        assert_eq!(terminal.border_label(false), None);
        assert_eq!(terminal.border_label(true).as_deref(), Some("claude"));

        terminal.set_manual_label(" reviewer ".into());
        assert_eq!(terminal.border_label(false).as_deref(), Some("reviewer"));
        assert_eq!(terminal.border_label(true).as_deref(), Some("reviewer"));

        terminal.set_manual_label("   ".into());
        assert_eq!(terminal.border_label(true).as_deref(), Some("claude"));

        terminal.set_manual_label("reviewer".into());
        terminal.clear_manual_label();
        assert_eq!(terminal.border_label(true).as_deref(), Some("claude"));
    }

    #[test]
    fn hook_authority_survives_unrelated_detected_agent_clear() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "zynk:custom".into(),
            "custom-agent".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state(None, AgentState::Unknown);

        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.effective_agent_label(), Some("custom-agent"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn full_lifecycle_hook_authority_ignores_detected_agent_clear_without_process_exit() {
        let now = Instant::now();
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            None,
            None,
            now,
        );

        let change = terminal.set_detected_state_with_screen_signals_at(
            None,
            AgentState::Unknown,
            false,
            false,
            false,
            false,
            now + Duration::from_millis(1),
        );

        assert!(terminal.hook_authority.is_some());
        assert_eq!(terminal.detected_agent, Some(Agent::Pi));
        assert_eq!(terminal.fallback_state, AgentState::Idle);
        assert_eq!(terminal.state, AgentState::Working);
        assert!(change.effective_state_change.is_none());
    }

    #[test]
    fn detected_agent_clear_clears_matching_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Cursor), AgentState::Idle);
        terminal.set_hook_authority(
            "zynk:cursor".into(),
            "cursor".into(),
            AgentState::Idle,
            None,
            None,
        );

        terminal.set_detected_state(None, AgentState::Unknown);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.fallback_state, AgentState::Unknown);
        assert_eq!(terminal.effective_agent_label(), None);
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn detected_agent_clear_clears_matching_working_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "zynk:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state(None, AgentState::Unknown);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.effective_agent_label(), None);
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn process_exit_clears_matching_hook_authority_before_reporting_idle() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Working);
        terminal.set_hook_authority(
            "zynk:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.set_detected_state_with_visible_blocker(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            false,
            true,
        );

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::Codex));
        assert_eq!(terminal.effective_agent_label(), Some("codex"));
        assert_eq!(terminal.state, AgentState::Idle);
    }

    #[test]
    fn stale_visible_screen_signal_does_not_override_newer_hook_authority() {
        let mut terminal = test_terminal();
        let observed = Instant::now();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Working,
            false,
            false,
            true,
            false,
            observed,
        );
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:claude".into(),
            "claude".into(),
            AgentState::Working,
            None,
            None,
            None,
            Some(1),
            observed + Duration::from_secs(1),
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Claude),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            observed,
        );

        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn stale_process_exit_does_not_clear_newer_same_agent_hook_authority() {
        let mut terminal = test_terminal();
        let observed = Instant::now();
        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Working,
            false,
            false,
            false,
            false,
            observed,
        );
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            None,
            None,
            Some(1),
            observed,
        );
        terminal.set_hook_authority_with_custom_status_at(
            "zynk:codex".into(),
            "codex".into(),
            AgentState::Working,
            None,
            Some("new turn".into()),
            None,
            Some(2),
            observed + Duration::from_secs(1),
        );

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Codex),
            AgentState::Idle,
            false,
            false,
            false,
            true,
            observed,
        );

        let authority = terminal.hook_authority.as_ref().expect("hook authority");
        assert_eq!(authority.custom_status.as_deref(), Some("new turn"));
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(terminal.effective_agent_label(), Some("codex"));
    }

    #[test]
    fn detected_agent_change_clears_previous_matching_hook_authority() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_hook_authority(
            "zynk:codex".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            None,
        );

        terminal.set_detected_state(Some(Agent::OpenCode), AgentState::Working);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, Some(Agent::OpenCode));
        assert_eq!(terminal.effective_agent_label(), Some("opencode"));
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn release_agent_clears_identity_immediately() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
        );

        terminal.release_agent("zynk:pi", "pi", None);

        assert!(terminal.hook_authority.is_none());
        assert_eq!(terminal.detected_agent, None);
        assert_eq!(terminal.fallback_state, AgentState::Unknown);
        assert_eq!(terminal.state, AgentState::Unknown);
    }

    #[test]
    fn stale_hook_report_sequence_is_ignored_for_same_source() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            Some(19),
        );

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().state,
            AgentState::Working
        );
    }

    #[test]
    fn accepted_hook_report_stores_session_ref() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        let mutation = terminal
            .set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::path(session_path.clone()),
                Some(20),
            )
            .expect("accepted report");

        assert!(mutation.session_ref_changed);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref())
                .map(|session_ref| (&session_ref.kind, session_ref.value.as_str())),
            Some((
                &crate::agent_resume::AgentSessionRefKind::Path,
                session_path.as_str()
            ))
        );
    }

    #[test]
    fn stale_hook_report_cannot_overwrite_session_ref() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        let new_session_path = test_session_path("new.jsonl");
        terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(20),
        );

        let mutation = terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(new_session_path),
            Some(19),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref())
                .map(|session_ref| session_ref.value.as_str()),
            Some(session_path.as_str())
        );
    }

    #[test]
    fn accepted_hook_report_without_session_ref_clears_previous_ref() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(20),
        );

        let mutation = terminal
            .set_hook_authority_with_session_ref(
                "zynk:pi".into(),
                "pi".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some(21),
            )
            .expect("accepted report");

        assert!(mutation.session_ref_changed);
        assert!(mutation.effective_state_change.is_none());
        assert!(terminal
            .hook_authority
            .as_ref()
            .unwrap()
            .session_ref
            .is_none());
    }

    #[test]
    fn different_same_agent_session_ref_is_ignored_until_current_session_clears() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "zynk:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_agent_session_ref(
            "zynk:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("nested-session"),
            Some(21),
        );

        assert!(mutation.is_none());
        assert_eq!(terminal.hook_report_sequences.get("zynk:claude"), Some(&21));
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("claude-session")
        );
    }

    #[test]
    fn claude_startup_session_ref_does_not_replace_existing_session_ref() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "zynk:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal.set_agent_session_ref_for_session_start(
            "zynk:claude".into(),
            "claude".into(),
            crate::agent_resume::AgentSessionRef::id("nested-session"),
            Some(21),
            Some("startup".into()),
        );

        assert!(mutation.is_none());
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("claude-session")
        );
    }

    #[test]
    fn claude_lifecycle_session_ref_replaces_existing_session_ref() {
        for session_start_source in ["clear", "resume", "compact"] {
            let mut terminal = test_terminal();
            terminal
                .set_agent_session_ref(
                    "zynk:claude".into(),
                    "claude".into(),
                    crate::agent_resume::AgentSessionRef::id("claude-session"),
                    Some(20),
                )
                .expect("initial session should be accepted");

            let next_session = format!("{session_start_source}-session");
            let mutation = terminal
                .set_agent_session_ref_for_session_start(
                    "zynk:claude".into(),
                    "claude".into(),
                    crate::agent_resume::AgentSessionRef::id(&next_session),
                    Some(21),
                    Some(session_start_source.into()),
                )
                .unwrap_or_else(|| panic!("{session_start_source} should replace the session"));

            assert!(
                mutation.session_ref_changed,
                "{session_start_source} should mark the session changed"
            );
            assert_eq!(
                terminal
                    .persisted_agent_session
                    .as_ref()
                    .map(|session| session.session_ref.value.as_str()),
                Some(next_session.as_str()),
                "{session_start_source} should store the replacement session"
            );
        }
    }

    #[test]
    fn repeated_same_agent_session_ref_is_accepted_without_session_change() {
        let mut terminal = test_terminal();
        terminal
            .set_agent_session_ref(
                "zynk:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal
            .set_agent_session_ref(
                "zynk:claude".into(),
                "claude".into(),
                crate::agent_resume::AgentSessionRef::id("claude-session"),
                Some(21),
            )
            .expect("same session should be accepted");

        assert!(!mutation.session_ref_changed);
    }

    #[test]
    fn hook_authority_preserves_current_session_ref_when_incoming_ref_differs() {
        let mut terminal = test_terminal();
        terminal
            .set_hook_authority_with_session_ref(
                "zynk:opencode".into(),
                "opencode".into(),
                AgentState::Working,
                None,
                None,
                crate::agent_resume::AgentSessionRef::id("opencode-session"),
                Some(20),
            )
            .expect("initial session should be accepted");

        let mutation = terminal
            .set_hook_authority_with_session_ref(
                "zynk:opencode".into(),
                "opencode".into(),
                AgentState::Blocked,
                Some("needs approval".into()),
                None,
                crate::agent_resume::AgentSessionRef::id("nested-session"),
                Some(21),
            )
            .expect("state update should still be accepted");

        assert!(!mutation.session_ref_changed);
        assert_eq!(terminal.state, AgentState::Blocked);
        assert_eq!(
            terminal
                .hook_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref())
                .map(|session_ref| session_ref.value.as_str()),
            Some("opencode-session")
        );
    }

    #[test]
    fn clearing_hook_authority_clears_session_ref() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(20),
        );

        let mutation = terminal
            .clear_hook_authority_with_mutation(Some("zynk:pi"), Some(21))
            .expect("accepted clear");

        assert!(mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
    }

    #[test]
    fn release_agent_clears_session_ref() {
        let mut terminal = test_terminal();
        let session_path = test_session_path("pi.jsonl");
        terminal.set_hook_authority_with_session_ref(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path),
            Some(20),
        );

        let mutation = terminal
            .release_agent_with_mutation("zynk:pi", "pi", Some(21))
            .expect("accepted release");

        assert!(mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_none());
    }

    #[test]
    fn release_agent_clears_matching_restored_session_ref_before_detection() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "zynk:hermes".into(),
            agent: "hermes".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("hermes-session").unwrap(),
        });

        let mutation = terminal
            .release_agent_with_mutation("zynk:hermes", "hermes", Some(21))
            .expect("accepted release");

        assert!(mutation.session_ref_changed);
        assert!(mutation.effective_state_change.is_none());
        assert!(terminal.persisted_agent_session.is_none());
    }

    #[test]
    fn respawn_cleanup_resets_restored_agent_status() {
        let mut terminal = test_terminal();
        terminal.respawn_shell_on_exit = true;
        terminal.set_agent_name("codex".into());
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "zynk:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("codex-session").unwrap(),
        });
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);

        terminal.clear_agent_runtime_identity_after_respawn();

        assert_eq!(terminal.state, AgentState::Unknown);
        assert!(terminal.detected_agent.is_none());
        assert!(terminal.agent_name.is_none());
        assert!(terminal.persisted_agent_session.is_none());
        assert!(!terminal.respawn_shell_on_exit);
    }

    #[test]
    fn detected_agent_disappearance_does_not_clear_full_lifecycle_hook_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_detected_state(Some(Agent::Kimi), AgentState::Idle);
        terminal.set_hook_authority_with_session_ref(
            "zynk:kimi".into(),
            "kimi".into(),
            AgentState::Working,
            None,
            None,
            crate::agent_resume::AgentSessionRef::id("kimi-session"),
            Some(20),
        );

        let mutation = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);

        assert!(!mutation.session_ref_changed);
        assert!(terminal.hook_authority.is_some());
        assert!(terminal.persisted_agent_session.is_none());
        assert_eq!(terminal.effective_agent_label(), Some("kimi"));
    }

    #[test]
    fn initial_unknown_detection_preserves_restored_session_ref() {
        let mut terminal = test_terminal();
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "zynk:hermes".into(),
            agent: "hermes".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("hermes-session").unwrap(),
        });

        let mutation = terminal.set_detected_state_with_mutation(None, AgentState::Unknown);
        assert!(!mutation.session_ref_changed);
        assert!(terminal.persisted_agent_session.is_some());
    }

    #[test]
    fn unsequenced_hook_report_is_ignored_after_source_uses_sequence() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            None,
        );

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
    }

    #[test]
    fn stale_release_sequence_is_ignored_for_same_source() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.release_agent("zynk:pi", "pi", Some(19));

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_some());
    }

    #[test]
    fn stale_clear_all_sequence_is_checked_against_current_authority_source() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        let change = terminal.clear_hook_authority(None, Some(19));

        assert!(change.is_none());
        assert_eq!(terminal.state, AgentState::Working);
        assert!(terminal.hook_authority.is_some());
    }

    #[test]
    fn same_sequence_from_different_sources_is_independent() {
        let mut terminal = test_terminal();
        terminal.set_hook_authority(
            "zynk:pi".into(),
            "pi".into(),
            AgentState::Working,
            None,
            Some(20),
        );

        terminal.set_hook_authority(
            "custom:pi".into(),
            "pi".into(),
            AgentState::Idle,
            None,
            Some(19),
        );

        assert_eq!(terminal.state, AgentState::Idle);
        assert_eq!(
            terminal.hook_authority.as_ref().unwrap().source,
            "custom:pi"
        );
    }
}
