use crate::api::schema::{
    AgentReadParams, AgentRenameParams, AgentStartParams, AgentStatus, AgentTarget, EmptyParams,
    Method, ReadFormat, ReadSource, Request, Subscription,
};

pub(super) fn run_agent_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_agent_help();
        return Ok(2);
    };

    if crate::cli::leaf_help_requested(args) {
        print_agent_help();
        return Ok(0);
    }

    match subcommand {
        "list" => agent_list(&args[1..]),
        "get" => agent_get(&args[1..]),
        "read" => agent_read(&args[1..]),
        "send" => agent_send(&args[1..]),
        "rename" => agent_rename(&args[1..]),
        "focus" => agent_focus(&args[1..]),
        "wait" => agent_wait(&args[1..]),
        "attach" => agent_attach(&args[1..]),
        "start" => agent_start(&args[1..]),
        "explain" => agent_explain(&args[1..]),
        "help" | "--help" | "-h" => {
            print_agent_help();
            Ok(0)
        }
        _ => {
            print_agent_help();
            Ok(2)
        }
    }
}

fn agent_explain(args: &[String]) -> std::io::Result<i32> {
    let mut file = None;
    let mut agent = None;
    let mut json = false;
    let mut verbose = false;
    let mut target = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--file" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --file");
                    return Ok(2);
                };
                file = Some(value.clone());
                index += 2;
            }
            "--agent" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent");
                    return Ok(2);
                };
                agent = Some(value.clone());
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "--format" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --format");
                    return Ok(2);
                };
                match value.as_str() {
                    "json" => json = true,
                    "text" => json = false,
                    other => {
                        eprintln!("invalid --format: {other} (expected text or json)");
                        return Ok(2);
                    }
                }
                index += 2;
            }
            "--verbose" | "-v" => {
                verbose = true;
                index += 1;
            }
            "help" | "--help" | "-h" => {
                eprintln!("usage: zynk agent explain <target> [--json|--verbose]");
                eprintln!("usage: zynk agent explain --file PATH --agent LABEL [--json|--verbose]");
                return Ok(0);
            }
            value if value.starts_with('-') => {
                eprintln!("unknown option: {value}");
                return Ok(2);
            }
            value => {
                if target.is_some() {
                    eprintln!("usage: zynk agent explain <target> [--json]");
                    return Ok(2);
                }
                target = Some(value.to_string());
                index += 1;
            }
        }
    }

    let explain = if let Some(path) = file {
        if target.is_some() {
            eprintln!("usage: zynk agent explain --file PATH --agent LABEL [--json]");
            return Ok(2);
        }
        let Some(agent_label) = agent else {
            eprintln!("zynk agent explain --file requires --agent LABEL");
            return Ok(2);
        };
        let content = std::fs::read_to_string(path)?;
        crate::detect::manifest::explain_to_json_value(&crate::detect::manifest::explain_for_label(
            &agent_label,
            &content,
        ))
    } else {
        let Some(target) = target else {
            eprintln!("usage: zynk agent explain <target> [--json]");
            eprintln!("usage: zynk agent explain --file PATH --agent LABEL [--json]");
            return Ok(2);
        };
        if agent.is_some() {
            eprintln!("--agent is only valid with --file");
            return Ok(2);
        }

        let response = super::send_request(&Request {
            id: "cli:agent:explain".into(),
            method: Method::AgentExplain(AgentTarget {
                target: target.to_owned(),
            }),
        })?;
        if response.get("error").is_some() {
            eprintln!("{}", serde_json::to_string(&response).unwrap());
            return Ok(1);
        }
        response["result"]["explain"].clone()
    };

    if json {
        println!("{explain}");
    } else {
        print_agent_explain_text(&explain, verbose);
    }
    Ok(0)
}

