use std::net::TcpListener;
use std::process::{Child, Command, Stdio};

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
    /// Kills the node when Chasm exits by any path, including a hard kill
    /// that skips `Drop`. Declared after `process` so it closes last.
    #[cfg(windows)]
    job: Option<KillOnCloseJob>,
    port: u16,
    fork_url: Option<String>,
    fork_block: Option<u64>,
    silent: bool,
}

impl AnvilNode {
    pub fn new(port: u16) -> Self {
        Self {
            process: None,
            #[cfg(windows)]
            job: None,
            port,
            fork_url: None,
            fork_block: None,
            silent: false,
        }
    }

    /// Spawn the node without inheriting a console: null stdio, and on Windows
    /// `CREATE_NO_WINDOW` so desktop builds never flash a terminal window.
    pub fn new_silent(port: u16) -> Self {
        let mut node = Self::new(port);
        node.silent = true;
        node
    }

    pub fn start(&mut self) -> anyhow::Result<()> {
        self.ensure_not_running()?;
        let child = self.base_command().spawn()?;
        self.adopt(child)?;
        self.fork_url = None;
        self.fork_block = None;
        Ok(())
    }

    pub fn start_fork(&mut self, fork_url: String, fork_block: Option<u64>) -> anyhow::Result<()> {
        self.ensure_not_running()?;
        let mut cmd = self.base_command();
        apply_fork_args(&mut cmd, &fork_url, fork_block);

        let child = cmd.spawn()?;
        self.adopt(child)?;
        self.fork_url = Some(fork_url);
        self.fork_block = fork_block;
        Ok(())
    }

    /// Contain the child before keeping it. A node that cannot be contained
    /// is killed rather than left able to outlive Chasm on the shared port.
    fn adopt(&mut self, mut child: Child) -> anyhow::Result<()> {
        #[cfg(windows)]
        match KillOnCloseJob::containing(&child) {
            Ok(job) => self.job = Some(job),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("Could not contain Anvil in a process job: {error}");
            }
        }
        self.process = Some(child);
        Ok(())
    }

    /// Shared `anvil` invocation for `start`/`start_fork`. Console output is
    /// kept unless the node was built silent, so existing callers are unchanged.
    fn base_command(&self) -> Command {
        let mut cmd = Command::new("anvil");
        cmd.arg("--port").arg(self.port.to_string());
        if self.silent {
            // The owned workspace node records opcode steps so mined
            // transactions can be stepped through in the debugger.
            cmd.arg("--steps-tracing");
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
            }
        }
        cmd
    }

    /// Refuse a second spawn while a live child is held; an exited child is
    /// reaped so the caller can retry after "exited before becoming ready".
    fn ensure_not_running(&mut self) -> anyhow::Result<()> {
        if let Some(child) = self.process.as_mut() {
            if child.try_wait()?.is_none() {
                anyhow::bail!("Anvil is already running on port {}.", self.port);
            }
            self.process = None;
        }
        Ok(())
    }

    pub fn duplicate_process_handle(&self) -> anyhow::Result<usize> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS};
            use windows_sys::Win32::System::Threading::GetCurrentProcess;
            let child = self
                .process
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Anvil is not running."))?;
            let mut duplicate = std::ptr::null_mut();
            if unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    child.as_raw_handle(),
                    GetCurrentProcess(),
                    &mut duplicate,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(duplicate as usize)
        }
        #[cfg(not(windows))]
        {
            Err(anyhow::anyhow!(
                "Owned signing process handles are not available on this platform."
            ))
        }
    }

    pub fn check_process(&mut self) -> anyhow::Result<()> {
        let process = self
            .process
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Anvil is not running."))?;
        if let Some(status) = process.try_wait()? {
            anyhow::bail!("Anvil exited before becoming ready ({status}).");
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        #[cfg(windows)]
        {
            self.job = None;
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

fn apply_fork_args(cmd: &mut Command, fork_url: &str, fork_block: Option<u64>) {
    cmd.arg("--fork-url").arg(fork_url);
    // Keep the local chain id: signing is restricted to 31337, so a fork can
    // never produce a transaction that is valid on the real network.
    cmd.arg("--chain-id").arg("31337");
    if let Some(block) = fork_block {
        cmd.arg("--fork-block-number").arg(block.to_string());
    }
}

impl Drop for AnvilNode {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Held only for its handle: closing it is what kills the contained node.
#[cfg(windows)]
struct KillOnCloseJob(#[allow(dead_code)] std::os::windows::io::OwnedHandle);

#[cfg(windows)]
impl KillOnCloseJob {
    fn containing(child: &Child) -> std::io::Result<Self> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if configured == 0
            || unsafe { AssignProcessToJobObject(handle.as_raw_handle(), child.as_raw_handle()) } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(handle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn console_mode_is_default_and_silent_is_opt_in() {
        assert!(!AnvilNode::new(8545).silent);
        assert!(AnvilNode::new_silent(8545).silent);
    }

    #[test]
    fn base_command_carries_the_port() {
        let cmd = AnvilNode::new(8545).base_command();
        assert_eq!(cmd.get_program().to_string_lossy(), "anvil");
        assert_eq!(args(&cmd), ["--port", "8545"]);
    }

    #[test]
    fn silent_builder_changes_only_stdio_not_args() {
        let console = AnvilNode::new(8545).base_command();
        let silent = AnvilNode::new_silent(8545).base_command();
        assert_eq!(args(&console), ["--port", "8545"]);
        assert_eq!(args(&silent), ["--port", "8545", "--steps-tracing"]);
    }

    #[test]
    fn fork_arguments_survive_the_shared_builder() {
        let mut cmd = Command::new("anvil");
        apply_fork_args(&mut cmd, "https://example.test/rpc", Some(42));
        assert_eq!(
            args(&cmd),
            [
                "--fork-url",
                "https://example.test/rpc",
                "--chain-id",
                "31337",
                "--fork-block-number",
                "42"
            ]
        );

        let mut cmd = Command::new("anvil");
        apply_fork_args(&mut cmd, "https://example.test/rpc", None);
        assert_eq!(args(&cmd), ["--fork-url", "https://example.test/rpc", "--chain-id", "31337"]);
    }
}
