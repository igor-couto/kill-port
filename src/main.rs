use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(killport::run(std::env::args_os().skip(1)))
}
