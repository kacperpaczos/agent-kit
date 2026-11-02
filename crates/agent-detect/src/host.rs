//! The only layer allowed to touch processes, environment variables, and
//! the clock.

#![allow(clippy::disallowed_methods)]
#[cfg(any(test, feature = "test-support"))]
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
#[cfg(any(test, feature = "test-support"))]
use std::sync::MutexGuard;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Result of a finished child process.
#[derive(Debug, Clone, PartialEq)]
pub struct CmdOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Why a program could not be run to completion.
#[derive(Debug, Clone, PartialEq)]
pub enum SpawnError {
    NotFound,
    PermissionDenied,
    Timeout,
    Other(String),
}

/// A streaming child to launch: program, args, PATH, working dir, stdin.
#[derive(Debug, Clone, PartialEq)]
pub struct SpawnSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub path_env: Vec<PathBuf>,
    pub cwd: PathBuf,
    pub stdin: String,
}

/// A running child whose stdout is consumed line by line.
pub trait ChildProcess: Send {
    /// Next stdout line without the trailing newline; `None` at EOF.
    fn next_line(&mut self) -> Option<String>;
    /// Everything the child wrote to stderr so far (drained in a thread).
    fn stderr(&mut self) -> String;
    fn kill(&mut self);
    /// Exit status after EOF; `None` when killed by a signal.
    fn wait(&mut self) -> Option<i32>;
    /// A handle that kills the child from another thread while
    /// `next_line` is blocked on a silent agent.
    fn killer(&self) -> Arc<dyn Fn() + Send + Sync>;
}

/// Host abstraction: processes, env vars, home, cwd, clock.
///
/// Tests substitute [`FakeHost`]; the app uses [`NativeHost`].
pub trait Host: Send + Sync {
    fn home(&self) -> PathBuf;
    /// Value of an environment variable; an empty value counts as unset.
    fn var(&self, key: &str) -> Option<String>;
    fn cwd(&self) -> Option<PathBuf>;
    fn now(&self) -> SystemTime;
    /// Runs `program` with `args`, PATH set to `path_env` joined by ':',
    /// stdin closed, killed after `timeout`.
    fn run(
        &self,
        program: &Path,
        args: &[String],
        path_env: &[PathBuf],
        timeout: Duration,
    ) -> std::result::Result<CmdOutput, SpawnError>;
    /// Streaming variant of [`Host::run`]: the caller reads stdout line
    /// by line while the child runs. Hosts that never stream may keep the
    /// default refusal.
    fn spawn(&self, _spec: &SpawnSpec) -> Result<Box<dyn ChildProcess>, SpawnError> {
        Err(SpawnError::Other(
            "streaming spawn not supported".to_string(),
        ))
    }
    /// `$SHELL -lc 'printf %s "$PATH"'` with stdin from `/dev/null`; `None`
    /// on timeout/failure.
    fn login_shell_path(&self, timeout: Duration) -> Option<Vec<PathBuf>>;
}

/// Real host: direct process spawning and environment access.
#[derive(Debug, Default)]
pub struct NativeHost;

