//! zynk fork: message-layer (ADR 0002). M1 = send + honest submit + F4 response.
//
// The public surface here is consumed by the cli send hooks (`agent send`,
// `pane run`, `pane send-text`) wired in Task 4.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendCommand {
    AgentSend,
    PaneRun,
    PaneSendText,
    // Native top-level verbs (ADR 0007 §2). Transport/delivery semantics are IDENTICAL
    // to `AgentSend` (resolve target → atomic `pane.send_input`); only the F4 `command`
    // label differs so `zynk send` and `zynk reply` are distinguishable in the envelope.
    ZynkSend,
    ZynkReply,
}

impl SendCommand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentSend => "agent send",
            Self::PaneRun => "pane run",
            Self::PaneSendText => "pane send-text",
            Self::ZynkSend => "zynk send",
            Self::ZynkReply => "zynk reply",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryStatus {
    Submitted,
    Drafted,
}

/// Honest submit semantics (ADR 0002 §4): agent send + pane run SUBMIT; send-text drafts.
pub fn delivery_status_for(cmd: SendCommand) -> DeliveryStatus {
    match cmd {
        // The native verbs share `agent send`'s atomic-submit transport.
        SendCommand::AgentSend
        | SendCommand::PaneRun
        | SendCommand::ZynkSend
        | SendCommand::ZynkReply => DeliveryStatus::Submitted,
        SendCommand::PaneSendText => DeliveryStatus::Drafted,
    }
}

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct Party {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<serde_json::Value>,
    // Durable anchors (ADR 0003): workspace/tab at send time, the source cwd, and the
    // git branch/sha resolved from that cwd. Filled best-effort; absent fields are omitted.
    // M2/M4 persist + thread these; M1 only populates them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SendResult {
    Ok,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetResolution {
    Resolved,
    NotFound,
    Ambiguous,
    /// Resolution could NOT be determined because the transport never reached the
    /// server (a dead/missing socket, an IO error). This is NOT a `not_found` (we
    /// never asked the server) and NOT a `resolved` (we proved nothing). It maps to
    /// the F4 `transport_failed` error code and MUST send nothing.
    Unknown,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Proof {
    pub proof_source: &'static str,
} // "pane.send_input" | "pane.send_text"

#[derive(Clone, Debug, serde::Serialize)]
pub struct SendError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

/// F4 response (ADR 0002 §5). On success: delivery_status+proof+submitted_at set, error None.
/// On failure: error set, and delivery_status/proof/submitted_at ABSENT (never claim a delivery).
#[derive(Clone, Debug, serde::Serialize)]
pub struct SendOutcome {
    pub result: SendResult,
    #[serde(serialize_with = "ser_command")]
    pub command: SendCommand,
    pub message_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_seq: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket_namespace: Option<String>,
    pub from: Party,
    pub to: Party,
    pub target_resolution: TargetResolution,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub message_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery_status: Option<DeliveryStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proof: Option<Proof>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<SendError>,
    pub next: String,
}

fn ser_command<S: serde::Serializer>(c: &SendCommand, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(c.as_str())
}

#[allow(clippy::too_many_arguments)]
impl SendOutcome {
    pub fn ok(
        command: SendCommand,
        message_id: String,
        from: Party,
        to: Party,
        target_resolution: TargetResolution,
        message_type: Option<String>,
        proof: Proof,
        submitted_at: String,
    ) -> Self {
        let ds = delivery_status_for(command);
        SendOutcome {
            result: SendResult::Ok,
            command,
            message_id,
            conversation_id: None,
            conversation_seq: None,
            body_hash: None,
            runtime_session_id: None,
            socket_namespace: None,
            from,
            to,
            target_resolution,
            message_type,
            delivery_status: Some(ds),
            proof: Some(proof),
            submitted_at: Some(submitted_at),
            error: None,
            next: match ds {
                // No auto-receipt exists: a submitted agent send delivers an agent-visible
                // Zynk header but never auto-advances to `received` (proof requires the
                // validated server `zynk.message_received` event). Be truthful — promise
                // no receipt.
                DeliveryStatus::Submitted => {
                    "delivered (submitted); agent targets receive a visible Zynk header and can reply via `zynk reply`"
                        .into()
                }
                DeliveryStatus::Drafted => "draft persisted; pane submit is deferred".into(),
            },
        }
    }
    pub fn failed(
        command: SendCommand,
        message_id: String,
        from: Party,
        to: Party,
        target_resolution: TargetResolution,
        message_type: Option<String>,
        error: SendError,
    ) -> Self {
        SendOutcome {
            result: SendResult::Failed,
            command,
            message_id,
            conversation_id: None,
            conversation_seq: None,
            body_hash: None,
            runtime_session_id: None,
            socket_namespace: None,
            from,
            to,
            target_resolution,
            message_type,
            delivery_status: None,
            proof: None,
            submitted_at: None,
            error: Some(error),
            next: "inspect the error and resend".into(),
        }
    }
    pub fn with_persistence(
        mut self,
        conversation_id: String,
        conversation_seq: i64,
        body_hash: String,
        runtime_session_id: String,
        socket_namespace: String,
    ) -> Self {
        self.conversation_id = Some(conversation_id);
        self.conversation_seq = Some(conversation_seq);
        self.body_hash = Some(body_hash);
        self.runtime_session_id = Some(runtime_session_id);
        self.socket_namespace = Some(socket_namespace);
        self
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }
    // The F4 surface (ADR 0002 §5) is "stable JSON + concise human text". The CLI
    // hooks emit `to_json()`; the human rendering is part of the public surface and
    // unit-tested, but no non-test caller consumes it yet (a later M wires a
    // human-readable mode), so it is dead in the bin build only.
    #[allow(dead_code)]
    pub fn human(&self) -> String {
        let status = self
            .delivery_status
            .map(|d| {
                serde_json::to_string(&d)
                    .unwrap_or_default()
                    .trim_matches('"')
                    .to_string()
            })
            .or_else(|| self.error.as_ref().map(|e| format!("failed:{}", e.code)))
            .unwrap_or_default();
        format!(
            "{} [{}] {} -> {} ({}) id={}",
            serde_json::to_string(&self.result)
                .unwrap_or_default()
                .trim_matches('"'),
            self.command.as_str(),
            self.from.agent.as_deref().unwrap_or("?"),
            self.to.agent.as_deref().unwrap_or("?"),
            status,
            self.message_id
        )
    }
}

/// Format a UNIX timestamp (seconds since the epoch) as an RFC3339 UTC string
/// (`YYYY-MM-DDThh:mm:ssZ`), with no external date/time dependency. Uses Howard
/// Hinnant's civil-from-days algorithm. Negative timestamps (pre-1970) are
/// clamped to the epoch — M1 only ever passes "now".
pub fn rfc3339_utc(unix_secs: i64) -> String {
    let secs = unix_secs.max(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert a count of days since 1970-01-01 to a `(year, month, day)` civil date
/// (proleptic Gregorian). Howard Hinnant, "chrono-Compatible Low-Level Date Algorithms".
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Current wall-clock time as an RFC3339 UTC string. Used for `submitted_at`.
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    rfc3339_utc(secs)
}

pub fn lowercase_hex_sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn body_hash(body: &str) -> String {
    lowercase_hex_sha256(body.as_bytes())
}

/// Unique-enough id without a uuid/rand dep: prefix + time-nanos + pid + a
/// process-local counter, sha256-folded to a short hex token.
pub fn new_prefixed_id(prefix: &str) -> String {
    use sha2::{Digest, Sha256};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut h = Sha256::new();
    h.update(prefix.as_bytes());
    h.update(nanos.to_le_bytes());
    h.update(std::process::id().to_le_bytes());
    h.update(seq.to_le_bytes());
    let d = h.finalize();
    let hex: String = d[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("{prefix}_{hex}")
}

pub fn new_message_id() -> String {
    new_prefixed_id("msg")
}

/// Parse the optional leading `--type <value>` option from a send's args, returning the
/// `(type, body)` pair. `--type` is recognized ONLY while scanning LEADING options: scan from
/// the front — if the next token is `--type`, consume `--type <value>`; if it is `--`, consume it
/// and STOP option scanning; otherwise option scanning STOPS and the text begins at this token.
/// The body is the remaining tokens joined with a single space, so a literal `--type` inside the
/// message body (once text has started, or after `--`) is preserved verbatim.
pub fn parse_type_and_text(args: &[String]) -> (Option<String>, String) {
    let mut message_type: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--type" {
            if let Some(value) = args.get(i + 1) {
                message_type = Some(value.clone());
                i += 2;
            } else {
                // `--type` with no value: not a valid leading option; treat as body start.
                break;
            }
        } else if args[i] == "--" {
            i += 1;
            break;
        } else {
            break;
        }
    }
    (message_type, args[i..].join(" "))
}

/// The trace spec parsed from a leading `--trace <value>` option (feature #107).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TraceSpec {
    /// `--trace <id>`: an explicit, NOT-yet-validated trace id (validated downstream
    /// via [`validate_trace_id`], which surfaces an explicit CLI error on bad input).
    Explicit(String),
    /// `--trace inherit`: copy the trace from the derived parent at send time.
    Inherit,
}

/// Parse leading `--type <T>` and `--trace <V>` options from a send's args, returning
/// `(type, trace, body)`. Generalizes [`parse_type_and_text`]: scan from the FRONT while
/// the next token is a recognized leading option (`--type <v>` or `--trace <v>`); on `--`,
/// consume it and STOP option scanning; on any other token, option scanning STOPS and the
/// body begins there. The body is the remaining tokens joined by a single space, so a
/// literal `--type`/`--trace` inside the body (once text starts, or after `--`) is verbatim.
///
/// `--trace` and `--type` may appear in either order. For a repeated option, LAST-WINS
/// (the final leading `--trace`/`--type` is the effective one) — so `--trace <id>` and
/// `--trace inherit` are alternatives resolved last-wins. The trace value is NOT validated
/// here; an `inherit` literal maps to [`TraceSpec::Inherit`], anything else to
/// [`TraceSpec::Explicit`].
pub fn parse_type_trace_and_text(args: &[String]) -> (Option<String>, Option<TraceSpec>, String) {
    let mut message_type: Option<String> = None;
    let mut trace: Option<TraceSpec> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--type" {
            if let Some(value) = args.get(i + 1) {
                message_type = Some(value.clone());
                i += 2;
            } else {
                // `--type` with no value: not a valid leading option; body starts here.
                break;
            }
        } else if args[i] == "--trace" {
            if let Some(value) = args.get(i + 1) {
                trace = Some(if value == "inherit" {
                    TraceSpec::Inherit
                } else {
                    TraceSpec::Explicit(value.clone())
                });
                i += 2;
            } else {
                // `--trace` with no value: not a valid leading option; body starts here.
                break;
            }
        } else if args[i] == "--" {
            i += 1;
            break;
        } else {
            break;
        }
    }
    (message_type, trace, args[i..].join(" "))
}

