//! zynk fork (M3a): `zynk zynk ...` CLI namespace.
//!
//! `zynk zynk message-received` is a thin socket client for the
//! server-authoritative `zynk.message_received` method. It is NOT the receipt
//! authority and never writes SQLite directly; the server validates and records
//! the receipt. Integrations call this after obtaining the exact protocol IDs from
//! an agent-native hook payload (never by scraping pane output).

use crate::api::schema::{Method, Request, ZynkMessageReceivedParams};

const USAGE: &str = "usage: zynk zynk message-received --pane-id <id> --message-id <id> \
--conversation-id <id> --conversation-seq <n> --runtime-session-id <id> \
--socket-namespace <path> [--receiver-seq <n>] [--status <s>] [--json]";

pub(super) fn run_zynk_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(|arg| arg.as_str()) {
        Some("message-received") => message_received(&args[1..]),
        Some("query") => query(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
            Ok(2)
        }
    }
}

const QUERY_USAGE: &str = "usage: zynk zynk query <text> \
[--workspace <id>] [--conversation <id>] [--agent <label>] [--since <rfc3339>] \
[--type <t>] [--branch <b>] [--cwd <p>] [--trace <id>] [--limit <n>] [--exact] [--json]";

/// zynk fork (M5a): `zynk zynk query` — in-process lexical/BM25 retrieval over the
/// global SQLite DB (read-only; no socket). Prints the F4-enveloped result as JSON
/// (`--json`) or concise human text (default). Exit 1 on a failed result.
fn query(args: &[String]) -> std::io::Result<i32> {
    use crate::zynk::retrieval::{run_query, QueryFilters, QueryResponse};

    let mut filters = QueryFilters::default();
    // Pre-detect --json so the F4 JSON envelope is honored even if --json appears
    // AFTER a bad flag (the loop may break early on an error).
    let json = args.iter().any(|a| a == "--json");
    let mut terms: Vec<String> = Vec::new();
    let mut filter_error: Option<(String, serde_json::Value)> = None;

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let value = args.get(index + 1).cloned();
        let mut advance = 2;
        // A value-required flag as the LAST arg (no value) is an invalid_filter,
        // not a silent no-op (mirrors --limit's strictness).
        if value.is_none()
            && matches!(
                arg,
                "--workspace"
                    | "--conversation"
                    | "--agent"
                    | "--since"
                    | "--type"
                    | "--branch"
                    | "--cwd"
                    | "--trace"
                    | "--limit"
            )
        {
            filter_error = Some((
                format!("{arg} requires a value"),
                serde_json::json!({ "flag": arg }),
            ));
            break;
        }
        match arg {
            "--workspace" => filters.workspace = value,
            "--conversation" => filters.conversation = value,
            "--agent" => filters.agent = value,
            "--since" => filters.since = value,
            "--type" => filters.message_type = value,
            "--branch" => filters.branch = value,
            "--cwd" => filters.cwd = value,
            "--trace" => {
                // Feature #107 (IM2): validate with the SAME gate the send path uses
                // (`validate_trace_id`) — explicit error on bad input, never a silent
                // strip. `value` is Some here (the arity guard above).
                match value
                    .as_deref()
                    .map(crate::zynk::message::validate_trace_id)
                {
                    Some(Ok(cleaned)) => filters.trace_id = Some(cleaned),
                    Some(Err((_code, message))) => {
                        filter_error = Some((message, serde_json::json!({ "trace": value })));
                    }
                    None => unreachable!("--trace arity is guarded above"),
                }
            }
            "--limit" => match value.as_deref().map(str::parse::<usize>) {
                Some(Ok(n)) => filters.limit = n,
                _ => {
                    filter_error = Some((
                        "--limit must be a non-negative integer".to_string(),
                        serde_json::json!({ "limit": value }),
                    ));
                }
            },
            "--exact" => {
                filters.exact = true;
                advance = 1;
            }
            "--json" => advance = 1, // pre-detected above
            other if other.starts_with("--") => {
                // An unknown flag is an F4 invalid_filter (NOT a raw usage/exit 2),
                // so the response stays F4-enveloped on stdout.
                filter_error = Some((
                    format!("unknown argument: {other}"),
                    serde_json::json!({ "flag": other, "usage": QUERY_USAGE }),
                ));
                break;
            }
            _ => {
                terms.push(args[index].clone());
                advance = 1;
            }
        }
        index += advance;
    }

    if let Some((message, context)) = filter_error {
        return emit_query(&QueryResponse::invalid_filter(message, context), json);
    }
    if let Some(since) = &filters.since {
        if !looks_like_since(since) {
            return emit_query(
                &QueryResponse::invalid_filter(
                    format!("--since must be an RFC3339 date/time, got {since:?}"),
                    serde_json::json!({ "since": since }),
                ),
                json,
            );
        }
    }

    let resp = run_query(&terms.join(" "), filters);
    emit_query(&resp, json)
}

