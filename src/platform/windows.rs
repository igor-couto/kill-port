#![allow(non_snake_case)]

use super::Listener;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{c_void, OsString};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::os::windows::ffi::OsStringExt;
use std::path::Path;
use std::ptr::null_mut;
use std::slice;

type Bool = i32;
type Dword = u32;
type Handle = *mut c_void;
type Pvoid = *mut c_void;
type Ulong = u32;

const AF_INET: u32 = 2;
const AF_INET6: u32 = 23;
const TCP_TABLE_OWNER_PID_LISTENER: i32 = 3;

const ERROR_SUCCESS: u32 = 0;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_NOT_SUPPORTED: u32 = 50;

const FALSE: Bool = 0;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const PROCESS_TERMINATE: u32 = 0x0001;

const IPV4_ROW_SIZE: usize = 24;
const IPV6_ROW_SIZE: usize = 56;
const MAX_TABLE_BYTES: usize = 64 * 1024 * 1024;

pub const SUPPORTS_GRACEFUL_TERMINATION: bool = false;

#[link(name = "iphlpapi")]
extern "system" {
    fn GetExtendedTcpTable(
        tcp_table: Pvoid,
        size: *mut Dword,
        order: Bool,
        address_family: Ulong,
        table_class: i32,
        reserved: Ulong,
    ) -> Dword;
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(desired_access: Dword, inherit_handle: Bool, process_id: Dword) -> Handle;
    fn TerminateProcess(process: Handle, exit_code: u32) -> Bool;
    fn CloseHandle(object: Handle) -> Bool;
    fn QueryFullProcessImageNameW(
        process: Handle,
        flags: Dword,
        exe_name: *mut u16,
        size: *mut Dword,
    ) -> Bool;
}

struct OwnedHandle(Handle);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by OpenProcess and is owned here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn listeners(port: u16) -> io::Result<Vec<Listener>> {
    let ipv4 = query_table(AF_INET)?;
    let ipv4_rows = parse_ipv4_table(&ipv4, port)?;

    let ipv6 = match query_table(AF_INET6) {
        Ok(table) => table,
        Err(error) if error.raw_os_error() == Some(ERROR_NOT_SUPPORTED as i32) => Vec::new(),
        Err(error) => return Err(error),
    };
    let ipv6_rows = parse_ipv6_table(&ipv6, port)?;

    let mut addresses_by_pid: BTreeMap<u32, BTreeSet<SocketAddr>> = BTreeMap::new();
    for (pid, address) in ipv4_rows.into_iter().chain(ipv6_rows) {
        addresses_by_pid.entry(pid).or_default().insert(address);
    }

    Ok(addresses_by_pid
        .into_iter()
        .map(|(pid, addresses)| Listener {
            pid,
            process_name: process_name(pid),
            addresses: addresses.into_iter().collect(),
        })
        .collect())
}

fn query_table(address_family: u32) -> io::Result<Vec<u8>> {
    let mut size = 0u32;
    // SAFETY: a null table with a zero size is the documented sizing query.
    let result = unsafe {
        GetExtendedTcpTable(
            null_mut(),
            &mut size,
            FALSE,
            address_family,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };

    if result != ERROR_SUCCESS && result != ERROR_INSUFFICIENT_BUFFER {
        return Err(io::Error::from_raw_os_error(result as i32));
    }

    if size == 0 {
        return Ok(Vec::new());
    }

    for _ in 0..3 {
        if size as usize > MAX_TABLE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("TCP table is unexpectedly large ({size} bytes)"),
            ));
        }

        // Use u32 storage so the table buffer satisfies the API's alignment.
        let words = (size as usize).div_ceil(4);
        let mut buffer = vec![0u32; words];
        let mut buffer_size = (words * 4) as u32;

        // SAFETY: buffer is valid for buffer_size bytes and the size argument
        // describes that allocation. The returned size is bounded below.
        let result = unsafe {
            GetExtendedTcpTable(
                buffer.as_mut_ptr().cast(),
                &mut buffer_size,
                FALSE,
                address_family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };

        if result == ERROR_SUCCESS {
            let length = (buffer_size as usize).min(words * 4);
            // SAFETY: the allocation contains words * 4 initialized bytes.
            let bytes = unsafe { slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), length) };
            return Ok(bytes.to_vec());
        }

        if result != ERROR_INSUFFICIENT_BUFFER {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        size = buffer_size;
    }

    Err(io::Error::other(
        "could not obtain the Windows TCP listener table",
    ))
}

