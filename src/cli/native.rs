//! zynk fork (M6 / ADR 0007 §2): the native top-level command surface.
//!
//! These are thin native verbs that REUSE the existing primitives — they do not
//! reimplement transport, persistence, the wire header, or retrieval:
//!   - `send` / `reply` resolve the target agent → its pane and submit via the IPC
//!     `pane.send_input` atomic path (the same transport `agent send` / `pane run`
//!     use), persisting via `crate::zynk::persistence::begin_send_attempt`. Body
//!     purity + header + honest delivery are unchanged. `reply` has NO `--reply-to`
//!     flag — the parent auto-derives (`derived_parent_id`; SPEC §5 / ADR 0002).
//!   - `thread` / `inbox` are READ-ONLY over the global DB (`open_query_readonly`,
//!     `PRAGMA query_only=1`), runtime-scoped on `socket_namespace`, ZERO delivery
//!     writes (`crate::zynk::inbox`).
//!   - `whoami` / `who` compose live-socket identity/topology (`pane.get`/`agent.list`).
//!     Identity is HOOK-AUTHORITATIVE (`agent_session`); a detection-only label is
//!     surfaced as explicitly `detected`, never as the authoritative identity.
//!   - `query` promotes `crate::zynk::retrieval::run_query` to a top-level verb (the
//!     legacy `zynk query` group stays for back-compat).
//!
//! All commands return the F4 envelope on stdout.

use crate::api::schema::{EmptyParams, Method, PaneSendInputParams, PaneTarget, Request};

/// The caller's source pane id from the host-protocol env. zynk exports
/// `ZYNK_PANE_ID` into every pane it spawns (ADR 0007 §5); an empty value is
/// treated as unset.
fn caller_pane_id_env() -> Option<String> {
    std::env::var("ZYNK_PANE_ID").ok().filter(|p| !p.is_empty())
}

/// `zynk send <target> [--type T] [--] <text...>` — a thin native verb over the
/// existing agent-resolve → pane.send_input transport. Returns the F4 `SendOutcome`.
/// Rich help for `send`/`reply`. The help flag is honored only in the first
/// (target) position, so `zynk send w2:p2 -- --help` still sends the body `--help`.
fn send_help_text(command: &str) -> String {
    let mut text = format!(
        "usage: zynk {command} <target> [--type T] [--trace <id|inherit>] [--] <text>\n\
         \n\
         \x20 <target>      a pane id like w2:p2 (re-read with `zynk pane list`)\n\
         \x20 --type <t>    free-form message type, e.g. request-review, request-changes, approve\n\
         \x20 --trace <id>  tag the message; `--trace inherit` continues the current trace\n\
         \x20 --            end options; the rest is the literal body (so `-- --help` sends \"--help\")\n\
         \n\
         Output: a JSON result. `delivery_status` + `proof` prove submission / input delivery,\n\
         not that the recipient read or understood it. Treat receipt/comprehension as proven only\n\
         by the recipient's own reply or stored evidence (`zynk thread`/`inbox`/`query`)."
    );
    if command == "reply" {
        text.push_str("\n`reply` auto-derives the parent; there is no --reply-to flag.");
    }
    text
}

fn print_send_help(command: &str) {
    eprintln!("{}", send_help_text(command));
}

pub(super) fn run_send_command(args: &[String]) -> std::io::Result<i32> {
    if args
        .first()
        .is_some_and(|arg| crate::cli::is_help_flag(arg))
    {
        print_send_help("send");
        return Ok(0);
    }
    if args.len() < 2 {
        print_send_help("send");
        return Ok(2);
    }
    native_send(
        crate::zynk::message::SendCommand::ZynkSend,
        &args[0],
        &args[1..],
    )
}

