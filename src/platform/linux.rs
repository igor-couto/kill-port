use super::Listener;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::path::Path;

pub const SUPPORTS_GRACEFUL_TERMINATION: bool = true;

const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;
const ESRCH: i32 = 3;

#[derive(Debug)]
struct SocketEntry {
    inode: u64,
    address: SocketAddr,
}

pub fn listeners(port: u16) -> io::Result<Vec<Listener>> {
    let mut sockets = read_sockets("/proc/net/tcp", false, port)?;
    match read_sockets("/proc/net/tcp6", true, port) {
        Ok(mut ipv6) => sockets.append(&mut ipv6),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    if sockets.is_empty() {
        return Ok(Vec::new());
    }

    let sockets_by_inode: HashMap<u64, SocketAddr> = sockets
        .iter()
        .map(|socket| (socket.inode, socket.address))
        .collect();

    let mut addresses_by_pid: BTreeMap<u32, BTreeSet<SocketAddr>> = BTreeMap::new();
    let mut matched_inodes = BTreeSet::new();

    let proc_entries = fs::read_dir("/proc").map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not inspect processes in /proc: {error}"),
        )
    })?;

    for entry in proc_entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };

        let fd_entries = match fs::read_dir(entry.path().join("fd")) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for fd in fd_entries.flatten() {
            let target = match fs::read_link(fd.path()) {
                Ok(target) => target,
                Err(_) => continue,
            };
            let Some(inode) = socket_inode(&target) else {
                continue;
            };
            let Some(address) = sockets_by_inode.get(&inode) else {
                continue;
            };

            addresses_by_pid.entry(pid).or_default().insert(*address);
            matched_inodes.insert(inode);
        }
    }

    // A socket can be visible in /proc/net while its owning process is hidden
    // (for example, a port held by another user without elevated privileges).
    // Fail closed instead of reporting that the port is free.
    let still_present = current_inodes(port)?;
    if still_present
        .iter()
        .any(|inode| !matched_inodes.contains(inode))
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "TCP port {port} is in use, but its owning process cannot be inspected; \
                 try running with elevated privileges"
            ),
        ));
    }

    let mut listeners: Vec<Listener> = addresses_by_pid
        .into_iter()
        .map(|(pid, addresses)| Listener {
            pid,
            process_name: process_name(pid),
            addresses: addresses.into_iter().collect(),
        })
        .collect();
    listeners.sort_by_key(|listener| listener.pid);
    Ok(listeners)
}

fn read_sockets(path: &str, ipv6: bool, port: u16) -> io::Result<Vec<SocketEntry>> {
    let contents = fs::read_to_string(path)
        .map_err(|error| io::Error::new(error.kind(), format!("could not read {path}: {error}")))?;

    Ok(contents
        .lines()
        .skip(1)
        .filter_map(|line| parse_socket_line(line, ipv6, port))
        .collect())
}

fn parse_socket_line(line: &str, ipv6: bool, port: u16) -> Option<SocketEntry> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 10 || fields[3] != "0A" {
        return None;
    }

    let (address_hex, port_hex) = fields[1].rsplit_once(':')?;
    let local_port = u16::from_str_radix(port_hex, 16).ok()?;
    if local_port != port {
        return None;
    }

    let address = if ipv6 {
        let mut bytes = decode_hex::<16>(address_hex)?;
        for word in bytes.chunks_exact_mut(4) {
            word.reverse();
        }
        SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::from(bytes), port, 0, 0))
    } else {
        let mut bytes = decode_hex::<4>(address_hex)?;
        bytes.reverse();
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(bytes), port))
    };

    let inode = fields[9].parse::<u64>().ok()?;
    if inode == 0 {
        return None;
    }

    Some(SocketEntry { inode, address })
}