fn print_agent_explain_text(explain: &serde_json::Value, verbose: bool) {
    println!("agent: {}", explain["agent"].as_str().unwrap_or("unknown"));
    println!("state: {}", explain["state"].as_str().unwrap_or("unknown"));
    println!(
        "manifest: {} {}",
        explain["manifest_source"].as_str().unwrap_or("none"),
        explain["manifest_version"].as_str().unwrap_or("unknown")
    );
    if let Some(rule) = explain["matched_rule"].as_object() {
        let rule_id = rule
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("-");
        println!(
            "rule: {} (region={} priority={})",
            rule_id,
            rule.get("region")
                .and_then(|value| value.as_str())
                .unwrap_or("-"),
            rule.get("priority")
                .and_then(|value| value.as_i64())
                .unwrap_or(0),
        );
        if let Some(preview) = matched_rule_region_preview(explain, rule_id) {
            println!("evidence: {preview:?}");
        }
    } else {
        println!("rule: none");
    }
    if let Some(reason) = explain["fallback_reason"].as_str() {
        println!("fallback_reason: {reason}");
    }
    if let Some(reason) = explain["screen_detection_skip_reason"].as_str() {
        println!("screen_detection_skip_reason: {reason}");
    }
    if let Some(reason) = explain["skipped_update_reason"].as_str() {
        println!("skipped_update_reason: {reason}");
    }
    if let Some(warning) = explain["warning"].as_str() {
        println!("warning: {warning}");
    }

    if !verbose {
        return;
    }

    println!(
        "visible: idle={} blocker={} working={}",
        explain["visible_idle"].as_bool().unwrap_or(false),
        explain["visible_blocker"].as_bool().unwrap_or(false),
        explain["visible_working"].as_bool().unwrap_or(false)
    );
    println!(
        "cached_remote_version: {}",
        explain["cached_remote_version"].as_str().unwrap_or("none")
    );
    println!(
        "local_override_shadowing_remote: {}",
        explain["local_override_shadowing_remote"]
            .as_bool()
            .unwrap_or(false)
    );
    if let Some(status) = explain["remote_update_status"].as_str() {
        println!("remote_update_status: {status}");
    }
    if let Some(error) = explain["remote_update_error"].as_str() {
        println!("remote_update_error: {error}");
    }
    if let Some(evaluated_rules) = explain["evaluated_rules"]
        .as_array()
        .filter(|rules| !rules.is_empty())
    {
        println!("evaluated_rules:");
        for rule in evaluated_rules {
            println!(
                "  {} {} priority={} region={} state={}",
                if rule["matched"].as_bool().unwrap_or(false) {
                    "✓"
                } else {
                    "✗"
                },
                rule["id"].as_str().unwrap_or("-"),
                rule["priority"].as_i64().unwrap_or(0),
                rule["region"].as_str().unwrap_or("-"),
                rule["state"].as_str().unwrap_or("unknown")
            );
            let evidence = &rule["evidence"];
            println!(
                "    matchers: contains={:?} regex={:?} line_regex={:?} all={} any={} not={}",
                evidence["contains"],
                evidence["regex"],
                evidence["line_regex"],
                evidence["all_count"].as_u64().unwrap_or(0),
                evidence["any_count"].as_u64().unwrap_or(0),
                evidence["not_count"].as_u64().unwrap_or(0)
            );
            println!(
                "    region: bytes={} preview={:?}",
                evidence["region_bytes"].as_u64().unwrap_or(0),
                evidence["region_preview"].as_str().unwrap_or("")
            );
        }
    }
}

fn matched_rule_region_preview<'a>(
    explain: &'a serde_json::Value,
    rule_id: &str,
) -> Option<&'a str> {
    explain["evaluated_rules"]
        .as_array()?
        .iter()
        .find(|rule| rule["id"].as_str() == Some(rule_id))?["evidence"]["region_preview"]
        .as_str()
        .filter(|preview| !preview.is_empty())
}

