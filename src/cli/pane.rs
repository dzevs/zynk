use crate::api::schema::{
    Method, PaneDirection, PaneEdgesParams, PaneFocusDirectionParams, PaneLayoutParams,
    PaneListParams, PaneMoveDestination, PaneMoveParams, PaneNeighborParams, PaneReadParams,
    PaneReleaseAgentParams, PaneRenameParams, PaneReportAgentParams, PaneReportAgentSessionParams,
    PaneReportMetadataParams, PaneResizeParams, PaneSendInputParams, PaneSendKeysParams,
    PaneSendTextParams, PaneSplitParams, PaneSwapParams, PaneTarget, PaneZoomMode, PaneZoomParams,
    ReadFormat, ReadSource, Request, SplitDirection,
};

pub(super) fn run_pane_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_pane_help();
        return Ok(2);
    };

    // `zynk pane <leaf> --help` / `-h` (exact position) -> group help. Safe: never
    // fires for a `--`-delimited payload like `pane run w1:p1 -- --help` (>2 args).
    if crate::cli::leaf_help_requested(args) {
        print_pane_help();
        return Ok(0);
    }

    match subcommand {
        "list" => pane_list(&args[1..]),
        "get" => pane_get(&args[1..]),
        "layout" => pane_layout(&args[1..]),
        "neighbor" => pane_neighbor(&args[1..]),
        "edges" => pane_edges(&args[1..]),
        "focus" => pane_focus(&args[1..]),
        "resize" => pane_resize(&args[1..]),
        "zoom" => pane_zoom(&args[1..]),
        "read" => pane_read(&args[1..]),
        "rename" => pane_rename(&args[1..]),
        "split" => pane_split(&args[1..]),
        "swap" => pane_swap(&args[1..]),
        "move" => pane_move(&args[1..]),
        "close" => pane_close(&args[1..]),
        "send-text" => pane_send_text(&args[1..]),
        "send-keys" => pane_send_keys(&args[1..]),
        "report-agent" => pane_report_agent(&args[1..]),
        "report-agent-session" => pane_report_agent_session(&args[1..]),
        "release-agent" => pane_release_agent(&args[1..]),
        "report-metadata" => pane_report_metadata(&args[1..]),
        "run" => pane_run(&args[1..]),
        "help" | "--help" | "-h" => {
            print_pane_help();
            Ok(0)
        }
        _ => {
            print_pane_help();
            Ok(2)
        }
    }
}

fn pane_list(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:list".into(),
        method: Method::PaneList(PaneListParams { workspace_id }),
    })?)
}

fn pane_get(args: &[String]) -> std::io::Result<i32> {
    if args.len() == 2 && crate::cli::is_help_flag(&args[1]) {
        eprintln!("usage: zynk pane get <pane_id>");
        return Ok(0);
    }
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: zynk pane get <pane_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: zynk pane get <pane_id>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:get".into(),
        method: Method::PaneGet(PaneTarget {
            pane_id: super::normalize_pane_id(raw_pane_id),
        }),
    })?)
}

fn pane_layout(args: &[String]) -> std::io::Result<i32> {
    let pane_id = match parse_optional_current_pane_args(args) {
        Ok(pane_id) => pane_id,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:layout".into(),
        method: Method::PaneLayout(PaneLayoutParams { pane_id }),
    })?)
}

fn pane_edges(args: &[String]) -> std::io::Result<i32> {
    let pane_id = match parse_optional_current_pane_args(args) {
        Ok(pane_id) => pane_id,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:edges".into(),
        method: Method::PaneEdges(PaneEdgesParams { pane_id }),
    })?)
}

fn pane_neighbor(args: &[String]) -> std::io::Result<i32> {
    let params = match parse_pane_neighbor_args(args) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:neighbor".into(),
        method: Method::PaneNeighbor(params),
    })?)
}

fn pane_focus(args: &[String]) -> std::io::Result<i32> {
    let params = match parse_pane_focus_args(args) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:focus".into(),
        method: Method::PaneFocusDirection(params),
    })?)
}

fn pane_resize(args: &[String]) -> std::io::Result<i32> {
    let params = match parse_pane_resize_args(args) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:resize".into(),
        method: Method::PaneResize(params),
    })?)
}

fn parse_optional_current_pane_args(args: &[String]) -> Result<Option<String>, String> {
    let mut pane_id = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--pane" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --pane".into());
                };
                pane_id = Some(super::normalize_pane_id(value));
                index += 2;
            }
            "--current" => {
                pane_id = None;
                index += 1;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok(pane_id)
}

fn parse_pane_neighbor_args(args: &[String]) -> Result<PaneNeighborParams, String> {
    let mut pane_id = None;
    let mut direction = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--pane" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --pane".into());
                };
                pane_id = Some(super::normalize_pane_id(value));
                index += 2;
            }
            "--current" => {
                pane_id = None;
                index += 1;
            }
            "--direction" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --direction".into());
                };
                direction = Some(parse_pane_direction(value)?);
                index += 2;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }

    let Some(direction) = direction else {
        return Err(
            "usage: zynk pane neighbor --direction left|right|up|down [--pane ID|--current]".into(),
        );
    };

    Ok(PaneNeighborParams { pane_id, direction })
}

fn parse_pane_focus_args(args: &[String]) -> Result<PaneFocusDirectionParams, String> {
    let params = parse_pane_neighbor_args(args).map_err(|_| {
        "usage: zynk pane focus --direction left|right|up|down [--pane ID|--current]".to_string()
    })?;
    Ok(PaneFocusDirectionParams {
        pane_id: params.pane_id,
        direction: params.direction,
    })
}

fn parse_pane_resize_args(args: &[String]) -> Result<PaneResizeParams, String> {
    let mut pane_id = None;
    let mut direction = None;
    let mut amount = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--pane" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --pane".into());
                };
                pane_id = Some(super::normalize_pane_id(value));
                index += 2;
            }
            "--current" => {
                pane_id = None;
                index += 1;
            }
            "--direction" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --direction".into());
                };
                direction = Some(parse_pane_direction(value)?);
                index += 2;
            }
            "--amount" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --amount".into());
                };
                let parsed = value
                    .parse::<f32>()
                    .map_err(|_| format!("invalid amount: {value}"))?;
                if !parsed.is_finite() {
                    return Err(format!("invalid amount: {value}"));
                }
                amount = Some(parsed);
                index += 2;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }

    let Some(direction) = direction else {
        return Err(
            "usage: zynk pane resize --direction left|right|up|down [--amount FLOAT] [--pane ID|--current]"
                .into(),
        );
    };

    Ok(PaneResizeParams {
        pane_id,
        direction,
        amount,
    })
}

