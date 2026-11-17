use crate::cli::{self, Command, Options};
use crate::platform::{self, Listener};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{self, Write};
use std::process;
use std::thread;
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const FORCE_GRACE: Duration = Duration::from_secs(2);

pub fn run<I>(args: I) -> u8
where
    I: IntoIterator<Item = OsString>,
{
    match cli::parse(args) {
        Ok(Command::Help) => {
            print!("{}", cli::help());
            0
        }
        Ok(Command::Version) => {
            println!("{}", cli::version());
            0
        }
        Ok(Command::Run(options)) => execute(options),
        Err(message) => {
            eprintln!("error: {message}");
            eprintln!();
            eprintln!("Usage: killport [OPTIONS] <PORT>");
            eprintln!();
            eprintln!("For more information, try '--help'.");
            2
        }
    }
}

fn execute(options: Options) -> u8 {
    let port = options.port;
    let listeners = match platform::listeners(port) {
        Ok(listeners) => listeners,
        Err(error) => {
            eprintln!("error: could not inspect TCP port {port}: {error}");
            return 1;
        }
    };

    if listeners.is_empty() {
        println!("Port {port} is already free.");
        return 0;
    }

    print_listeners(port, &listeners);
    println!();

    if options.dry_run {
        println!("Dry run: no processes were terminated.");
        return 0;
    }

    if let Some((pid, reason)) = protected_target(&listeners) {
        eprintln!("error: refusing to terminate PID {pid} because {reason}.");
        return 1;
    }

    let targets: BTreeSet<u32> = listeners.iter().map(|listener| listener.pid).collect();
    let mut stop_errors: BTreeMap<u32, io::Error> = BTreeMap::new();

    for pid in &targets {
        let name = listener_name(&listeners, *pid);
        print!("Stopping PID {pid} ({})... ", display_name(name));
        let _ = io::stdout().flush();

        match platform::terminate(*pid) {
            Ok(()) => println!("ok"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                println!("process already exited")
            }
            Err(error) => {
                println!("failed");
                stop_errors.insert(*pid, error);
            }
        }
    }

    let deadline = Instant::now() + options.timeout;
    let mut state = wait_for_release(port, &targets, deadline);

    let force_targets: Vec<Listener> = match &state {
        WaitState::Listening {
            listeners,
            replaced: false,
        } if platform::SUPPORTS_GRACEFUL_TERMINATION => listeners
            .iter()
            .filter(|listener| targets.contains(&listener.pid))
            .cloned()
            .collect(),
        _ => Vec::new(),
    };

    if !force_targets.is_empty() {
        println!(
            "Port {port} is still in use after {}s; forcing termination.",
            options.timeout.as_secs()
        );

        let mut forced_pids = BTreeSet::new();
        for listener in &force_targets {
            let pid = listener.pid;
            forced_pids.insert(pid);
            print!("Forcing PID {pid} ({})... ", display_name(listener.name()));
            let _ = io::stdout().flush();

            match platform::force_terminate(pid) {
                Ok(()) => println!("ok"),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    println!("process already exited")
                }
                Err(error) => {
                    println!("failed");
                    stop_errors.insert(pid, error);
                }
            }
        }

        state = wait_for_release(port, &forced_pids, Instant::now() + FORCE_GRACE);
    }

    match state {
        WaitState::Released => {
            println!("Port {port} is now free.");
            0
        }
        WaitState::Listening {
            listeners,
            replaced,
        } => {
            if replaced {
                eprintln!(
                    "error: port {port} was claimed by another process after termination began; \
                     it was left running"
                );
            } else {
                eprintln!("error: port {port} is still in use");
            }
            print_listener_rows(&listeners, true);
            print_stop_errors(&stop_errors);
            1
        }
        WaitState::DiscoveryFailed(error) => {
            eprintln!("error: could not verify that TCP port {port} was released: {error}");
            print_stop_errors(&stop_errors);
            1
        }
    }
}