/// `zynk reply <target> [--type T] [--] <text...>` — identical transport to `send`;
/// the parent auto-derives (`begin_send_attempt` fills `derived_parent_id` from the
/// target's latest message). There is NO `--reply-to` flag (SPEC §5 / ADR 0002).
pub(super) fn run_reply_command(args: &[String]) -> std::io::Result<i32> {
    if args
        .first()
        .is_some_and(|arg| crate::cli::is_help_flag(arg))
    {
        print_send_help("reply");
        return Ok(0);
    }
    if args.len() < 2 {
        print_send_help("reply");
        return Ok(2);
    }
    native_send(
        crate::zynk::message::SendCommand::ZynkReply,
        &args[0],
        &args[1..],
    )
}

/// Shared send path for `send`/`reply`. Resolves the target agent to its pane and
/// submits via the atomic `pane.send_input`, persisting + prepending the wire header
/// exactly as `agent send` does. `command` is the native F4 label
/// (`SendCommand::ZynkSend` / `ZynkReply`) — transport/delivery/header/persistence are
/// IDENTICAL regardless; only the emitted `command` differs. `rest` is
/// `[--type T] [--] <text...>`.
fn native_send(
    command: crate::zynk::message::SendCommand,
    target: &str,
    rest: &[String],
) -> std::io::Result<i32> {
    use crate::zynk::message::{
        new_message_id, now_rfc3339, parse_type_trace_and_text, resolve_source, resolve_target,
        validate_trace_id, Party, Proof, SendError, SendOutcome, TargetResolution, TraceSpec,
    };
    use crate::zynk::persistence::{
        append_delivery_event, attach_to_outcome, empty_event_payload, failed_event_payload,
        resolve_parent_trace_id, transport_effect_context, DeliveryEventInput, DeliveryEventType,
        SendAttempt,
    };

    let (message_type, trace_spec, text) = parse_type_trace_and_text(rest);

    let send = |request: Request| super::send_request(&request);
    let from = resolve_source(caller_pane_id_env(), send);
    let (to, resolution) = resolve_target(target, send);
    let message_id = new_message_id();

    // Feature #107 (IM1): resolve the per-message trace_id. `--trace <id>` is validated
    // up front (explicit error on bad input, never a silent strip); `--trace inherit`
    // copies the derived parent's trace (or sends WITHOUT a trace + a stderr note when no
    // parent trace exists — never an invented conversation trace).
    let trace_id: Option<String> = match &trace_spec {
        Some(TraceSpec::Explicit(raw)) => match validate_trace_id(raw) {
            Ok(clean) => Some(clean),
            Err((code, message)) => {
                let outcome = SendOutcome::failed(
                    command,
                    message_id,
                    from,
                    to,
                    resolution,
                    message_type,
                    SendError {
                        code: code.into(),
                        message,
                        context: None,
                    },
                );
                println!("{}", outcome.to_json());
                return Ok(2);
            }
        },
        Some(TraceSpec::Inherit) => match resolve_parent_trace_id(&from, &to) {
            Ok(Some(parent)) => Some(parent),
            Ok(None) => {
                eprintln!("note: --trace inherit found no parent trace; sending without trace");
                None
            }
            Err(err) => {
                eprintln!(
                    "note: --trace inherit could not read the parent trace ({}); sending without trace",
                    err.code
                );
                None
            }
        },
        None => None,
    };

    match resolution {
        TargetResolution::Resolved => {
            let Some(pane_id) = to.pane.clone() else {
                let outcome = SendOutcome::failed(
                    command,
                    message_id,
                    from,
                    to,
                    TargetResolution::Resolved,
                    message_type,
                    SendError {
                        code: "transport_failed".into(),
                        message: "resolved agent has no pane id".into(),
                        context: None,
                    },
                );
                println!("{}", outcome.to_json());
                return Ok(1);
            };

            let created_at = now_rfc3339();
            let record = match crate::zynk::persistence::begin_send_attempt(SendAttempt {
                command,
                message_id: &message_id,
                target_arg: target,
                from: &from,
                to: &to,
                message_type: message_type.as_deref(),
                body: &text,
                created_at: &created_at,
                trace_id,
            }) {
                Ok(record) => record,
                Err(err) => {
                    let outcome = SendOutcome::failed(
                        command,
                        message_id,
                        from,
                        to,
                        TargetResolution::Resolved,
                        message_type,
                        SendError {
                            code: err.code.into(),
                            message: err.message,
                            context: None,
                        },
                    );
                    println!("{}", outcome.to_json());
                    return Ok(1);
                }
            };

            // The agent-VISIBLE header is PREPENDED to the wire text for EVERY agent
            // target (claude/codex/pi alike — uniform, not an allowlist); the persisted
            // body/body_hash/FTS stay pure. The header is awareness, NOT receipt proof:
            // it never advances delivery_status (still `submitted`).
            let text = if crate::zynk::header::is_agent_target(&to) {
                let header_options = crate::zynk::header::resolve_header_options();
                let display_home = crate::zynk::header::display_home();
                crate::zynk::header::prepend_header(
                    &crate::zynk::header::render_header(
                        &from,
                        &to,
                        &record,
                        message_type.as_deref(),
                        header_options,
                        display_home.as_deref(),
                    ),
                    &text,
                )
            } else {
                text
            };

            let result = super::send_request(&Request {
                id: "cli:send".into(),
                method: Method::PaneSendInput(PaneSendInputParams {
                    pane_id,
                    text,
                    keys: vec!["Enter".into()],
                }),
            });
            match result {
                Ok(response) if response.get("error").is_none() => {
                    let submitted_at = now_rfc3339();
                    let event_result = append_delivery_event(DeliveryEventInput {
                        message_id: &record.message_id,
                        event_type: DeliveryEventType::Submitted,
                        proof_source: "pane.send_input",
                        timestamp: &submitted_at,
                        payload: empty_event_payload(),
                    });
                    match event_result {
                        Ok(()) => {
                            let outcome = SendOutcome::ok(
                                command,
                                record.message_id.clone(),
                                from,
                                to,
                                TargetResolution::Resolved,
                                message_type,
                                Proof {
                                    proof_source: "pane.send_input",
                                },
                                submitted_at,
                            );
                            println!("{}", attach_to_outcome(outcome, &record).to_json());
                            Ok(0)
                        }
                        Err(err) => {
                            let outcome = SendOutcome::failed(
                                command,
                                record.message_id.clone(),
                                from,
                                to,
                                TargetResolution::Resolved,
                                message_type,
                                SendError {
                                    code: "delivery_event_persist_failed".into(),
                                    message: err.message,
                                    context: Some(transport_effect_context(
                                        "submitted_unrecorded",
                                        err.code.to_string(),
                                    )),
                                },
                            );
                            println!("{}", attach_to_outcome(outcome, &record).to_json());
                            Ok(1)
                        }
                    }
                }
                other => {
                    let detail = match other {
                        Ok(response) => serde_json::to_string(&response).unwrap_or_default(),
                        Err(err) => err.to_string(),
                    };
                    let _ = append_delivery_event(DeliveryEventInput {
                        message_id: &record.message_id,
                        event_type: DeliveryEventType::Failed,
                        proof_source: "pane.send_input",
                        timestamp: &now_rfc3339(),
                        payload: failed_event_payload(format!("pane.send_input failed: {detail}")),
                    });
                    let outcome = SendOutcome::failed(
                        command,
                        record.message_id.clone(),
                        from,
                        to,
                        TargetResolution::Resolved,
                        message_type,
                        SendError {
                            code: "transport_failed".into(),
                            message: format!("pane.send_input failed: {detail}"),
                            context: None,
                        },
                    );
                    println!("{}", attach_to_outcome(outcome, &record).to_json());
                    Ok(1)
                }
            }
        }
        TargetResolution::NotFound => {
            let outcome = SendOutcome::failed(
                command,
                message_id,
                from,
                Party::default(),
                TargetResolution::NotFound,
                message_type,
                SendError {
                    code: "target_not_found".into(),
                    message: format!("no agent resolves the target '{target}'"),
                    context: None,
                },
            );
            println!("{}", outcome.to_json());
            Ok(1)
        }
        TargetResolution::Ambiguous => {
            let outcome = SendOutcome::failed(
                command,
                message_id,
                from,
                Party::default(),
                TargetResolution::Ambiguous,
                message_type,
                SendError {
                    code: "agent_target_ambiguous".into(),
                    message: format!("the target '{target}' matches more than one agent"),
                    context: None,
                },
            );
            println!("{}", outcome.to_json());
            Ok(1)
        }
        TargetResolution::Unknown => {
            let outcome = SendOutcome::failed(
                command,
                message_id,
                from,
                Party::default(),
                TargetResolution::Unknown,
                message_type,
                SendError {
                    code: "transport_failed".into(),
                    message: format!("could not reach zynk to resolve the target '{target}'"),
                    context: None,
                },
            );
            println!("{}", outcome.to_json());
            Ok(1)
        }
    }
}

