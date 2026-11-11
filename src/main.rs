use std::process::ExitCode;

const USAGE: &str = "usage: hq <command>";

fn main() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}