fn agent_start(args: &[String]) -> std::io::Result<i32> {
    let Some(name) = args.first() else {
        eprintln!("usage: zynk agent start <name> [--cwd PATH] [--workspace ID] [--tab ID] [--split right|down] [--focus|--no-focus] -- <argv...>");
        return Ok(2);
    };

    let Some(separator) = args.iter().position(|arg| arg == "--") else {
        eprintln!("usage: zynk agent start <name> [--cwd PATH] [--workspace ID] [--tab ID] [--split right|down] [--focus|--no-focus] -- <argv...>");
        return Ok(2);
    };
    if separator == args.len() - 1 {
        eprintln!("agent start requires argv after --");
        return Ok(2);
    }

    let mut cwd = None;
    let mut workspace_id = None;
    let mut tab_id = None;
    let mut split = None;
    let mut focus = false;

    let mut index = 1;
    while index < separator {
        match args[index].as_str() {
            "--cwd" => {
                let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--workspace" => {
                let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--tab" => {
                let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
                    eprintln!("missing value for --tab");
                    return Ok(2);
                };
                tab_id = Some(super::normalize_tab_id(value));
                index += 2;
            }
            "--split" => {
                let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
                    eprintln!("missing value for --split");
                    return Ok(2);
                };
                split = Some(super::parse_split_direction(value)?);
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:start".into(),
        method: Method::AgentStart(AgentStartParams {
            name: name.clone(),
            cwd,
            workspace_id,
            tab_id,
            split,
            focus,
            argv: args[separator + 1..].to_vec(),
        }),
    })?)
}

fn agent_list(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: zynk agent list");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:list".into(),
        method: Method::AgentList(EmptyParams::default()),
    })?)
}

fn agent_get(args: &[String]) -> std::io::Result<i32> {
    if args.len() == 2 && crate::cli::is_help_flag(&args[1]) {
        eprintln!("usage: zynk agent get <target>");
        return Ok(0);
    }
    let Some(target) = args.first() else {
        eprintln!("usage: zynk agent get <target>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: zynk agent get <target>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:get".into(),
        method: Method::AgentGet(AgentTarget {
            target: target.clone(),
        }),
    })?)
}

fn agent_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: zynk agent focus <target>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: zynk agent focus <target>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:focus".into(),
        method: Method::AgentFocus(AgentTarget {
            target: target.clone(),
        }),
    })?)
}

fn agent_attach(args: &[String]) -> std::io::Result<i32> {
    let (target, takeover) =
        match super::parse_attach_target(args, "usage: zynk agent attach <target> [--takeover]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    let response = resolve_agent_target(&target, "cli:agent:attach:resolve")?;
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }
    let Some(terminal_id) = response["result"]["agent"]["terminal_id"].as_str() else {
        eprintln!("agent attach failed: response did not include terminal_id");
        return Ok(1);
    };
    crate::client::run_terminal_attach(terminal_id.to_owned(), takeover)?;
    Ok(0)
}

fn agent_wait(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: zynk agent wait <target> --status <idle|working|blocked|unknown> [--timeout MS]");
        return Ok(2);
    };

    let mut timeout_ms = None;
    let mut desired_status = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--status" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --status");
                    return Ok(2);
                };
                desired_status = Some(parse_agent_wait_status(value)?);
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = Some(super::parse_u64_flag("--timeout", value)?);
                index += 2;
            }
            "help" | "--help" | "-h" => {
                eprintln!("usage: zynk agent wait <target> --status <idle|working|blocked|unknown> [--timeout MS]");
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(agent_status) = desired_status else {
        eprintln!("missing required --status");
        return Ok(2);
    };

    let response = resolve_agent_target(target, "cli:agent:wait:resolve")?;
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }
    if response["result"]["agent"]["agent_status"]
        .as_str()
        .is_some_and(|current| agent_wait_status_satisfied(agent_status, current))
    {
        println!("{}", serde_json::to_string(&response).unwrap());
        return Ok(0);
    }

    let Some(pane_id) = response["result"]["agent"]["pane_id"].as_str() else {
        eprintln!("agent wait failed: response did not include pane_id");
        return Ok(1);
    };

    let subscriptions = if agent_status == AgentStatus::Idle {
        vec![
            Subscription::PaneAgentStatusChanged {
                pane_id: pane_id.to_owned(),
                agent_status: Some(AgentStatus::Idle),
            },
            Subscription::PaneAgentStatusChanged {
                pane_id: pane_id.to_owned(),
                agent_status: Some(AgentStatus::Done),
            },
        ]
    } else {
        vec![Subscription::PaneAgentStatusChanged {
            pane_id: pane_id.to_owned(),
            agent_status: Some(agent_status),
        }]
    };

    super::wait_for_agent_change(
        Request {
            id: "cli:agent:wait".into(),
            method: Method::EventsSubscribe(crate::api::schema::EventsSubscribeParams {
                subscriptions,
            }),
        },
        timeout_ms,
        "timed out waiting for agent status change",
    )
}

