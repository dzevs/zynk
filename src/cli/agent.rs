use crate::api::schema::{
    AgentInfo, AgentPromptParams, AgentReadParams, AgentRenameParams, AgentSendKeysParams,
    AgentStartParams, AgentStatus, AgentTarget, EmptyParams, ErrorBody, ErrorResponse, Method,
    ReadFormat, ReadSource, Request, ResponseResult, SuccessResponse,
};
use std::time::{Duration, Instant};

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
        "send-keys" => agent_send_keys(&args[1..]),
        "prompt" => agent_prompt(&args[1..]),
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
        eprintln!(
            "usage: zynk agent start <name> --kind KIND --pane ID [--timeout MS] [-- <args...>]"
        );
        return Ok(2);
    };
    let separator = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let mut kind = None;
    let mut pane = None;
    let mut timeout_ms = None;
    let mut index = 1;
    while index < separator {
        let option = args[index].as_str();
        if !matches!(option, "--kind" | "--pane" | "--timeout") {
            eprintln!("unknown option: {option}");
            return Ok(2);
        }
        let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
            eprintln!("missing value for {option}");
            return Ok(2);
        };
        match option {
            "--kind" if kind.is_none() => kind = crate::detect::parse_agent_label(value),
            "--pane" if pane.is_none() => pane = Some(super::normalize_pane_id(value)),
            "--timeout" if timeout_ms.is_none() => match value.parse::<u64>() {
                Ok(ms) if ms > 3000 && ms <= 300000 => timeout_ms = Some(ms),
                _ => {
                    eprintln!(
                        "agent start timeout must be greater than 3000ms and at most 300000ms"
                    );
                    return Ok(2);
                }
            },
            _ => {
                eprintln!("duplicate option: {option}");
                return Ok(2);
            }
        }
        if option == "--kind" && kind.is_none() {
            eprintln!("unsupported interactive agent kind {value}");
            return Ok(2);
        }
        index += 2;
    }
    let (Some(kind), Some(pane_id)) = (kind, pane) else {
        eprintln!("agent start requires --kind and --pane");
        return Ok(2);
    };
    let expected_kind = crate::detect::agent_label(kind);
    let mut response = super::send_request(&Request {
        id: "cli:agent:start".into(),
        method: Method::AgentStart(AgentStartParams {
            name: name.clone(),
            kind: expected_kind.into(),
            pane_id,
            timeout_ms,
            args: args.get(separator + 1..).unwrap_or_default().to_vec(),
        }),
    })?;
    let decoded = decode_agent_response(&response, "cli:agent:start", true);
    let started = match decoded {
        Ok(agent) if agent.name.as_deref() == Some(name) && !agent.terminal_id.is_empty() => agent,
        Ok(_) => {
            return super::print_response(&cli_agent_error(
                "cli:agent:start",
                invalid_agent_response(),
            ))
        }
        Err(error) => return super::print_response(&cli_agent_error("cli:agent:start", error)),
    };
    match wait_for_started_agent(
        &started.terminal_id,
        name,
        expected_kind,
        Duration::from_millis(timeout_ms.unwrap_or(30000)),
    )? {
        Ok(agent) => {
            response["result"]["agent"] =
                serde_json::to_value(agent).map_err(std::io::Error::other)?;
            super::print_response(&response)
        }
        Err(error) => super::print_response(&cli_agent_error("cli:agent:start", error)),
    }
}

fn invalid_agent_response() -> ErrorBody {
    ErrorBody {
        code: "invalid_response".into(),
        message: "invalid or contradictory agent response".into(),
    }
}

fn cli_agent_error(id: &str, error: ErrorBody) -> serde_json::Value {
    serde_json::json!({"id": id, "error": error})
}

fn decode_agent_response(
    value: &serde_json::Value,
    id: &str,
    started: bool,
) -> Result<AgentInfo, ErrorBody> {
    if value.get("error").is_some() {
        let response: ErrorResponse =
            serde_json::from_value(value.clone()).map_err(|_| invalid_agent_response())?;
        return if response.id == id {
            Err(response.error)
        } else {
            Err(invalid_agent_response())
        };
    }
    let response: SuccessResponse =
        serde_json::from_value(value.clone()).map_err(|_| invalid_agent_response())?;
    if response.id != id {
        return Err(invalid_agent_response());
    }
    match response.result {
        ResponseResult::AgentStarted { agent, .. } if started => Ok(agent),
        ResponseResult::AgentInfo { agent } if !started => Ok(agent),
        _ => Err(invalid_agent_response()),
    }
}

