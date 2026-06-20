//! zynk fork: agent-VISIBLE message HEADER + the persisted protocol-ID fields.
//!
//! Every native zynk message to an AGENT target (claude/codex/pi alike) is prefixed
//! on the wire with a readable [`render_header`] box, PREPENDED before the pure body
//! via [`prepend_header`]. The header is for **agent AWARENESS only — it is NOT
//! receipt proof**: a delivered/visible header never advances `delivery_status` (it
//! stays `submitted`), and the server-authoritative `zynk.message_received` event
//! remains the sole proof of receipt. The header is uniform (NOT an allowlist) and is
//! NEVER stripped by any receiver.
//!
//! The structured [`protocol_id_fields`] (persisted in the `protocol_json` DB column)
//! are still emitted UNIFORMLY for every send command (incl. `pane send-text` drafts)
//! per ADR 0005. `messages.body`/`body_hash`/FTS stay pure — the header rides only the
//! transmitted wire text, never the persisted body.
//!
//! Plan: `docs/zynk/plans/2026-06-14-m3b-m4-footer-live-receipt.md` (D1/D2/D4);
//! ADR `docs/zynk/decisions/0009-visible-message-header-replaces-receipt-footer.md`.

use std::borrow::Cow;

use unicode_width::UnicodeWidthStr;

use crate::config::HeaderOptions;
use crate::zynk::message::Party;
use crate::zynk::persistence::PersistedSend;

/// Protocol-ID schema version (the `v` field) carried in the persisted `protocol_json`.
pub const PROTOCOL_VERSION: i64 = 1;

/// The protocol-ID fields persisted in the `protocol_json` DB column so the persisted IDs
/// never drift. `type` (message type) is omitted when `None`. The wire is the header
/// (NOT these JSON fields), but persistence still records the structured IDs here.
pub fn protocol_id_fields(
    message_id: &str,
    conversation_id: &str,
    conversation_seq: i64,
    runtime_session_id: &str,
    socket_namespace: &str,
    body_hash: &str,
    message_type: Option<&str>,
) -> serde_json::Value {
    let mut obj = serde_json::json!({
        "v": PROTOCOL_VERSION,
        "message_id": message_id,
        "conversation_id": conversation_id,
        "conversation_seq": conversation_seq,
        "runtime_session_id": runtime_session_id,
        "socket_namespace": socket_namespace,
        "body_hash": body_hash,
    });
    if let (Some(t), Some(map)) = (message_type, obj.as_object_mut()) {
        map.insert("type".to_string(), serde_json::Value::String(t.to_string()));
    }
    obj
}

/// Render an optional [`Party`] field as the display text for a header line, falling
/// back to `"-"` when the field is absent — the header must NEVER panic on a sparse
/// party (cwd/agent/pane unknown render as "-").
fn or_dash(value: Option<&str>) -> &str {
    value.unwrap_or("-")
}

/// The reply pane is the FROM party's pane (the recipient replies back to the sender).
/// `"-"` when the sender's pane is unknown — the reply line still renders gracefully.
fn reply_pane(from: &Party) -> &str {
    or_dash(from.pane.as_deref())
}

/// The fixed box title that opens the top border row (Feature #107 IM3, Q4 closed box).
const HEADER_TITLE: &str = "Zynk message";

/// The DISPLAY width (terminal columns, unicode-aware) of `s`. Used for every width
/// computation in the box so wide (CJK/emoji) glyphs and the right border line up by
/// COLUMNS, never byte or `char` length.
fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Truncate `s` to at most `max` DISPLAY columns, inserting an ellipsis (`…`) in the
/// MIDDLE — for cwd/path-like values, where both the root and the leaf carry meaning
/// (e.g. `/home/z…/zynk`). When `s` already fits, it is returned unchanged. `max` is in
/// columns; the ellipsis itself costs one column. Never splits a multi-byte glyph.
fn truncate_middle(s: &str, max: usize) -> String {
    if display_width(s) <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".to_string();
    }
    // Reserve one column for the ellipsis; split the rest left/right (left favored).
    let budget = max - 1;
    let right_budget = budget / 2;
    let left_budget = budget - right_budget;

    let mut left = String::new();
    let mut left_w = 0usize;
    for ch in s.chars() {
        let cw = display_width(&ch.to_string());
        if left_w + cw > left_budget {
            break;
        }
        left.push(ch);
        left_w += cw;
    }

    let mut right_rev = String::new();
    let mut right_w = 0usize;
    for ch in s.chars().rev() {
        let cw = display_width(&ch.to_string());
        if right_w + cw > right_budget {
            break;
        }
        right_rev.push(ch);
        right_w += cw;
    }
    let right: String = right_rev.chars().rev().collect();
    format!("{left}…{right}")
}