fn resolve_agent_target(target: &str, request_id: &str) -> std::io::Result<serde_json::Value> {
    super::send_request(&Request {
        id: request_id.into(),
        method: Method::AgentGet(AgentTarget {
            target: target.to_owned(),
        }),
    })
}

fn agent_rename(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: zynk agent rename <target> <name>|--clear");
        return Ok(2);
    };
    if args.len() < 2 {
        eprintln!("usage: zynk agent rename <target> <name>|--clear");
        return Ok(2);
    }
    let name = if args.len() == 2 && args[1] == "--clear" {
        None
    } else {
        Some(args[1..].join(" "))
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:rename".into(),
        method: Method::AgentRename(AgentRenameParams {
            target: target.clone(),
            name,
        }),
    })?)
}

fn agent_send(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: zynk agent send <target> [--type T] [--] <text>");
        return Ok(2);
    }

    use crate::api::schema::PaneSendInputParams;
    use crate::zynk::message::{
        new_message_id, now_rfc3339, parse_type_and_text, resolve_source, resolve_target, Party,
        Proof, SendCommand, SendError, SendOutcome, TargetResolution,
    };
    use crate::zynk::persistence::{
        append_delivery_event, attach_to_outcome, empty_event_payload, failed_event_payload,
        transport_effect_context, DeliveryEventInput, DeliveryEventType, SendAttempt,
    };

    let target = &args[0];
    let (message_type, text) = parse_type_and_text(&args[1..]);

    // The transport used by both the resolvers and the submit. `send_request`
    // borrows a `&Request`; the resolvers pass an owned `Request`.
    let send = |request: Request| super::send_request(&request);

    let from = resolve_source(crate::config::env_first(&["ZYNK_PANE_ID"]), send);
    let (to, resolution) = resolve_target(target, send);
    let message_id = new_message_id();

    // ADR 0002 honest-submit correction: resolve the agent to its pane and submit
    // via `pane.send_input` (atomic), NOT zynk's literal-no-Enter `agent.send`.
    match resolution {
        TargetResolution::Resolved => {
            let Some(pane_id) = to.pane.clone() else {
                // Resolved but no pane id (should not happen): refuse to claim delivery.
                let outcome = SendOutcome::failed(
                    SendCommand::AgentSend,
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
                command: SendCommand::AgentSend,
                message_id: &message_id,
                target_arg: target,
                from: &from,
                to: &to,
                message_type: message_type.as_deref(),
                body: &text,
                created_at: &created_at,
                // `agent send` (legacy) does not expose `--trace` (feature #107 wires the
                // four native/pane verbs); it always persists with no trace.
                trace_id: None,
            }) {
                Ok(record) => record,
                Err(err) => {
                    let outcome = SendOutcome::failed(
                        SendCommand::AgentSend,
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

            // zynk: PREPEND the agent-VISIBLE header to the delivered text for EVERY
            // agent target (claude/codex/pi alike — uniform, not an allowlist). The
            // persisted body/body_hash/FTS above stay pure; the header rides only the
            // wire text and is awareness, NOT receipt proof (delivery_status unchanged).
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
                id: "cli:agent:send".into(),
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
                                SendCommand::AgentSend,
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
                                SendCommand::AgentSend,
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
                        SendCommand::AgentSend,
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
            // zynk normalizes zynk's `agent_not_found` to the F4 code `target_not_found`.
            let outcome = SendOutcome::failed(
                SendCommand::AgentSend,
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
                SendCommand::AgentSend,
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
            // The transport never reached the server (dead/missing socket): we could
            // not resolve the target AT ALL, so report it honestly as a transport
            // failure (NOT `target_not_found`) and submit NOTHING.
            let outcome = SendOutcome::failed(
                SendCommand::AgentSend,
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

fn agent_read(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: zynk agent read <target> [--source visible|recent|recent-unwrapped|detection] [--lines N] [--format text|ansi] [--ansi]");
        return Ok(2);
    };

    let mut source = ReadSource::Recent;
    let mut lines = None;
    let mut format = ReadFormat::Text;
    let mut strip_ansi = true;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = super::parse_read_source(value)?;
                index += 2;
            }
            "--lines" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --lines");
                    return Ok(2);
                };
                lines = Some(super::parse_u32_flag("--lines", value)?);
                index += 2;
            }
            "--format" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --format");
                    return Ok(2);
                };
                format = super::parse_read_format(value)?;
                strip_ansi = !matches!(format, ReadFormat::Ansi);
                index += 2;
            }
            "--ansi" => {
                format = ReadFormat::Ansi;
                strip_ansi = false;
                index += 1;
            }
            // `zynk agent read <target> --help` -> command help (read takes no body).
            other if crate::cli::is_help_flag(other) => {
                eprintln!("usage: zynk agent read <target> [--source visible|recent|recent-unwrapped|detection] [--lines N] [--format text|ansi] [--ansi]");
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                eprintln!("run `zynk agent read --help` for command help");
                return Ok(2);
            }
        }
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:read".into(),
        method: Method::AgentRead(AgentReadParams {
            target: target.clone(),
            source,
            lines,
            format,
            strip_ansi,
        }),
    })?)
}

