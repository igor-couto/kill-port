# killport

[![CI](https://github.com/igor-couto/kill-port/actions/workflows/ci.yml/badge.svg)](https://github.com/igor-couto/kill-port/actions/workflows/ci.yml)

A small command-line tool that frees a TCP port by finding and terminating the
processes listening on it. It runs on Linux and Windows and has no runtime
dependencies or third-party Rust crates.

## Usage

```text
killport [OPTIONS] <PORT>
```

```console
$ killport 3000
Found 1 process listening on TCP port 3000:
  PID 12345  node  (127.0.0.1:3000)

Stopping PID 12345 (node)... ok
Port 3000 is now free.
```

Only TCP sockets in the listening state are considered. If the port has no
listener, the command exits successfully and prints `Port 3000 is already
free.`, which makes it safe to use in scripts.

### Preview what would be killed

```console
$ killport --dry-run 3000
Found 1 process listening on TCP port 3000:
  PID 12345  node  (127.0.0.1:3000)

Dry run: no processes were terminated.
```

### Other options

```text
-n, --dry-run         List matching processes without terminating them
    --timeout <SECS>  Seconds to wait for the port to close before forcing an exit
                      (0-3600) [default: 5]
-h, --help            Print help
-V, --version         Print version
```

## Behavior

- Finds every process that owns a listening TCP socket on the requested port,
  including IPv4 and IPv6 listeners.
- On Linux, reads `/proc/net/tcp*` and maps socket inodes to processes through
  `/proc/<pid>/fd`. It sends `SIGTERM` first and escalates to `SIGKILL` after
  `--timeout` if the port is still held.
- On Windows, reads the TCP listener table through `GetExtendedTcpTable` and
  calls `TerminateProcess` for each owning process. Termination is immediate.
- If processes share a port (for example with `SO_REUSEPORT`), all of them are
  stopped.
- The current process and Windows system processes (PID 0-4) are never
  terminated. If a process binds the port during shutdown, it is left alone and
  the command reports failure.
- Linux hides file descriptors of other users' processes unless you have
  elevated privileges. If the port is in use but the owner cannot be inspected,
  the command fails rather than claiming the port is free.

## Install

### Prebuilt binaries

[Releases](https://github.com/igor-couto/kill-port/releases) ship ready-to-run
builds, so a Rust toolchain is not required:

| Platform | File |
| --- | --- |
| Linux x86_64 | `killport-<version>-linux-x86_64.tar.gz` |
| Linux aarch64 | `killport-<version>-linux-aarch64.tar.gz` |
| Linux armv7 | `killport-<version>-linux-armv7.tar.gz` |
| Linux riscv64 | `killport-<version>-linux-riscv64.tar.gz` |
| Windows x86_64 | `killport-<version>-windows-x86_64.zip` |
| Windows aarch64 | `killport-<version>-windows-aarch64.zip` |

On Linux, extract the archive and install the binary:

```console
$ tar -xzf killport-0.1.0-linux-x86_64.tar.gz
$ sudo install -m 0755 killport /usr/local/bin/killport
```

On Windows, extract the ZIP and copy `killport.exe` to a directory on your
`PATH`.

### RPM packages

Releases also include RPM packages for x86_64, aarch64, and riscv64:

```console
$ sudo rpm -U killport-0.1.0-1.x86_64.rpm
```

### Debian and Ubuntu packages

Releases also include `.deb` packages for amd64, arm64, and riscv64:

```console
$ sudo apt install ./killport_0.1.0-1_amd64.deb
```

### Verifying a download

When release signing is configured, every asset has a detached ASCII-armored
signature:

```console
$ gpg --verify killport-0.1.0-linux-x86_64.tar.gz.asc killport-0.1.0-linux-x86_64.tar.gz
```

### Build from source

From this directory:

```console
cargo install --path .
```

Or build release binaries:

```console
cargo build --release
```

The executable is written to `target/release/killport` on Linux or
`target\release\killport.exe` on Windows. To use it without specifying the
build path, copy it to a directory on your `PATH`; for example, on Windows:

```powershell
$installDir = "$env:LOCALAPPDATA\Programs\killport"
New-Item -ItemType Directory -Force $installDir | Out-Null
Copy-Item target\release\killport.exe $installDir
# Add $installDir to the user PATH, then open a new terminal.
```

Killing processes owned by another user requires `sudo` on Linux or an
elevated terminal on Windows.

## Exit codes

| Code | Meaning |
| ---: | --- |
| `0` | The port is free, or a dry run completed successfully |
| `1` | Discovery failed or the port could not be freed |
| `2` | Invalid command-line usage |

## Development

```console
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

The integration test spawns a helper process that owns a real TCP listener,
then runs the CLI against it. The test suite therefore runs on both supported
operating systems without any external tools. GitHub Actions runs the same
checks on Ubuntu and Windows.

## License

Released under the [MIT License](LICENSE). You are free to use, modify, and
redistribute this software, including commercially, provided the copyright
notice and license text are retained.

### Author

* **Igor Couto** - [igor.fcouto@gmail.com](mailto:igor.fcouto@gmail.com)