/// `zynk thread <conversation_id|message_id> [--json]` — READ-ONLY transcript.
pub(super) fn run_thread_command(args: &[String]) -> std::io::Result<i32> {
    let mut selector: Option<&str> = None;
    let json = args.iter().any(|a| a == "--json");
    for arg in args {
        match arg.as_str() {
            "--json" => {}
            "help" | "--help" | "-h" => {
                eprintln!("usage: zynk thread <conversation_id|message_id> [--json]");
                return Ok(0);
            }
            other if other.starts_with("--") => {
                eprintln!("unknown option: {other}");
                eprintln!("usage: zynk thread <conversation_id|message_id> [--json]");
                return Ok(2);
            }
            other if selector.is_none() => selector = Some(other),
            _ => {
                eprintln!("usage: zynk thread <conversation_id|message_id> [--json]");
                return Ok(2);
            }
        }
    }

    let Some(selector) = selector else {
        // Emit the F4 envelope (not a bare usage) so the response stays machine-readable.
        let resp = crate::zynk::inbox::ThreadResponse::missing_selector();
        return emit_thread(&resp, json);
    };
    let resp = crate::zynk::inbox::run_thread(selector);
    emit_thread(&resp, json)
}

fn emit_thread(resp: &crate::zynk::inbox::ThreadResponse, json: bool) -> std::io::Result<i32> {
    if json {
        println!("{}", resp.to_json());
    } else {
        println!("{}", resp.to_human());
    }
    Ok(if resp.is_failed() { 1 } else { 0 })
}

