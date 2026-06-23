use crate::api::schema::{
    EmptyParams, Method, Request, WorkspaceCreateParams, WorkspaceRenameParams, WorkspaceTarget,
};

pub(super) fn run_workspace_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_workspace_help();
        return Ok(2);
    };

    if crate::cli::leaf_help_requested(args) {
        print_workspace_help();
        return Ok(0);
    }

    match subcommand {
        "list" => workspace_list(&args[1..]),
        "create" => workspace_create(&args[1..]),
        "get" => workspace_get(&args[1..]),
        "focus" => workspace_focus(&args[1..]),
        "rename" => workspace_rename(&args[1..]),
        "close" => workspace_close(&args[1..]),
        "help" | "--help" | "-h" => {
            print_workspace_help();
            Ok(0)
        }
        _ => {
            print_workspace_help();
            Ok(2)
        }
    }
}

fn workspace_list(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: zynk workspace list");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:workspace:list".into(),
        method: Method::WorkspaceList(EmptyParams::default()),
    })?)
}

fn workspace_create(args: &[String]) -> std::io::Result<i32> {
    let mut cwd = None;
    let mut focus = false;
    let mut label = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                label = Some(value.clone());
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
        id: "cli:workspace:create".into(),
        method: Method::WorkspaceCreate(WorkspaceCreateParams { cwd, focus, label }),
    })?)
}

fn workspace_get(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_workspace_id) = args.first() else {
        eprintln!("usage: zynk workspace get <workspace_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: zynk workspace get <workspace_id>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:workspace:get".into(),
        method: Method::WorkspaceGet(WorkspaceTarget {
            workspace_id: super::normalize_workspace_id(raw_workspace_id),
        }),
    })?)
}

fn workspace_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_workspace_id) = args.first() else {
        eprintln!("usage: zynk workspace focus <workspace_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: zynk workspace focus <workspace_id>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:workspace:focus".into(),
        method: Method::WorkspaceFocus(WorkspaceTarget {
            workspace_id: super::normalize_workspace_id(raw_workspace_id),
        }),
    })?)
}

fn workspace_rename(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: zynk workspace rename <workspace_id> <label>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:workspace:rename".into(),
        method: Method::WorkspaceRename(WorkspaceRenameParams {
            workspace_id: super::normalize_workspace_id(&args[0]),
            label: args[1..].join(" "),
        }),
    })?)
}

fn workspace_close(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_workspace_id) = args.first() else {
        eprintln!("usage: zynk workspace close <workspace_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: zynk workspace close <workspace_id>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:workspace:close".into(),
        method: Method::WorkspaceClose(WorkspaceTarget {
            workspace_id: super::normalize_workspace_id(raw_workspace_id),
        }),
    })?)
}

fn print_workspace_help() {
    eprintln!("zynk workspace commands:");
    eprintln!("  zynk workspace list");
    eprintln!("  zynk workspace create [--cwd PATH] [--label TEXT] [--focus] [--no-focus]");
    eprintln!("  zynk workspace get <workspace_id>");
    eprintln!("  zynk workspace focus <workspace_id>");
    eprintln!("  zynk workspace rename <workspace_id> <label>");
    eprintln!("  zynk workspace close <workspace_id>");
}
