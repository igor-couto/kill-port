use std::net::SocketAddr;

#[derive(Debug, Clone)]
pub struct Listener {
    pub pid: u32,
    pub process_name: Option<String>,
    pub addresses: Vec<SocketAddr>,
}

impl Listener {
    pub fn name(&self) -> &str {
        self.process_name.as_deref().unwrap_or("unknown")
    }
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{
    force_terminate, is_system_pid, listeners, terminate, SUPPORTS_GRACEFUL_TERMINATION,
};

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::{
    force_terminate, is_system_pid, listeners, terminate, SUPPORTS_GRACEFUL_TERMINATION,
};

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
compile_error!("killport supports only Linux and Windows");