// --- Feature #107 (IM1): per-message trace_id (ADR-pending; stored in meta_json) ---

/// Maximum length of an explicit `--trace <id>`. A trace_id is a free-form,
/// single-line printable token; 128 chars is generous for a propagated id while
/// keeping the meta_json small. (Locked decision: max 128 chars.)
pub const MAX_TRACE_ID_LEN: usize = 128;

/// Validate + normalize an explicitly provided `--trace <id>` value.
///
/// Locked decisions (feature #107):
/// - Free-form PRINTABLE single-line string; `--trace` is ONLY for explicit input,
///   so this is never called for the absent case — the caller passes `None` then.
/// - `trim` surrounding whitespace; an EMPTY-after-trim value is REJECTED (an
///   explicit `--trace ""`/`--trace "   "` is an error, never a silent no-trace).
/// - REJECT if longer than [`MAX_TRACE_ID_LEN`] chars (counted AFTER trim).
/// - REJECT any ASCII control char (incl. `\t`/`\n`/`\r`) or other Unicode control
///   / non-printable code point — explicit error, NOT a silent strip.
///
/// On success returns the cleaned (trimmed) string. On failure returns a
/// `(code, message)` pair the CLI maps onto its F4 `SendError` surface.
pub fn validate_trace_id(raw: &str) -> Result<String, (&'static str, String)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err((
            "invalid_trace_id",
            "--trace was given an empty value; provide a printable trace id or omit --trace".into(),
        ));
    }
    let char_count = trimmed.chars().count();
    if char_count > MAX_TRACE_ID_LEN {
        return Err((
            "invalid_trace_id",
            format!("--trace id is {char_count} chars; the maximum is {MAX_TRACE_ID_LEN}"),
        ));
    }
    // Reject ASCII control chars AND any other control / non-printable code point
    // (this also rejects embedded newlines/tabs — a trace id is a single-line token).
    if let Some(bad) = trimmed.chars().find(|c| c.is_control()) {
        return Err((
            "invalid_trace_id",
            format!(
                "--trace id contains a disallowed control character (U+{:04X}); use printable chars only",
                bad as u32
            ),
        ));
    }
    Ok(trimmed.to_string())
}