/// `zynk trace <id> [--json]` — READ-ONLY list of every message carrying the given
/// trace id, across conversations, in the active runtime scope (oldest first).
/// Feature #107 (IM2); served by the partial index `idx_messages_trace_id`.
pub(super) fn run_trace_command(args: &[String]) -> std::io::Result<i32> {
    const TRACE_USAGE: &str = "usage: zynk trace <id> [--json]";
    let mut trace: Option<&str> = None;
    let json = args.iter().any(|a| a == "--json");
    for arg in args {
        match arg.as_str() {
            "--json" => {}
            "help" | "--help" | "-h" => {
                eprintln!("{TRACE_USAGE}");
                return Ok(0);
            }
            other if other.starts_with("--") => {
                eprintln!("unknown option: {other}");
                eprintln!("{TRACE_USAGE}");
                return Ok(2);
            }
            other if trace.is_none() => trace = Some(other),
            _ => {
                eprintln!("{TRACE_USAGE}");
                return Ok(2);
            }
        }
    }

    let Some(trace) = trace else {
        // Emit the F4 envelope (not a bare usage) so the response stays machine-readable.
        let resp = crate::zynk::inbox::TraceResponse::invalid_trace(
            "invalid_trace_id",
            "trace requires a <trace_id>",
            serde_json::json!({}),
        );
        return emit_trace(&resp, json);
    };
    let resp = crate::zynk::inbox::run_trace(trace);
    emit_trace(&resp, json)
}

fn emit_trace(resp: &crate::zynk::inbox::TraceResponse, json: bool) -> std::io::Result<i32> {
    if json {
        println!("{}", resp.to_json());
    } else {
        println!("{}", resp.to_human());
    }
    Ok(if resp.is_failed() { 1 } else { 0 })
}