fn print_listeners(port: u16, listeners: &[Listener]) {
    let noun = if listeners.len() == 1 {
        "process"
    } else {
        "processes"
    };
    println!(
        "Found {} {noun} listening on TCP port {port}:",
        listeners.len()
    );
    print_listener_rows(listeners, false);
}

fn print_listener_rows(listeners: &[Listener], to_stderr: bool) {
    for listener in listeners {
        let addresses = listener
            .addresses
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let line = format!(
            "  PID {}  {}  ({})",
            listener.pid,
            display_name(listener.name()),
            addresses
        );
        if to_stderr {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
}

fn display_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|character| {
            if character.is_control() {
                '?'
            } else {
                character
            }
        })
        .collect();
    const MAX_CHARS: usize = 80;
    if cleaned.chars().count() <= MAX_CHARS {
        cleaned
    } else {
        let mut shortened: String = cleaned.chars().take(MAX_CHARS - 3).collect();
        shortened.push_str("...");
        shortened
    }
}

fn listener_name(listeners: &[Listener], pid: u32) -> &str {
    listeners
        .iter()
        .find(|listener| listener.pid == pid)
        .map(Listener::name)
        .unwrap_or("unknown")
}

fn protected_target(listeners: &[Listener]) -> Option<(u32, &'static str)> {
    for listener in listeners {
        if listener.pid == process::id() {
            return Some((listener.pid, "it is the current process"));
        }
        if platform::is_system_pid(listener.pid) {
            return Some((listener.pid, "it is a critical system process"));
        }
    }
    None
}

fn print_stop_errors(errors: &BTreeMap<u32, io::Error>) {
    if errors.is_empty() {
        return;
    }

    eprintln!();
    eprintln!("Termination errors:");
    for (pid, error) in errors {
        eprintln!("  PID {pid}: {error}");
    }
    if errors
        .values()
        .any(|error| error.kind() == io::ErrorKind::PermissionDenied)
    {
        eprintln!("hint: run with elevated privileges to stop processes owned by other users.");
    }
}

#[derive(Debug)]
enum WaitState {
    Released,
    Listening {
        listeners: Vec<Listener>,
        replaced: bool,
    },
    DiscoveryFailed(io::Error),
}

fn wait_for_release(port: u16, targets: &BTreeSet<u32>, deadline: Instant) -> WaitState {
    loop {
        let result = platform::listeners(port);
        let now = Instant::now();

        match result {
            Ok(listeners) if listeners.is_empty() => return WaitState::Released,
            Ok(listeners) => {
                let replaced = !listeners
                    .iter()
                    .any(|listener| targets.contains(&listener.pid));
                if replaced || now >= deadline {
                    return WaitState::Listening {
                        listeners,
                        replaced,
                    };
                }
            }
            Err(error) => {
                if now >= deadline {
                    return WaitState::DiscoveryFailed(error);
                }
            }
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            thread::sleep(remaining.min(POLL_INTERVAL));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn listener(pid: u32, name: Option<&str>, address: &str) -> Listener {
        Listener {
            pid,
            process_name: name.map(str::to_string),
            addresses: vec![address.parse::<SocketAddr>().unwrap()],
        }
    }

    #[test]
    fn sanitizes_process_names() {
        assert_eq!(display_name("node"), "node");
        assert_eq!(display_name("a\nb\u{1b}[31m"), "a?b?[31m");
        let long_name = "x".repeat(100);
        let shortened = display_name(&long_name);
        assert_eq!(shortened.chars().count(), 80);
        assert!(shortened.ends_with("..."));
    }

    #[test]
    fn protects_the_current_process() {
        let current = listener(process::id(), Some("killport"), "127.0.0.1:3000");
        assert_eq!(
            protected_target(&[current]),
            Some((process::id(), "it is the current process"))
        );
    }

    #[test]
    fn finds_names_by_pid() {
        let listeners = vec![
            listener(7, Some("node"), "127.0.0.1:3000"),
            listener(8, None, "[::1]:3000"),
        ];
        assert_eq!(listener_name(&listeners, 7), "node");
        assert_eq!(listener_name(&listeners, 8), "unknown");
    }
}