// --- Task 3: native source/target metadata resolution (ADR 0002 §3, ADR 0003 anchors) ---

use std::path::Path;
use std::process::Command;

use crate::api::schema::{AgentTarget, Method, PaneTarget, Request};

/// Read the named string field from a pane/agent info JSON object, or `None`.
fn info_str(info: &serde_json::Value, key: &str) -> Option<String> {
    info.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// PURE assembly: map a zynk `pane.get`/`agent.get` info object (a `PaneInfo`/`AgentInfo`,
/// `schema.rs` PaneInfo:228 / AgentInfo:756) to a [`Party`].
///
/// Field mapping (verbatim, no derivation): `agent`→agent, `pane_id`→pane, `terminal_id`→
/// terminal_id, `workspace_id`→workspace, `tab_id`→tab, `cwd`→cwd, and `agent_session` carried
/// verbatim as the nested JSON object when present. `branch`/`git_sha` are NOT derived here —
/// that is the [`resolve_source`]/[`resolve_target`] layer's job (it runs git off the cwd).
///
/// A bare shell pane (no `agent`/`agent_session`) yields a [`Party`] with those two `None`; an
/// empty/non-object value yields an empty [`Party`] (never panics).
pub fn party_from_pane_info(info: &serde_json::Value) -> Party {
    Party {
        agent: info_str(info, "agent"),
        pane: info_str(info, "pane_id"),
        terminal_id: info_str(info, "terminal_id"),
        agent_session: info.get("agent_session").filter(|v| !v.is_null()).cloned(),
        workspace: info_str(info, "workspace_id"),
        tab: info_str(info, "tab_id"),
        cwd: info_str(info, "cwd"),
        branch: None,
        git_sha: None,
    }
}

/// Best-effort git branch + full sha for a working directory. Runs `git -C <cwd> rev-parse`
/// via [`Command`]; ANY failure (no git, not a repo, detached, IO error) yields `None` for that
/// field and NEVER panics or errors the send. Returns `(branch, sha)`.
pub fn git_meta(cwd: &Path) -> (Option<String>, Option<String>) {
    fn git_capture(cwd: &Path, args: &[&str]) -> Option<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
    // `--abbrev-ref HEAD` is "HEAD" when detached; treat that as "no branch".
    let branch = git_capture(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]).filter(|b| b != "HEAD");
    let sha = git_capture(cwd, &["rev-parse", "HEAD"]);
    (branch, sha)
}

