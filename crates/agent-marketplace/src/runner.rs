//! Argv-only command execution behind an injectable [`CommandRunner`].
//! No shell is ever involved: every value is its own argument.
//! [`ProcessRunner`] is the only place in this crate that spawns processes.

#![allow(clippy::disallowed_methods)]

use std::fmt;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Shared cancellation flag: clones observe the same flag, so a UI thread
/// can stop a command another thread is running.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> CancelToken {
        CancelToken::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Two tokens are equal when they share one flag.
impl PartialEq for CancelToken {
    fn eq(&self, other: &CancelToken) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for CancelToken {}

/// One command to run: program, argv, working dir, extra env, time limit,
/// cancellation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub program: String,
    pub args: Vec<String>,
    /// `None` runs in the caller's current directory.
    pub cwd: Option<PathBuf>,
    /// Added to (not replacing) the inherited environment.
    pub env: Vec<(String, String)>,
    /// One deadline for the process to exit and its pipes to close.
    pub timeout: Duration,
    pub cancel: CancelToken,
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

/// Why a command did not run to completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// It could not be started (not found, permission, bad cwd).
    Spawn(String),
    /// The deadline passed before the process exited.
    Timeout(Duration),
    /// The cancel token was set.
    Cancelled,
    /// Waiting on the process failed.
    Io(String),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::Spawn(msg) => write!(f, "could not start: {msg}"),
            RunError::Timeout(after) => write!(f, "timed out after {}s", after.as_secs_f32()),
            RunError::Cancelled => f.write_str("cancelled"),
            RunError::Io(msg) => write!(f, "I/O error: {msg}"),
        }
    }
}

impl std::error::Error for RunError {}

/// Runs commands; injected so tests never spawn a process.
pub trait CommandRunner {
    /// `Err` only when the command did not run to completion; a non-zero
    /// exit is an `Ok` with that status.
    fn run(&self, cmd: &Command) -> Result<Output, RunError>;
}

/// Runs commands as real child processes in their own process group:
/// stdin closed, both pipes drained concurrently.
///
/// One deadline covers the child's exit and its pipes closing. On timeout
/// or cancel the whole group is killed. A child that exited while a
/// descendant still holds its pipes yields its own status and the output
/// read before the deadline.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn run(&self, cmd: &Command) -> Result<Output, RunError> {
        if cmd.cancel.is_cancelled() {
            return Err(RunError::Cancelled);
        }
        let deadline = Instant::now() + cmd.timeout;
        let mut command = std::process::Command::new(&cmd.program);
        command
            .args(&cmd.args)
            .envs(cmd.env.iter().map(|(k, v)| (k, v)))
            // npx may spawn node as a grandchild; only a group kill reaches it.
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = &cmd.cwd {
            command.current_dir(cwd);
        }
        let mut child = command
            .spawn()
            .map_err(|e| RunError::Spawn(format!("{}: {e}", cmd.program)))?;
        let stdout = Pipe::drain(child.stdout.take());
        let stderr = Pipe::drain(child.stderr.take());
        let mut status = None;
        loop {
            if status.is_none() {
                match child.try_wait() {
                    Ok(Some(exit)) => status = Some(exit),
                    Ok(None) => {}
                    Err(e) => {
                        kill_group(&mut child);
                        return Err(RunError::Io(e.to_string()));
                    }
                }
            }
            if let Some(exit) = status
                && stdout.done()
                && stderr.done()
            {
                return Ok(Output {
                    status: exit.code(),
                    stdout: stdout.take(),
                    stderr: stderr.take(),
                });
            }
            if cmd.cancel.is_cancelled() {
                kill_group(&mut child);
                return Err(RunError::Cancelled);
            }
            if Instant::now() >= deadline {
                kill_group(&mut child);
                return match status {
                    // The child itself finished; only a descendant held the
                    // pipes. Its result stands with what was read.
                    Some(exit) => Ok(Output {
                        status: exit.code(),
                        stdout: stdout.take(),
                        stderr: stderr.take(),
                    }),
                    None => Err(RunError::Timeout(cmd.timeout)),
                };
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// SIGKILL the child's process group (descendants included) and reap the
/// child. Reader threads end once the killed processes' pipes close; they
/// are detached, never joined, so a stray holder cannot block the caller.
fn kill_group(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: plain syscall on the process group we created for the
        // child (`process_group(0)` makes its pid the group id).
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// A pipe drained on its own thread into a shared buffer, readable at any
/// time without joining the thread.
struct Pipe {
    buf: Arc<Mutex<Vec<u8>>>,
    done: Arc<AtomicBool>,
}

impl Pipe {
    fn drain<R: Read + Send + 'static>(stream: Option<R>) -> Pipe {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(AtomicBool::new(false));
        let (thread_buf, thread_done) = (Arc::clone(&buf), Arc::clone(&done));
        std::thread::spawn(move || {
            if let Some(mut stream) = stream {
                let mut chunk = [0u8; 8192];
                loop {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => match thread_buf.lock() {
                            Ok(mut buf) => buf.extend_from_slice(&chunk[..n]),
                            Err(_) => break,
                        },
                    }
                }
            }
            thread_done.store(true, Ordering::SeqCst);
        });
        Pipe { buf, done }
    }

    fn done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    fn take(&self) -> String {
        let bytes = self.buf.lock().map(|b| b.clone()).unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}