fn pane_zoom(args: &[String]) -> std::io::Result<i32> {
    let params = match parse_pane_zoom_args(args) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:zoom".into(),
        method: Method::PaneZoom(params),
    })?)
}

fn parse_pane_zoom_args(args: &[String]) -> Result<PaneZoomParams, String> {
    let mut pane_id = None;
    let mut mode = PaneZoomMode::Toggle;
    let mut mode_seen = false;

    let mut index = 0;
    if args
        .first()
        .is_some_and(|arg| !arg.as_str().starts_with("--"))
    {
        pane_id = args.first().map(|arg| super::normalize_pane_id(arg));
        index = 1;
    }
    while index < args.len() {
        match args[index].as_str() {
            "--pane" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --pane".into());
                };
                pane_id = Some(super::normalize_pane_id(value));
                index += 2;
            }
            "--current" => {
                pane_id = None;
                index += 1;
            }
            "--toggle" => {
                if mode_seen {
                    return Err("provide only one of --toggle, --on, or --off".into());
                }
                mode = PaneZoomMode::Toggle;
                mode_seen = true;
                index += 1;
            }
            "--on" => {
                if mode_seen {
                    return Err("provide only one of --toggle, --on, or --off".into());
                }
                mode = PaneZoomMode::On;
                mode_seen = true;
                index += 1;
            }
            "--off" => {
                if mode_seen {
                    return Err("provide only one of --toggle, --on, or --off".into());
                }
                mode = PaneZoomMode::Off;
                mode_seen = true;
                index += 1;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }

    Ok(PaneZoomParams { pane_id, mode })
}

fn pane_rename(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: zynk pane rename <pane_id> <label>|--clear");
        return Ok(2);
    };
    if args.len() < 2 {
        eprintln!("usage: zynk pane rename <pane_id> <label>|--clear");
        return Ok(2);
    }
    let label = if args.len() == 2 && args[1] == "--clear" {
        None
    } else {
        Some(args[1..].join(" "))
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:rename".into(),
        method: Method::PaneRename(PaneRenameParams {
            pane_id: super::normalize_pane_id(raw_pane_id),
            label,
        }),
    })?)
}

fn pane_read(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: zynk pane read <pane_id> [--source visible|recent|recent-unwrapped|detection] [--lines N] [--format text|ansi] [--ansi]");
        return Ok(2);
    };

    let pane_id = super::normalize_pane_id(raw_pane_id);
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
                index += 2;
            }
            "--ansi" => {
                format = ReadFormat::Ansi;
                index += 1;
            }
            "--raw" => {
                format = ReadFormat::Ansi;
                strip_ansi = false;
                index += 1;
            }
            // `zynk pane read <id> --help` -> command help (read takes no body, so a
            // help flag in option position is unambiguous; a `--help` consumed as a
            // flag value above still errors as a bad value, never reaching here).
            other if crate::cli::is_help_flag(other) => {
                eprintln!("usage: zynk pane read <pane_id> [--source visible|recent|recent-unwrapped|detection] [--lines N] [--format text|ansi] [--ansi]");
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                eprintln!("run `zynk pane read --help` for command help");
                return Ok(2);
            }
        }
    }

    let response = super::send_request(&Request {
        id: "cli:pane:read".into(),
        method: Method::PaneRead(PaneReadParams {
            pane_id,
            source,
            lines,
            format,
            strip_ansi,
        }),
    })?;

    if let Some(error) = response.get("error") {
        eprintln!("{}", serde_json::to_string(error).unwrap());
        return Ok(1);
    }

    if let Some(text) = response["result"]["read"]["text"].as_str() {
        print!("{text}");
    }
    Ok(0)
}

fn pane_split(args: &[String]) -> std::io::Result<i32> {
    let params = match parse_pane_split_args(args) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:split".into(),
        method: Method::PaneSplit(params),
    })?)
}

fn parse_pane_split_args(args: &[String]) -> Result<PaneSplitParams, String> {
    let mut pane_id = None;
    let mut direction = None;
    let mut ratio = None;
    let mut cwd = None;
    let mut focus = false;

    let mut index = 0;
    if args
        .first()
        .is_some_and(|arg| !arg.as_str().starts_with("--"))
    {
        pane_id = args.first().map(|arg| super::normalize_pane_id(arg));
        index = 1;
    }
    while index < args.len() {
        match args[index].as_str() {
            "--pane" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --pane".into());
                };
                pane_id = Some(super::normalize_pane_id(value));
                index += 2;
            }
            "--current" => {
                pane_id = None;
                index += 1;
            }
            "--direction" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --direction".into());
                };
                direction =
                    Some(super::parse_split_direction(value).map_err(|err| err.to_string())?);
                index += 2;
            }
            "--ratio" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --ratio".into());
                };
                let parsed = value
                    .parse::<f32>()
                    .map_err(|_| format!("invalid ratio: {value}"))?;
                if !parsed.is_finite() {
                    return Err(format!("invalid ratio: {value}"));
                }
                ratio = Some(parsed);
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --cwd".into());
                };
                cwd = Some(value.clone());
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
            other => return Err(format!("unknown option: {other}")),
        }
    }

    let Some(direction) = direction else {
        return Err(
            "usage: zynk pane split [<pane_id>|--pane ID|--current] --direction right|down [--ratio FLOAT] [--cwd PATH] [--focus] [--no-focus]"
                .into(),
        );
    };

    Ok(PaneSplitParams {
        workspace_id: None,
        target_pane_id: pane_id,
        direction,
        ratio,
        cwd,
        focus,
    })
}