/// Resolve the SOURCE [`Party`] for a send. `zynk_pane_id` is the caller's `ZYNK_PANE_ID`
/// (the live source pane); when `None` the source is an empty [`Party`] (valid + sparse, NOT an
/// error — a non-zynk caller has no source pane). Otherwise query `pane.get` for that pane and
/// fill the party, then derive `branch`/`git_sha` best-effort from the resolved `cwd`. A `pane.get`
/// error or transport failure leaves a sparse [`Party`] (never aborts the send).
///
/// `send` is the server transport (mirrors `cli::send_request`); kept as a parameter so the
/// assembly is unit-tested with canned responses and the LIVE wiring lands in Task 4/5.
pub fn resolve_source<S>(zynk_pane_id: Option<String>, send: S) -> Party
where
    S: Fn(Request) -> std::io::Result<serde_json::Value>,
{
    let Some(pane_id) = zynk_pane_id else {
        return Party::default();
    };
    let request = Request {
        id: "zynk:resolve:source".into(),
        method: Method::PaneGet(PaneTarget {
            pane_id: pane_id.clone(),
        }),
    };
    let mut party = match send(request) {
        Ok(value) if value.get("error").is_none() => party_from_pane_info(&value["result"]["pane"]),
        // pane.get failed (pane gone, transport error): keep a sparse source, never abort.
        _ => Party::default(),
    };
    if let Some(cwd) = party.cwd.as_deref() {
        let (branch, sha) = git_meta(Path::new(cwd));
        party.branch = branch;
        party.git_sha = sha;
    }
    party
}