fn wait_for_started_agent(
    terminal_id: &str,
    name: &str,
    kind: &str,
    timeout: Duration,
) -> std::io::Result<Result<AgentInfo, ErrorBody>> {
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        return Ok(Err(ErrorBody {
            code: "invalid_agent_timeout".into(),
            message: "agent wait deadline is out of range".into(),
        }));
    };
    let mut first = true;
    loop {
        if Instant::now() >= deadline {
            let _ = super::send_request_unchecked(&Request {
                id: "cli:agent:start:timeout".into(),
                method: Method::AgentGet(AgentTarget {
                    target: terminal_id.into(),
                }),
            });
            return Ok(Err(ErrorBody { code: "agent_start_timeout".into(), message: "agent did not become interactive before the timeout; the command may have run and its label remains".into() }));
        }
        let request = Request {
            id: "cli:agent:start".into(),
            method: Method::AgentGet(AgentTarget {
                target: terminal_id.into(),
            }),
        };
        let value = if first {
            first = false;
            super::send_request(&request)?
        } else {
            super::send_request_unchecked(&request)?
        };
        let agent = match decode_agent_response(&value, &request.id, false) {
            Ok(agent) => agent,
            Err(error) => {
                return Ok(Err(if error.code == "invalid_response" {
                    error
                } else {
                    ErrorBody {
                        code: "agent_start_failed".into(),
                        message: "agent target disappeared before becoming interactive".into(),
                    }
                }))
            }
        };
        let error = if agent.terminal_id != terminal_id {
            Some(ErrorBody {
                code: "agent_name_not_found".into(),
                message: format!("named agent {name} changed terminal"),
            })
        } else if agent.agent.as_deref().is_some_and(|actual| actual != kind) {
            Some(ErrorBody {
                code: "agent_kind_mismatch".into(),
                message: format!("agent {name} is not {kind}"),
            })
        } else if agent.name.as_deref() != Some(name) {
            Some(ErrorBody {
                code: "agent_name_not_found".into(),
                message: format!("named agent {name} not found"),
            })
        } else if matches!(
            agent.agent_status,
            AgentStatus::Idle | AgentStatus::Done | AgentStatus::Blocked
        ) && agent.interactive_ready
        {
            return Ok(Ok(agent));
        } else if !agent.launch_pending {
            Some(ErrorBody {
                code: "agent_start_failed".into(),
                message: "agent launch ended before becoming interactive".into(),
            })
        } else {
            None
        };
        if let Some(error) = error {
            return Ok(Err(error));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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

fn agent_wait_status_matches(status: AgentStatus, until: &[AgentStatus]) -> bool {
    if until.is_empty() {
        matches!(
            status,
            AgentStatus::Idle | AgentStatus::Done | AgentStatus::Blocked
        )
    } else {
        until.contains(&status)
    }
}

fn agent_wait(args: &[String]) -> std::io::Result<i32> {
    let Some(name) = args
        .first()
        .filter(|name| !name.is_empty() && !name.starts_with('-'))
    else {
        eprintln!("usage: zynk agent wait <name> [--until STATUS]... [--timeout MS]");
        return Ok(2);
    };
    let mut until = Vec::new();
    let mut timeout = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--until" => {
                let Some(value) = args
                    .get(index + 1)
                    .filter(|value| !value.is_empty() && !value.starts_with('-'))
                else {
                    eprintln!("--until requires at least one status");
                    return Ok(2);
                };
                match super::parse_agent_status(value) {
                    Ok(status) => until.push(status),
                    Err(err) => {
                        eprintln!("{err}");
                        return Ok(2);
                    }
                }
                index += 2;
            }
            "--timeout" if timeout.is_none() => {
                let Some(ms) = args
                    .get(index + 1)
                    .and_then(|value| value.parse::<u64>().ok())
                else {
                    eprintln!("invalid or missing --timeout");
                    return Ok(2);
                };
                let duration = Duration::from_millis(ms);
                if Instant::now().checked_add(duration).is_none() {
                    eprintln!("agent wait deadline is out of range");
                    return Ok(2);
                }
                timeout = Some(duration);
                index += 2;
            }
            "help" | "--help" | "-h" => {
                eprintln!("usage: zynk agent wait <name> [--until STATUS]... [--timeout MS]");
                return Ok(0);
            }
            other => {
                eprintln!("unknown or duplicate option: {other}");
                return Ok(2);
            }
        }
    }

    let mut terminal_id: Option<String> = None;
    let mut deadline = None;
    let mut first_poll = true;
    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return super::print_response(&cli_agent_error(
                "cli:agent:wait",
                ErrorBody {
                    code: "timeout".into(),
                    message: "timed out waiting for agent completion".into(),
                },
            ));
        }
        let initial = terminal_id.is_none();
        let request = Request {
            id: if initial {
                "cli:agent:wait:resolve"
            } else {
                "cli:agent:wait"
            }
            .into(),
            method: Method::AgentGet(AgentTarget {
                target: terminal_id.as_deref().unwrap_or(name).to_owned(),
            }),
        };
        let result = if initial || first_poll {
            if !initial {
                first_poll = false;
            }
            super::send_request(&request)
        } else {
            super::send_request_unchecked(&request)
        };
        let response = match result {
            Ok(response) => response,
            Err(err) => {
                if super::protocol_mismatch_response(&err).is_some() {
                    return Err(err);
                }
                let error = if request_error_is_invalid_json(&err) {
                    invalid_agent_response()
                } else {
                    ErrorBody {
                        code: "agent_wait_transport_failed".into(),
                        message: err.to_string(),
                    }
                };
                return super::print_response(&cli_agent_error(&request.id, error));
            }
        };
        let decoded = if response.get("error").is_some() && response.get("result").is_some() {
            Err(invalid_agent_response())
        } else {
            decode_agent_response(&response, &request.id, false)
        };
        let agent = match decoded {
            Ok(agent) => agent,
            Err(error) => return super::print_response(&cli_agent_error(&request.id, error)),
        };
        let status_matches = agent_wait_status_matches(agent.agent_status, &until);
        let error = if initial && agent.terminal_id.is_empty() {
            Some(invalid_agent_response())
        } else if terminal_id
            .as_deref()
            .is_some_and(|pinned| agent.terminal_id != pinned)
            || agent.name.as_deref() != Some(name.as_str())
        {
            Some(ErrorBody {
                code: "agent_name_not_found".into(),
                message: format!("named agent {name} no longer owns the target terminal"),
            })
        } else if agent.agent_status == AgentStatus::Unknown && !status_matches {
            Some(ErrorBody {
                code: "agent_not_running".into(),
                message: "agent is no longer running".into(),
            })
        } else {
            None
        };
        if let Some(error) = error {
            return super::print_response(&cli_agent_error(&request.id, error));
        }
        if status_matches {
            return super::print_response(&response);
        }
        if initial {
            terminal_id = Some(agent.terminal_id);
            deadline = match timeout {
                Some(timeout) => match Instant::now().checked_add(timeout) {
                    Some(deadline) => Some(deadline),
                    None => {
                        return super::print_response(&cli_agent_error(
                            "cli:agent:wait",
                            ErrorBody {
                                code: "invalid_agent_timeout".into(),
                                message: "agent wait deadline is out of range".into(),
                            },
                        ))
                    }
                },
                None => None,
            };
        } else {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
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

struct PromptArgs {
    name: String,
    text: String,
    message_type: Option<String>,
    trace: Option<crate::zynk::message::TraceSpec>,
    wait: bool,
    until: Vec<AgentStatus>,
    timeout_ms: Option<u64>,
}

fn parse_prompt_args(args: &[String]) -> Result<PromptArgs, String> {
    use crate::zynk::message::TraceSpec;

    let name = args.first().filter(|name| !name.is_empty() && !name.starts_with('-'))
        .ok_or("usage: zynk agent prompt <name> [--type T] [--trace ID|inherit] [--wait] [--until STATUS]... [--timeout MS] [--] <text>")?;
    let mut parsed = PromptArgs {
        name: name.clone(),
        text: String::new(),
        message_type: None,
        trace: None,
        wait: false,
        until: Vec::new(),
        timeout_ms: None,
    };
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--" => {
                index += 1;
                break;
            }
            "--wait" if !parsed.wait => {
                parsed.wait = true;
                index += 1;
            }
            "--until" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.is_empty() && !value.starts_with('-'))
                    .ok_or("--until requires at least one status")?;
                parsed
                    .until
                    .push(super::parse_agent_status(value).map_err(|error| error.to_string())?);
                index += 2;
            }
            option @ ("--type" | "--trace" | "--timeout") => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.is_empty() && !value.starts_with("--"))
                    .ok_or_else(|| format!("missing value for {option}"))?;
                match option {
                    "--type" if parsed.message_type.is_none() => {
                        parsed.message_type = Some(value.clone())
                    }
                    "--trace" if parsed.trace.is_none() => {
                        parsed.trace = Some(if value == "inherit" {
                            TraceSpec::Inherit
                        } else {
                            TraceSpec::Explicit(
                                crate::zynk::message::validate_trace_id(value)
                                    .map_err(|(_, message)| message)?,
                            )
                        });
                    }
                    "--timeout" if parsed.timeout_ms.is_none() => {
                        let timeout = value
                            .parse::<u64>()
                            .map_err(|_| "invalid --timeout".to_string())?;
                        Instant::now()
                            .checked_add(Duration::from_millis(timeout))
                            .ok_or("agent wait deadline is out of range")?;
                        parsed.timeout_ms = Some(timeout);
                    }
                    _ => return Err(format!("duplicate option: {option}")),
                }
                index += 2;
            }
            option if option.starts_with('-') => {
                return Err(format!("unknown or duplicate option: {option}"))
            }
            _ => break,
        }
    }
    if parsed.timeout_ms.is_some() && !parsed.wait {
        return Err("--timeout requires --wait".into());
    }
    if !parsed.until.is_empty() && !parsed.wait {
        return Err("--until requires --wait".into());
    }
    parsed.text = args[index..].join(" ");
    if parsed.text.is_empty() {
        return Err("agent prompt must not be empty".into());
    }
    Ok(parsed)
}

