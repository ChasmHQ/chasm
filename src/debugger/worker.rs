use super::{
    config::{Config, MAX_REQUEST, MAX_RESPONSE, PROTOCOL},
    GatewayError,
};
use serde_json::{json, Value};
use std::{process::Stdio, sync::Arc};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

/// The child handle is deliberately independent of the serialized JSONL driver.
/// Cancellation must never wait for a wedged engine response or stdin lock.
pub(super) struct Killer(tokio::sync::Mutex<Option<ProcessTree>>);

struct ProcessTree {
    child: Child,
    #[cfg(windows)]
    job: WindowsJob,
}

impl ProcessTree {
    fn terminate(&mut self) {
        #[cfg(windows)]
        self.job.terminate();
        #[cfg(unix)]
        if let Some(pid) = self.child.id() {
            // The unreaped child owns this process-group id; never kill by name.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
    }
}

impl Killer {
    pub async fn kill(&self) {
        let mut guard = self.0.lock().await;
        if let Some(tree) = guard.as_mut() {
            tree.terminate();
            let _ = tree.child.wait().await;
        }
        guard.take();
    }
}

impl Drop for Killer {
    fn drop(&mut self) {
        if let Some(mut tree) = self.0.get_mut().take() {
            tree.terminate();
        }
    }
}

#[cfg(windows)]
struct WindowsJob(usize);

#[cfg(windows)]
impl WindowsJob {
    fn attach(child: &Child) -> Result<Self, GatewayError> {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::{JobObjects::*, Threading::*},
        };
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(GatewayError::worker(
                    "Could not create debugger process job.",
                ));
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            );
            let process = OpenProcess(
                PROCESS_SET_QUOTA | PROCESS_TERMINATE,
                0,
                child.id().unwrap(),
            );
            let assigned = !process.is_null() && AssignProcessToJobObject(job, process) != 0;
            if !process.is_null() {
                CloseHandle(process);
            }
            if configured == 0 || !assigned {
                CloseHandle(job);
                return Err(GatewayError::worker(
                    "Could not contain debugger subprocess in a process job.",
                ));
            }
            Ok(Self(job as usize))
        }
    }

    fn terminate(&self) {
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0 as _, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0 as _);
        }
    }
}

