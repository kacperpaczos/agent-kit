//! `ProcessRunner` with real `/bin/sh` children only: exit status, one
//! deadline covering exit and pipes, and cancellation.

use std::time::{Duration, Instant};

use agent_marketplace::{CancelToken, Command, CommandRunner, ProcessRunner, RunError};

fn sh(script: &str, timeout: Duration) -> Command {
    Command {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        cwd: None,
        env: vec![("MARKER".to_string(), "m1".to_string())],
        timeout,
        cancel: CancelToken::new(),
    }
}

#[test]
fn collects_status_stdout_stderr_cwd_and_env() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cmd = sh(
        "pwd; echo \"$MARKER\"; echo err >&2; exit 3",
        Duration::from_secs(10),
    );
    cmd.cwd = Some(tmp.path().to_path_buf());
    let out = ProcessRunner.run(&cmd).unwrap();
    assert_eq!(out.status, Some(3));
    let real = std::fs::canonicalize(tmp.path()).unwrap();
    assert_eq!(out.stdout, format!("{}\nm1\n", real.display()));
    assert_eq!(out.stderr, "err\n");
}

#[test]
fn missing_program_is_a_spawn_error() {
    let mut cmd = sh("", Duration::from_secs(1));
    cmd.program = "/nonexistent/definitely-not-here".to_string();
    assert!(matches!(ProcessRunner.run(&cmd), Err(RunError::Spawn(_))));
}

#[test]
fn hanging_child_times_out() {
    let start = Instant::now();
    let res = ProcessRunner.run(&sh("sleep 30", Duration::from_millis(300)));
    assert!(matches!(res, Err(RunError::Timeout(_))), "{res:?}");
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn deadline_covers_pipes_held_by_a_background_descendant() {
    let start = Instant::now();
    let res = ProcessRunner.run(&sh("sleep 30 & echo hi", Duration::from_millis(800)));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "took {:?}",
        start.elapsed()
    );
    // The direct child exited normally; what it wrote is kept.
    let out = res.unwrap();
    assert_eq!(out.status, Some(0));
    assert_eq!(out.stdout, "hi\n");
}

#[test]
fn cancel_mid_run_returns_promptly() {
    let cmd = sh("sleep 30", Duration::from_secs(60));
    let token = cmd.cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        token.cancel();
    });
    let start = Instant::now();
    let res = ProcessRunner.run(&cmd);
    canceller.join().unwrap();
    assert!(matches!(res, Err(RunError::Cancelled)), "{res:?}");
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn already_cancelled_token_never_spawns() {
    let cmd = sh("echo should-not-run", Duration::from_secs(5));
    cmd.cancel.cancel();
    assert!(cmd.cancel.is_cancelled());
    assert!(matches!(ProcessRunner.run(&cmd), Err(RunError::Cancelled)));
}
