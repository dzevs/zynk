// Modified by the zynk project: this file differs from the upstream version it was derived from.
// See NOTICE ("Modified files (Apache-2.0 provenance)") for the provenance and the license terms.
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;

pub(crate) fn command_for_argv_in_dir(program: &str, args: &[String], cwd: &Path) -> Command {
    let program = program_for_cwd(program, cwd);
    let mut command = command_for_program(&program);
    command.args(args).current_dir(cwd);
    command
}

fn program_for_cwd(program: &str, cwd: &Path) -> OsString {
    let path = Path::new(program);
    if path.is_relative() && program.contains('/') {
        let relative = path.strip_prefix(Path::new(".")).unwrap_or(path);
        cwd.join(relative).into_os_string()
    } else {
        path.as_os_str().to_os_string()
    }
}

fn command_for_program(program: &OsStr) -> Command {
    Command::new(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_executable_script(path: &Path, contents: &[u8]) {
        use std::io::Write as _;
        use std::process::Stdio;

        // Opening an executable for writing in this process lets an unrelated
        // concurrent fork inherit that descriptor and can make exec fail with
        // ETXTBSY. Keep the writer in a short-lived child instead.
        let mut child = Command::new("/bin/sh")
            .args([
                "-c",
                "umask 077; cat > \"$1\" && chmod 700 \"$1\"",
                "zynk-plugin-command-fixture",
            ])
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn script fixture writer");
        child
            .stdin
            .take()
            .expect("script fixture writer stdin")
            .write_all(contents)
            .expect("write script fixture");
        let output = child
            .wait_with_output()
            .expect("wait for script fixture writer");
        assert!(output.status.success(), "{output:?}");
    }

    #[test]
    fn m857_relative_program_resolves_from_plugin_root_without_retokenizing_args() {
        let root = std::env::temp_dir().join(format!(
            "zynk-plugin-command-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let script = root.join("bin/capture");
        write_executable_script(
            &script,
            b"#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$1\" \"$2\"\n",
        );

        let output = command_for_argv_in_dir(
            "./bin/capture",
            &["two words".into(), "$literal".into()],
            &root,
        )
        .output()
        .unwrap();
        assert!(output.status.success(), "{output:?}");
        let lines = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            lines.lines().collect::<Vec<_>>(),
            [
                root.display().to_string(),
                "two words".into(),
                "$literal".into()
            ]
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