struct Driver {
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

pub(super) struct Worker {
    driver: tokio::sync::Mutex<Driver>,
    pub killer: Arc<Killer>,
    stderr: tokio::task::JoinHandle<()>,
    secrets: Vec<String>,
}

impl Worker {
    pub fn spawn(config: &Config, provider_env: Option<&str>) -> Result<Arc<Self>, GatewayError> {
        // Resolve relative overrides before changing the child's working directory,
        // but keep the venv's interpreter symlink: Python finds pyvenv.cfg beside it.
        let python = std::path::absolute(&config.python).map_err(|_| GatewayError::unavailable())?;
        if !python.is_file() {
            return Err(GatewayError::unavailable());
        }
        let mut command = Command::new(python);
        command.arg("-I").arg("-u");
        #[cfg(test)]
        if let Some(script) = &config.worker_script {
            command.arg(script);
        } else {
            command.arg("-m").arg("chasm_debugger.web_service");
        }
        #[cfg(not(test))]
        command.arg("-m").arg("chasm_debugger.web_service");
        // Never import Python modules from an uploaded resource or analyzed project.
        command
            .current_dir(&config.service_dir)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for key in [
            "PATH",
            "SystemRoot",
            "WINDIR",
            "COMSPEC",
            "PATHEXT",
            "HOME",
            "USERPROFILE",
            "TEMP",
            "TMP",
            "TMPDIR",
            "LANG",
            "LC_ALL",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
        ] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        // Only the selected server-configured provider alias gets a credential.
        let mut secrets = Vec::new();
        for name in config.providers.values() {
            if let Ok(value) = std::env::var(name) {
                if !value.is_empty() {
                    secrets.push(value);
                }
            }
            // PATH and other runtime keys are rejected by Config validation.
            command.env_remove(name);
        }
        if let Some(name) = provider_env {
            let value = std::env::var(name).map_err(|_| {
                GatewayError::bad("Configured provider environment variable is not set.")
            })?;
            if value.is_empty() {
                return Err(GatewayError::bad(
                    "Configured provider environment variable is empty.",
                ));
            }
            command.env(name, value);
        }
        #[cfg(unix)]
        command.process_group(0);
        // Hide the service's console window when spawned from the desktop build;
        // piped stdio is unaffected, so the JSONL driver still works the same.
        #[cfg(windows)]
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        let mut child = command.spawn().map_err(|_| GatewayError::unavailable())?;
        #[cfg(windows)]
        let job = match WindowsJob::attach(&child) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.start_kill();
                return Err(error);
            }
        };
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let mut stderr = child.stderr.take().unwrap();
        // Drain fixed-size chunks, deliberately retain/log none of the possibly secret output.
        let stderr = tokio::spawn(async move {
            let mut chunk = [0u8; 4096];
            while let Ok(count) = stderr.read(&mut chunk).await {
                if count == 0 {
                    break;
                }
            }
        });
        Ok(Arc::new(Self {
            driver: tokio::sync::Mutex::new(Driver {
                input,
                output,
                next_id: 0,
            }),
            killer: Arc::new(Killer(tokio::sync::Mutex::new(Some(ProcessTree {
                child,
                #[cfg(windows)]
                job,
            })))),
            stderr,
            secrets,
        }))
    }

    pub async fn hello(&self) -> Result<Value, GatewayError> {
        let hello = self.request("hello", json!({})).await?;
        if hello.get("protocol").and_then(Value::as_str) != Some(PROTOCOL) {
            return Err(GatewayError::worker(
                "Debugger service protocol mismatch; update the local debugger installation.",
            ));
        }
        if hello.get("available").and_then(Value::as_bool) == Some(false) {
            return Err(GatewayError::worker("Debugger native engine is unavailable; rebuild/install the local chasm-debugger environment."));
        }
        Ok(hello)
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, GatewayError> {
        let mut driver = self.driver.lock().await;
        driver.next_id += 1;
        let id = driver.next_id.to_string();
        let mut request =
            serde_json::to_vec(&json!({"id": id, "method": method, "params": params}))
                .map_err(|_| GatewayError::bad("Request could not be serialized."))?;
        if request.len() > MAX_REQUEST {
            return Err(GatewayError::bad(
                "Debugger command exceeds the 256 KiB service limit.",
            ));
        }
        request.push(b'\n');
        driver
            .input
            .write_all(&request)
            .await
            .map_err(|_| GatewayError::worker("Debugger service input closed."))?;
        driver
            .input
            .flush()
            .await
            .map_err(|_| GatewayError::worker("Debugger service input closed."))?;
        let response = bounded_line(&mut driver.output).await?;
        let mut value: Value = serde_json::from_slice(&response)
            .map_err(|_| GatewayError::worker("Debugger service returned invalid JSON."))?;
        if value.get("id").and_then(Value::as_str) != Some(&id) {
            return Err(GatewayError::worker(
                "Debugger service response id mismatch.",
            ));
        }
        scrub(&mut value, &self.secrets);
        if let Some(error) = value.get("error") {
            let code = error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("operation_failed");
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Debugger operation failed.");
            return Err(GatewayError::service(code, message));
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| GatewayError::worker("Debugger service response has no result."))
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stderr.abort();
    }
}

async fn bounded_line(reader: &mut BufReader<ChildStdout>) -> Result<Vec<u8>, GatewayError> {
    let mut line = Vec::new();
    loop {
        let bytes = reader
            .fill_buf()
            .await
            .map_err(|_| GatewayError::worker("Debugger service output failed."))?;
        if bytes.is_empty() {
            return Err(GatewayError::worker(
                "Debugger service exited before responding.",
            ));
        }
        let end = bytes.iter().position(|b| *b == b'\n');
        let count = end.map_or(bytes.len(), |index| index + 1);
        if line.len() + count > MAX_RESPONSE {
            return Err(GatewayError::worker(
                "Debugger service response exceeds the 16 MiB limit.",
            ));
        }
        line.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(line);
        }
    }
}

fn scrub(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(text) => {
            for secret in secrets {
                *text = text.replace(secret, "[redacted]");
            }
            // Service diagnostics may include an RPC URL not equal to the whole env value.
            for prefix in ["https://", "http://", "wss://", "ws://"] {
                while let Some(start) = text.find(prefix) {
                    let end = text[start..]
                        .find(|c: char| c.is_whitespace() || c == '\"' || c == '\'')
                        .map_or(text.len(), |n| start + n);
                    text.replace_range(start..end, "[redacted-url]");
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| scrub(item, secrets)),
        Value::Object(items) => items.values_mut().for_each(|item| scrub(item, secrets)),
        _ => {}
    }
}
