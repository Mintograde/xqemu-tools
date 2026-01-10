use tokio::net::TcpStream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use serde_json::Value;
use anyhow::{Result, Context, anyhow};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct QmpClient {
    // We wrap BufReader<TcpStream> in Mutex to preserve the buffer state across calls
    stream: Arc<Mutex<BufReader<TcpStream>>>,
}

impl QmpClient {
    pub async fn connect(host: &str, port: u16) -> Result<Self> {
        let addr = format!("{}:{}", host, port);
        let stream = TcpStream::connect(&addr).await.context("Failed to connect to QMP")?;
        
        let client = Self {
            stream: Arc::new(Mutex::new(BufReader::new(stream))),
        };

        client.negotiate().await?;
        Ok(client)
    }

    async fn negotiate(&self) -> Result<()> {
        let mut reader = self.stream.lock().await;
        let mut line = String::new();

        // Read greeting
        reader.read_line(&mut line).await?;
        let greeting: Value = serde_json::from_str(&line)?;
        if greeting.get("QMP").is_none() {
            return Err(anyhow!("Invalid QMP greeting: {}", line));
        }

        // Send capabilities negotiation
        let cmd = r#"{"execute": "qmp_capabilities"}"#;
        reader.get_mut().write_all(cmd.as_bytes()).await?;
        reader.get_mut().write_all(b"\n").await?;

        // Read response
        line.clear();
        reader.read_line(&mut line).await?;
        let response: Value = serde_json::from_str(&line)?;
        
        if response.get("return").is_some() {
            Ok(())
        } else {
            Err(anyhow!("QMP capabilities negotiation failed: {}", line))
        }
    }

    pub async fn execute(&self, command: &str, args: Option<serde_json::Value>) -> Result<Value> {
        let mut cmd_obj = serde_json::json!({
            "execute": command
        });
        
        if let Some(a) = args {
            cmd_obj["arguments"] = a;
        }

        let cmd_str = serde_json::to_string(&cmd_obj)?;
        
        let mut reader = self.stream.lock().await;
        
        reader.get_mut().write_all(cmd_str.as_bytes()).await?;
        reader.get_mut().write_all(b"\n").await?;

        // Read until we get a return or error, skipping events
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await?;
            if line.trim().is_empty() {
                continue;
            }

            // Detect and skip non-JSON lines if any (shouldn't be, but good for robustness)
            let response: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue, 
            };
            
            if response.get("event").is_some() {
                // Ignore events for now
                continue;
            }

            if let Some(ret) = response.get("return") {
                return Ok(ret.clone());
            }

            if let Some(err) = response.get("error") {
                return Err(anyhow!("QMP error: {:?}", err));
            }
        }
    }

    pub async fn human_monitor_command(&self, command_line: &str) -> Result<String> {
        let args = serde_json::json!({
            "command-line": command_line
        });
        let ret = self.execute("human-monitor-command", Some(args)).await?;
        ret.as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("human-monitor-command returned non-string"))
    }

    pub async fn gva2gpa(&self, addr: u32) -> Result<u32> {
        let cmd = format!("gva2gpa 0x{:x}", addr);
        let output = self.human_monitor_command(&cmd).await?;
        
        // Expected output: "gpa: 0x..."
        // Or error
        
        if let Some(val_str) = output.trim().strip_prefix("gpa: ") {
             let val_str = val_str.trim().trim_start_matches("0x");
             return u32::from_str_radix(val_str, 16).with_context(|| format!("Failed to parse GPA from output: {}", output));
        }

        Err(anyhow!("Failed to parse gva2gpa output: {}", output))
    }

    pub async fn gpa2hva(&self, addr: u32) -> Result<u64> {
        let cmd = format!("gpa2hva 0x{:x}", addr);
        let output = self.human_monitor_command(&cmd).await?;
        
        // Output format: "Host virtual address for 0x2fad20 (xbox.ram) is 000000001262ad20"
        
        if output.contains("No memory is mapped") {
            return Err(anyhow!("No memory mapped at 0x{:x}", addr));
        }

        let parts: Vec<&str> = output.trim().split_whitespace().collect();
        if let Some(last) = parts.last() {
             let val_str = last.trim_start_matches("0x");
             return u64::from_str_radix(val_str, 16).with_context(|| format!("Failed to parse HVA from output: {}", output));
        }

        Err(anyhow!("Failed to parse gpa2hva output: {}", output))
    }

    pub async fn gva2hva(&self, addr: u32) -> Result<u64> {
        let gpa = self.gva2gpa(addr).await?;
        self.gpa2hva(gpa).await
    }
}