fn emit_query(resp: &crate::zynk::retrieval::QueryResponse, json: bool) -> std::io::Result<i32> {
    if json {
        println!("{}", resp.to_json());
    } else {
        println!("{}", resp.to_human());
    }
    Ok(if resp.is_failed() { 1 } else { 0 })
}

/// Lightweight RFC3339-prefix check for `--since` (avoids a date dependency).
/// Accepts `YYYY-MM-DD` and `YYYY-MM-DDThh:mm:ssZ` with an in-range month/day; the
/// DB compare is lexicographic on the RFC3339 `created_at` TEXT.
fn looks_like_since(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 10
        || !b[0..4].iter().all(u8::is_ascii_digit)
        || b[4] != b'-'
        || !b[5..7].iter().all(u8::is_ascii_digit)
        || b[7] != b'-'
        || !b[8..10].iter().all(u8::is_ascii_digit)
    {
        return false;
    }
    let month = (b[5] - b'0') * 10 + (b[6] - b'0');
    let day = (b[8] - b'0') * 10 + (b[9] - b'0');
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

fn message_received(args: &[String]) -> std::io::Result<i32> {
    // `--pane-id` defaults to the integration's pane env: `ZYNK_PANE_ID` primary,
    // `ZYNK_PANE_ID` transitional compat (ADR 0007 §5).
    let mut pane_id = crate::config::env_first(&["ZYNK_PANE_ID"]);
    let mut message_id: Option<String> = None;
    let mut conversation_id: Option<String> = None;
    let mut conversation_seq: Option<i64> = None;
    let mut runtime_session_id: Option<String> = None;
    let mut socket_namespace: Option<String> = None;
    let mut receiver_seq: Option<i64> = None;
    let mut status: Option<String> = None;

    let mut index = 0;
    while index < args.len() {
        let value = args.get(index + 1).cloned();
        let mut advance = 2;
        match args[index].as_str() {
            "--pane-id" => pane_id = value,
            "--message-id" => message_id = value,
            "--conversation-id" => conversation_id = value,
            "--conversation-seq" => conversation_seq = value.and_then(|v| v.parse().ok()),
            "--runtime-session-id" => runtime_session_id = value,
            "--socket-namespace" => socket_namespace = value,
            "--receiver-seq" => receiver_seq = value.and_then(|v| v.parse().ok()),
            "--status" => status = value,
            // JSON is always emitted (stable agent/test surface); `--json` is
            // accepted for forward-compatibility with the documented interface.
            "--json" => advance = 1,
            other => {
                eprintln!("unknown argument: {other}\n{USAGE}");
                return Ok(2);
            }
        }
        index += advance;
    }

    let (
        Some(pane_id),
        Some(message_id),
        Some(conversation_id),
        Some(conversation_seq),
        Some(runtime_session_id),
        Some(socket_namespace),
    ) = (
        pane_id,
        message_id,
        conversation_id,
        conversation_seq,
        runtime_session_id,
        socket_namespace,
    )
    else {
        eprintln!(
            "error: --pane-id (or ZYNK_PANE_ID / ZYNK_PANE_ID), --message-id, --conversation-id, \
             --conversation-seq, --runtime-session-id and --socket-namespace are required\n{USAGE}"
        );
        return Ok(2);
    };

    let response = super::send_request(&Request {
        id: "cli:zynk:message-received".into(),
        method: Method::ZynkMessageReceived(ZynkMessageReceivedParams {
            pane_id,
            message_id,
            conversation_id,
            conversation_seq,
            runtime_session_id,
            socket_namespace,
            receiver_seq,
            timestamp: None,
            status,
            receiver_agent_session: None,
        }),
    })?;

    let failed = response.get("error").is_some();
    println!("{}", serde_json::to_string(&response).unwrap());
    Ok(if failed { 1 } else { 0 })
}