/// `zynk inbox [--agent <me>] [--limit N] [--json]` — READ-ONLY list of messages
/// addressed to the caller. The caller defaults to the live pane identity.
pub(super) fn run_inbox_command(args: &[String]) -> std::io::Result<i32> {
    let json = args.iter().any(|a| a == "--json");
    let mut agent: Option<String> = None;
    let mut limit: usize = 50;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--agent" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent");
                    return Ok(2);
                };
                agent = Some(value.clone());
                index += 2;
            }
            "--limit" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --limit");
                    return Ok(2);
                };
                match value.parse::<usize>() {
                    Ok(n) => limit = n,
                    Err(_) => {
                        eprintln!("--limit must be a non-negative integer");
                        return Ok(2);
                    }
                }
                index += 2;
            }
            "--json" => index += 1,
            "help" | "--help" | "-h" => {
                eprintln!("usage: zynk inbox [--agent <me>] [--limit N] [--json]");
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                eprintln!("usage: zynk inbox [--agent <me>] [--limit N] [--json]");
                return Ok(2);
            }
        }
    }

    // Default the caller to the LIVE pane identity (hook-authoritative agent_session,
    // falling back to the pane's authoritative agent label) when --agent is absent.
    let agent = match agent {
        Some(a) => a,
        None => match caller_agent_label() {
            Some(a) => a,
            None => {
                let resp = crate::zynk::inbox::InboxResponse::unidentified_caller(
                    "no --agent given and the caller's pane identity could not be resolved",
                );
                return emit_inbox(&resp, json);
            }
        },
    };

    let resp = crate::zynk::inbox::run_inbox(&agent, limit);
    emit_inbox(&resp, json)
}

fn emit_inbox(resp: &crate::zynk::inbox::InboxResponse, json: bool) -> std::io::Result<i32> {
    if json {
        println!("{}", resp.to_json());
    } else {
        println!("{}", resp.to_human());
    }
    Ok(if resp.is_failed() { 1 } else { 0 })
}

/// Resolve the caller's agent label from the live pane (`ZYNK_PANE_ID` → `pane.get`),
/// preferring the HOOK-AUTHORITATIVE `agent_session.agent`, then the pane's
/// authoritative `agent` label. Returns `None` when no pane/identity resolves.
fn caller_agent_label() -> Option<String> {
    let pane_id = caller_pane_id_env()?;
    let value = super::send_request(&Request {
        id: "cli:inbox:whoami".into(),
        method: Method::PaneGet(PaneTarget {
            pane_id: pane_id.clone(),
        }),
    })
    .ok()?;
    if value.get("error").is_some() {
        return None;
    }
    let pane = &value["result"]["pane"];
    pane.get("agent_session")
        .and_then(|s| s.get("agent"))
        .and_then(|a| a.as_str())
        .or_else(|| pane.get("agent").and_then(|a| a.as_str()))
        .map(str::to_string)
}

