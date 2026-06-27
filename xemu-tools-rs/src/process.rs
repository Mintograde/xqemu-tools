use anyhow::{anyhow, Result};
use std::ffi::OsStr;
use std::thread;
use std::time::Duration;
use sysinfo::System;

#[derive(Debug, Clone)]
pub struct XemuProcess {
    pub pid: u32,
}

pub fn wait_for_xemu() -> Result<XemuProcess> {
    loop {
        if let Some(process) = find_xemu_process() {
            println!("xemu pid is {} ({:#x})", process.pid, process.pid);
            return Ok(process);
        }
        println!("waiting 1 more seconds for xemu to start");
        thread::sleep(Duration::from_secs(1));
    }
}

fn find_xemu_process() -> Option<XemuProcess> {
    let mut system = System::new_all();
    system.refresh_all();

    for (pid, process) in system.processes() {
        if process.name().eq_ignore_ascii_case(OsStr::new("xemu.exe")) {
            let cmdline = process
                .cmd()
                .iter()
                .map(|part| part.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
            if cmdline.contains("-qmp") || cmdline.is_empty() {
                return Some(XemuProcess { pid: pid.as_u32() });
            }
        }
    }
    None
}

pub fn process_error_context(pid: u32) -> anyhow::Error {
    anyhow!("failed to access xemu process pid {pid}; make sure this process has permission to read xemu memory")
}