fn pane_swap(args: &[String]) -> std::io::Result<i32> {
    let params = match parse_pane_swap_args(args) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:swap".into(),
        method: Method::PaneSwap(params),
    })?)
}

fn pane_move(args: &[String]) -> std::io::Result<i32> {
    let params = match parse_pane_move_args(args) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:move".into(),
        method: Method::PaneMove(params),
    })?)
}

fn parse_pane_move_args(args: &[String]) -> Result<PaneMoveParams, String> {
    let Some(raw_pane_id) = args.first() else {
        return Err(pane_move_usage());
    };
    if raw_pane_id.starts_with('-') {
        return Err(pane_move_usage());
    }

    let pane_id = super::normalize_pane_id(raw_pane_id);
    let mut tab_id = None;
    let mut new_tab = false;
    let mut new_workspace = false;
    let mut workspace_id = None;
    let mut target_pane_id = None;
    let mut split = None;
    let mut ratio = None;
    let mut label = None;
    let mut tab_label = None;
    let mut focus = true;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--tab" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --tab".into());
                };
                tab_id = Some(super::normalize_tab_id(value));
                index += 2;
            }
            "--new-tab" => {
                new_tab = true;
                index += 1;
            }
            "--new-workspace" => {
                new_workspace = true;
                index += 1;
            }
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --workspace".into());
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--target-pane" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --target-pane".into());
                };
                target_pane_id = Some(super::normalize_pane_id(value));
                index += 2;
            }
            "--split" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --split".into());
                };
                split = Some(parse_split_direction(value)?);
                index += 2;
            }
            "--ratio" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --ratio".into());
                };
                let parsed = value
                    .parse::<f32>()
                    .map_err(|_| format!("invalid ratio: {value}"))?;
                if !parsed.is_finite() {
                    return Err(format!("invalid ratio: {value}"));
                }
                ratio = Some(parsed);
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --label".into());
                };
                label = Some(value.clone());
                index += 2;
            }
            "--tab-label" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --tab-label".into());
                };
                tab_label = Some(value.clone());
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
            other => return Err(format!("unknown option: {other}")),
        }
    }

    let destination_count =
        usize::from(tab_id.is_some()) + usize::from(new_tab) + usize::from(new_workspace);
    if destination_count != 1 {
        return Err(pane_move_usage());
    }

    let destination = if let Some(tab_id) = tab_id {
        let Some(split) = split else {
            return Err(pane_move_usage());
        };
        if workspace_id.is_some()
            || new_tab
            || new_workspace
            || label.is_some()
            || tab_label.is_some()
        {
            return Err(pane_move_usage());
        }
        PaneMoveDestination::Tab {
            tab_id,
            target_pane_id,
            split,
            ratio,
        }
    } else if new_tab {
        if split.is_some() || target_pane_id.is_some() || new_workspace || tab_label.is_some() {
            return Err(pane_move_usage());
        }
        PaneMoveDestination::NewTab {
            workspace_id,
            label,
        }
    } else {
        if split.is_some() || target_pane_id.is_some() || workspace_id.is_some() || new_tab {
            return Err(pane_move_usage());
        }
        PaneMoveDestination::NewWorkspace { label, tab_label }
    };

    Ok(PaneMoveParams {
        pane_id,
        destination,
        focus,
    })
}

fn pane_move_usage() -> String {
    "usage: zynk pane move <pane_id> --tab <tab_id> --split right|down [--target-pane ID] [--ratio FLOAT] [--focus|--no-focus]\n       zynk pane move <pane_id> --new-tab [--workspace ID] [--label TEXT] [--focus|--no-focus]\n       zynk pane move <pane_id> --new-workspace [--label TEXT] [--tab-label TEXT] [--focus|--no-focus]"
        .into()
}

fn parse_pane_swap_args(args: &[String]) -> Result<PaneSwapParams, String> {
    let mut pane_id = None;
    let mut direction = None;
    let mut source_pane_id = None;
    let mut target_pane_id = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--pane" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --pane".into());
                };
                pane_id = Some(super::normalize_pane_id(value));
                index += 2;
            }
            "--current" => {
                pane_id = None;
                index += 1;
            }
            "--direction" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --direction".into());
                };
                direction = Some(parse_pane_direction(value)?);
                index += 2;
            }
            "--source-pane" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --source-pane".into());
                };
                source_pane_id = Some(super::normalize_pane_id(value));
                index += 2;
            }
            "--target-pane" => {
                let Some(value) = args.get(index + 1) else {
                    return Err("missing value for --target-pane".into());
                };
                target_pane_id = Some(super::normalize_pane_id(value));
                index += 2;
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }

    let directional = direction.is_some();
    let explicit = source_pane_id.is_some() || target_pane_id.is_some();
    match (directional, explicit) {
        (true, false) => Ok(PaneSwapParams {
            pane_id,
            direction,
            ..PaneSwapParams::default()
        }),
        (false, true) if pane_id.is_none() && source_pane_id.is_some() && target_pane_id.is_some() => {
            Ok(PaneSwapParams {
                source_pane_id,
                target_pane_id,
                ..PaneSwapParams::default()
            })
        }
        _ => Err(
            "usage: zynk pane swap --direction left|right|up|down [--pane ID|--current]\n       zynk pane swap --source-pane ID --target-pane ID"
                .into(),
        ),
    }
}

fn parse_split_direction(value: &str) -> Result<SplitDirection, String> {
    match value {
        "right" => Ok(SplitDirection::Right),
        "down" => Ok(SplitDirection::Down),
        _ => Err(format!(
            "invalid split direction: {value} (expected right or down)"
        )),
    }
}

fn parse_pane_direction(value: &str) -> Result<PaneDirection, String> {
    match value {
        "left" => Ok(PaneDirection::Left),
        "right" => Ok(PaneDirection::Right),
        "up" => Ok(PaneDirection::Up),
        "down" => Ok(PaneDirection::Down),
        _ => Err(format!(
            "invalid pane direction: {value} (expected left, right, up, or down)"
        )),
    }
}

fn pane_close(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: zynk pane close <pane_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: zynk pane close <pane_id>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:pane:close".into(),
        method: Method::PaneClose(PaneTarget {
            pane_id: super::normalize_pane_id(raw_pane_id),
        }),
    })?)
}

