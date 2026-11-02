//! Running one agent turn: spawn, stream stdout through a parser, honor a
//! cooperative stop request, and always end with exactly one `TurnEnd`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use agent_detect::host::{Host, SpawnError, SpawnSpec};

use crate::event::AgentEvent;
use crate::transport::{TurnSpec, invocation, parser};

/// Cooperative stop flag shared between the UI and a running turn.
/// `Clone` hands the same flag to other threads.
#[derive(Clone, Default)]
pub struct Stop(Arc<AtomicBool>);

impl Stop {
    #[allow(clippy::new_without_default)] // paired with stop(); Default is for Clone
    pub fn new() -> Stop {
        Stop(Arc::new(AtomicBool::new(false)))
    }

    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// What one finished turn amounted to.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnSummary {
    /// The agent's own session id captured from the stream, for resume.
    pub session_id: Option<String>,
    pub ok: bool,
}

/// How often the watcher thread polls the stop flag while `next_line` blocks.
const STOP_POLL: Duration = Duration::from_millis(100);

/// Most stderr characters kept when an agent dies with an error.
const STDERR_TAIL: usize = 2000;

/// After `TurnEnd`, a process still alive this long is killed (the
/// post-turn stdout drain must be bounded).
const DRAIN_KILL: Duration = Duration::from_secs(5);

/// Runs one agent turn (plan Task 7). Every parsed event goes to `on_event`;
/// the turn always ends with exactly one `TurnEnd`.
pub fn run_turn(
    host: &dyn Host,
    bin: &Path,
    path_env: &[PathBuf],
    spec: &TurnSpec,
    stop: &Stop,
    on_event: &mut dyn FnMut(AgentEvent),
) -> TurnSummary {
    let inv = invocation(spec);
    let spawn_spec = SpawnSpec {
        program: bin.to_path_buf(),
        args: inv.args,
        path_env: path_env.to_vec(),
        cwd: spec.cwd.clone(),
        stdin: inv.stdin,
    };
    let mut child = match host.spawn(&spawn_spec) {
        Ok(child) => child,
        Err(e) => {
            on_event(AgentEvent::Error {
                message: spawn_error_message(&e, bin),
            });
            on_event(AgentEvent::TurnEnd { ok: false });
            return TurnSummary {
                session_id: None,
                ok: false,
            };
        }
    };

    let killer = child.killer();
    let exited = Arc::new(AtomicBool::new(false));
    let turn_ended = Arc::new(AtomicBool::new(false));
    let watcher = spawn_watcher(
        stop.clone(),
        Arc::clone(&killer),
        Arc::clone(&exited),
        Arc::clone(&turn_ended),
    );

    let mut line_parser = parser(spec.transport, &spec.cwd);
    let mut session_id: Option<String> = None;
    let mut turn_ok: Option<bool> = None;
    // An `Error`/`NeedsLogin` already emitted during the turn.
    let mut failed = false;
    let mut stopped = false;
    while turn_ok.is_none() {
        let Some(line) = child.next_line() else { break };
        if stop.is_stopped() {
            child.kill();
            stopped = true;
            break;
        }
        for event in line_parser.parse_line(&line) {
            match &event {
                AgentEvent::SessionStarted { id } => session_id = Some(id.clone()),
                AgentEvent::Error { .. } | AgentEvent::NeedsLogin { .. } => failed = true,
                AgentEvent::TurnEnd { ok } => turn_ok = Some(*ok),
                AgentEvent::Text { .. } | AgentEvent::ToolCall { .. } => {}
            }
            on_event(event);
        }
    }
    if turn_ok.is_some() && !stopped {
        // Keep the stop watcher armed (it kills the group 5 s after
        // `TurnEnd`) and drain the rest of stdout, discarded, so a chatty
        // agent cannot block on a full pipe after its turn is over.
        turn_ended.store(true, Ordering::SeqCst);
        while child.next_line().is_some() {}
    }

    let status = child.wait();
    exited.store(true, Ordering::SeqCst);
    let _ = watcher.join();

    if stopped || (turn_ok.is_none() && stop.is_stopped()) {
        // The watcher kills a silent agent; its blocked `next_line` then
        // returns EOF. Both stop paths report the same thing.
        on_event(AgentEvent::Error {
            message: "Zatrzymano".to_string(),
        });
        on_event(AgentEvent::TurnEnd { ok: false });
        return TurnSummary {
            session_id,
            ok: false,
        };
    }
    if let Some(ok) = turn_ok {
        return TurnSummary { session_id, ok };
    }

    // EOF without `TurnEnd`: OpenCode ends the turn by exiting. A clean
    // exit with no prior error is a successful turn; anything else reports
    // why the agent died (Review Focus 3).
    if status == Some(0) && !failed {
        on_event(AgentEvent::TurnEnd { ok: true });
        return TurnSummary {
            session_id,
            ok: true,
        };
    }
    if !failed {
        let stderr = child.stderr();
        if !stderr.is_empty() && crate::event::is_login_message(&stderr) {
            on_event(AgentEvent::NeedsLogin { message: stderr });
        } else {
            on_event(AgentEvent::Error {
                message: eof_error_message(&stderr, status),
            });
        }
    }
    on_event(AgentEvent::TurnEnd { ok: false });
    TurnSummary {
        session_id,
        ok: false,
    }
}

/// Watches `stop` while `next_line` blocks: kills the child from this
/// thread, because the reading thread cannot observe the flag mid-read.
/// Stays armed until the process has exited (`exited`); once the turn has
/// ended (`turn_ended`), a process still alive after 5 s is killed too, so
/// a lingering agent cannot outlive its turn.
fn spawn_watcher(
    stop: Stop,
    killer: Arc<dyn Fn() + Send + Sync>,
    exited: Arc<AtomicBool>,
    turn_ended: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut drain_deadline: Option<Instant> = None;
        loop {
            if stop.is_stopped() {
                killer();
                return;
            }
            if turn_ended.load(Ordering::SeqCst) {
                let started = *drain_deadline.get_or_insert_with(Instant::now);
                if Instant::now() >= started + DRAIN_KILL {
                    killer();
                    return;
                }
            }
            if exited.load(Ordering::SeqCst) {
                return;
            }
            thread::sleep(STOP_POLL);
        }
    })
}

fn spawn_error_message(err: &SpawnError, bin: &Path) -> String {
    match err {
        SpawnError::NotFound => format!("Nie znaleziono programu agenta: {}", bin.display()),
        other => format!("Nie udało się uruchomić agenta: {other:?}"),
    }
}

fn eof_error_message(stderr: &str, status: Option<i32>) -> String {
    if !stderr.is_empty() {
        let total = stderr.chars().count();
        if total <= STDERR_TAIL {
            stderr.to_string()
        } else {
            stderr.chars().skip(total - STDERR_TAIL).collect()
        }
    } else {
        match status {
            Some(code) => format!("Agent zakończył się bez odpowiedzi (kod {code})"),
            None => "Agent zakończył się bez odpowiedzi".to_string(),
        }
    }
}