fn decode_hex<const N: usize>(value: &str) -> Option<[u8; N]> {
    if !value.is_ascii() || value.len() != N * 2 {
        return None;
    }

    let mut bytes = [0u8; N];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

fn socket_inode(path: &Path) -> Option<u64> {
    let value = path.to_str()?;
    value
        .strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

fn current_inodes(port: u16) -> io::Result<BTreeSet<u64>> {
    let mut inodes = BTreeSet::new();
    for (path, ipv6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
        match read_sockets(path, ipv6, port) {
            Ok(sockets) => inodes.extend(sockets.into_iter().map(|socket| socket.inode)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(inodes)
}

fn process_name(pid: u32) -> Option<String> {
    let value = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let name = value.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

pub fn is_system_pid(pid: u32) -> bool {
    pid == 0
}

pub fn terminate(pid: u32) -> io::Result<()> {
    send_signal(pid, SIGTERM)
}

pub fn force_terminate(pid: u32) -> io::Result<()> {
    send_signal(pid, SIGKILL)
}

fn send_signal(pid: u32, signal: i32) -> io::Result<()> {
    let pid = i32::try_from(pid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid process ID"))?;

    // SAFETY: kill(2) is called with a validated PID and a known signal number.
    // A failure is reported through errno.
    if unsafe { kill(pid, signal) } == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ESRCH) {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "process no longer exists",
        ));
    }
    Err(error)
}

extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    const TCP_HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";

    fn tcp_line(local: &str, state: &str, inode: &str) -> String {
        format!(
            "   0: {local} 00000000:0000 {state} 00000000:00000000 00:00000000 00000000     0        0 {inode} 1 0000000000000000 100 0 0 10 0\n"
        )
    }

    #[test]
    fn parses_ipv4_listener() {
        let line = tcp_line("0100007F:1F90", "0A", "12345");
        let entry = parse_socket_line(&line, false, 8080).expect("socket should parse");
        assert_eq!(entry.inode, 12345);
        assert_eq!(entry.address, "127.0.0.1:8080".parse().unwrap());
    }

    #[test]
    fn parses_wildcard_ipv4_listener() {
        let line = tcp_line("00000000:0050", "0A", "7");
        let entry = parse_socket_line(&line, false, 80).expect("socket should parse");
        assert_eq!(entry.address, "0.0.0.0:80".parse().unwrap());
    }

    #[test]
    fn parses_ipv6_loopback_listener() {
        let line = tcp_line("00000000000000000000000001000000:1F90", "0A", "99");
        let entry = parse_socket_line(&line, true, 8080).expect("socket should parse");
        assert_eq!(entry.address, "[::1]:8080".parse().unwrap());
    }

    #[test]
    fn ignores_non_listeners_and_other_ports() {
        let established = tcp_line("0100007F:1F90", "01", "1");
        let other_port = tcp_line("0100007F:0050", "0A", "2");
        assert!(parse_socket_line(&established, false, 8080).is_none());
        assert!(parse_socket_line(&other_port, false, 8080).is_none());
    }

    #[test]
    fn skips_malformed_lines() {
        assert!(parse_socket_line("", false, 8080).is_none());
        assert!(parse_socket_line("not a proc line", false, 8080).is_none());
        let bad_hex = tcp_line("ZZZZZZZZ:1F90", "0A", "1");
        assert!(parse_socket_line(&bad_hex, false, 8080).is_none());
        let zero_inode = tcp_line("0100007F:1F90", "0A", "0");
        assert!(parse_socket_line(&zero_inode, false, 8080).is_none());
    }

    #[test]
    fn reads_only_the_requested_port_from_a_table() {
        let contents = format!(
            "{TCP_HEADER}{}{}{}",
            tcp_line("0100007F:1F90", "0A", "11"),
            tcp_line("0100007F:0050", "0A", "22"),
            tcp_line("0100007F:1F90", "01", "33"),
        );
        let path =
            std::env::temp_dir().join(format!("killport-proc-test-{}-{}", std::process::id(), 1));
        fs::write(&path, contents).unwrap();
        let sockets = read_sockets(path.to_str().unwrap(), false, 8080).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(sockets.len(), 1);
        assert_eq!(sockets[0].inode, 11);
    }

    #[test]
    fn extracts_socket_inodes() {
        assert_eq!(socket_inode(Path::new("socket:[1234]")), Some(1234));
        assert_eq!(socket_inode(Path::new("/tmp/file")), None);
        assert_eq!(socket_inode(Path::new("socket:[nope]")), None);
    }
}
