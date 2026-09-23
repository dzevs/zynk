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
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$1\" \"$2\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();

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
