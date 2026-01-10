use sysinfo::System;
use anyhow::{Result, anyhow};
use regex::Regex;

pub struct QmpAddr {
    pub host: String,
    pub port: u16,
}

pub struct ProcessInfo {
    pub pid: u32,
    pub qmp_addrs: Vec<QmpAddr>,
}

pub fn find_xemu_process() -> Result<ProcessInfo> {
    let mut sys = System::new_all();
    sys.refresh_all();

    for (pid, process) in sys.processes() {
        let name = process.name();
        let name_str = name.to_string_lossy();
        
        if name_str.to_ascii_lowercase() == "xemu.exe" {
            let cmd_line = process.cmd();
            let cmd_string = cmd_line.iter()
                .map(|s| s.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");

            // Find all instances of -qmp tcp:host:port
            let re = Regex::new(r"-qmp tcp:(?P<address>[^:]+):(?P<port>\d+)")?;
            let mut qmp_addrs = Vec::new();
            
            for caps in re.captures_iter(&cmd_string) {
                let host = caps.name("address").map(|m| m.as_str()).unwrap_or("localhost").to_string();
                let port = caps.name("port").map(|m| m.as_str().parse::<u16>().unwrap_or(4444)).unwrap_or(4444);
                qmp_addrs.push(QmpAddr { host, port });
            }

            if !qmp_addrs.is_empty() {
                return Ok(ProcessInfo {
                    pid: pid.as_u32(),
                    qmp_addrs,
                });
            }
        }
    }

    Err(anyhow!("xemu.exe not found or QMP args missing"))
}
