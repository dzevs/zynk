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
        let base = support::test_root().join(format!("complete-{}-{stamp}", std::process::id()));
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
    assert!(output.stderr.is_empty());
}

#[test]
fn completion_generates_every_shell_deterministically_without_runtime() {
    let cli = OfflineCli::new();
    for shell in ["zsh", "bash", "elvish", "fish", "powershell"] {
        let first = cli.run(&["completion", shell]);
        successful(&first);
        let second = cli.run(&["completion", shell]);
        successful(&second);
        assert_eq!(first.stdout, second.stdout, "{shell}");
        let script = String::from_utf8(first.stdout).unwrap();
        assert!(script.contains("zynk"), "{shell}");
        assert!(script.contains("workspace"), "{shell}");
        assert!(script.len() > 100, "{shell}");
        let alias = cli.run(&["completions", shell]);
        successful(&alias);
        assert_eq!(alias.stdout, second.stdout, "{shell}");
        cli.assert_no_runtime_created();
    }
}

#[test]
fn completion_zsh_emits_space_separated_options() {
    let cli = OfflineCli::new();
    let output = cli.run(&["completion", "zsh"]);
    successful(&output);
    let script = String::from_utf8(output.stdout).unwrap();
    assert!(script.starts_with("#compdef zynk"));
    assert!(script.contains("--session["));
    assert!(!script.contains("--session=["));
    assert!(!script.contains("--workspace=["));
    cli.assert_no_runtime_created();
}

#[test]
fn completion_help_and_argument_errors_are_offline() {
    let cli = OfflineCli::new();
    for args in [
        vec!["completion", "--help"],
        vec!["completion", "-h"],
        vec!["--help"],
    ] {
        let output = cli.run(&args);
        assert!(output.status.success());
        let combined = [output.stdout, output.stderr].concat();
        assert!(String::from_utf8_lossy(&combined).contains("zynk completion"));
    }
    for args in [
        vec!["completion"],
        vec!["completion", "unknown"],
        vec!["completion", "zsh", "extra"],
        vec!["completion", "help", "extra"],
    ] {
        let output = cli.run(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
    }
    cli.assert_no_runtime_created();
}