/// Truncate `s` to at most `max` DISPLAY columns, keeping the HEAD and dropping the tail
/// with a trailing ellipsis — for ids/trace/conv, where the prefix is the meaningful,
/// stable part (e.g. `msg_abc…`). When `s` already fits, it is returned unchanged.
fn truncate_tail(s: &str, max: usize) -> String {
    if display_width(s) <= max {
        return s.to_string();
    }
    if max <= 1 {
        return "…".to_string();
    }
    let budget = max - 1;
    let mut head = String::new();
    let mut head_w = 0usize;
    for ch in s.chars() {
        let cw = display_width(&ch.to_string());
        if head_w + cw > budget {
            break;
        }
        head.push(ch);
        head_w += cw;
    }
    format!("{head}…")
}

/// A single interior field line: a fixed label and its value, plus the truncation policy
/// (`Middle` for paths, `Tail` for ids). The value is truncated to fit the inner box
/// width by DISPLAY columns at draw time, never before — so the box width is computed
/// from un-truncated content first.
struct Field {
    text: String,
    ellipsis: Ellipsis,
}

#[derive(Clone, Copy)]
enum Ellipsis {
    Middle,
    Tail,
    /// No truncation policy needed (the label-only or always-fits lines); falls back to
    /// `Tail` if it somehow exceeds the inner width (defensive, never expected).
    None,
}

impl Field {
    fn middle(text: String) -> Self {
        Self {
            text,
            ellipsis: Ellipsis::Middle,
        }
    }
    fn tail(text: String) -> Self {
        Self {
            text,
            ellipsis: Ellipsis::Tail,
        }
    }
    fn plain(text: String) -> Self {
        Self {
            text,
            ellipsis: Ellipsis::None,
        }
    }

    /// The field text fit to `inner` DISPLAY columns per its truncation policy.
    fn fit(&self, inner: usize) -> String {
        if display_width(&self.text) <= inner {
            return self.text.clone();
        }
        match self.ellipsis {
            Ellipsis::Middle => truncate_middle(&self.text, inner),
            Ellipsis::Tail | Ellipsis::None => truncate_tail(&self.text, inner),
        }
    }
}

/// A `$HOME` value usable for display compaction, normalized for prefix matching: it must be
/// an ABSOLUTE POSIX path (starts with `/`) and not the bare root `/` (which would collapse
/// every absolute path to `~`). Trailing slashes are trimmed. Returns the trimmed home, or
/// `None` when the value is empty, `/`, or relative — the caller then leaves the path as-is.
/// Never canonicalizes or touches the filesystem.
fn normalize_display_home(home: &str) -> Option<&str> {
    if !home.starts_with('/') {
        return None; // relative (or Windows-style) HOME → no compaction
    }
    let trimmed = home.trim_end_matches('/');
    if trimmed.is_empty() {
        return None; // bare "/" (or "///") → never compact every absolute path
    }
    Some(trimmed)
}

/// Display-only: collapse a `$HOME`-prefixed absolute path to `~` / `~/relative`. Used ONLY to
/// render the human-facing message-header cwd lines — NEVER for storage/DB/JSON/API/query,
/// which keep the absolute `Party.cwd`. Returns the input unchanged when `home` is absent or
/// unusable, or when `path` is not under `home`. Boundary-safe: home `/home/user` does NOT
/// match `/home/user2` or `/home/userer` (a match needs the exact `home` followed by `/`).
/// Does not canonicalize or resolve symlinks; a Windows-style path never matches the absolute
/// POSIX home, so it is left unchanged.
fn compact_home<'a>(path: &'a str, home: Option<&str>) -> Cow<'a, str> {
    let Some(home) = home.and_then(normalize_display_home) else {
        return Cow::Borrowed(path);
    };
    if path == home {
        return Cow::Borrowed("~");
    }
    // Require the exact `home` THEN a `/` separator, so `/home/user` cannot match
    // `/home/user2`; a relative or Windows-style path simply fails the prefix and is kept.
    if let Some(rel) = path
        .strip_prefix(home)
        .and_then(|rest| rest.strip_prefix('/'))
    {
        return Cow::Owned(format!("~/{rel}"));
    }
    Cow::Borrowed(path)
}

/// The runtime `$HOME` for header cwd compaction, or `None` to leave paths absolute. Degrades
/// safely: unset, non-UTF8, empty, `/`, or relative `HOME` all yield `None` (compaction is
/// then a no-op). Read only at render time; never persisted.
pub(crate) fn display_home() -> Option<String> {
    let home = std::env::var_os("HOME")?.into_string().ok()?;
    normalize_display_home(&home)?; // only thread a home compact_home would honor
    Some(home)
}

