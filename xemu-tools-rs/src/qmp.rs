use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

pub struct QmpClient {
    host: String,
    port: u16,
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl QmpClient {
    pub fn connect_with_retry(host: impl Into<String>, port: u16) -> Result<Self> {
        let host = host.into();
        let mut attempt = 0;
        loop {
            println!("Trying to connect {attempt}");
            match Self::connect(&host, port) {
                Ok(client) => return Ok(client),
                Err(err) if attempt < 5 => {
                    attempt += 1;
                    eprintln!("QMP connect failed: {err:#}");
                    thread::sleep(Duration::from_secs(1));
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn connect(host: &str, port: u16) -> Result<Self> {
        let stream = TcpStream::connect((host, port))
            .with_context(|| format!("failed to connect to QMP at {host}:{port}"))?;
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        stream.set_write_timeout(Some(Duration::from_millis(500)))?;
        let writer = stream.try_clone()?;
        let mut client = Self {
            host: host.to_string(),
            port,
            reader: BufReader::new(stream),
            writer,
        };
        let greeting = client.read_message(false)?;
        if greeting.get("QMP").is_none() {
            bail!("invalid QMP greeting: {greeting}");
        }
        let response = client.cmd(json!({"execute": "qmp_capabilities"}))?;
        if response.get("return").is_none() {
            bail!("QMP capabilities negotiation failed: {response}");
        }
        Ok(client)
    }

    pub fn reconnect(&mut self) -> Result<()> {
        let replacement = Self::connect(&self.host, self.port)?;
        *self = replacement;
        Ok(())
    }

    pub fn cmd(&mut self, command: Value) -> Result<Value> {
        let mut bytes = serde_json::to_vec(&command)?;
        bytes.push(b'\n');
        self.writer.write_all(&bytes)?;
        self.writer.flush()?;
        self.read_message(false)
    }

    pub fn human_monitor_command(&mut self, command_line: &str) -> Result<String> {
        let response = self.cmd(json!({
            "execute": "human-monitor-command",
            "arguments": {"command-line": command_line}
        }))?;
        response
            .get("return")
            .and_then(Value::as_str)
            .map(|value| value.replace('\r', ""))
            .ok_or_else(|| anyhow!("unexpected QMP response for {command_line:?}: {response}"))
    }

    pub fn gva2gpa(&mut self, addr: u64) -> Result<u64> {
        let response = self.human_monitor_command(&format!("gva2gpa {addr}"))?;
        let data = response
            .lines()
            .filter_map(|line| line.split_once("gpa: ").map(|(_, rhs)| rhs.trim()))
            .collect::<Vec<_>>()
            .join(" ");
        parse_hex_u64(&data).with_context(|| {
            format!("failed to parse gva2gpa response for {addr:#x}: {response:?}")
        })
    }

    pub fn gpa2hva(&mut self, addr: u64) -> Result<u64> {
        let response = self.human_monitor_command(&format!("gpa2hva {addr}"))?;
        let data = response
            .lines()
            .filter_map(|line| line.split_once(" is ").map(|(_, rhs)| rhs.trim()))
            .collect::<Vec<_>>()
            .join(" ");
        parse_hex_u64(&data).with_context(|| {
            format!("failed to parse gpa2hva response for {addr:#x}: {response:?}")
        })
    }

    pub fn gva2hva(&mut self, addr: u64) -> Result<u64> {
        let gpa = self.gva2gpa(addr)?;
        self.gpa2hva(gpa)
    }

    fn read_message(&mut self, only_event: bool) -> Result<Value> {
        loop {
            let mut line = String::new();
            let bytes = self.reader.read_line(&mut line)?;
            if bytes == 0 {
                bail!("QMP disconnected");
            }
            let response: Value = serde_json::from_str(&line)?;
            if response.get("event").is_some() && !only_event {
                continue;
            }
            return Ok(response);
        }
    }
}

fn parse_hex_u64(value: &str) -> Result<u64> {
    let value = value.trim().trim_start_matches("0x");
    if value.is_empty() {
        bail!("empty hex string")
    }
    Ok(u64::from_str_radix(value, 16)?)
}