#[derive(serde::Serialize)]
struct AgentPromptOutcome {
    #[serde(flatten)]
    outcome: crate::zynk::message::SendOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    wait: Option<AgentPromptWaitOutcome>,
}

#[derive(serde::Serialize)]
#[serde(tag = "result", rename_all = "lowercase")]
enum AgentPromptWaitOutcome {
    Ok {
        agent: Box<AgentInfo>,
    },
    Failed {
        error: crate::zynk::message::SendError,
    },
}

fn prompt_response_baseline(
    value: serde_json::Value,
    name: &str,
    terminal_id: &str,
) -> Result<u64, crate::zynk::message::SendError> {
    use crate::zynk::{message::SendError, persistence::transport_effect_context};
    let invalid = || SendError {
        code: "invalid_response".into(),
        message: "invalid or contradictory agent prompt response".into(),
        context: Some(transport_effect_context(
            "submission_unverified",
            "agent_prompted response could not be verified".into(),
        )),
    };
    if value.get("error").is_some() {
        if value.get("result").is_some() {
            return Err(invalid());
        }
        let response: ErrorResponse = serde_json::from_value(value).map_err(|_| invalid())?;
        if response.id != "cli:agent:prompt" {
            return Err(invalid());
        }
        return Err(SendError {
            code: response.error.code,
            message: response.error.message,
            context: None,
        });
    }
    let response: SuccessResponse = serde_json::from_value(value).map_err(|_| invalid())?;
    if response.id != "cli:agent:prompt" {
        return Err(invalid());
    }
    match response.result {
        ResponseResult::AgentPrompted {
            agent,
            baseline_state_change_seq,
        } if agent.terminal_id == terminal_id && agent.name.as_deref() == Some(name) => {
            Ok(baseline_state_change_seq)
        }
        _ => Err(invalid()),
    }
}