/// The header cwd cell: the absolute `Party.cwd` compacted to `~`/`~/…` for display, or `-`
/// when the party has no cwd. Display-only; `Party.cwd` itself is never mutated.
fn cwd_display<'a>(cwd: Option<&'a str>, home: Option<&str>) -> Cow<'a, str> {
    match cwd {
        Some(path) => compact_home(path, home),
        None => Cow::Borrowed("-"),
    }
}

/// The agent-VISIBLE wire HEADER box (Feature #107 IM3 — closed content-width box).
/// PREPENDED before the pure body via [`prepend_header`]. Missing optional fields
/// (cwd/agent/pane unknown) render as "-"; the `type` line is OMITTED when there is no
/// message type; the `trace:` line is present ONLY when `record.trace_id` is `Some`.
/// `body_hash` is intentionally NOT shown — the header is awareness, not machine-parse.
///
/// By default the `reply:` and `note:` lines are HIDDEN (Q1). `options.verbose` re-adds
/// them (the `[header] verbose=true` config / `ZYNK_HEADER_VERBOSE=1` escape hatch).
///
/// The box is drawn at a single inner width = `min(widest field, options.max_width - 4)`
/// computed by DISPLAY columns (unicode-aware), so the title row, every interior line,
/// and both borders all reach the SAME total display width — the right border aligns by
/// COLUMNS, never byte length. Over-wide fields are truncated (never wrapped): cwd/paths
/// use a middle ellipsis, ids/trace/conv use a tail ellipsis.
///
/// `from`/`to` supply the agent/pane/cwd display; `record` supplies the ids + conv seq.
/// `home` (when `Some`) compacts `$HOME`-prefixed cwds to `~`/`~/…` for DISPLAY only — the
/// stored/serialized `Party.cwd` is never changed. Never panics on a sparse party.
pub fn render_header(
    from: &Party,
    to: &Party,
    record: &PersistedSend,
    message_type: Option<&str>,
    options: HeaderOptions,
    home: Option<&str>,
) -> String {
    let mut fields: Vec<Field> = vec![
        Field::middle(format!(
            "from: {} {}  cwd: {}",
            or_dash(from.agent.as_deref()),
            or_dash(from.pane.as_deref()),
            cwd_display(from.cwd.as_deref(), home),
        )),
        Field::middle(format!(
            "to:   {} {}  cwd: {}",
            or_dash(to.agent.as_deref()),
            or_dash(to.pane.as_deref()),
            cwd_display(to.cwd.as_deref(), home),
        )),
    ];
    if let Some(t) = message_type {
        fields.push(Field::tail(format!("type: {t}")));
    }
    fields.push(Field::tail(format!("id:   {}", record.message_id)));
    fields.push(Field::tail(format!(
        "conv: {}#{}",
        record.conversation_id, record.conversation_seq
    )));
    // Feature #107 (IM3): the `trace:` line rides the wire ONLY when this message carries
    // a trace id. It is awareness-only and (like the whole header) NEVER persisted in
    // `messages.body`/`body_hash`/FTS — body purity is unchanged.
    if let Some(trace) = record.trace_id.as_deref() {
        fields.push(Field::tail(format!("trace: {trace}")));
    }
    if options.verbose {
        // Q1 escape hatch: the old compat lines, appended at the end before the bottom
        // border (HIDDEN by default; shown only in verbose mode).
        fields.push(Field::plain(format!(
            "reply: zynk reply {} -- \"<your response>\"",
            reply_pane(from)
        )));
        fields.push(Field::plain(
            "note: header is for agent awareness; not receipt proof".to_string(),
        ));
    }

    // Top border prefix: "╭─ Zynk message " (its leading "╭" is the corner; the rest of
    // the prefix sits inside the `inner + 2` dashed span).
    let title_prefix = format!("╭─ {HEADER_TITLE} ");
    // Columns of the dashed span the prefix already fills (everything past the corner).
    let title_consumed = display_width(&title_prefix) - 1;

    // The drawn box reserves 4 columns of chrome per interior line: "│ " (2) + " │" (2).
    // The inner content width is the widest field, never wider than the budget, and never
    // so narrow that the box title cannot fit (so the top border stays aligned).
    let budget = options.max_width.saturating_sub(4).max(1);
    let widest = fields
        .iter()
        .map(|f| display_width(&f.text))
        .max()
        .unwrap_or(0);
    // `inner + 2` is the dashed span; it must leave >= 1 trailing dash after the title,
    // i.e. `inner + 2 >= title_consumed + 1`, hence this lower bound. The budget is the
    // hard upper bound (the width cap) and wins only when it is itself >= the floor.
    let title_floor = (title_consumed + 1).saturating_sub(2);
    let lower = title_floor.max(1);
    let inner = widest.clamp(lower, budget.max(lower));

    let mut lines = Vec::with_capacity(fields.len() + 2);

    // The interior rows span `inner + 2` dashed columns between the corners ("│ " +
    // content padded to inner + " │" => 1 + 1 + inner + 1 + 1; the dashed span is
    // inner + 2). The bottom border matches it exactly.
    let total_dashes = inner + 2;
    let title_dashes = total_dashes - title_consumed;
    lines.push(format!("{title_prefix}{}╮", "─".repeat(title_dashes)));

    for field in &fields {
        let content = field.fit(inner);
        let pad = inner.saturating_sub(display_width(&content));
        lines.push(format!("│ {content}{} │", " ".repeat(pad)));
    }

    lines.push(format!("╰{}╯", "─".repeat(total_dashes)));
    lines.join("\n")
}