/// Resolve the TARGET [`Party`] + its [`TargetResolution`] for an `agent send`. Query `agent.get`
/// for `target` (agent terminals only — `schema.rs` AgentGet:64 → `AgentInfo`). On success →
/// (`party_from_pane_info`, `Resolved`). On a SERVER error, thread the resolution status from the
/// error `code`: `agent_target_ambiguous` → `Ambiguous`, anything else (incl. `agent_not_found`)
/// → `NotFound`, always with an EMPTY [`Party`] — the server answered, so a `not_found` is HONEST.
/// A transport IO error (dead/missing socket) cannot prove ANY resolution: we never reached the
/// server, so it is `Unknown` (NOT `not_found`), and Task 4 maps it to the F4 `transport_failed`
/// code, sending nothing.
pub fn resolve_target<S>(target: &str, send: S) -> (Party, TargetResolution)
where
    S: Fn(Request) -> std::io::Result<serde_json::Value>,
{
    let request = Request {
        id: "zynk:resolve:target".into(),
        method: Method::AgentGet(AgentTarget {
            target: target.to_string(),
        }),
    };
    match send(request) {
        Ok(value) => {
            if let Some(error) = value.get("error") {
                let code = error.get("code").and_then(|c| c.as_str()).unwrap_or("");
                let resolution = match code {
                    "agent_target_ambiguous" => TargetResolution::Ambiguous,
                    _ => TargetResolution::NotFound,
                };
                (Party::default(), resolution)
            } else {
                (
                    party_from_pane_info(&value["result"]["agent"]),
                    TargetResolution::Resolved,
                )
            }
        }
        // Transport IO error: we never reached the server — resolution is UNKNOWN, never
        // `not_found` (which would dishonestly claim the server denied the target).
        Err(_) => (Party::default(), TargetResolution::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_send_and_pane_run_are_submitted() {
        assert_eq!(
            delivery_status_for(SendCommand::AgentSend),
            DeliveryStatus::Submitted
        );
        assert_eq!(
            delivery_status_for(SendCommand::PaneRun),
            DeliveryStatus::Submitted
        );
    }

    #[test]
    fn pane_send_text_is_drafted() {
        assert_eq!(
            delivery_status_for(SendCommand::PaneSendText),
            DeliveryStatus::Drafted
        );
    }

    #[test]
    fn rfc3339_utc_formats_known_epochs() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        // 2026-06-10T12:34:56Z == 1_781_094_896 seconds since the epoch.
        assert_eq!(rfc3339_utc(1_781_094_896), "2026-06-10T12:34:56Z");
        // Leap-day boundary: 2024-02-29T00:00:00Z.
        assert_eq!(rfc3339_utc(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn now_rfc3339_is_well_formed_zulu() {
        let s = now_rfc3339();
        assert_eq!(s.len(), 20, "len of YYYY-MM-DDThh:mm:ssZ: {s}");
        assert!(s.ends_with('Z'), "UTC suffix: {s}");
        assert!(s.starts_with("20"), "21st-century year: {s}");
        // Round-trips the digit/separator shape.
        let (date, time) = s[..s.len() - 1].split_once('T').expect("T separator");
        assert_eq!(date.split('-').count(), 3, "Y-M-D: {s}");
        assert_eq!(time.split(':').count(), 3, "h:m:s: {s}");
    }

    #[test]
    fn message_id_is_unique_and_nonempty() {
        let a = new_message_id();
        let b = new_message_id();
        assert!(!a.is_empty());
        assert_ne!(a, b);
    }

    fn party(agent: &str, pane: &str) -> Party {
        Party {
            agent: Some(agent.into()),
            pane: Some(pane.into()),
            ..Party::default()
        }
    }

    #[test]
    fn ok_outcome_json_is_stable() {
        let o = SendOutcome::ok(
            SendCommand::AgentSend,
            "m1".into(),
            party("claude", "w1-2"),
            party("codex", "w1-1"),
            TargetResolution::Resolved,
            Some("review".into()),
            Proof {
                proof_source: "pane.send_input",
            },
            "2026-06-10T00:00:00Z".into(),
        );
        let v: serde_json::Value = serde_json::from_str(&o.to_json()).unwrap();
        assert_eq!(v["result"], "ok");
        assert_eq!(v["command"], "agent send");
        assert_eq!(v["delivery_status"], "submitted");
        assert_eq!(v["proof"]["proof_source"], "pane.send_input");
        assert_eq!(v["target_resolution"], "resolved");
        assert_eq!(v["type"], "review");
        for k in ["message_id", "from", "to", "next"] {
            assert!(v.get(k).is_some(), "missing {k}: {v}");
        }
        // A submitted send is TRUTHFUL about `next`: there is no auto-receipt, so it must
        // NOT promise a received state (no "await received"). It states the agent-visible
        // header + the `zynk reply` affordance.
        let next = v["next"].as_str().expect("next is a string");
        assert!(
            !next.contains("await received") && !next.contains("received"),
            "submitted `next` must not promise a received state: {next:?}"
        );
        assert!(
            next.contains("submitted") && next.contains("header") && next.contains("zynk reply"),
            "submitted `next` should describe the header + reply affordance: {next:?}"
        );
        assert!(v.get("error").is_none(), "ok must have no error: {v}");
        assert!(o.human().contains("agent send") && o.human().contains("m1"));
    }

    #[test]
    fn failed_outcome_claims_no_delivery() {
        let o = SendOutcome::failed(
            SendCommand::AgentSend,
            "m2".into(),
            Party::default(),
            Party::default(),
            TargetResolution::NotFound,
            Some("review".into()),
            SendError {
                code: "target_not_found".into(),
                message: "no agent 'ghost'".into(),
                context: None,
            },
        );
        let v: serde_json::Value = serde_json::from_str(&o.to_json()).unwrap();
        assert_eq!(v["result"], "failed");
        assert_eq!(v["target_resolution"], "not_found");
        assert_eq!(v["error"]["code"], "target_not_found");
        // a FAILED send must NOT claim a delivery happened (ADR 0002/0024 honesty):
        assert!(
            v.get("delivery_status").is_none(),
            "failed must not claim delivery_status: {v}"
        );
        assert!(v.get("proof").is_none(), "failed must not claim proof: {v}");
        assert!(
            v.get("submitted_at").is_none(),
            "failed must not claim submitted_at: {v}"
        );
    }

    #[test]
    fn type_leading_is_parsed() {
        let (t, b) = parse_type_and_text(&[
            "--type".into(),
            "review".into(),
            "hello".into(),
            "world".into(),
        ]);
        assert_eq!(t.as_deref(), Some("review"));
        assert_eq!(b, "hello world");
    }

    #[test]
    fn literal_dashdashtype_in_body_is_preserved() {
        let (t, b) = parse_type_and_text(&["say".into(), "--type".into(), "later".into()]);
        assert_eq!(t, None);
        assert_eq!(b, "say --type later");
    }

    #[test]
    fn double_dash_ends_options() {
        let (t, b) = parse_type_and_text(&["--".into(), "--type".into(), "x".into()]);
        assert_eq!(t, None);
        assert_eq!(b, "--type x");
    }

    #[test]
    fn type_then_double_dash_then_literal() {
        let (t, b) = parse_type_and_text(&[
            "--type".into(),
            "review".into(),
            "--".into(),
            "--foo".into(),
        ]);
        assert_eq!(t.as_deref(), Some("review"));
        assert_eq!(b, "--foo");
    }

    #[test]
    fn no_type_no_dashdash() {
        let (t, b) = parse_type_and_text(&["plain".into(), "body".into()]);
        assert_eq!(t, None);
        assert_eq!(b, "plain body");
    }

    // --- Task 3: native source/target metadata resolution ---

    #[test]
    fn party_from_full_agent_pane_info_populates_all_fields() {
        // Shape mirrors a zynk `pane.get`/`agent.get` info object (PaneInfo/AgentInfo).
        let info = serde_json::json!({
            "pane_id": "w65abc-2",
            "terminal_id": "term-7",
            "workspace_id": "w65abc",
            "tab_id": "tab-1",
            "agent": "codex",
            "agent_session": { "source": "codex", "agent": "codex", "kind": "rollout-path", "value": "/x/sess.json" },
            "cwd": "/home/user/workspace/zynk"
        });
        let p = party_from_pane_info(&info);
        assert_eq!(p.agent.as_deref(), Some("codex"));
        assert_eq!(p.pane.as_deref(), Some("w65abc-2"));
        assert_eq!(p.terminal_id.as_deref(), Some("term-7"));
        assert_eq!(p.workspace.as_deref(), Some("w65abc"));
        assert_eq!(p.tab.as_deref(), Some("tab-1"));
        assert_eq!(p.cwd.as_deref(), Some("/home/user/workspace/zynk"));
        // agent_session is carried verbatim as the nested JSON object.
        assert_eq!(p.agent_session.as_ref().unwrap()["value"], "/x/sess.json");
        // branch/git_sha are NOT derived by the pure assembly (that is the resolve_* layer's job).
        assert_eq!(p.branch, None);
        assert_eq!(p.git_sha, None);
    }

    #[test]
    fn party_from_bare_shell_pane_has_no_agent_fields() {
        // A plain shell pane: no `agent`/`agent_session`, but pane/terminal/workspace are set.
        let info = serde_json::json!({
            "pane_id": "w65abc-1",
            "terminal_id": "term-3",
            "workspace_id": "w65abc",
            "tab_id": "tab-1",
            "cwd": "/tmp/scratch"
        });
        let p = party_from_pane_info(&info);
        assert_eq!(p.pane.as_deref(), Some("w65abc-1"));
        assert_eq!(p.terminal_id.as_deref(), Some("term-3"));
        assert_eq!(p.workspace.as_deref(), Some("w65abc"));
        assert_eq!(p.cwd.as_deref(), Some("/tmp/scratch"));
        assert_eq!(p.agent, None);
        assert_eq!(p.agent_session, None);
    }

    #[test]
    fn party_from_empty_info_is_empty_no_panic() {
        let p = party_from_pane_info(&serde_json::json!({}));
        assert_eq!(p.agent, None);
        assert_eq!(p.pane, None);
        assert_eq!(p.terminal_id, None);
        assert_eq!(p.workspace, None);
        assert_eq!(p.tab, None);
        assert_eq!(p.cwd, None);
        assert_eq!(p.agent_session, None);
        // A non-object value also must not panic.
        let p2 = party_from_pane_info(&serde_json::Value::Null);
        assert_eq!(p2.pane, None);
    }

    #[test]
    fn resolve_source_with_no_zynk_pane_id_is_sparse_default() {
        // ZYNK_PANE_ID absent => empty (valid, NOT an error); the send fn is never called.
        let called = std::cell::Cell::new(false);
        let send = |_req: Request| -> std::io::Result<serde_json::Value> {
            called.set(true);
            Ok(serde_json::json!({}))
        };
        let p = resolve_source(None, send);
        assert!(
            !called.get(),
            "send must not be called when ZYNK_PANE_ID is absent"
        );
        assert_eq!(p.pane, None);
        assert_eq!(p.agent, None);
    }

    #[test]
    fn resolve_source_queries_pane_get_and_fills_party() {
        let send = |req: Request| -> std::io::Result<serde_json::Value> {
            // The source resolution must query pane.get for the given ZYNK_PANE_ID.
            match req.method {
                Method::PaneGet(t) => {
                    assert_eq!(t.pane_id, "w65abc-2");
                    Ok(serde_json::json!({
                        "result": {
                            "pane": {
                                "pane_id": "w65abc-2",
                                "terminal_id": "term-7",
                                "workspace_id": "w65abc",
                                "tab_id": "tab-1",
                                "agent": "claude"
                            }
                        }
                    }))
                }
                other => panic!("unexpected method: {other:?}"),
            }
        };
        let p = resolve_source(Some("w65abc-2".into()), send);
        assert_eq!(p.pane.as_deref(), Some("w65abc-2"));
        assert_eq!(p.agent.as_deref(), Some("claude"));
        assert_eq!(p.terminal_id.as_deref(), Some("term-7"));
        assert_eq!(p.workspace.as_deref(), Some("w65abc"));
    }

    #[test]
    fn resolve_source_with_server_error_is_sparse_not_a_panic() {
        // A pane.get error (e.g. pane gone) leaves a sparse `from` — never an abort.
        let send = |_req: Request| -> std::io::Result<serde_json::Value> {
            Ok(serde_json::json!({ "error": { "code": "pane_not_found", "message": "gone" } }))
        };
        let p = resolve_source(Some("dead-pane".into()), send);
        assert_eq!(p.agent, None);
        assert_eq!(p.pane, None);
    }

    #[test]
    fn resolve_target_resolved_fills_party_from_agent_info() {
        let send = |req: Request| -> std::io::Result<serde_json::Value> {
            match req.method {
                Method::AgentGet(t) => {
                    assert_eq!(t.target, "codex");
                    Ok(serde_json::json!({
                        "result": {
                            "agent": {
                                "pane_id": "w65abc-1",
                                "terminal_id": "term-3",
                                "workspace_id": "w65abc",
                                "tab_id": "tab-1",
                                "agent": "codex",
                                "agent_session": { "source": "codex", "agent": "codex", "kind": "rollout-path", "value": "/x/s.json" }
                            }
                        }
                    }))
                }
                other => panic!("unexpected method: {other:?}"),
            }
        };
        let (p, res) = resolve_target("codex", send);
        assert_eq!(res, TargetResolution::Resolved);
        assert_eq!(p.agent.as_deref(), Some("codex"));
        assert_eq!(p.pane.as_deref(), Some("w65abc-1"));
        assert_eq!(p.terminal_id.as_deref(), Some("term-3"));
        assert!(p.agent_session.is_some());
    }

    #[test]
    fn resolve_target_not_found_returns_empty_party() {
        let send = |_req: Request| -> std::io::Result<serde_json::Value> {
            Ok(
                serde_json::json!({ "error": { "code": "agent_not_found", "message": "no agent ghost" } }),
            )
        };
        let (p, res) = resolve_target("ghost", send);
        assert_eq!(res, TargetResolution::NotFound);
        assert_eq!(p.agent, None);
        assert_eq!(p.pane, None);
    }

    #[test]
    fn resolve_target_ambiguous_returns_empty_party() {
        let send = |_req: Request| -> std::io::Result<serde_json::Value> {
            Ok(
                serde_json::json!({ "error": { "code": "agent_target_ambiguous", "message": "two candidates" } }),
            )
        };
        let (p, res) = resolve_target("codex", send);
        assert_eq!(res, TargetResolution::Ambiguous);
        assert_eq!(p.agent, None);
    }

    #[test]
    fn resolve_target_transport_error_is_unknown() {
        // A transport IO error never reached the server: resolution is UNKNOWN, NOT not_found
        // (Task 4 maps Unknown → the F4 `transport_failed` code, sending nothing).
        let send = |_req: Request| -> std::io::Result<serde_json::Value> {
            Err(std::io::Error::other("socket closed"))
        };
        let (p, res) = resolve_target("codex", send);
        assert_eq!(res, TargetResolution::Unknown);
        assert_eq!(p.agent, None);
    }

    #[test]
    fn git_meta_in_a_temp_repo_returns_branch_and_sha() {
        // Portability: build a SELF-CONTAINED throwaway git repo in a unique /tmp dir
        // (no dependence on the live checkout's branch — that fails under detached HEAD,
        // a packaged build, or no `.git`). A committed repo resolves a branch + 40-hex sha.
        use std::process::Command as Cmd;
        let dir = std::env::temp_dir().join(format!(
            "zynk-gitmeta-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp repo dir");

        // `git init -q` (force a deterministic branch name so the assertion is robust
        // regardless of the host's `init.defaultBranch`), then one empty commit.
        let init = Cmd::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["init", "-q", "-b", "work"])
            .status();
        // -b may be unsupported on very old git; fall back to a plain init.
        if !matches!(&init, Ok(s) if s.success()) {
            let _ = Cmd::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["init", "-q"])
                .status()
                .expect("git init");
        }
        let commit = Cmd::new("git")
            .arg("-C")
            .arg(&dir)
            .args([
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "x",
            ])
            .status()
            .expect("git commit");
        assert!(commit.success(), "empty commit should succeed");

        let (branch, sha) = git_meta(&dir);
        assert!(branch.is_some(), "expected a branch in the temp repo");
        let sha = sha.expect("expected a sha in the temp repo");
        assert_eq!(sha.len(), 40, "full sha is 40 hex chars: {sha}");
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_meta_on_non_repo_is_none_not_a_panic() {
        let (branch, sha) = git_meta(std::path::Path::new("/"));
        assert_eq!(branch, None);
        assert_eq!(sha, None);
    }

    // --- Feature #107 (IM1): trace_id validation + leading --trace parsing ---

    #[test]
    fn validate_trace_id_accepts_a_normal_token() {
        assert_eq!(
            validate_trace_id("review-2026-06-16-a").unwrap(),
            "review-2026-06-16-a"
        );
        // Surrounding whitespace is trimmed.
        assert_eq!(validate_trace_id("  trace-1  ").unwrap(), "trace-1");
        // Printable unicode is allowed.
        assert_eq!(validate_trace_id("café—99").unwrap(), "café—99");
    }

    #[test]
    fn validate_trace_id_rejects_empty_after_trim() {
        assert!(validate_trace_id("").is_err());
        assert!(validate_trace_id("   ").is_err());
    }

    #[test]
    fn validate_trace_id_rejects_over_128_chars() {
        let ok = "a".repeat(MAX_TRACE_ID_LEN);
        assert_eq!(validate_trace_id(&ok).unwrap().len(), MAX_TRACE_ID_LEN);
        let too_long = "a".repeat(MAX_TRACE_ID_LEN + 1);
        let err = validate_trace_id(&too_long).unwrap_err();
        assert_eq!(err.0, "invalid_trace_id");
        assert!(err.1.contains("maximum"));
    }

    #[test]
    fn validate_trace_id_rejects_control_chars_with_explicit_error() {
        for bad in ["a\tb", "a\nb", "a\rb", "a\u{0007}b", "a\u{0000}b"] {
            let err = validate_trace_id(bad).unwrap_err();
            assert_eq!(err.0, "invalid_trace_id", "{bad:?} must be rejected");
            assert!(
                err.1.contains("control"),
                "error should mention control char: {:?}",
                err.1
            );
        }
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_trace_explicit_and_inherit() {
        let (t, trace, body) =
            parse_type_trace_and_text(&strs(&["--trace", "x9", "hello", "world"]));
        assert_eq!(t, None);
        assert_eq!(trace, Some(TraceSpec::Explicit("x9".into())));
        assert_eq!(body, "hello world");

        let (_, trace, body) = parse_type_trace_and_text(&strs(&["--trace", "inherit", "hi"]));
        assert_eq!(trace, Some(TraceSpec::Inherit));
        assert_eq!(body, "hi");
    }

    #[test]
    fn parse_trace_interleaves_with_type_either_order() {
        let (t, trace, body) =
            parse_type_trace_and_text(&strs(&["--type", "review", "--trace", "t1", "body"]));
        assert_eq!(t.as_deref(), Some("review"));
        assert_eq!(trace, Some(TraceSpec::Explicit("t1".into())));
        assert_eq!(body, "body");

        let (t, trace, body) =
            parse_type_trace_and_text(&strs(&["--trace", "t1", "--type", "review", "body"]));
        assert_eq!(t.as_deref(), Some("review"));
        assert_eq!(trace, Some(TraceSpec::Explicit("t1".into())));
        assert_eq!(body, "body");
    }

    #[test]
    fn parse_trace_last_wins() {
        // `--trace <id>` then `--trace inherit`: alternatives, last-wins.
        let (_, trace, body) =
            parse_type_trace_and_text(&strs(&["--trace", "first", "--trace", "inherit", "msg"]));
        assert_eq!(trace, Some(TraceSpec::Inherit));
        assert_eq!(body, "msg");
    }

    #[test]
    fn parse_trace_double_dash_and_literal_in_body_preserved() {
        // After `--`, a literal `--trace` is body text, not an option.
        let (_, trace, body) = parse_type_trace_and_text(&strs(&["--", "--trace", "x"]));
        assert_eq!(trace, None);
        assert_eq!(body, "--trace x");
        // Once text has started, `--trace` is verbatim.
        let (_, trace, body) = parse_type_trace_and_text(&strs(&["say", "--trace", "y"]));
        assert_eq!(trace, None);
        assert_eq!(body, "say --trace y");
    }

    #[test]
    fn parse_trace_dangling_flag_is_body_start() {
        // `--trace` with no following token is not a valid leading option; body starts there.
        let (_, trace, body) = parse_type_trace_and_text(&strs(&["--trace"]));
        assert_eq!(trace, None);
        assert_eq!(body, "--trace");
    }

    #[test]
    fn parse_type_and_text_unchanged_baseline() {
        // The new parser does not regress the original `--type` extraction.
        let (t, b) = parse_type_and_text(&strs(&["--type", "review", "hello"]));
        assert_eq!(t.as_deref(), Some("review"));
        assert_eq!(b, "hello");
    }
}