impl Host for NativeHost {
    fn home(&self) -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
    }

    fn var(&self, key: &str) -> Option<String> {
        non_empty(std::env::var(key).ok())
    }

    fn cwd(&self) -> Option<PathBuf> {
        std::env::current_dir().ok()
    }

    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn run(
        &self,
        program: &Path,
        args: &[String],
        path_env: &[PathBuf],
        timeout: Duration,
    ) -> std::result::Result<CmdOutput, SpawnError> {
        let joined =
            std::env::join_paths(path_env).map_err(|e| SpawnError::Other(e.to_string()))?;
        let mut child = Command::new(program)
            .args(args)
            .env("PATH", joined)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(map_spawn_err)?;
        // Drain both pipes concurrently: a child writing more than the OS
        // pipe buffer (~64 KiB) would otherwise block on write, never exit,
        // and turn every large-output command into a timeout.
        let stdout_reader = drain_piped(child.stdout.take());
        let stderr_reader = drain_piped(child.stderr.take());
        // try_wait + 20 ms sleep until the timeout budget is exhausted.
        let mut remaining = timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => return Err(map_spawn_err(e)),
            }
            if remaining.is_zero() {
                let _ = child.kill();
                let _ = child.wait();
                // Reader threads are deliberately not joined: a grandchild
                // may still hold the pipes open.
                return Err(SpawnError::Timeout);
            }
            let step = remaining.min(Duration::from_millis(20));
            std::thread::sleep(step);
            remaining = remaining.saturating_sub(step);
        };
        Ok(CmdOutput {
            status: status.code(),
            stdout: join_reader(stdout_reader),
            stderr: join_reader(stderr_reader),
        })
    }

    fn spawn(&self, spec: &SpawnSpec) -> Result<Box<dyn ChildProcess>, SpawnError> {
        let joined =
            std::env::join_paths(&spec.path_env).map_err(|e| SpawnError::Other(e.to_string()))?;
        let mut child = Command::new(&spec.program)
            .args(&spec.args)
            .env("PATH", joined)
            .current_dir(&spec.cwd)
            // Own process group: npm launchers (codex, opencode) spawn the
            // real agent as a grandchild; only a group kill reaches it.
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(map_spawn_err)?;
        // The whole prompt goes in on its own thread, then the handle drops
        // and closes the pipe (the EOF the agent needs). A synchronous
        // write would deadlock the moment the prompt exceeds the pipe
        // buffer and the child writes stdout/stderr before reading stdin.
        if let Some(mut stdin) = child.stdin.take() {
            let data = spec.stdin.clone();
            std::thread::spawn(move || {
                let _ = stdin.write_all(data.as_bytes());
            });
        }
        let stdout = BufReader::new(child.stdout.take().expect("stdout piped above"));
        let stderr = Arc::new(Mutex::new(String::new()));
        let reader_done = Arc::new(AtomicBool::new(false));
        drain_stderr(
            Some(child.stderr.take().expect("stderr piped above")),
            &stderr,
            &reader_done,
        );
        Ok(Box::new(NativeChild {
            child: Arc::new(Mutex::new(child)),
            stdout,
            stderr,
            reader_done,
        }))
    }
    fn login_shell_path(&self, timeout: Duration) -> Option<Vec<PathBuf>> {
        // `$SHELL` when the environment exports it; GUI sessions sometimes
        // don't, so fall back to bash. A *non-interactive* login shell
        // (`-lc`, never `-lic`): an interactive one would read stdin, and a
        // backgrounded GUI started from a terminal gets group-stopped by
        // its job control (SIGTTIN), wedging the caller. `run` gives the
        // child /dev/null on stdin either way.
        let shell = self.var("SHELL").unwrap_or_else(|| "/bin/bash".to_string());
        let args = vec!["-lc".to_string(), r#"printf %s "$PATH""#.to_string()];
        let path_env: Vec<PathBuf> = self
            .var("PATH")
            .map(|p| p.split(':').map(PathBuf::from).collect())
            .unwrap_or_default();
        let out = self
            .run(Path::new(&shell), &args, &path_env, timeout)
            .ok()?;
        if out.status != Some(0) {
            return None;
        }
        Some(out.stdout.split(':').map(PathBuf::from).collect())
    }
}

/// [`ChildProcess`] backed by a real `std::process::Child`.
struct NativeChild {
    /// Shared so the killer handle can kill while the owner reads stdout.
    /// `wait` locks it only after EOF, so the readers never contend.
    child: Arc<Mutex<Child>>,
    stdout: BufReader<ChildStdout>,
    /// stderr accumulated so far by the drain thread.
    stderr: Arc<Mutex<String>>,
    /// Set when the stderr drain thread finished; `wait` waits on this
    /// flag (bounded) instead of joining the thread.
    reader_done: Arc<AtomicBool>,
}

impl ChildProcess for NativeChild {
    fn next_line(&mut self) -> Option<String> {
        let mut bytes = Vec::new();
        match self.stdout.read_until(b'\n', &mut bytes) {
            Ok(0) | Err(_) => None,
            Ok(_) => {
                if bytes.last() == Some(&b'\n') {
                    bytes.pop();
                    if bytes.last() == Some(&b'\r') {
                        bytes.pop();
                    }
                }
                Some(String::from_utf8_lossy(&bytes).into_owned())
            }
        }
    }