fn parse_ipv4_table(table: &[u8], port: u16) -> io::Result<Vec<(u32, SocketAddr)>> {
    let count = row_count(table, IPV4_ROW_SIZE)?;
    let mut rows = Vec::with_capacity(count);

    for index in 0..count {
        let row = &table[4 + index * IPV4_ROW_SIZE..4 + (index + 1) * IPV4_ROW_SIZE];
        let local_port = u16::from_be_bytes([row[8], row[9]]);
        if local_port != port {
            continue;
        }

        let address = Ipv4Addr::new(row[4], row[5], row[6], row[7]);
        let pid = read_u32(row, 20)?;
        rows.push((pid, SocketAddr::V4(SocketAddrV4::new(address, local_port))));
    }

    Ok(rows)
}

fn parse_ipv6_table(table: &[u8], port: u16) -> io::Result<Vec<(u32, SocketAddr)>> {
    let count = row_count(table, IPV6_ROW_SIZE)?;
    let mut rows = Vec::with_capacity(count);

    for index in 0..count {
        let row = &table[4 + index * IPV6_ROW_SIZE..4 + (index + 1) * IPV6_ROW_SIZE];
        let local_port = u16::from_be_bytes([row[20], row[21]]);
        if local_port != port {
            continue;
        }

        let address = Ipv6Addr::from(
            <[u8; 16]>::try_from(&row[..16]).expect("IPv6 address has a fixed length"),
        );
        let scope_id = read_u32(row, 16)?;
        let pid = read_u32(row, 52)?;
        rows.push((
            pid,
            SocketAddr::V6(SocketAddrV6::new(address, local_port, 0, scope_id)),
        ));
    }

    Ok(rows)
}

fn row_count(table: &[u8], row_size: usize) -> io::Result<usize> {
    let count = read_u32(table, 0)? as usize;
    let available = table.len().saturating_sub(4) / row_size;
    if count > available {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated Windows TCP listener table",
        ));
    }
    Ok(count)
}

fn read_u32(bytes: &[u8], offset: usize) -> io::Result<u32> {
    let value = bytes.get(offset..offset + 4).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated Windows TCP listener table",
        )
    })?;
    Ok(u32::from_ne_bytes([value[0], value[1], value[2], value[3]]))
}

fn open_process(access: u32, pid: u32) -> io::Result<OwnedHandle> {
    // pid 0 is the Windows idle process. Opening it is invalid and must never
    // reach TerminateProcess.
    if pid == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid process ID 0",
        ));
    }

    // SAFETY: OpenProcess has no pointer arguments and returns either a valid
    // handle or null.
    let handle = unsafe { OpenProcess(access, FALSE, pid) };
    if handle.is_null() {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "process no longer exists",
            ));
        }
        return Err(error);
    }
    Ok(OwnedHandle(handle))
}

