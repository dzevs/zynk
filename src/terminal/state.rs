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
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuppressedHookReport {
    agent_label: String,
    session_ref: Option<crate::agent_resume::AgentSessionRef>,
    observed_at: Instant,
    reason: HookSuppressionReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HookSuppressionReason {
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
    /// (`expire_stale_session_evidence_on_process_loss`), while an observation captured
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
}

impl StaleHookSession {
    /// Record a fresh-process observation, keeping the NEWEST and never crossing the
    /// loss boundary: a replayed or reordered older observation must not roll the
    /// evidence back over a newer one, nor assert a process the latest loss observation
    /// has already shown gone.
    fn record_fresh_process_evidence(&mut self, observed_at: Instant) {
        if self
            .last_loss_observed_at
            .is_some_and(|lost_at| observed_at <= lost_at)
        {
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
            .is_some_and(|recorded_at| recorded_at < observed_at)
        {
            self.fresh_process_evidence = None;
        }
    }
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
            self.clear_hook_suppression_for_detected_agent(previous_detected_agent, agent, now);
        }
        self.fallback_state = fallback_state;
        self.fallback_visible_blocker = visible_blocker && fallback_state == AgentState::Blocked;
        self.fallback_observed_at = Some(now);
        if process_exited
            && self.hook_authority_not_newer_than(now)
            && self.hook_authority.as_ref().is_some_and(|authority| {
                crate::detect::parse_agent_label(&authority.agent_label) == agent
            })
        {
            let cleared_source = self
                .hook_authority
                .as_ref()
                .map(|authority| authority.source.clone());
            self.suppress_current_hook_authority(HookSuppressionReason::ProcessExit);
            if let Some(source) = cleared_source {
                self.hook_report_sequences.remove(&source);
            }
            self.hook_authority = None;
        }
        // A session-identity-only integration lives and dies with its process: it
        // holds no lifecycle authority to arbitrate, so its identity is dropped on
        // the same evidence that drops its session, and whenever the detected agent
        // contradicts the label the hook reported. `hook_identity_not_newer_than` is
        // the same freshness comparison the authority path applies: an observation
        // captured BEFORE the report it would erase decides nothing. Retiring the
        // identity also suppresses its owner, so a late callback cannot undo this.
        if self.hook_identity_not_newer_than(now)
            && ((process_exited
                && self.hook_identity.as_ref().is_some_and(|identity| {
                    crate::detect::parse_agent_label(&identity.agent_label) == agent
                }))
                || self.hook_identity_conflicts_with_detected_agent(agent))
        {
            self.retire_hook_identity(if process_exited {
                HookSuppressionReason::ProcessExit
            } else {
                HookSuppressionReason::HookClear
            });
        }
        // Pending reclaim evidence OUTLIVES the identity it was recorded against, so
        // its lifetime cannot be bounded by the block above: that block runs only while
        // an identity is installed, and the window a delayed resume arrives in has none.
        self.expire_stale_session_evidence_on_process_loss(agent, process_exited, now);
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
            self.suppress_current_hook_authority(HookSuppressionReason::HookClear);
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
        self.hook_authority = Some(HookAuthority {
            source,
            agent_label,
            state,
            message,
            custom_status,
            reported_at: now,
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
            if let Some(suppressed_ref) = suppressed.session_ref {
                self.remember_stale_hook_session(
                    source.to_string(),
                    suppressed.agent_label,
                    suppressed_ref,
                    None,
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
    fn retire_hook_identity(&mut self, reason: HookSuppressionReason) -> Option<HookIdentity> {
        let identity = self.hook_identity.take()?;
        if reason == HookSuppressionReason::ProcessExit {
            self.hook_report_sequences.remove(&identity.source);
        }
        self.suppress_hook_report(&identity.source, &identity.agent_label, reason);
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

    fn suppress_current_hook_authority(&mut self, reason: HookSuppressionReason) {
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
            self.suppress_hook_report_with_session_ref(source, agent_label, session_ref, reason);
        }
    }

    fn suppress_hook_report(
        &mut self,
        source: &str,
        agent_label: &str,
        reason: HookSuppressionReason,
    ) {
        if Self::hook_report_retirement_applies(source, agent_label) {
            let session_ref = self.owned_session_ref(source, agent_label);
            self.suppress_hook_report_with_session_ref(
                source.to_string(),
                agent_label.to_string(),
                session_ref,
                reason,
            );
        }
    }

    fn suppress_hook_report_with_session_ref(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        reason: HookSuppressionReason,
    ) {
        // Evidence is scoped to the LATEST retirement: a process observed before this
        // one proves nothing about a session retired now, so a resume must wait for a
        // new observation.
        if let Some(stale_sessions) = self.stale_hook_sessions.get_mut(&source) {
            for stale in stale_sessions
                .iter_mut()
                .filter(|stale| stale.agent_label == agent_label)
            {
                stale.fresh_process_evidence = None;
            }
        }
        self.suppressed_hook_reports.insert(
            source,
            SuppressedHookReport {
                agent_label,
                session_ref,
                observed_at: Instant::now(),
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
    /// (`expire_stale_session_evidence_on_process_loss`). WHY this exists at all: the
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
                        && stale.fresh_process_evidence.is_some()
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

    fn clear_hook_suppression_for_detected_agent(
        &mut self,
        previous_detected_agent: Option<Agent>,
        detected_agent: Option<Agent>,
        observed_at: Instant,
    ) {
        let Some(detected_agent) = detected_agent else {
            return;
        };
        if previous_detected_agent == Some(detected_agent) {
            return;
        }
        let detected_label = crate::detect::agent_label(detected_agent);
        let mut stale_sessions = Vec::new();
        self.suppressed_hook_reports.retain(|source, suppressed| {
            let should_clear =
                crate::detect::parse_agent_label(&suppressed.agent_label) == Some(detected_agent);
            if should_clear {
                if let Some(session_ref) = suppressed.session_ref.clone() {
                    stale_sessions.push((
                        source.clone(),
                        suppressed.agent_label.clone(),
                        session_ref,
                    ));
                }
            }
            !should_clear
        });
        // Reaching here IS the fresh process observation: this owner was retired, and
        // its agent is the detected process again. The session it anchored stays stale
        // — a late callback must not resurrect it — but the observation is recorded on
        // it, so the agent's own explicit resume of that session can reclaim it.
        for (source, agent_label, session_ref) in stale_sessions {
            self.remember_stale_hook_session(source, agent_label, session_ref, Some(observed_at));
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
        self.hook_report_sequences
            .retain(|source, _| !Self::hook_report_retirement_applies(source, detected_label));
    }

    fn remember_stale_hook_session(
        &mut self,
        source: String,
        agent_label: String,
        session_ref: crate::agent_resume::AgentSessionRef,
        fresh_process_evidence: Option<Instant>,
    ) {
        let source_stale_sessions = self.stale_hook_sessions.entry(source).or_default();
        if let Some(existing) = source_stale_sessions.iter_mut().find(|existing| {
            existing.agent_label == agent_label && existing.session_ref == session_ref
        }) {
            if let Some(observed_at) = fresh_process_evidence {
                existing.record_fresh_process_evidence(observed_at);
            }
            return;
        }
        source_stale_sessions.push(StaleHookSession {
            agent_label,
            session_ref,
            fresh_process_evidence,
            last_loss_observed_at: None,
        });
    }

    /// Record that a LATER observation shows this owner's process gone, expiring any
    /// pending reclaim evidence it outdates.
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
    fn expire_stale_session_evidence_on_process_loss(
        &mut self,
        detected_agent: Option<Agent>,
        process_exited: bool,
        observed_at: Instant,
    ) {
        for stale in self.stale_hook_sessions.values_mut().flatten() {
            let Some(stale_agent) = crate::detect::parse_agent_label(&stale.agent_label) else {
                continue;
            };
            if (process_exited && detected_agent == Some(stale_agent))
                || detected_agent.is_some_and(|detected_agent| detected_agent != stale_agent)
            {
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
                | ("zynk:opencode", "opencode", Some("new"))
                | ("zynk:pi", "pi", Some("new" | "resume" | "fork"))
                | (
                    "zynk:omp",
                    "omp",
                    Some("startup" | "new" | "resume" | "fork")
                )
        )
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
        if !self.accept_hook_report(&source, seq) {
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
            self.suppress_current_hook_authority(HookSuppressionReason::HookClear);
        }
        let cleared_identity = if should_clear_identity {
            self.retire_hook_identity(HookSuppressionReason::HookClear)
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
        self.suppress_hook_report(source, agent_label, HookSuppressionReason::HookClear);
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
        let now = Instant::now();
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

        terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            true,
            now + Duration::from_millis(1),
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
        let now = Instant::now();
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

        let change = terminal.set_detected_state_with_screen_signals_at(
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            true,
            false,
            false,
            now,
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
    fn opencode_new_session_ref_replaces_existing_session_ref() {
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
                crate::agent_resume::AgentSessionRef::id("opencode-new"),
                Some(21),
                Some("new".into()),
            )
            .expect("new should replace the session");

        assert!(mutation.session_ref_changed);
        assert_eq!(
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("opencode-new")
        );
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
        let observed = Instant::now();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_report(&mut terminal, 20);

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
        let observed = Instant::now();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");

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
        let observed = Instant::now();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
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
        let observed = Instant::now();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
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
        let observed = Instant::now();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
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
        let observed = Instant::now();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
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
        let observed = Instant::now();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");
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
        let observed = Instant::now();
        terminal.set_detected_state(Some(Agent::Hermes), AgentState::Idle);
        identity_session_start(&mut terminal, "existing-session", 20, "startup")
            .expect("initial session report");

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
