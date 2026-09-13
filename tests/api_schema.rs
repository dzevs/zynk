mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct OfflineCli {
    base: PathBuf,
}

impl OfflineCli {
    fn new() -> Self {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = support::test_root().join(format!("schema-{}-{stamp}", std::process::id()));
        fs::create_dir(&base).unwrap();
        Self { base }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_zynk"))
            .args(args)
            .env_clear()
            .env("HOME", self.base.join("home"))
            .env("XDG_CONFIG_HOME", self.base.join("config"))
            .env("XDG_DATA_HOME", self.base.join("data"))
            .env("XDG_CACHE_HOME", self.base.join("cache"))
            .env("XDG_RUNTIME_DIR", self.base.join("runtime"))
            .env("ZYNK_HOME", self.base.join("db"))
            .env("ZYNK_SQLITE_HOME", self.base.join("sqlite"))
            .env("ZYNK_SOCKET_PATH", self.base.join("runtime/s.sock"))
            .env("ZYNK_CLIENT_SOCKET_PATH", self.base.join("runtime/c.sock"))
            .current_dir(&self.base)
            .output()
            .unwrap()
    }

    fn assert_no_runtime_created(&self) {
        for name in ["home", "config", "data", "cache", "runtime", "db", "sqlite"] {
            assert!(!self.base.join(name).exists(), "offline CLI created {name}");
        }
    }
}

impl Drop for OfflineCli {
    fn drop(&mut self) {
        support::cleanup_test_base(&self.base);
    }
}

fn successful(output: &Output) {
    assert!(
        output.status.success(),
        "status {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn api_schema_json_is_deterministic_and_needs_no_runtime() {
    let cli = OfflineCli::new();
    let first = cli.run(&["api", "schema", "--json"]);
    successful(&first);
    let document: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(document["title"], "Zynk API");
    assert!(first.stdout.ends_with(b"\n"));
    let second = cli.run(&["api", "schema", "--json"]);
    successful(&second);
    assert_eq!(first.stdout, second.stdout);
    assert!(first.stderr.is_empty());
    assert!(second.stderr.is_empty());
    cli.assert_no_runtime_created();
}

#[test]
fn api_schema_output_file_matches_json_and_preserves_io_errors() {
    let cli = OfflineCli::new();
    let json = cli.run(&["api", "schema", "--json"]);
    successful(&json);
    let written = cli.run(&["api", "schema", "--output", "schema.json"]);
    successful(&written);
    assert_eq!(fs::read(cli.base.join("schema.json")).unwrap(), json.stdout);
    let failed = cli.run(&["api", "schema", "--output", "missing/schema.json"]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(!failed.stderr.is_empty());
    assert!(!cli.base.join("missing").exists());
    cli.assert_no_runtime_created();
}

#[test]
fn api_schema_help_summary_and_argument_errors_are_offline() {
    let cli = OfflineCli::new();
    for args in [
        vec!["api", "--help"],
        vec!["api", "schema", "--help"],
        vec!["--help"],
    ] {
        let help = cli.run(&args);
        successful(&help);
        let combined = [help.stdout, help.stderr].concat();
        assert!(String::from_utf8_lossy(&combined).contains("zynk api"));
    }
    let summary = cli.run(&["api", "schema"]);
    successful(&summary);
    let text = String::from_utf8(summary.stdout).unwrap();
    assert!(text.starts_with("Zynk API schema\n"));
    assert!(text.contains("Use `zynk api schema --json`"));
    assert!(text.len() < 400);
    for args in [
        vec!["api"],
        vec!["api", "unknown"],
        vec!["api", "schema", "--unknown"],
        vec!["api", "schema", "--output"],
        vec!["api", "schema", "--json", "--output", "bad.json"],
        vec!["api", "schema", "--output", "bad.json", "--json"],
        vec!["api", "schema", "extra"],
    ] {
        let output = cli.run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
    assert!(!cli.base.join("bad.json").exists());
    cli.assert_no_runtime_created();
}
