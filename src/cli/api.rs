use crate::api::schema::export::protocol_schema_document;
use crate::api::schema::{EmptyParams, Method, Request};

pub(super) fn run_api_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("schema") => api_schema(&args[1..]),
        Some("snapshot") => api_snapshot(&args[1..]),
        Some("help" | "--help" | "-h") if args.len() == 1 => {
            print_api_help();
            Ok(0)
        }
        _ => {
            print_api_help();
            Ok(2)
        }
    }
}

fn api_schema(args: &[String]) -> std::io::Result<i32> {
    match args {
        [] => print!("{}", schema_summary_text()),
        [flag] if flag == "--json" => print!("{}", schema_json()?),
        [flag, path] if flag == "--output" => {
            std::fs::write(path, schema_json()?)?;
            println!("wrote API schema to {path}");
        }
        [flag] if flag == "--output" => {
            eprintln!("missing value for --output");
            return Ok(2);
        }
        [flag] if matches!(flag.as_str(), "help" | "--help" | "-h") => print_api_help(),
        [other] if other.starts_with('-') => {
            eprintln!("unknown option: {other}");
            return Ok(2);
        }
        _ => {
            print_api_help();
            return Ok(2);
        }
    }
    Ok(0)
}

fn api_snapshot(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: zynk api snapshot");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:api:snapshot".into(),
        method: Method::SessionSnapshot(EmptyParams::default()),
    })?)
}

fn schema_json() -> std::io::Result<String> {
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&protocol_schema_document())?
    ))
}

fn schema_summary_text() -> String {
    let value = protocol_schema_document();
    let schemas = value["schemas"]
        .as_object()
        .map(|schemas| {
            schemas
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    format!(
        "Zynk API schema\nprotocol: {}\nschema_version: {}\nschemas: {}\n\nUse `zynk api schema --json` to print the full schema.\nUse `zynk api schema --output PATH` to write it to a file.\n",
        value["protocol"], value["schema_version"], schemas,
    )
}

fn print_api_help() {
    eprintln!("usage: zynk api snapshot");
    eprintln!("usage: zynk api schema [--json | --output PATH]");
}
