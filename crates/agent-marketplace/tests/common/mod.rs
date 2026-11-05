//! Test doubles shared by the adapter tests: a runner that records every
//! command and answers from a script, and temp-dir skill builders.
#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use agent_marketplace::{Command, CommandRunner, Output, RunError};

/// Records every command and answers from a queue of scripted outputs.
#[derive(Default)]
pub struct FakeRunner {
    calls: RefCell<Vec<Command>>,
    outputs: RefCell<VecDeque<Result<Output, RunError>>>,
}

impl FakeRunner {
    pub fn with(outputs: Vec<Result<Output, RunError>>) -> FakeRunner {
        FakeRunner {
            calls: RefCell::default(),
            outputs: RefCell::new(outputs.into()),
        }
    }
    pub fn calls(&self) -> Vec<Command> {
        self.calls.borrow().clone()
    }
    /// Argv of every call, program first.
    pub fn argvs(&self) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .map(|c| std::iter::once(c.program).chain(c.args).collect())
            .collect()
    }
}

impl CommandRunner for &FakeRunner {
    fn run(&self, cmd: &Command) -> Result<Output, RunError> {
        self.calls.borrow_mut().push(cmd.clone());
        self.outputs
            .borrow_mut()
            .pop_front()
            .expect("unexpected extra command")
    }
}

pub fn out(status: i32, stdout: &str) -> Result<Output, RunError> {
    Ok(Output {
        status: Some(status),
        stdout: stdout.to_string(),
        stderr: String::new(),
    })
}

pub fn err_out(status: i32, stdout: &str, stderr: &str) -> Result<Output, RunError> {
    Ok(Output {
        status: Some(status),
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
    })
}

pub fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// `<root>/<rel>/SKILL.md` with `body`; returns the skill directory.
pub fn skill_dir(root: &Path, rel: &str, body: &str) -> PathBuf {
    let dir = root.join(rel);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), body).unwrap();
    dir
}

pub fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Replace `{ROOT}` in a fixture with a temp dir path.
pub fn rooted(fixture: &str, root: &Path) -> String {
    fixture.replace("{ROOT}", root.to_str().unwrap())
}
