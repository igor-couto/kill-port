use std::env;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(10);

#[test]
#[ignore = "helper process spawned by cli_integration_tests"]
fn port_holder_process() {
    let Ok(port) = env::var("KILL_PORT_TEST_HOLDER_PORT") else {
        return;
    };
    let port: u16 = port.parse().expect("valid test port");
    let _listener = TcpListener::bind(("127.0.0.1", port)).expect("bind test listener");
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

#[test]
fn frees_a_listening_tcp_port() {
    let port = reserve_port();
    let mut holder = ChildGuard::spawn(port);

    wait_until_listening(port).unwrap_or_else(|error| {
        panic!(
            "helper process {} did not listen on port {port}: {error}",
            holder.0.id()
        )
    });

    let dry_run = run_cli(&["--dry-run", &port.to_string()]);
    assert!(
        dry_run.status.success(),
        "dry run failed: {}",
        String::from_utf8_lossy(&dry_run.stderr)
    );
    let dry_run_stdout = String::from_utf8_lossy(&dry_run.stdout);
    assert!(dry_run_stdout.contains(&format!("PID {}", holder.0.id())));
    assert!(dry_run_stdout.contains("Dry run"));
    assert!(
        holder.0.try_wait().unwrap().is_none(),
        "dry run must not terminate the listener"
    );

    let output = run_cli(&[&port.to_string()]);
    assert!(
        output.status.success(),
        "kill failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(&format!("Port {port} is now free.")),
        "unexpected output: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        wait_for_exit(&mut holder.0, WAIT),
        "listener process was not terminated"
    );
}

#[test]
fn reports_an_unused_port_as_free() {
    let port = reserve_port();
    let output = run_cli(&[&port.to_string()]);
    assert!(
        output.status.success(),
        "unexpected failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("already free"));
}

#[test]
fn rejects_invalid_usage_with_exit_code_two() {
    for args in [&["not-a-port"][..], &["0"][..], &[][..]] {
        let output = run_cli(args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "args {args:?}: stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("error:"));
    }
}

fn run_cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_killport"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run killport")
}

fn reserve_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("reserve a local port")
        .local_addr()
        .expect("local address")
        .port()
}

fn wait_until_listening(port: u16) -> Result<(), String> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err("timed out waiting for the listener".to_string())
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(_) => return false,
        }
    }
    false
}

struct ChildGuard(Child);

impl ChildGuard {
    fn spawn(port: u16) -> Self {
        let executable = env::current_exe().expect("integration test executable");
        let child = Command::new(executable)
            .args(["--ignored", "--exact", "port_holder_process"])
            .env("KILL_PORT_TEST_HOLDER_PORT", port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn port holder");
        Self(child)
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
