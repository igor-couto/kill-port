use std::ffi::OsString;
use std::time::Duration;

const DEFAULT_TIMEOUT_SECS: u64 = 5;
const MAX_TIMEOUT_SECS: u64 = 3_600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub port: u16,
    pub dry_run: bool,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Run(Options),
}

pub fn help() -> String {
    format!(
        "\
Kill all processes listening on a TCP port.

Usage:
  killport [OPTIONS] <PORT>

Arguments:
  <PORT>  TCP port to free (1-65535)

Options:
  -n, --dry-run         List matching processes without terminating them
      --timeout <SECS>  Seconds to wait for the port to close before forcing an exit \
(0-{max}) [default: {default}]
  -h, --help            Print help
  -V, --version         Print version

Linux sends SIGTERM first and escalates to SIGKILL after the timeout. Windows
terminates matching processes immediately. Only TCP listener sockets are
considered.

Exit codes:
  0  The port is free
  1  The port could not be freed
  2  Invalid command-line usage
",
        max = MAX_TIMEOUT_SECS,
        default = DEFAULT_TIMEOUT_SECS
    )
}

pub fn version() -> String {
    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

pub fn parse<I>(args: I) -> Result<Command, String>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut port = None;
    let mut dry_run = false;
    let mut timeout = Duration::from_secs(DEFAULT_TIMEOUT_SECS);
    let mut positional_only = false;

    while let Some(raw_arg) = args.next() {
        let arg = raw_arg
            .into_string()
            .map_err(|_| "arguments must be valid UTF-8".to_string())?;

        if !positional_only {
            match arg.as_str() {
                "-h" | "--help" => return Ok(Command::Help),
                "-V" | "--version" => return Ok(Command::Version),
                "-n" | "--dry-run" => {
                    dry_run = true;
                    continue;
                }
                "--" => {
                    positional_only = true;
                    continue;
                }
                "--timeout" => {
                    let value = args
                        .next()
                        .ok_or_else(|| "option '--timeout' requires a value".to_string())?
                        .into_string()
                        .map_err(|_| "arguments must be valid UTF-8".to_string())?;
                    timeout = parse_timeout(&value)?;
                    continue;
                }
                _ => {}
            }

            if let Some(value) = arg.strip_prefix("--timeout=") {
                timeout = parse_timeout(value)?;
                continue;
            }

            if arg.starts_with('-') && arg != "-" {
                return Err(format!("unknown option '{arg}'"));
            }
        }

        if port.is_some() {
            return Err(format!("unexpected extra argument '{arg}'"));
        }

        let value = arg
            .parse::<u16>()
            .map_err(|_| format!("invalid port '{arg}': expected a number from 1 to 65535"))?;
        if value == 0 {
            return Err("invalid port '0': expected a number from 1 to 65535".to_string());
        }
        port = Some(value);
    }

    let port = port.ok_or_else(|| "missing required argument '<PORT>'".to_string())?;

    Ok(Command::Run(Options {
        port,
        dry_run,
        timeout,
    }))
}

fn parse_timeout(value: &str) -> Result<Duration, String> {
    let seconds = value
        .parse::<u64>()
        .map_err(|_| format!("invalid timeout '{value}': expected a whole number of seconds"))?;
    if seconds > MAX_TIMEOUT_SECS {
        return Err(format!(
            "invalid timeout '{value}': expected a value from 0 to {MAX_TIMEOUT_SECS} seconds"
        ));
    }
    Ok(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(OsString::from))
    }

    fn run_options(args: &[&str]) -> Options {
        match parse_args(args) {
            Ok(Command::Run(options)) => options,
            other => panic!("expected run command, got {other:?}"),
        }
    }

    #[test]
    fn parses_port_with_defaults() {
        assert_eq!(
            run_options(&["3000"]),
            Options {
                port: 3000,
                dry_run: false,
                timeout: Duration::from_secs(5),
            }
        );
    }

    #[test]
    fn parses_options_in_any_order() {
        let options = run_options(&["--timeout", "12", "-n", "8080"]);
        assert_eq!(options.port, 8080);
        assert!(options.dry_run);
        assert_eq!(options.timeout, Duration::from_secs(12));

        let options = run_options(&["9000", "--dry-run", "--timeout=0"]);
        assert_eq!(options.port, 9000);
        assert!(options.dry_run);
        assert_eq!(options.timeout, Duration::ZERO);
    }

    #[test]
    fn supports_option_terminator() {
        assert_eq!(run_options(&["--", "3000"]).port, 3000);
    }

    #[test]
    fn rejects_invalid_ports() {
        for args in [
            &[] as &[&str],
            &["0"],
            &["65536"],
            &["abc"],
            &["-1"],
            &["3000", "4000"],
        ] {
            assert!(parse_args(args).is_err(), "args should fail: {args:?}");
        }
    }

    #[test]
    fn rejects_invalid_options() {
        assert!(parse_args(&["--wat", "3000"]).is_err());
        assert!(parse_args(&["--timeout"]).is_err());
        assert!(parse_args(&["--timeout", "nope", "3000"]).is_err());
        assert!(parse_args(&["--timeout", "3601", "3000"]).is_err());
    }

    #[test]
    fn help_and_version_do_not_require_a_port() {
        assert_eq!(parse_args(&["--help"]), Ok(Command::Help));
        assert_eq!(parse_args(&["-V"]), Ok(Command::Version));
    }

    #[test]
    fn help_documents_core_behavior() {
        let help = help();
        assert!(help.contains("Usage:"));
        assert!(help.contains("--dry-run"));
        assert!(help.contains("SIGTERM"));
        assert!(help.contains("Exit codes:"));
    }
}