fn pane_send_text(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!(
            "usage: zynk pane send-text <pane_id> [--type T] [--trace <id|inherit>] [--] <text>"
        );
        return Ok(2);
    }

    use crate::zynk::message::{
        new_message_id, now_rfc3339, parse_type_trace_and_text, resolve_source, Party, Proof,
        SendCommand, SendError, SendOutcome, TargetResolution,
    };
    use crate::zynk::persistence::{
        append_delivery_event, attach_to_outcome, empty_event_payload, failed_event_payload,
        transport_effect_context, DeliveryEventInput, DeliveryEventType, SendAttempt,
    };

    let pane_id = super::normalize_pane_id(&args[0]);
    let (message_type, trace_spec, text) = parse_type_trace_and_text(&args[1..]);

    let send = |request: Request| super::send_request(&request);
    let from = resolve_source(crate::config::env_first(&["ZYNK_PANE_ID"]), send);
    let (to, resolution) = resolve_pane_target_party(&pane_id);
    let message_id = new_message_id();

    // Gate the submit on resolution: do NOT write any bytes to a pane that is
    // unreachable (transport) or absent (server said not_found).
    match resolution {
        TargetResolution::Resolved => {}
        TargetResolution::Unknown => {
            let outcome = SendOutcome::failed(
                SendCommand::PaneSendText,
                message_id,
                from,
                Party::default(),
                TargetResolution::Unknown,
                message_type,
                SendError {
                    code: "transport_failed".into(),
                    message: format!("could not reach zynk to resolve pane '{pane_id}'"),
                    context: None,
                },
            );
            println!("{}", outcome.to_json());
            return Ok(1);
        }
        TargetResolution::NotFound | TargetResolution::Ambiguous => {
            let outcome = SendOutcome::failed(
                SendCommand::PaneSendText,
                message_id,
                from,
                Party::default(),
                TargetResolution::NotFound,
                message_type,
                SendError {
                    code: "pane_not_found".into(),
                    message: format!("no pane resolves '{pane_id}'"),
                    context: None,
                },
            );
            println!("{}", outcome.to_json());
            return Ok(1);
        }
    }

    // Feature #107 (IM1): resolve the per-message trace_id (validated explicit, or inherit
    // from the derived parent). See `native_send` for the full semantics.
    let trace_id = match resolve_trace_id(&trace_spec, &from, &to) {
        Ok(id) => id,
        Err((code, message)) => {
            let outcome = SendOutcome::failed(
                SendCommand::PaneSendText,
                message_id,
                from,
                to,
                TargetResolution::Resolved,
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
    };

    let created_at = now_rfc3339();
    let record = match crate::zynk::persistence::begin_send_attempt(SendAttempt {
        command: SendCommand::PaneSendText,
        message_id: &message_id,
        target_arg: &pane_id,
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
                SendCommand::PaneSendText,
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

    let result = super::send_request(&Request {
        id: "cli:pane:send-text".into(),
        method: Method::PaneSendText(PaneSendTextParams { pane_id, text }),
    });
    match result {
        Ok(response) if response.get("error").is_none() => {
            let drafted_at = now_rfc3339();
            let event_result = append_delivery_event(DeliveryEventInput {
                message_id: &record.message_id,
                event_type: DeliveryEventType::Drafted,
                proof_source: "pane.send_text",
                timestamp: &drafted_at,
                payload: empty_event_payload(),
            });
            match event_result {
                Ok(()) => {
                    let outcome = SendOutcome::ok(
                        SendCommand::PaneSendText,
                        record.message_id.clone(),
                        from,
                        to,
                        TargetResolution::Resolved,
                        message_type,
                        Proof {
                            proof_source: "pane.send_text",
                        },
                        drafted_at,
                    );
                    println!("{}", attach_to_outcome(outcome, &record).to_json());
                    Ok(0)
                }
                Err(err) => {
                    let outcome = SendOutcome::failed(
                        SendCommand::PaneSendText,
                        record.message_id.clone(),
                        from,
                        to,
                        TargetResolution::Resolved,
                        message_type,
                        SendError {
                            code: "delivery_event_persist_failed".into(),
                            message: err.message,
                            context: Some(transport_effect_context(
                                "drafted_unrecorded",
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
            // Resolution succeeded, but the send failed (the pane went away mid-flight
            // or the transport dropped). This is NO CONFIRMED DELIVERY — not a resolution
            // failure — so keep `target_resolution=resolved` and report `transport_failed`.
            let detail = match other {
                Ok(response) => serde_json::to_string(&response).unwrap_or_default(),
                Err(err) => err.to_string(),
            };
            let failed_at = now_rfc3339();
            let _ = append_delivery_event(DeliveryEventInput {
                message_id: &record.message_id,
                event_type: DeliveryEventType::Failed,
                proof_source: "pane.send_text",
                timestamp: &failed_at,
                payload: failed_event_payload(format!("pane.send_text failed: {detail}")),
            });
            let outcome = SendOutcome::failed(
                SendCommand::PaneSendText,
                record.message_id.clone(),
                from,
                to,
                TargetResolution::Resolved,
                message_type,
                SendError {
                    code: "transport_failed".into(),
                    message: format!("pane.send_text failed: {detail}"),
                    context: None,
                },
            );
            println!("{}", attach_to_outcome(outcome, &record).to_json());
            Ok(1)
        }
    }
}

/// Resolve a pane target into its [`Party`] metadata AND its
/// [`TargetResolution`] for the F4 response, distinguishing a RESOLUTION failure
/// from a TRANSPORT failure. A `pane.get` success → (`Party`, `Resolved`); a
/// SERVER `error.code` containing `pane_not_found` → (empty, `NotFound`) — the
/// server answered, so the pane truly does not exist; any OTHER server error or a
/// transport IO error (we never reached the server) → (empty, `Unknown`). The
/// caller MUST NOT submit on `NotFound`/`Unknown`.
fn resolve_pane_target_party(
    pane_id: &str,
) -> (
    crate::zynk::message::Party,
    crate::zynk::message::TargetResolution,
) {
    use crate::zynk::message::{party_from_pane_info, Party, TargetResolution};
    let request = Request {
        id: "cli:pane:resolve-target".into(),
        method: Method::PaneGet(PaneTarget {
            pane_id: pane_id.to_string(),
        }),
    };
    match super::send_request(&request) {
        Ok(value) => {
            if let Some(error) = value.get("error") {
                let code = error.get("code").and_then(|c| c.as_str()).unwrap_or("");
                let resolution = if code.contains("pane_not_found") {
                    TargetResolution::NotFound
                } else {
                    // Server answered with some OTHER error: we cannot prove the pane
                    // is absent, so do not claim `not_found` — treat as unknown.
                    TargetResolution::Unknown
                };
                (Party::default(), resolution)
            } else {
                (
                    party_from_pane_info(&value["result"]["pane"]),
                    TargetResolution::Resolved,
                )
            }
        }
        // Transport IO error: we never reached the server — resolution is UNKNOWN.
        Err(_) => (Party::default(), TargetResolution::Unknown),
    }
}

/// Feature #107 (IM1): resolve the per-message `trace_id` for the pane send commands.
///
/// - `--trace <id>` → validated up front; an INVALID explicit value returns
///   `Err((code, message))` so the caller emits an explicit F4 error and sends NOTHING.
/// - `--trace inherit` → copies the derived parent's trace; when the parent has no trace
///   (or there is no parent / the DB read fails), sends WITHOUT a trace and notes it on
///   stderr — NEVER an invented conversation trace, never an error.
/// - no `--trace` → `Ok(None)`.
fn resolve_trace_id(
    trace_spec: &Option<crate::zynk::message::TraceSpec>,
    from: &crate::zynk::message::Party,
    to: &crate::zynk::message::Party,
) -> Result<Option<String>, (&'static str, String)> {
    use crate::zynk::message::{validate_trace_id, TraceSpec};
    match trace_spec {
        Some(TraceSpec::Explicit(raw)) => validate_trace_id(raw).map(Some),
        Some(TraceSpec::Inherit) => {
            match crate::zynk::persistence::resolve_parent_trace_id(from, to) {
                Ok(Some(parent)) => Ok(Some(parent)),
                Ok(None) => {
                    eprintln!("note: --trace inherit found no parent trace; sending without trace");
                    Ok(None)
                }
                Err(err) => {
                    eprintln!(
                        "note: --trace inherit could not read the parent trace ({}); sending without trace",
                        err.code
                    );
                    Ok(None)
                }
            }
        }
        None => Ok(None),
    }
}

fn pane_send_keys(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: zynk pane send-keys <pane_id> <key> [key ...]");
        return Ok(2);
    }

    let pane_id = super::normalize_pane_id(&args[0]);
    let keys = args[1..].to_vec();
    super::send_ok_request(Method::PaneSendKeys(PaneSendKeysParams { pane_id, keys }))
}

fn pane_run(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!(
            "usage: zynk pane run <pane_id> [--type T] [--trace <id|inherit>] [--] <command>"
        );
        return Ok(2);
    }

    use crate::zynk::message::{
        new_message_id, now_rfc3339, parse_type_trace_and_text, resolve_source, Party, Proof,
        SendCommand, SendError, SendOutcome, TargetResolution,
    };
    use crate::zynk::persistence::{
        append_delivery_event, attach_to_outcome, empty_event_payload, failed_event_payload,
        transport_effect_context, DeliveryEventInput, DeliveryEventType, SendAttempt,
    };

    let pane_id = super::normalize_pane_id(&args[0]);
    let (message_type, trace_spec, text) = parse_type_trace_and_text(&args[1..]);

    let send = |request: Request| super::send_request(&request);
    let from = resolve_source(crate::config::env_first(&["ZYNK_PANE_ID"]), send);
    let (to, resolution) = resolve_pane_target_party(&pane_id);
    let message_id = new_message_id();

    // Gate the submit on resolution: do NOT submit to a pane that is unreachable
    // (transport) or absent (server said not_found).
    match resolution {
        TargetResolution::Resolved => {}
        TargetResolution::Unknown => {
            let outcome = SendOutcome::failed(
                SendCommand::PaneRun,
                message_id,
                from,
                Party::default(),
                TargetResolution::Unknown,
                message_type,
                SendError {
                    code: "transport_failed".into(),
                    message: format!("could not reach zynk to resolve pane '{pane_id}'"),
                    context: None,
                },
            );
            println!("{}", outcome.to_json());
            return Ok(1);
        }
        TargetResolution::NotFound | TargetResolution::Ambiguous => {
            let outcome = SendOutcome::failed(
                SendCommand::PaneRun,
                message_id,
                from,
                Party::default(),
                TargetResolution::NotFound,
                message_type,
                SendError {
                    code: "pane_not_found".into(),
                    message: format!("no pane resolves '{pane_id}'"),
                    context: None,
                },
            );
            println!("{}", outcome.to_json());
            return Ok(1);
        }
    }

    // Feature #107 (IM1): resolve the per-message trace_id (validated explicit, or inherit
    // from the derived parent). See `native_send` / `resolve_trace_id` for the semantics.
    let trace_id = match resolve_trace_id(&trace_spec, &from, &to) {
        Ok(id) => id,
        Err((code, message)) => {
            let outcome = SendOutcome::failed(
                SendCommand::PaneRun,
                message_id,
                from,
                to,
                TargetResolution::Resolved,
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
    };

    let created_at = now_rfc3339();
    let record = match crate::zynk::persistence::begin_send_attempt(SendAttempt {
        command: SendCommand::PaneRun,
        message_id: &message_id,
        target_arg: &pane_id,
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
                SendCommand::PaneRun,
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

    // zynk: PREPEND the agent-VISIBLE header to the delivered text for EVERY agent
    // target (claude/codex/pi alike — uniform, not an allowlist). The persisted
    // body/body_hash/FTS are already pure; the header rides only the wire text and is
    // awareness, NOT receipt proof (delivery_status stays `submitted`).
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
        id: "cli:pane:run".into(),
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
                        SendCommand::PaneRun,
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
                        SendCommand::PaneRun,
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
            // Resolution succeeded, but the submit failed. NO CONFIRMED DELIVERY (text
            // may have been written before the Enter, per panes.rs PaneSendInput) — not a
            // resolution failure, so keep `target_resolution=resolved`.
            let detail = match other {
                Ok(response) => serde_json::to_string(&response).unwrap_or_default(),
                Err(err) => err.to_string(),
            };
            let failed_at = now_rfc3339();
            let _ = append_delivery_event(DeliveryEventInput {
                message_id: &record.message_id,
                event_type: DeliveryEventType::Failed,
                proof_source: "pane.send_input",
                timestamp: &failed_at,
                payload: failed_event_payload(format!("pane.send_input failed: {detail}")),
            });
            let outcome = SendOutcome::failed(
                SendCommand::PaneRun,
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

fn pane_report_agent(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: zynk pane report-agent <pane_id> --source ID --agent LABEL --state idle|working|blocked|unknown [--message TEXT] [--custom-status TEXT] [--seq N] [--agent-session-id ID] [--agent-session-path PATH]");
        return Ok(2);
    };

    let pane_id = super::normalize_pane_id(raw_pane_id);
    let mut source = None;
    let mut agent = None;
    let mut state = None;
    let mut message = None;
    let mut custom_status = None;
    let mut seq = None;
    let mut agent_session_id = None;
    let mut agent_session_path = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = Some(value.clone());
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
            "--state" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --state");
                    return Ok(2);
                };
                state = Some(super::parse_pane_agent_state(value)?);
                index += 2;
            }
            "--message" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --message");
                    return Ok(2);
                };
                message = Some(value.clone());
                index += 2;
            }
            "--custom-status" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --custom-status");
                    return Ok(2);
                };
                custom_status = Some(value.clone());
                index += 2;
            }
            "--seq" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --seq");
                    return Ok(2);
                };
                seq = Some(super::parse_u64_flag("--seq", value)?);
                index += 2;
            }
            "--agent-session-id" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent-session-id");
                    return Ok(2);
                };
                agent_session_id = Some(value.clone());
                index += 2;
            }
            "--agent-session-path" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent-session-path");
                    return Ok(2);
                };
                agent_session_path = Some(value.clone());
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(source) = source.and_then(|source| {
        let source = source.trim().to_string();
        (!source.is_empty()).then_some(source)
    }) else {
        eprintln!("missing required --source");
        return Ok(2);
    };
    let Some(agent) = agent else {
        eprintln!("missing required --agent");
        return Ok(2);
    };
    let Some(state) = state else {
        eprintln!("missing required --state");
        return Ok(2);
    };

    super::send_ok_request(Method::PaneReportAgent(PaneReportAgentParams {
        pane_id,
        source,
        agent,
        state,
        message,
        custom_status,
        seq,
        agent_session_id,
        agent_session_path,
    }))
}

fn pane_report_agent_session(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: zynk pane report-agent-session <pane_id> --source ID --agent LABEL [--seq N] [--agent-session-id ID] [--agent-session-path PATH] [--session-start-source SOURCE]");
        return Ok(2);
    };

    let pane_id = super::normalize_pane_id(raw_pane_id);
    let mut source = None;
    let mut agent = None;
    let mut seq = None;
    let mut agent_session_id = None;
    let mut agent_session_path = None;
    let mut session_start_source = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = Some(value.clone());
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
            "--seq" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --seq");
                    return Ok(2);
                };
                seq = Some(super::parse_u64_flag("--seq", value)?);
                index += 2;
            }
            "--agent-session-id" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent-session-id");
                    return Ok(2);
                };
                agent_session_id = Some(value.clone());
                index += 2;
            }
            "--agent-session-path" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent-session-path");
                    return Ok(2);
                };
                agent_session_path = Some(value.clone());
                index += 2;
            }
            "--session-start-source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --session-start-source");
                    return Ok(2);
                };
                session_start_source = Some(value.clone());
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(source) = source.and_then(|source| {
        let source = source.trim().to_string();
        (!source.is_empty()).then_some(source)
    }) else {
        eprintln!("missing required --source");
        return Ok(2);
    };
    let Some(agent) = agent else {
        eprintln!("missing required --agent");
        return Ok(2);
    };

    super::send_ok_request(Method::PaneReportAgentSession(
        PaneReportAgentSessionParams {
            pane_id,
            source,
            agent,
            seq,
            agent_session_id,
            agent_session_path,
            session_start_source,
        },
    ))
}