/// The binding wire join: `delivered = header + "\n\n" + body`. The header is
/// PREPENDED (before the body), is NEVER stripped by any receiver, and is wire-only —
/// the persisted `messages.body` stays the pure body.
pub fn prepend_header(header: &str, body: &str) -> String {
    format!("{header}\n\n{body}")
}

/// Resolve the [`HeaderOptions`] a CLI send command should render with: load the live
/// `[header]` config (defaults when absent) and fold in the `ZYNK_HEADER_VERBOSE` env
/// override. This is the single resolution point the send call sites share so env-vs-
/// config precedence (Feature #107 IM3 Q1) is identical everywhere.
pub fn resolve_header_options() -> HeaderOptions {
    let header_config = crate::config::Config::load().config.header;
    let env_verbose = crate::config::env_first(&[crate::config::HEADER_VERBOSE_ENV_VAR]);
    header_config.resolve(env_verbose.as_deref())
}

/// True iff the target has an agent identity — the header is for agent AWARENESS, so a
/// detected agent label is acceptable here (this is NOT a control-path/receipt
/// decision; the proof invariant is unchanged). Uniform across claude/codex/pi: any
/// agent target gets the header, never an allowlist.
pub fn is_agent_target(to: &Party) -> bool {
    to.agent_session.is_some() || to.agent.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec() -> PersistedSend {
        PersistedSend {
            message_id: "msg_x".into(),
            conversation_id: "conv_y".into(),
            conversation_seq: 7,
            body_hash: "abc123".into(),
            runtime_session_id: "rt_z".into(),
            socket_namespace: "/tmp/zynk.sock".into(),
            trace_id: None,
        }
    }

    fn rec_with_trace(trace: &str) -> PersistedSend {
        PersistedSend {
            trace_id: Some(trace.into()),
            ..rec()
        }
    }

    /// Default (hidden reply/note) options with a roomy width cap.
    fn opts() -> HeaderOptions {
        HeaderOptions {
            verbose: false,
            max_width: 100,
        }
    }

    /// Verbose options (reply/note shown) with a roomy width cap.
    fn opts_verbose() -> HeaderOptions {
        HeaderOptions {
            verbose: true,
            max_width: 100,
        }
    }

    /// Feature #107 (IM3) review fix B: the resolved [`HeaderOptions`] a sender renders
    /// with reflect a `[header]` config — proving the PER-SEND-INVOCATION client path
    /// (`resolve_header_options` → `Config::load`) honors the section. We point the config
    /// loader at a temp file and scrub the verbose env so config alone drives the result.
    #[test]
    fn resolve_header_options_reflects_header_config() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "zynk-header-config-{}-{}.toml",
            std::process::id(),
            crate::zynk::message::new_prefixed_id("hdr")
        ));
        std::fs::write(&path, "[header]\nverbose = true\nmax_width = 64\n").unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
        std::env::remove_var(crate::config::HEADER_VERBOSE_ENV_VAR);

        let resolved = resolve_header_options();

        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_file(&path);

        assert!(
            resolved.verbose,
            "config verbose=true must drive the option"
        );
        assert_eq!(
            resolved.max_width, 64,
            "config max_width must drive the cap"
        );
    }

    /// Feature #116 regression: a config produced by the Settings `save_header_*` write
    /// path (`upsert_section_value`) is honored by `resolve_header_options()`, and a
    /// truthy `ZYNK_HEADER_VERBOSE` overrides a config `verbose = false`.
    #[test]
    fn resolve_header_options_honors_settings_writeback_and_env_override() {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let path = std::env::temp_dir().join(format!(
            "zynk-header-writeback-{}-{}.toml",
            std::process::id(),
            crate::zynk::message::new_prefixed_id("hdr")
        ));
        // Build the file exactly as the Settings save fns would: upsert verbose +
        // max_width into the `[header]` section of an otherwise-populated config.
        let base = "onboarding = false\n\n[ui.toast]\ndelivery = \"terminal\"\n";
        let content = crate::config::upsert_section_value(base, "header", "verbose", "false");
        let content = crate::config::upsert_section_value(&content, "header", "max_width", "64");
        std::fs::write(&path, &content).unwrap();
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &path);
        std::env::remove_var(crate::config::HEADER_VERBOSE_ENV_VAR);

        // Config alone: verbose=false, max_width=64 honored.
        let resolved = resolve_header_options();
        assert!(!resolved.verbose, "config verbose=false honored");
        assert_eq!(resolved.max_width, 64, "config max_width honored");

        // Env override wins over config verbose=false.
        std::env::set_var(crate::config::HEADER_VERBOSE_ENV_VAR, "1");
        let resolved = resolve_header_options();
        assert!(
            resolved.verbose,
            "ZYNK_HEADER_VERBOSE=1 overrides config verbose=false"
        );
        assert_eq!(resolved.max_width, 64, "max_width unaffected by env");

        std::env::remove_var(crate::config::HEADER_VERBOSE_ENV_VAR);
        std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR);
        let _ = std::fs::remove_file(&path);
    }

    fn party_with_session(agent: &str) -> Party {
        Party {
            agent_session: Some(
                serde_json::json!({"source":"hook","agent":agent,"kind":"id","value":"v"}),
            ),
            ..Party::default()
        }
    }

    fn full_party(agent: &str, pane: &str, cwd: &str) -> Party {
        Party {
            agent: Some(agent.into()),
            pane: Some(pane.into()),
            cwd: Some(cwd.into()),
            ..Party::default()
        }
    }

    #[test]
    fn compact_home_collapses_home_and_subpaths() {
        let home = Some("/home/user");
        assert_eq!(compact_home("/home/user", home).as_ref(), "~");
        assert_eq!(
            compact_home("/home/user/workspace/projectx", home).as_ref(),
            "~/workspace/projectx"
        );
    }

    #[test]
    fn compact_home_leaves_non_home_and_relative_paths_unchanged() {
        let home = Some("/home/user");
        assert_eq!(compact_home("/etc/zynk", home).as_ref(), "/etc/zynk");
        assert_eq!(
            compact_home("workspace/projectx", home).as_ref(),
            "workspace/projectx"
        );
    }

    #[test]
    fn compact_home_ignores_absent_or_unusable_home() {
        // None, empty, bare root "/", and a relative HOME all leave the path untouched.
        assert_eq!(compact_home("/home/user/x", None).as_ref(), "/home/user/x");
        assert_eq!(
            compact_home("/home/user/x", Some("")).as_ref(),
            "/home/user/x"
        );
        assert_eq!(
            compact_home("/home/user/x", Some("/")).as_ref(),
            "/home/user/x"
        );
        assert_eq!(
            compact_home("/home/user/x", Some("home/zeus")).as_ref(),
            "/home/user/x"
        );
    }

    #[test]
    fn compact_home_normalizes_trailing_slash_home() {
        assert_eq!(
            compact_home("/home/user/workspace", Some("/home/user/")).as_ref(),
            "~/workspace"
        );
        assert_eq!(
            compact_home("/home/user", Some("/home/user/")).as_ref(),
            "~"
        );
    }

    #[test]
    fn compact_home_is_boundary_safe() {
        // A sibling whose name merely starts with the home string must NOT be compacted.
        let home = Some("/home/user");
        assert_eq!(compact_home("/home/user2", home).as_ref(), "/home/user2");
        assert_eq!(compact_home("/home/userer", home).as_ref(), "/home/userer");
        assert_eq!(
            compact_home("/home/user2/proj", home).as_ref(),
            "/home/user2/proj"
        );
    }

    #[test]
    fn compact_home_leaves_windows_paths_unchanged() {
        // Unix slash logic must not rewrite a Windows-style path.
        assert_eq!(
            compact_home(r"C:\Users\zeus\proj", Some("/home/user")).as_ref(),
            r"C:\Users\zeus\proj"
        );
    }

    /// Assert the closed-box invariants: title row, both corners, every interior border,
    /// and that EVERY line shares one display width (unicode columns, not byte length).
    fn assert_closed_box(h: &str) {
        let lines: Vec<&str> = h.lines().collect();
        assert!(lines.len() >= 3, "box has a top, body, and bottom: {h}");
        let first = lines.first().unwrap();
        let last = lines.last().unwrap();
        assert!(first.starts_with("╭─ Zynk message"), "title row: {h}");
        assert!(first.ends_with('╮'), "top-right corner: {h}");
        assert!(last.starts_with('╰'), "bottom-left corner: {h}");
        assert!(last.ends_with('╯'), "bottom-right corner: {h}");
        for line in &lines[1..lines.len() - 1] {
            assert!(line.starts_with("│ "), "interior left border: {line:?}");
            assert!(line.ends_with(" │"), "interior right border: {line:?}");
        }
        let want = display_width(first);
        for line in &lines {
            assert_eq!(
                display_width(line),
                want,
                "all lines share one display width: line {line:?} in {h}"
            );
        }
    }

    #[test]
    fn render_header_is_a_closed_content_width_box() {
        let from = full_party("claude", "w1-2", "/home/user/a");
        let to = full_party("codex", "w1-1", "/home/user/b");
        let h = render_header(&from, &to, &rec(), Some("review"), opts(), None);
        assert_closed_box(&h);
        assert!(
            h.contains("│ from: claude w1-2  cwd: /home/user/a"),
            "from line: {h}"
        );
        assert!(
            h.contains("│ to:   codex w1-1  cwd: /home/user/b"),
            "to line: {h}"
        );
        assert!(h.contains("type: review"), "type line: {h}");
        assert!(h.contains("id:   msg_x"), "id line: {h}");
        assert!(h.contains("conv: conv_y#7"), "conv line: {h}");
        // body_hash is NOT shown in the header (awareness, not machine-parse).
        assert!(!h.contains("abc123"), "body_hash must not appear: {h}");
    }

    #[test]
    fn render_header_compacts_home_cwd_for_display() {
        let from = full_party("claude", "w1-2", "/home/user/workspace/projectx");
        let to = full_party("codex", "w1-1", "/home/user");
        let h = render_header(
            &from,
            &to,
            &rec(),
            Some("review"),
            opts(),
            Some("/home/user"),
        );
        assert!(
            h.contains("from: claude w1-2  cwd: ~/workspace/projectx"),
            "from cwd compacted to ~: {h}"
        );
        assert!(
            h.contains("to:   codex w1-1  cwd: ~"),
            "to cwd equal to home renders bare ~: {h}"
        );
        assert_closed_box(&h);
    }

    #[test]
    fn render_header_keeps_non_home_cwd_absolute() {
        let from = full_party("claude", "w1-2", "/etc/zynk");
        let to = full_party("codex", "w1-1", "/home/user/p");
        let h = render_header(
            &from,
            &to,
            &rec(),
            Some("review"),
            opts(),
            Some("/home/user"),
        );
        assert!(h.contains("cwd: /etc/zynk"), "non-home stays absolute: {h}");
        assert!(h.contains("cwd: ~/p"), "home-prefixed compacted: {h}");
        assert_closed_box(&h);
    }

    #[test]
    fn render_header_without_home_keeps_cwd_absolute() {
        let from = full_party("claude", "w1-2", "/home/user/a");
        let to = full_party("codex", "w1-1", "/home/user/b");
        let h = render_header(&from, &to, &rec(), Some("review"), opts(), None);
        assert!(h.contains("cwd: /home/user/a"), "home=None → absolute: {h}");
        assert!(!h.contains('~'), "no tilde when home is None: {h}");
    }

    #[test]
    fn render_header_compaction_is_display_only_party_cwd_stays_absolute() {
        let from = full_party("claude", "w1-2", "/home/user/workspace/projectx");
        let to = full_party("codex", "w1-1", "/home/user");
        let h = render_header(
            &from,
            &to,
            &rec(),
            Some("review"),
            opts(),
            Some("/home/user"),
        );
        assert!(
            h.contains("cwd: ~/workspace/projectx"),
            "header compacted: {h}"
        );
        // The Party.cwd — the JSON/API/storage-facing field — is NEVER mutated by display.
        assert_eq!(from.cwd.as_deref(), Some("/home/user/workspace/projectx"));
        let json = serde_json::to_value(&from).expect("serialize party");
        assert_eq!(
            json.get("cwd").and_then(|v| v.as_str()),
            Some("/home/user/workspace/projectx"),
            "serialized Party.cwd stays absolute: {json}"
        );
    }

    #[test]
    fn render_header_default_hides_reply_and_note() {
        let from = full_party("claude", "w1-2", "/a");
        let to = full_party("codex", "w1-1", "/b");
        let h = render_header(&from, &to, &rec(), Some("review"), opts(), None);
        assert!(!h.contains("reply:"), "reply line HIDDEN by default: {h}");
        assert!(!h.contains("note:"), "note line HIDDEN by default: {h}");
        assert_closed_box(&h);
    }

    #[test]
    fn render_header_verbose_shows_reply_and_note() {
        let from = full_party("claude", "w1-2", "/a");
        let to = full_party("codex", "w1-1", "/b");
        let h = render_header(&from, &to, &rec(), Some("review"), opts_verbose(), None);
        assert!(
            h.contains("reply: zynk reply w1-2 -- \"<your response>\""),
            "verbose reply line points at the sender's pane: {h}"
        );
        assert!(
            h.contains("note: header is for agent awareness; not receipt proof"),
            "verbose awareness note: {h}"
        );
        assert_closed_box(&h);
    }

    #[test]
    fn render_header_trace_line_present_iff_trace_id_some() {
        let from = full_party("claude", "w1-2", "/a");
        let to = full_party("codex", "w1-1", "/b");
        let without = render_header(&from, &to, &rec(), None, opts(), None);
        assert!(
            !without.contains("trace:"),
            "no trace line absent: {without}"
        );

        let with = render_header(&from, &to, &rec_with_trace("trace_abc"), None, opts(), None);
        assert!(
            with.contains("trace: trace_abc"),
            "trace line present: {with}"
        );
        assert_closed_box(&with);
    }

    #[test]
    fn render_header_omits_type_line_when_none() {
        let from = full_party("claude", "w1-2", "/a");
        let to = full_party("codex", "w1-1", "/b");
        let h = render_header(&from, &to, &rec(), None, opts(), None);
        assert!(!h.contains("type:"), "type line omitted when None: {h}");
        // The other lines are still present.
        assert!(h.contains("id:   msg_x"), "id still present: {h}");
        assert!(h.contains("conv: conv_y#7"), "conv still present: {h}");
        assert_closed_box(&h);
    }

    #[test]
    fn render_header_renders_dash_for_missing_fields_no_panic() {
        // A sparse `from` (no agent/pane/cwd) must render "-" for each, never panic.
        let from = Party::default();
        let to = party_with_session("pi");
        let h = render_header(&from, &to, &rec(), None, opts(), None);
        assert!(
            h.contains("│ from: - -  cwd: -"),
            "missing from → dashes: {h}"
        );
        assert_closed_box(&h);
        // And the verbose reply line is a dash when the sender pane is unknown.
        let v = render_header(&from, &to, &rec(), None, opts_verbose(), None);
        assert!(
            v.contains("reply: zynk reply - -- \"<your response>\""),
            "reply pane is a dash when sender pane unknown: {v}"
        );
    }

    #[test]
    fn render_header_wide_glyph_cwd_keeps_box_aligned() {
        // A CJK cwd is 2 display columns per glyph; the right border MUST align by display
        // width, not byte length (each CJK char is 3 bytes, 2 columns).
        let from = full_party("claude", "w1-2", "/家/项目");
        let to = full_party("codex", "w1-1", "/emoji/😀/dir");
        let h = render_header(&from, &to, &rec(), Some("review"), opts(), None);
        assert_closed_box(&h);
    }

    #[test]
    fn render_header_truncates_overlong_cwd_with_middle_ellipsis() {
        let long = format!("/home/user/{}/leaf", "x".repeat(200));
        let from = full_party("claude", "w1-2", &long);
        let to = full_party("codex", "w1-1", "/b");
        let h = render_header(&from, &to, &rec(), Some("review"), opts(), None);
        assert_closed_box(&h);
        // Truncation keeps the box within the cap and uses a MIDDLE ellipsis for the path
        // (root + leaf preserved, middle elided).
        for line in h.lines() {
            assert!(display_width(line) <= 100, "within max_width 100: {line:?}");
        }
        let from_line = h
            .lines()
            .find(|l| l.contains("from:"))
            .expect("a from line");
        assert!(from_line.contains('…'), "ellipsis present: {from_line:?}");
        assert!(
            from_line.contains("/home/user/"),
            "root kept: {from_line:?}"
        );
        assert!(from_line.contains("leaf"), "leaf kept: {from_line:?}");
    }

    #[test]
    fn render_header_truncates_overlong_id_with_tail_ellipsis() {
        let rec = PersistedSend {
            message_id: format!("msg_{}", "a".repeat(300)),
            ..rec_with_trace(&format!("trace_{}", "b".repeat(300)))
        };
        let from = full_party("claude", "w1-2", "/a");
        let to = full_party("codex", "w1-1", "/b");
        let h = render_header(&from, &to, &rec, Some("review"), opts(), None);
        assert_closed_box(&h);
        for line in h.lines() {
            assert!(display_width(line) <= 100, "within max_width 100: {line:?}");
        }
        let id_line = h.lines().find(|l| l.contains("id:")).expect("an id line");
        // TAIL ellipsis: the head is kept, the line ends with the ellipsis (then border).
        assert!(id_line.contains("id:   msg_aaa"), "head kept: {id_line:?}");
        assert!(id_line.contains('…'), "ellipsis present: {id_line:?}");
        let trace_line = h
            .lines()
            .find(|l| l.contains("trace:"))
            .expect("a trace line");
        assert!(
            trace_line.contains("trace: trace_bbb"),
            "trace head kept: {trace_line:?}"
        );
        assert!(trace_line.contains('…'), "trace ellipsis: {trace_line:?}");
    }

    #[test]
    fn render_header_respects_small_max_width_cap() {
        // A tiny cap is clamped up to the renderer floor; the box still closes cleanly.
        let from = full_party("claude", "w1-2", "/home/user/some/deep/path");
        let to = full_party("codex", "w1-1", "/home/user/other/deep/path");
        let h = render_header(
            &from,
            &to,
            &rec(),
            Some("review"),
            HeaderOptions {
                verbose: false,
                max_width: 40,
            },
            None,
        );
        assert_closed_box(&h);
        for line in h.lines() {
            assert!(display_width(line) <= 40, "within max_width 40: {line:?}");
        }
    }

    #[test]
    fn render_header_is_deterministic() {
        let from = full_party("claude", "w1-2", "/a");
        let to = full_party("codex", "w1-1", "/b");
        assert_eq!(
            render_header(&from, &to, &rec(), Some("review"), opts(), None),
            render_header(&from, &to, &rec(), Some("review"), opts(), None)
        );
    }

    #[test]
    fn render_header_is_wire_only_not_in_persisted_body() {
        // The rendered box — including the trace line — is wire-only: it is prepended to
        // the body for delivery, but the persisted body NEVER contains the header or the
        // trace line. We model the persisted body as the pure body string.
        let from = full_party("claude", "w1-2", "/a");
        let to = full_party("codex", "w1-1", "/b");
        let body = "pure body sentinel";
        let header = render_header(
            &from,
            &to,
            &rec_with_trace("trace_secret"),
            Some("review"),
            opts(),
            None,
        );
        // The trace id rides only the header, never the persisted body.
        assert!(
            header.contains("trace: trace_secret"),
            "trace on wire: {header}"
        );
        assert!(!body.contains("trace_secret"), "trace NOT in body");
        assert!(!body.contains("Zynk message"), "header box NOT in body");
        // prepend_header is the only join; the body remains the suffix, untouched.
        let wire = prepend_header(&header, body);
        assert!(
            wire.ends_with(body),
            "body rides unchanged as the suffix: {wire}"
        );
        assert_ne!(wire, body, "wire differs from the pure body");
    }

    #[test]
    fn prepend_header_joins_header_before_body() {
        // delivered = header + "\n\n" + body (header PREPENDED, before the body).
        assert_eq!(prepend_header("HEAD", "hello world"), "HEAD\n\nhello world");
    }

    #[test]
    fn protocol_id_fields_omit_type_when_none() {
        let v = protocol_id_fields("m", "c", 1, "rt", "s", "h", None);
        assert!(v.get("type").is_none(), "type omitted when None: {v}");
        let v2 = protocol_id_fields("m", "c", 1, "rt", "s", "h", Some("approve"));
        assert_eq!(v2["type"], "approve");
        assert_eq!(v2["v"], PROTOCOL_VERSION);
    }

    #[test]
    fn is_agent_target_true_for_session_or_label() {
        // An IPC-sourced agent_session is an agent target.
        assert!(is_agent_target(&party_with_session("pi")));
        // A detected agent label alone is also an agent target (awareness, not control).
        let detection_only = Party {
            agent: Some("codex".into()),
            ..Party::default()
        };
        assert!(is_agent_target(&detection_only));
    }

    #[test]
    fn is_agent_target_false_for_plain_pane() {
        // A plain shell pane (no agent_session, no agent label) is NOT an agent target.
        let plain = Party {
            pane: Some("w1-1".into()),
            ..Party::default()
        };
        assert!(!is_agent_target(&plain));
        assert!(!is_agent_target(&Party::default()));
    }
}