fn process_name(pid: u32) -> Option<String> {
    let handle = open_process(PROCESS_QUERY_LIMITED_INFORMATION, pid).ok()?;
    let mut buffer = vec![0u16; 32_768];
    let mut length = buffer.len() as u32;

    // SAFETY: buffer is writable for length UTF-16 units.
    if unsafe { QueryFullProcessImageNameW(handle.0, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
        return None;
    }

    let length = (length as usize).min(buffer.len());
    let path = OsString::from_wide(&buffer[..length]);
    Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .or_else(|| Some(path.to_string_lossy().into_owned()))
}

pub fn is_system_pid(pid: u32) -> bool {
    pid <= 4
}

pub fn terminate(pid: u32) -> io::Result<()> {
    let process = open_process(PROCESS_TERMINATE, pid)?;

    // SAFETY: the handle was opened with PROCESS_TERMINATE and is valid for
    // this call.
    if unsafe { TerminateProcess(process.0, 1) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn force_terminate(pid: u32) -> io::Result<()> {
    terminate(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ipv4_table(rows: &[(Ipv4Addr, u16, u32)]) -> Vec<u8> {
        let mut table = Vec::with_capacity(4 + rows.len() * IPV4_ROW_SIZE);
        table.extend((rows.len() as u32).to_ne_bytes());
        for (address, port, pid) in rows {
            table.extend(2u32.to_ne_bytes()); // MIB_TCP_STATE_LISTEN
            table.extend(address.octets());
            table.extend(port.to_be_bytes());
            table.extend([0u8, 0u8]);
            table.extend([0u8; 4]);
            table.extend([0u8; 2]);
            table.extend([0u8; 2]);
            table.extend(pid.to_ne_bytes());
        }
        table
    }

    fn ipv6_table(rows: &[(Ipv6Addr, u16, u32)]) -> Vec<u8> {
        let mut table = Vec::with_capacity(4 + rows.len() * IPV6_ROW_SIZE);
        table.extend((rows.len() as u32).to_ne_bytes());
        for (address, port, pid) in rows {
            table.extend(address.octets());
            table.extend(0u32.to_ne_bytes()); // scope id
            table.extend(port.to_be_bytes());
            table.extend([0u8, 0u8]);
            table.extend([0u8; 16]);
            table.extend(0u32.to_ne_bytes()); // remote scope id
            table.extend([0u8; 2]);
            table.extend([0u8; 2]);
            table.extend(2u32.to_ne_bytes()); // state
            table.extend(pid.to_ne_bytes());
        }
        table
    }

    #[test]
    fn parses_ipv4_listener_table() {
        let table = ipv4_table(&[
            (Ipv4Addr::LOCALHOST, 8080, 1234),
            (Ipv4Addr::UNSPECIFIED, 80, 4321),
            (Ipv4Addr::new(192, 168, 1, 10), 8080, 99),
        ]);
        let rows = parse_ipv4_table(&table, 8080).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], (1234, "127.0.0.1:8080".parse().unwrap()));
        assert_eq!(rows[1], (99, "192.168.1.10:8080".parse().unwrap()));
    }

    #[test]
    fn parses_ipv6_listener_table() {
        let table = ipv6_table(&[(Ipv6Addr::LOCALHOST, 8080, 1234)]);
        let rows = parse_ipv6_table(&table, 8080).unwrap();
        assert_eq!(rows, vec![(1234, "[::1]:8080".parse().unwrap())]);
    }

    #[test]
    fn filters_other_ports() {
        let ipv4 = ipv4_table(&[(Ipv4Addr::LOCALHOST, 80, 1234)]);
        assert!(parse_ipv4_table(&ipv4, 8080).unwrap().is_empty());

        let ipv6 = ipv6_table(&[(Ipv6Addr::LOCALHOST, 80, 1234)]);
        assert!(parse_ipv6_table(&ipv6, 8080).unwrap().is_empty());
    }

    #[test]
    fn rejects_truncated_tables() {
        let table = ipv4_table(&[(Ipv4Addr::LOCALHOST, 8080, 1234)]);
        assert!(parse_ipv4_table(&table[..10], 8080).is_err());
        assert!(parse_ipv4_table(&[], 8080).is_err());
    }

    #[test]
    fn system_processes_are_protected() {
        assert!(is_system_pid(0));
        assert!(is_system_pid(4));
        assert!(!is_system_pid(42));
    }
}