fn request_error_is_invalid_json(error: &std::io::Error) -> bool {
    matches!(
        error
            .get_ref()
            .and_then(|source| source.downcast_ref::<crate::api::client::ApiClientError>()),
        Some(crate::api::client::ApiClientError::Json(_))
    )
}

fn wait_after_prompt(
    terminal_id: &str,
    name: &str,
    baseline: u64,
    until: &[AgentStatus],
    timeout_ms: Option<u64>,
) -> Result<AgentInfo, crate::zynk::message::SendError> {
    use crate::zynk::message::SendError;
    let error = |body: ErrorBody| SendError {
        code: body.code,
        message: body.message,
        context: None,
    };
    let deadline = timeout_ms
        .map(|ms| {
            Instant::now()
                .checked_add(Duration::from_millis(ms))
                .ok_or_else(|| {
                    error(ErrorBody {
                        code: "invalid_agent_timeout".into(),
                        message: "agent wait deadline is out of range".into(),
                    })
                })
        })
        .transpose()?;
    let mut first = true;
    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(error(ErrorBody {
                code: "timeout".into(),
                message: "timed out waiting for agent completion".into(),
            }));
        }
        let request = Request {
            id: "cli:agent:prompt".into(),
            method: Method::AgentGet(AgentTarget {
                target: terminal_id.into(),
            }),
        };
        let value = if first {
            first = false;
            super::send_request(&request)
        } else {
            super::send_request_unchecked(&request)
        }
        .map_err(|err| {
            if let Some(response) = super::protocol_guard::error_response(&err) {
                error(response.error.clone())
            } else {
                SendError {
                    code: if request_error_is_invalid_json(&err) {
                        "invalid_response"
                    } else {
                        "transport_failed"
                    }
                    .into(),
                    message: err.to_string(),
                    context: None,
                }
            }
        })?;
        if value.get("error").is_some() && value.get("result").is_some() {
            return Err(error(invalid_agent_response()));
        }
        let agent = decode_agent_response(&value, &request.id, false).map_err(error)?;
        if value["result"]["agent"]["state_change_seq"]
            .as_u64()
            .is_none()
        {
            return Err(error(invalid_agent_response()));
        }
        if agent.terminal_id != terminal_id || agent.name.as_deref() != Some(name) {
            return Err(error(ErrorBody {
                code: "agent_name_not_found".into(),
                message: format!("named agent {name} changed terminal or name"),
            }));
        }
        let status_matches = agent_wait_status_matches(agent.agent_status, until)
            && agent.state_change_seq > baseline;
        if agent.agent_status == AgentStatus::Unknown && !status_matches {
            return Err(error(ErrorBody {
                code: "agent_not_running".into(),
                message: format!("agent {name} is not running"),
            }));
        }
        if status_matches {
            return Ok(agent);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn agent_prompt(args: &[String]) -> std::io::Result<i32> {
    use crate::zynk::message::{
        new_message_id, now_rfc3339, resolve_source, resolve_target, Proof, SendCommand, SendError,
        SendOutcome, TargetResolution, TraceSpec,
    };
    use crate::zynk::persistence::{
        append_delivery_event, attach_to_outcome, begin_send_attempt, failed_event_payload,
        transport_effect_context, DeliveryEventInput, DeliveryEventType, SendAttempt,
    };
    let args = match parse_prompt_args(args) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };
    let send = |request: Request| super::send_request(&request);
    let from = resolve_source(crate::config::env_first(&["ZYNK_PANE_ID"]), send);
    let (to, resolution) = resolve_target(&args.name, send);
    let message_id = new_message_id();
    let failure = |error| {
        SendOutcome::failed(
            SendCommand::AgentPrompt,
            message_id.clone(),
            from.clone(),
            to.clone(),
            resolution,
            args.message_type.clone(),
            error,
        )
    };
    let resolution_error = match resolution {
        TargetResolution::Resolved => None,
        TargetResolution::NotFound => Some((
            "target_not_found",
            format!("no agent resolves the target '{}'", args.name),
        )),
        TargetResolution::Ambiguous => Some((
            "agent_target_ambiguous",
            format!("the target '{}' matches more than one agent", args.name),
        )),
        TargetResolution::Unknown => Some((
            "transport_failed",
            format!("could not reach zynk to resolve the target '{}'", args.name),
        )),
    };
    if let Some((code, message)) = resolution_error {
        println!(
            "{}",
            failure(SendError {
                code: code.into(),
                message,
                context: None
            })
            .to_json()
        );
        return Ok(1);
    }
    let Some(terminal_id) = to.terminal_id.as_deref().filter(|id| !id.is_empty()) else {
        println!(
            "{}",
            failure(SendError {
                code: "transport_failed".into(),
                message: "resolved agent has no terminal_id".into(),
                context: None
            })
            .to_json()
        );
        return Ok(1);
    };
    let trace_id = match args.trace.as_ref() {
        Some(TraceSpec::Inherit) => {
            match crate::zynk::persistence::resolve_parent_trace_id(&from, &to) {
                Ok(Some(trace)) => Some(trace),
                Ok(None) => {
                    eprintln!("note: --trace inherit found no parent trace; sending without trace");
                    None
                }
                Err(err) => {
                    eprintln!("note: --trace inherit could not read the parent trace ({}); sending without trace", err.code);
                    None
                }
            }
        }
        Some(TraceSpec::Explicit(trace)) => Some(trace.clone()),
        None => None,
    };
    let record = match begin_send_attempt(SendAttempt {
        command: SendCommand::AgentPrompt,
        message_id: &message_id,
        target_arg: &args.name,
        from: &from,
        to: &to,
        message_type: args.message_type.as_deref(),
        body: &args.text,
        created_at: &now_rfc3339(),
        trace_id,
    }) {
        Ok(record) => record,
        Err(err) => {
            println!(
                "{}",
                failure(SendError {
                    code: err.code.into(),
                    message: err.message,
                    context: None
                })
                .to_json()
            );
            return Ok(1);
        }
    };
    let text = if crate::zynk::header::is_agent_target(&to) {
        crate::zynk::header::prepend_header(
            &crate::zynk::header::render_header(
                &from,
                &to,
                &record,
                args.message_type.as_deref(),
                crate::zynk::header::resolve_header_options(),
                crate::zynk::header::display_home().as_deref(),
            ),
            &args.text,
        )
    } else {
        args.text.clone()
    };
    let response = super::send_request(&Request {
        id: "cli:agent:prompt".into(),
        method: Method::AgentPrompt(AgentPromptParams {
            target: args.name.clone(),
            text,
            expected_terminal_id: Some(terminal_id.into()),
            wait: None,
        }),
    });
    let baseline = match response
        .map_err(|err| {
            let invalid_json = request_error_is_invalid_json(&err);
            SendError {
                code: if invalid_json {
                    "invalid_response"
                } else {
                    "transport_failed"
                }
                .into(),
                message: err.to_string(),
                context: Some(transport_effect_context(
                    "submission_unverified",
                    if invalid_json {
                        "agent.prompt response was not valid JSON"
                    } else {
                        "agent.prompt response was not received"
                    }
                    .into(),
                )),
            }
        })
        .and_then(|value| prompt_response_baseline(value, &args.name, terminal_id))
    {
        Ok(baseline) => baseline,
        Err(mut error) => {
            let uncertain = error.context.is_some();
            if let Err(err) = append_delivery_event(DeliveryEventInput {
                message_id: &record.message_id,
                event_type: DeliveryEventType::Failed,
                proof_source: "agent.prompt",
                timestamp: &now_rfc3339(),
                payload: failed_event_payload(&error.message),
            }) {
                error = SendError {
                    code: "delivery_event_persist_failed".into(),
                    message: err.message,
                    context: Some(transport_effect_context(
                        if uncertain {
                            "submission_unverified"
                        } else {
                            "refused_unrecorded"
                        },
                        err.code.into(),
                    )),
                };
            }
            let mut outcome = failure(error);
            if uncertain {
                outcome.next = "submission is unverified; do not resubmit automatically".into();
            }
            println!("{}", attach_to_outcome(outcome, &record).to_json());
            return Ok(1);
        }
    };
    let submitted_at = now_rfc3339();
    if let Err(err) = append_delivery_event(DeliveryEventInput {
        message_id: &record.message_id,
        event_type: DeliveryEventType::Submitted,
        proof_source: "agent.prompt",
        timestamp: &submitted_at,
        payload: serde_json::json!({"terminal_id":terminal_id}),
    }) {
        let mut outcome = failure(SendError {
            code: "delivery_event_persist_failed".into(),
            message: err.message,
            context: Some(transport_effect_context(
                "submitted_unrecorded",
                err.code.into(),
            )),
        });
        outcome.next = "submission occurred but could not be recorded; do not resubmit".into();
        println!("{}", attach_to_outcome(outcome, &record).to_json());
        return Ok(1);
    }
    let mut outcome = attach_to_outcome(
        SendOutcome::ok(
            SendCommand::AgentPrompt,
            record.message_id.clone(),
            from.clone(),
            to.clone(),
            resolution,
            args.message_type.clone(),
            Proof {
                proof_source: "agent.prompt",
            },
            submitted_at,
        ),
        &record,
    );
    let mut exit = 0;
    let wait = if args.wait {
        Some(
            match wait_after_prompt(
                terminal_id,
                &args.name,
                baseline,
                &args.until,
                args.timeout_ms,
            ) {
                Ok(agent) => AgentPromptWaitOutcome::Ok {
                    agent: Box::new(agent),
                },
                Err(mut error) => {
                    let effect = match error.code.as_str() {
                        "timeout" => "submitted_wait_timeout",
                        "transport_failed" => "submitted_wait_transport_failed",
                        "protocol_mismatch" => "submitted_wait_protocol_mismatch",
                        "agent_name_not_found" => "submitted_wait_name_lost",
                        "agent_not_found" => "submitted_wait_terminal_lost",
                        "agent_not_running" => "submitted_wait_not_running",
                        "invalid_response" => "submitted_wait_invalid_response",
                        _ => "submitted_wait_refused",
                    };
                    error.context = Some(transport_effect_context(effect, error.message.clone()));
                    outcome.next =
                        "submission is recorded but waiting failed; do not resubmit".into();
                    exit = 3;
                    AgentPromptWaitOutcome::Failed { error }
                }
            },
        )
    } else {
        None
    };
    println!(
        "{}",
        serde_json::to_string(&AgentPromptOutcome { outcome, wait })
            .map_err(std::io::Error::other)?
    );
    Ok(exit)
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

fn agent_send_keys(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: zynk agent send-keys <target> <key> [key ...]");
        return Ok(2);
    }

    super::send_ok_request(Method::AgentSendKeys(AgentSendKeysParams {
        target: args[0].clone(),
        keys: args[1..].to_vec(),
    }))
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

fn print_agent_help() {
    eprintln!("zynk agent commands:");
    eprintln!("  zynk agent list");
    eprintln!("  zynk agent get <target>");
    eprintln!("  zynk agent read <target> [--source visible|recent|recent-unwrapped|detection] [--lines N] [--format text|ansi] [--ansi]");
    eprintln!("  zynk agent send <target> [--type T] [--] <text>");
    eprintln!("  zynk agent send-keys <target> <key> [key ...]");
    eprintln!("  zynk agent prompt <name> [--type T] [--trace ID|inherit] [--wait] [--until STATUS]... [--timeout MS] [--] <text>");
    eprintln!("    Submit only when ready; --wait observes later idle/done/blocked. Exit 3 means submitted but waiting failed: do not resubmit.");
    eprintln!("  zynk agent rename <target> <name>|--clear");
    eprintln!("  zynk agent focus <target>");
    eprintln!("  zynk agent wait <name> [--until STATUS]... [--timeout MS]");
    eprintln!("    Completes on idle, done or blocked, including Pending idle; use agent start for launch readiness.");
    eprintln!("  zynk agent attach <target> [--takeover]");
    eprintln!("  zynk agent start <name> --kind KIND --pane ID [--timeout MS] [-- <args...>]");
    eprintln!("    Waits for interactive readiness; timeout releases the reservation but keeps the label and does not prove the command did not run.");
    eprintln!("  zynk agent explain <target> [--json|--verbose]");
    eprintln!("  zynk agent explain --file PATH --agent LABEL [--json|--verbose]");
    eprintln!("  targets accept terminal ids, unique agent names, detected/reported agent labels, and legacy pane ids");
    eprintln!(
        "  agent send writes literal text; use pane run when you want command text plus Enter"
    );
}