fn pane_release_agent(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: zynk pane release-agent <pane_id> --source ID --agent LABEL [--seq N]");
        return Ok(2);
    };

    let pane_id = super::normalize_pane_id(raw_pane_id);
    let mut source = None;
    let mut agent = None;
    let mut seq = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = Some(value.clone());
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
            "--seq" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --seq");
                    return Ok(2);
                };
                seq = Some(super::parse_u64_flag("--seq", value)?);
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(source) = source.and_then(|source| {
        let source = source.trim().to_string();
        (!source.is_empty()).then_some(source)
    }) else {
        eprintln!("missing required --source");
        return Ok(2);
    };
    let Some(agent) = agent else {
        eprintln!("missing required --agent");
        return Ok(2);
    };

    super::send_ok_request(Method::PaneReleaseAgent(PaneReleaseAgentParams {
        pane_id,
        source,
        agent,
        seq,
    }))
}

fn pane_report_metadata(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_pane_id) = args.first() else {
        eprintln!("usage: zynk pane report-metadata <pane_id> --source ID [--agent LABEL] [--applies-to-source ID] [--title TEXT|--clear-title] [--display-agent TEXT|--clear-display-agent] [--custom-status TEXT|--clear-custom-status] [--state-label STATUS=TEXT] [--clear-state-labels] [--seq N] [--ttl-ms N]");
        return Ok(2);
    };

    let pane_id = super::normalize_pane_id(raw_pane_id);
    let mut source = None;
    let mut agent = None;
    let mut applies_to_source = None;
    let mut title = None;
    let mut display_agent = None;
    let mut custom_status = None;
    let mut state_labels = std::collections::HashMap::new();
    let mut clear_title = false;
    let mut clear_display_agent = false;
    let mut clear_custom_status = false;
    let mut clear_state_labels = false;
    let mut seq = None;
    let mut ttl_ms = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = Some(value.clone());
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
            "--applies-to-source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --applies-to-source");
                    return Ok(2);
                };
                applies_to_source = Some(value.clone());
                index += 2;
            }
            "--title" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --title");
                    return Ok(2);
                };
                title = Some(value.clone());
                index += 2;
            }
            "--clear-title" => {
                clear_title = true;
                index += 1;
            }
            "--display-agent" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --display-agent");
                    return Ok(2);
                };
                display_agent = Some(value.clone());
                index += 2;
            }
            "--clear-display-agent" => {
                clear_display_agent = true;
                index += 1;
            }
            "--custom-status" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --custom-status");
                    return Ok(2);
                };
                custom_status = Some(value.clone());
                index += 2;
            }
            "--clear-custom-status" => {
                clear_custom_status = true;
                index += 1;
            }
            "--state-label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --state-label");
                    return Ok(2);
                };
                let Some((status, label)) = value.split_once('=') else {
                    eprintln!("expected --state-label STATUS=TEXT");
                    return Ok(2);
                };
                let status = status.trim().to_ascii_lowercase();
                if !matches!(
                    status.as_str(),
                    "idle" | "working" | "blocked" | "done" | "unknown"
                ) {
                    eprintln!("unknown state label: {status}");
                    return Ok(2);
                }
                state_labels.insert(status, label.to_string());
                index += 2;
            }
            "--clear-state-labels" => {
                clear_state_labels = true;
                index += 1;
            }
            "--seq" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --seq");
                    return Ok(2);
                };
                seq = Some(super::parse_u64_flag("--seq", value)?);
                index += 2;
            }
            "--ttl-ms" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --ttl-ms");
                    return Ok(2);
                };
                ttl_ms = Some(super::parse_u64_flag("--ttl-ms", value)?);
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(source) = source.and_then(|source| {
        let source = source.trim().to_string();
        (!source.is_empty()).then_some(source)
    }) else {
        eprintln!("missing required --source");
        return Ok(2);
    };
    if applies_to_source
        .as_deref()
        .is_some_and(|source| source.trim().is_empty())
    {
        eprintln!("missing value for --applies-to-source");
        return Ok(2);
    }
    if title.is_some() && clear_title
        || display_agent.is_some() && clear_display_agent
        || custom_status.is_some() && clear_custom_status
        || !state_labels.is_empty() && clear_state_labels
    {
        eprintln!("cannot set and clear the same metadata field");
        return Ok(2);
    }
    if title.is_none()
        && display_agent.is_none()
        && custom_status.is_none()
        && state_labels.is_empty()
        && !clear_title
        && !clear_display_agent
        && !clear_custom_status
        && !clear_state_labels
    {
        eprintln!("missing metadata field to set or clear");
        return Ok(2);
    }

    super::send_ok_request(Method::PaneReportMetadata(PaneReportMetadataParams {
        pane_id,
        source,
        agent,
        applies_to_source,
        title,
        display_agent,
        custom_status,
        state_labels,
        clear_title,
        clear_display_agent,
        clear_custom_status,
        clear_state_labels,
        seq,
        ttl_ms,
    }))
}