fn agent_wait_status_satisfied(desired: AgentStatus, current: &str) -> bool {
    match desired {
        AgentStatus::Idle => matches!(current, "idle" | "done"),
        AgentStatus::Working => current == "working",
        AgentStatus::Blocked => current == "blocked",
        AgentStatus::Unknown => current == "unknown",
        AgentStatus::Done => false,
    }
}

fn parse_agent_wait_status(value: &str) -> std::io::Result<AgentStatus> {
    match value {
        "idle" => Ok(AgentStatus::Idle),
        "working" => Ok(AgentStatus::Working),
        "blocked" => Ok(AgentStatus::Blocked),
        "unknown" => Ok(AgentStatus::Unknown),
        "done" => Err(std::io::Error::other(
            "done is a UI attention state; use idle for CLI agent completion waits",
        )),
        _ => Err(std::io::Error::other(format!(
            "invalid agent status: {value} (expected idle, working, blocked, or unknown)"
        ))),
    }
}

fn print_agent_help() {
    eprintln!("zynk agent commands:");
    eprintln!("  zynk agent list");
    eprintln!("  zynk agent get <target>");
    eprintln!("  zynk agent read <target> [--source visible|recent|recent-unwrapped|detection] [--lines N] [--format text|ansi] [--ansi]");
    eprintln!("  zynk agent send <target> [--type T] [--] <text>");
    eprintln!("  zynk agent rename <target> <name>|--clear");
    eprintln!("  zynk agent focus <target>");
    eprintln!("  zynk agent wait <target> --status <idle|working|blocked|unknown> [--timeout MS]");
    eprintln!("  zynk agent attach <target> [--takeover]");
    eprintln!("  zynk agent start <name> [--cwd PATH] [--workspace ID] [--tab ID] [--split right|down] [--focus|--no-focus] -- <argv...>");
    eprintln!("  zynk agent explain <target> [--json|--verbose]");
    eprintln!("  zynk agent explain --file PATH --agent LABEL [--json|--verbose]");
    eprintln!("  targets accept terminal ids, unique agent names, detected/reported agent labels, and legacy pane ids");
    eprintln!(
        "  agent send writes literal text; use pane run when you want command text plus Enter"
    );
}
