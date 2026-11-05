//! Argv-only command execution behind an injectable [`CommandRunner`].
//! No shell is ever involved: every value is its own argument.
//! [`ProcessRunner`] is the only place in this crate that spawns processes.

#![allow(clippy::disallowed_methods)]

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

/// One command to run: program, argv, working dir, extra env, time limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
    /// `None` runs in the caller's current directory.
    pub cwd: Option<PathBuf>,
    /// Added to (not replacing) the inherited environment.
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
}

/// Result of a finished command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    /// Exit code; `None` when killed by a signal.
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

/// Runs commands; injected so tests never spawn a process.
pub trait CommandRunner {
    /// `Err` only when the command could not run to completion (not found,
    /// timeout, I/O); a non-zero exit is an `Ok` with that status.
    fn run(&self, cmd: &Command) -> Result<Output, String>;
}

/// Runs commands as real child processes: stdin closed, both pipes
/// drained concurrently, the whole process group killed on timeout.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn run(&self, cmd: &Command) -> Result<Output, String> {
        let mut command = std::process::Command::new(&cmd.program);
        command
            .args(&cmd.args)
            .envs(cmd.env.iter().map(|(k, v)| (k, v)))
            // npx spawns node as a grandchild; only a group kill reaches it.
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &cmd.cwd {
            command.current_dir(cwd);
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("{}: {e}", cmd.program))?;
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let mut remaining = cmd.timeout;
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            if remaining.is_zero() {
                // SAFETY: plain syscall on our own child's process group.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{} timed out after {}s",
                    cmd.program,
                    cmd.timeout.as_secs()
                ));
            }
            let step = remaining.min(Duration::from_millis(20));
            std::thread::sleep(step);
            remaining = remaining.saturating_sub(step);
        };
        Ok(Output {
            status: status.code(),
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        })
    }
}

fn drain<R: Read + Send + 'static>(stream: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut stream) = stream {
            let _ = stream.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}
