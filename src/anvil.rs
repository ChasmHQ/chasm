use std::net::TcpListener;
use std::process::{Command, Child};

/// Find an available TCP port starting at `preferred`, scanning upward.
/// On Windows, Hyper-V/WSL2 reserves certain port ranges causing WSAEACCES (os error 10013).
/// This avoids those ports by probing with a real bind before passing the port to Anvil.
pub fn find_available_port(preferred: u16) -> u16 {
    for port in preferred..=preferred.saturating_add(100) {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    // Fallback: let OS pick any free port
    let listener = TcpListener::bind("127.0.0.1:0").expect("No free port found");
    listener.local_addr().unwrap().port()
}

pub struct AnvilNode {
    process: Option<Child>,
    port: u16,
    fork_url: Option<String>,
    fork_block: Option<u64>,
}

impl AnvilNode {
    pub fn new(port: u16) -> Self {
        Self { process: None, port, fork_url: None, fork_block: None }
    }

    pub fn start(&mut self) -> anyhow::Result<()> {
        let child = Command::new("anvil")
            .arg("--port")
            .arg(self.port.to_string())
            .spawn()?;
        
        self.process = Some(child);
        self.fork_url = None;
        self.fork_block = None;
        Ok(())
    }

    pub fn start_fork(&mut self, fork_url: String, fork_block: Option<u64>) -> anyhow::Result<()> {
        let mut cmd = Command::new("anvil");
        cmd.arg("--port").arg(self.port.to_string());
        cmd.arg("--fork-url").arg(&fork_url);
        if let Some(block) = fork_block {
            cmd.arg("--fork-block-number").arg(block.to_string());
        }

        let child = cmd.spawn()?;
        self.process = Some(child);
        self.fork_url = Some(fork_url);
        self.fork_block = fork_block;
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
        }
    }

    pub fn is_running(&self) -> bool {
        self.process.is_some()
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn fork_info(&self) -> (Option<String>, Option<u64>) {
        (self.fork_url.clone(), self.fork_block)
    }
}

impl Drop for AnvilNode {
    fn drop(&mut self) {
        self.stop();
    }
}