    fn stderr(&mut self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn kill(&mut self) {
        kill_shared(&self.child);
    }

    fn killer(&self) -> Arc<dyn Fn() + Send + Sync> {
        let child = Arc::clone(&self.child);
        Arc::new(move || kill_shared(&child))
    }

    fn wait(&mut self) -> Option<i32> {
        // try_wait polling, bounded: a wedged child (or a grandchild still
        // holding the pipes) must not block this forever.
        let mut deadline = Instant::now() + WAIT_DEADLINE;
        let mut killed = false;
        let status = loop {
            let mut guard = self
                .child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match guard.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(_) => return None,
            }
            drop(guard);
            if Instant::now() >= deadline {
                if killed {
                    return None;
                }
                // Still running after the deadline: do not leave it behind.
                // Kill the group and reap it once it is gone.
                kill_shared(&self.child);
                killed = true;
                deadline = Instant::now() + KILL_ESCALATE + Duration::from_secs(1);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // Give the stderr drain thread a moment to land its tail; after
        // READER_DEADLINE the thread is detached, not joined.
        let deadline = Instant::now() + READER_DEADLINE;
        while !self.reader_done.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        status.code()
    }
}

/// How long `wait` polls `try_wait` before giving up on the child.
const WAIT_DEADLINE: Duration = Duration::from_secs(3);

/// How long `wait` waits for the stderr drain thread after the child exit.
const READER_DEADLINE: Duration = Duration::from_secs(2);

/// Before SIGTERM escalation turns into SIGKILL.
const KILL_ESCALATE: Duration = Duration::from_secs(2);

/// Kill the child behind a shared handle; a dead child is not an error.
/// SIGTERM goes to the whole process group (npm launchers run the real
/// agent as a grandchild); a thread escalates to SIGKILL if the group is
/// still alive 2 s later.
fn kill_shared(child: &Mutex<Child>) {
    let pgid = {
        let guard = child
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.id() as i32
    };
    signal_group(pgid, libc::SIGTERM);
    std::thread::spawn(move || {
        let deadline = Instant::now() + KILL_ESCALATE;
        while Instant::now() < deadline {
            if !group_alive(pgid) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        signal_group(pgid, libc::SIGKILL);
    });
}

fn signal_group(pgid: i32, sig: i32) {
    // Errors (ESRCH once the group is gone) are fine: killing a dead
    // process is not an error.
    unsafe {
        libc::killpg(pgid, sig);
    }
}

fn group_alive(pgid: i32) -> bool {
    unsafe { libc::killpg(pgid, 0) == 0 }
}

/// Append a piped stream to `sink` chunk by chunk on a dedicated thread.
fn drain_stderr<R: Read + Send + 'static>(
    stream: Option<R>,
    sink: &Arc<Mutex<String>>,
    done: &Arc<AtomicBool>,
) {
    let sink = Arc::clone(sink);
    let done = Arc::clone(done);
    std::thread::spawn(move || {
        let mut stream = match stream {
            Some(s) => s,
            None => {
                done.store(true, Ordering::SeqCst);
                return;
            }
        };
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut guard = sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    guard.push_str(&String::from_utf8_lossy(&chunk[..n]));
                }
            }
        }
        done.store(true, Ordering::SeqCst);
    });
}

/// One `script_stream` entry: program, stdout lines, stderr, exit status.
#[cfg(any(test, feature = "test-support"))]
type StreamScript = (PathBuf, Vec<String>, String, Option<i32>);

/// Test double for [`Host`]: fresh temp HOME, scripted runs, recorded calls.
///
/// Enabled by the `test-support` feature.
#[cfg(any(test, feature = "test-support"))]
pub struct FakeHost {
    home: tempfile::TempDir,
    vars: HashMap<String, String>,
    cwd: Option<PathBuf>,
    now: SystemTime,
    login_path: Option<Vec<PathBuf>>,
    scripts: Vec<(
        PathBuf,
        Vec<String>,
        std::result::Result<CmdOutput, SpawnError>,
    )>,
    /// Streamed children by program: stdout lines, stderr, exit status.
    streams: Mutex<Vec<StreamScript>>,
    calls: Mutex<Vec<(PathBuf, Vec<String>)>>,
    spawn_specs: Mutex<Vec<SpawnSpec>>,
}