/// `zynk whoami [--json]` — the caller's live identity, hook-authoritative.
pub(super) fn run_whoami_command(args: &[String]) -> std::io::Result<i32> {
    let json = args.iter().any(|a| a == "--json");
    for arg in args {
        match arg.as_str() {
            "--json" => {}
            "help" | "--help" | "-h" => {
                eprintln!("usage: zynk whoami [--json]");
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                eprintln!("usage: zynk whoami [--json]");
                return Ok(2);
            }
        }
    }

    let socket_namespace = crate::zynk::runtime::socket_namespace();
    let runtime_session_id = crate::zynk::runtime::read_runtime_id().ok();

    let Some(pane_id) = caller_pane_id_env() else {
        let resp = serde_json::json!({
            "result": "failed",
            "command": "zynk whoami",
            "code": "caller_unidentified",
            "message": "ZYNK_PANE_ID is not set — whoami needs the caller's source pane",
            "socket_namespace": socket_namespace,
            "next": "run whoami from inside an agent pane (ZYNK_PANE_ID set by the integration)",
        });
        return emit_value(&resp, json, true);
    };

    let value = super::send_request(&Request {
        id: "cli:whoami".into(),
        method: Method::PaneGet(PaneTarget {
            pane_id: pane_id.clone(),
        }),
    })?;
    if value.get("error").is_some() {
        let resp = serde_json::json!({
            "result": "failed",
            "command": "zynk whoami",
            "code": "pane_not_found",
            "message": format!("could not resolve the caller pane '{pane_id}'"),
            "context": value.get("error").cloned().unwrap_or(serde_json::Value::Null),
            "socket_namespace": socket_namespace,
            "next": "check ZYNK_PANE_ID and that the zynk server is reachable",
        });
        return emit_value(&resp, json, true);
    }

    let pane = &value["result"]["pane"];
    // HOOK-AUTHORITATIVE identity: the agent comes from agent_session, NEVER detection.
    let agent_session = pane.get("agent_session").filter(|v| !v.is_null());
    let authoritative_agent = agent_session
        .and_then(|s| s.get("agent"))
        .and_then(|a| a.as_str());
    // A detection/report-derived label that is NOT backed by a hook agent_session is
    // surfaced as explicitly `detected`, never promoted to the authoritative identity.
    let pane_agent = pane.get("agent").and_then(|a| a.as_str());
    let detected = if authoritative_agent.is_none() {
        pane_agent
    } else {
        None
    };

    let resp = serde_json::json!({
        "result": "ok",
        "command": "zynk whoami",
        "type": "zynk_whoami_result",
        "agent": authoritative_agent,
        "agent_session": agent_session.cloned().unwrap_or(serde_json::Value::Null),
        "detected": detected.map(|a| serde_json::json!({ "agent": a, "source": "detection" })),
        "pane_id": pane.get("pane_id").and_then(|v| v.as_str()).unwrap_or(&pane_id),
        "terminal_id": pane.get("terminal_id"),
        "workspace_id": pane.get("workspace_id"),
        "tab_id": pane.get("tab_id"),
        "cwd": pane.get("cwd"),
        "runtime_session_id": runtime_session_id,
        "socket_namespace": socket_namespace,
        "next": "identity is hook-authoritative (agent_session); a 'detected' label is non-authoritative",
    });
    emit_value(&resp, json, false)
}