fn print_pane_help() {
    eprintln!("zynk pane commands:");
    eprintln!("  zynk pane list [--workspace <workspace_id>]");
    eprintln!("  zynk pane get <pane_id>");
    eprintln!("  zynk pane layout [--pane ID|--current]");
    eprintln!("  zynk pane neighbor --direction left|right|up|down [--pane ID|--current]");
    eprintln!("  zynk pane edges [--pane ID|--current]");
    eprintln!("  zynk pane focus --direction left|right|up|down [--pane ID|--current]");
    eprintln!(
        "  zynk pane resize --direction left|right|up|down [--amount FLOAT] [--pane ID|--current]"
    );
    eprintln!("  zynk pane zoom [<pane_id>|--pane ID|--current] [--toggle|--on|--off]");
    eprintln!("  zynk pane rename <pane_id> <label>|--clear");
    eprintln!("  zynk pane read <pane_id> [--source visible|recent|recent-unwrapped|detection] [--lines N] [--format text|ansi] [--ansi]");
    eprintln!(
        "  zynk pane split [<pane_id>|--pane ID|--current] --direction right|down [--ratio FLOAT] [--cwd PATH] [--focus] [--no-focus]"
    );
    eprintln!("  zynk pane swap --direction left|right|up|down [--pane ID|--current]");
    eprintln!("  zynk pane swap --source-pane ID --target-pane ID");
    eprintln!("  zynk pane move <pane_id> --tab <tab_id> --split right|down [--target-pane ID] [--ratio FLOAT] [--focus|--no-focus]");
    eprintln!(
        "  zynk pane move <pane_id> --new-tab [--workspace ID] [--label TEXT] [--focus|--no-focus]"
    );
    eprintln!("  zynk pane move <pane_id> --new-workspace [--label TEXT] [--tab-label TEXT] [--focus|--no-focus]");
    eprintln!("  zynk pane close <pane_id>");
    eprintln!("  zynk pane send-text <pane_id> [--type T] [--trace <id|inherit>] [--] <text>");
    eprintln!("  zynk pane send-keys <pane_id> <key> [key ...]");
    eprintln!("  zynk pane report-agent <pane_id> --source ID --agent LABEL --state idle|working|blocked|unknown [--message TEXT] [--custom-status TEXT] [--seq N] [--agent-session-id ID] [--agent-session-path PATH]");
    eprintln!("  zynk pane report-agent-session <pane_id> --source ID --agent LABEL [--seq N] [--agent-session-id ID] [--agent-session-path PATH]");
    eprintln!("  zynk pane release-agent <pane_id> --source ID --agent LABEL [--seq N]");
    eprintln!("  zynk pane report-metadata <pane_id> --source ID [--agent LABEL] [--applies-to-source ID] [--title TEXT|--clear-title] [--display-agent TEXT|--clear-display-agent] [--custom-status TEXT|--clear-custom-status] [--state-label STATUS=TEXT] [--clear-state-labels] [--seq N] [--ttl-ms N]");
    eprintln!("  zynk pane run <pane_id> [--type T] [--trace <id|inherit>] [--] <command>");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn parse_pane_split_args_accepts_ratio() {
        let params = parse_pane_split_args(&args(&[
            "issue-1",
            "--direction",
            "right",
            "--ratio",
            "0.333",
        ]))
        .unwrap();

        assert_eq!(params.target_pane_id, Some("issue-1".into()));
        assert_eq!(params.direction, crate::api::schema::SplitDirection::Right);
        assert_eq!(params.ratio, Some(0.333));
    }

    #[test]
    fn parse_pane_split_args_accepts_current_target() {
        let params = parse_pane_split_args(&args(&["--direction", "down", "--current"])).unwrap();

        assert_eq!(params.target_pane_id, None);
        assert_eq!(params.direction, crate::api::schema::SplitDirection::Down);
    }

    #[test]
    fn parse_pane_split_args_accepts_pane_option() {
        let params =
            parse_pane_split_args(&args(&["--pane", "issue-2", "--direction", "right"])).unwrap();

        assert_eq!(params.target_pane_id, Some("issue-2".into()));
        assert_eq!(params.direction, crate::api::schema::SplitDirection::Right);
    }

    #[test]
    fn parse_pane_swap_args_accepts_directional_current() {
        let params = parse_pane_swap_args(&args(&["--direction", "right"])).unwrap();

        assert_eq!(params.pane_id, None);
        assert_eq!(params.direction, Some(PaneDirection::Right));
        assert_eq!(params.source_pane_id, None);
        assert_eq!(params.target_pane_id, None);
    }

    #[test]
    fn parse_pane_swap_args_accepts_explicit_source_and_target() {
        let params = parse_pane_swap_args(&args(&[
            "--source-pane",
            "issue-1",
            "--target-pane",
            "issue-2",
        ]))
        .unwrap();

        assert_eq!(params.direction, None);
        assert_eq!(params.source_pane_id, Some("issue-1".into()));
        assert_eq!(params.target_pane_id, Some("issue-2".into()));
    }

    #[test]
    fn parse_pane_swap_args_rejects_mixed_forms() {
        let err = parse_pane_swap_args(&args(&[
            "--direction",
            "left",
            "--source-pane",
            "issue-1",
            "--target-pane",
            "issue-2",
        ]))
        .unwrap_err();

        assert!(err.contains("usage: zynk pane swap"));
    }

    #[test]
    fn parse_pane_move_args_accepts_existing_tab_destination() {
        let params = parse_pane_move_args(&args(&[
            "issue-1",
            "--tab",
            "issue:2",
            "--split",
            "right",
            "--target-pane",
            "issue-3",
            "--ratio",
            "0.25",
            "--no-focus",
        ]))
        .unwrap();

        assert_eq!(params.pane_id, "issue-1");
        assert!(!params.focus);
        assert_eq!(
            params.destination,
            PaneMoveDestination::Tab {
                tab_id: "issue:2".into(),
                target_pane_id: Some("issue-3".into()),
                split: SplitDirection::Right,
                ratio: Some(0.25),
            }
        );
    }

    #[test]
    fn parse_pane_move_args_rejects_target_pane_without_tab() {
        let err =
            parse_pane_move_args(&args(&["issue-1", "--target-pane", "issue-2"])).unwrap_err();

        assert!(err.contains("usage: zynk pane move"));
    }

    #[test]
    fn parse_pane_move_args_rejects_non_finite_ratio() {
        let err = parse_pane_move_args(&args(&[
            "issue-1", "--tab", "issue:2", "--split", "right", "--ratio", "NaN",
        ]))
        .unwrap_err();

        assert!(err.contains("invalid ratio"));
    }

    #[test]
    fn parse_pane_zoom_args_defaults_to_current_toggle() {
        let params = parse_pane_zoom_args(&args(&[])).unwrap();

        assert_eq!(params.pane_id, None);
        assert_eq!(params.mode, PaneZoomMode::Toggle);
    }

    #[test]
    fn parse_pane_zoom_args_accepts_positional_pane_and_on() {
        let params = parse_pane_zoom_args(&args(&["issue-1", "--on"])).unwrap();

        assert_eq!(params.pane_id, Some("issue-1".into()));
        assert_eq!(params.mode, PaneZoomMode::On);
    }

    #[test]
    fn parse_pane_zoom_args_accepts_pane_option_and_off() {
        let params = parse_pane_zoom_args(&args(&["--pane", "issue-2", "--off"])).unwrap();

        assert_eq!(params.pane_id, Some("issue-2".into()));
        assert_eq!(params.mode, PaneZoomMode::Off);
    }

    #[test]
    fn parse_pane_zoom_args_rejects_multiple_modes() {
        let err = parse_pane_zoom_args(&args(&["--on", "--off"])).unwrap_err();

        assert!(err.contains("provide only one"));
    }

    #[test]
    fn parse_pane_neighbor_args_accepts_directional_current() {
        let params = parse_pane_neighbor_args(&args(&["--direction", "down"])).unwrap();

        assert_eq!(params.pane_id, None);
        assert_eq!(params.direction, PaneDirection::Down);
    }

    #[test]
    fn parse_optional_current_pane_args_accepts_explicit_pane() {
        let pane_id = parse_optional_current_pane_args(&args(&["--pane", "issue-2"])).unwrap();

        assert_eq!(pane_id, Some("issue-2".into()));
    }

    #[test]
    fn parse_pane_focus_args_accepts_directional_current() {
        let params = parse_pane_focus_args(&args(&["--direction", "up"])).unwrap();

        assert_eq!(params.pane_id, None);
        assert_eq!(params.direction, PaneDirection::Up);
    }

    #[test]
    fn parse_pane_resize_args_accepts_amount_and_pane() {
        let params = parse_pane_resize_args(&args(&[
            "--pane",
            "issue-2",
            "--direction",
            "left",
            "--amount",
            "0.125",
        ]))
        .unwrap();

        assert_eq!(params.pane_id, Some("issue-2".into()));
        assert_eq!(params.direction, PaneDirection::Left);
        assert_eq!(params.amount, Some(0.125));
    }
}
