use std::process::Command;

pub(crate) fn command_for_argv(program: &str, args: &[String]) -> Command {
    let mut command = command_for_program(program);
    command.args(args);
    command
}

fn command_for_program(program: &str) -> Command {
    Command::new(program)
}