/// `zynk who [--json]` — the live participant topology (`agent.list`).
pub(super) fn run_who_command(args: &[String]) -> std::io::Result<i32> {
    let json = args.iter().any(|a| a == "--json");
    for arg in args {
        match arg.as_str() {
            "--json" => {}
            "help" | "--help" | "-h" => {
                eprintln!("usage: zynk who [--json]");
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                eprintln!("usage: zynk who [--json]");
                return Ok(2);
            }
        }
    }

    let socket_namespace = crate::zynk::runtime::socket_namespace();
    let value = super::send_request(&Request {
        id: "cli:who".into(),
        method: Method::AgentList(EmptyParams::default()),
    })?;
    if value.get("error").is_some() {
        let resp = serde_json::json!({
            "result": "failed",
            "command": "zynk who",
            "code": "transport_failed",
            "message": "could not reach the zynk server to list participants",
            "context": value.get("error").cloned().unwrap_or(serde_json::Value::Null),
            "socket_namespace": socket_namespace,
            "next": "check that the zynk server is reachable",
        });
        return emit_value(&resp, json, true);
    }

    // Project each agent into a participant record. Identity is HOOK-AUTHORITATIVE: the
    // agent label is the agent_session.agent when present, else the reported pane label
    // surfaced as `detected`.
    let empty = Vec::new();
    let agents = value["result"]["agents"].as_array().unwrap_or(&empty);
    let participants: Vec<serde_json::Value> = agents
        .iter()
        .map(|a| {
            let agent_session = a.get("agent_session").filter(|v| !v.is_null());
            let authoritative = agent_session
                .and_then(|s| s.get("agent"))
                .and_then(|x| x.as_str());
            let pane_agent = a.get("agent").and_then(|x| x.as_str());
            let detected = if authoritative.is_none() {
                pane_agent
            } else {
                None
            };
            serde_json::json!({
                "agent": authoritative.or(pane_agent),
                "authoritative": authoritative.is_some(),
                "detected": detected,
                "pane_id": a.get("pane_id"),
                "terminal_id": a.get("terminal_id"),
                "workspace_id": a.get("workspace_id"),
                "tab_id": a.get("tab_id"),
                "agent_status": a.get("agent_status"),
                "agent_session": agent_session.cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect();

    let resp = serde_json::json!({
        "result": "ok",
        "command": "zynk who",
        "type": "zynk_who_result",
        "count": participants.len(),
        "participants": participants,
        "socket_namespace": socket_namespace,
        "next": "participant identity is hook-authoritative (agent_session); 'detected' labels are non-authoritative",
    });
    emit_value(&resp, json, false)
}

fn emit_value(resp: &serde_json::Value, _json: bool, failed: bool) -> std::io::Result<i32> {
    // whoami/who always emit JSON (the stable identity surface); `--json` is accepted
    // for forward-compat with the documented interface but does not change the output.
    println!("{}", serde_json::to_string(resp).unwrap());
    Ok(if failed { 1 } else { 0 })
}

/// `zynk query <text...> [filters] [--json]` — the existing retrieval command promoted
/// to a top-level verb. Mirrors the legacy `zynk query` parsing (kept for back-compat).
pub(super) fn run_query_command(args: &[String]) -> std::io::Result<i32> {
    use crate::zynk::retrieval::{run_query, QueryFilters, QueryResponse};

    const QUERY_USAGE: &str = "usage: zynk query <text> \
[--workspace <id>] [--conversation <id>] [--agent <label>] [--since <rfc3339>] \
[--type <t>] [--branch <b>] [--cwd <p>] [--trace <id>] [--limit <n>] [--exact] [--json]";

    let mut filters = QueryFilters::default();
    let json = args.iter().any(|a| a == "--json");
    let mut terms: Vec<String> = Vec::new();
    let mut filter_error: Option<(String, serde_json::Value)> = None;

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let value = args.get(index + 1).cloned();
        let mut advance = 2;
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
                // Feature #107 (IM2): validate the trace id with the SAME gate the send
                // path uses (`validate_trace_id`) — an explicit error on bad input,
                // never a silent strip. `value` is Some here (the arity guard above).
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
            "--json" => advance = 1,
            "help" | "--help" | "-h" => {
                eprintln!("{QUERY_USAGE}");
                return Ok(0);
            }
            other if other.starts_with("--") => {
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

/// Lightweight RFC3339-prefix check for `--since` (avoids a date dependency); mirrors
/// the legacy `zynk query` validation.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn send_reply_help_flag_exits_zero() {
        // help in the target position -> Ok(0), printed before any socket dispatch.
        assert_eq!(run_send_command(&v(&["--help"])).unwrap(), 0);
        assert_eq!(run_send_command(&v(&["-h"])).unwrap(), 0);
        assert_eq!(run_reply_command(&v(&["--help"])).unwrap(), 0);
        assert_eq!(run_reply_command(&v(&["-h"])).unwrap(), 0);
    }

    #[test]
    fn send_reply_missing_args_still_exit_two() {
        assert_eq!(run_send_command(&v(&[])).unwrap(), 2);
        assert_eq!(run_send_command(&v(&["w2:p2"])).unwrap(), 2);
        assert_eq!(run_reply_command(&v(&["w2:p2"])).unwrap(), 2);
    }

    #[test]
    fn send_help_text_documents_flags_and_proof_semantics() {
        let help = send_help_text("send");
        for token in [
            "--type",
            "--trace",
            "literal body",
            "w2:p2",
            "request-review",
            "approve",
        ] {
            assert!(help.contains(token), "send help must mention `{token}`");
        }
        assert!(help.contains("delivery_status") && help.contains("proof"));
        assert!(
            help.to_lowercase().contains("not that the recipient"),
            "send help must clarify proof != receipt/comprehension"
        );
        // reply variant adds the parent-auto-derive note
        assert!(send_help_text("reply").contains("no --reply-to flag"));
    }
}