#[cfg(any(test, feature = "test-support"))]
impl FakeHost {
    /// New `FakeHost` with an empty temporary HOME and no scripted runs.
    #[allow(clippy::new_without_default)] // test helper: Default would just duplicate new()
    pub fn new() -> FakeHost {
        FakeHost {
            home: tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}")),
            vars: HashMap::new(),
            cwd: None,
            now: SystemTime::UNIX_EPOCH,
            login_path: None,
            scripts: Vec::new(),
            streams: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            spawn_specs: Mutex::new(Vec::new()),
        }
    }

    pub fn home_path(&self) -> &Path {
        self.home.path()
    }

    pub fn set_var(&mut self, key: &str, value: &str) {
        self.vars.insert(key.to_string(), value.to_string());
    }

    pub fn set_cwd(&mut self, cwd: Option<PathBuf>) {
        self.cwd = cwd;
    }

    pub fn set_now(&mut self, now: SystemTime) {
        self.now = now;
    }

    pub fn set_login_path(&mut self, login_path: Option<Vec<PathBuf>>) {
        self.login_path = login_path;
    }

    /// Make `run(program, args)` return `result`.
    pub fn script(
        &mut self,
        program: impl Into<PathBuf>,
        args: &[&str],
        result: std::result::Result<CmdOutput, SpawnError>,
    ) {
        self.scripts.push((
            program.into(),
            args.iter().map(|a| a.to_string()).collect(),
            result,
        ));
    }

    /// Make `spawn(program, ..)` yield `stdout_lines`, then `stderr`, then
    /// `status` (first script whose program matches).
    pub fn script_stream(
        &self,
        program: impl Into<PathBuf>,
        stdout_lines: Vec<String>,
        stderr: &str,
        status: Option<i32>,
    ) {
        self.streams
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push((program.into(), stdout_lines, stderr.to_string(), status));
    }

    /// All `spawn` invocations so far, in order (specs include stdin).
    pub fn spawned(&self) -> Vec<SpawnSpec> {
        self.spawn_specs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// All `run` invocations so far, in order.
    pub fn calls(&self) -> Vec<(PathBuf, Vec<String>)> {
        self.locked_calls().clone()
    }

    fn locked_calls(&self) -> MutexGuard<'_, Vec<(PathBuf, Vec<String>)>> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Host for FakeHost {
    fn home(&self) -> PathBuf {
        self.home.path().to_path_buf()
    }

    fn var(&self, key: &str) -> Option<String> {
        non_empty(self.vars.get(key).cloned())
    }

    fn cwd(&self) -> Option<PathBuf> {
        self.cwd.clone()
    }

    fn now(&self) -> SystemTime {
        self.now
    }

    fn run(
        &self,
        program: &Path,
        args: &[String],
        _path_env: &[PathBuf],
        _timeout: Duration,
    ) -> std::result::Result<CmdOutput, SpawnError> {
        self.locked_calls()
            .push((program.to_path_buf(), args.to_vec()));
        for (p, a, result) in &self.scripts {
            if p.as_path() == program && a.as_slice() == args {
                return result.clone();
            }
        }
        Err(SpawnError::NotFound)
    }

    fn spawn(&self, spec: &SpawnSpec) -> Result<Box<dyn ChildProcess>, SpawnError> {
        self.spawn_specs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(spec.clone());
        let streams = self
            .streams
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (p, lines, stderr, status) in streams.iter() {
            if *p == spec.program {
                return Ok(Box::new(FakeChild {
                    lines: Arc::new(Mutex::new(lines.clone())),
                    stderr: stderr.clone(),
                    status: *status,
                }));
            }
        }
        Err(SpawnError::NotFound)
    }

    fn login_shell_path(&self, _timeout: Duration) -> Option<Vec<PathBuf>> {
        self.login_path.clone()
    }
}

/// [`ChildProcess`] replaying a `script_stream` script.
#[cfg(any(test, feature = "test-support"))]
struct FakeChild {
    /// Shared so the killer handle can end the stream mid-replay.
    lines: Arc<Mutex<Vec<String>>>,
    stderr: String,
    status: Option<i32>,
}

#[cfg(any(test, feature = "test-support"))]
impl ChildProcess for FakeChild {
    fn next_line(&mut self) -> Option<String> {
        let mut lines = self
            .lines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if lines.is_empty() {
            None
        } else {
            Some(lines.remove(0))
        }
    }

    fn stderr(&mut self) -> String {
        self.stderr.clone()
    }

    fn kill(&mut self) {
        kill_fake_lines(&self.lines);
    }

    fn killer(&self) -> Arc<dyn Fn() + Send + Sync> {
        let lines = Arc::clone(&self.lines);
        Arc::new(move || kill_fake_lines(&lines))
    }

    fn wait(&mut self) -> Option<i32> {
        self.status
    }
}

/// Ending a scripted stream: drop all remaining lines.
#[cfg(any(test, feature = "test-support"))]
fn kill_fake_lines(lines: &Mutex<Vec<String>>) {
    lines
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

/// `Some(value)` unless the value is absent or empty.
fn non_empty(value: Option<String>) -> Option<String> {
    match value {
        None => None,
        Some(v) if v.is_empty() => None,
        Some(v) => Some(v),
    }
}

fn map_spawn_err(e: std::io::Error) -> SpawnError {
    match e.kind() {
        std::io::ErrorKind::NotFound => SpawnError::NotFound,
        std::io::ErrorKind::PermissionDenied => SpawnError::PermissionDenied,
        _ => SpawnError::Other(e.to_string()),
    }
}

/// Read a piped child stream to a string.
fn read_piped<R: Read>(stream: Option<R>) -> String {
    let mut bytes = Vec::new();
    if let Some(mut s) = stream {
        let _ = s.read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Drain a piped child stream on a dedicated thread.
fn drain_piped<R: Read + Send + 'static>(stream: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(|| read_piped(stream))
}

/// Collect what a drain thread read; empty if the thread panicked.
fn join_reader(reader: std::thread::JoinHandle<String>) -> String {
    reader.join().unwrap_or_default()
}
