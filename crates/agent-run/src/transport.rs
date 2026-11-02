//! Transports: agent command-line construction (flags in code, per spec E1
//! §6.1) and the line-parser seam. The three parsers arrive in Tasks 3–5.

use std::path::{Path, PathBuf};

use crate::event::AgentEvent;

/// A supported agent transport, named as in agent-detect's `agents.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Transport {
    ClaudeStreamJson,
    CodexExecJson,
    OpencodeRunJson,
}

impl Transport {
    /// Map a registry transport name to a transport; `None` for unknown names
    /// (including `acp`, which is out of scope for stage 2).
    pub fn from_registry(name: &str) -> Option<Transport> {
        match name {
            "claude-stream-json" => Some(Transport::ClaudeStreamJson),
            "codex-exec-json" => Some(Transport::CodexExecJson),
            "opencode-run-json" => Some(Transport::OpencodeRunJson),
            _ => None,
        }
    }
}

/// One agent turn: what to run, where, and the user's prompt (via stdin).
pub struct TurnSpec {
    pub transport: Transport,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub resume: Option<String>,
    pub message: String,
}

/// A prepared agent invocation: argv plus the prompt delivered on stdin.
pub struct Invocation {
    pub args: Vec<String>,
    pub stdin: String,
}

/// A model name that means "the agent's default" and must not be passed.
fn model_flag(model: &Option<String>) -> Option<&str> {
    match model.as_deref() {
        None | Some("") | Some("default") => None,
        Some(m) => Some(m),
    }
}

/// Build the agent argv and stdin for `spec`. The prompt is never in argv.
pub fn invocation(spec: &TurnSpec) -> Invocation {
    match spec.transport {
        Transport::ClaudeStreamJson => {
            let mut args = [
                "-p",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "bypassPermissions",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
            if let Some(m) = model_flag(&spec.model) {
                args.extend(["--model", m].map(String::from));
            }
            if let Some(id) = &spec.resume {
                args.extend(["--resume", id.as_str()].map(String::from));
            }
            let stdin = serde_json::json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [{ "type": "text", "text": spec.message }],
                }
            })
            .to_string();
            Invocation {
                args,
                stdin: format!("{stdin}\n"),
            }
        }
        Transport::CodexExecJson => {
            let mut args = vec!["exec".to_string()];
            if let Some(id) = &spec.resume {
                args.extend(
                    [
                        "resume",
                        "--json",
                        "--skip-git-repo-check",
                        "-c",
                        "sandbox_mode=\"workspace-write\"",
                    ]
                    .map(String::from),
                );
                if let Some(m) = model_flag(&spec.model) {
                    args.extend(["-m", m].map(String::from));
                }
                args.push(id.clone());
            } else {
                args.extend(
                    [
                        "--json",
                        "--skip-git-repo-check",
                        "--sandbox",
                        "workspace-write",
                        "-C",
                        &spec.cwd.to_string_lossy(),
                    ]
                    .map(String::from),
                );
                if let Some(m) = model_flag(&spec.model) {
                    args.extend(["-m", m].map(String::from));
                }
            }
            Invocation {
                args,
                stdin: spec.message.clone(),
            }
        }
        Transport::OpencodeRunJson => {
            let mut args = [
                "run",
                "--format",
                "json",
                "--dangerously-skip-permissions",
                "--dir",
                &spec.cwd.to_string_lossy(),
            ]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
            if let Some(id) = &spec.resume {
                args.extend(["-s", id.as_str()].map(String::from));
            }
            if let Some(m) = model_flag(&spec.model) {
                args.extend(["-m", m].map(String::from));
            }
            Invocation {
                args,
                stdin: spec.message.clone(),
            }
        }
    }
}

/// Parses one stdout line of an agent stream into events.
pub trait LineParser {
    fn parse_line(&mut self, line: &str) -> Vec<AgentEvent>;
}

/// Line parser for `transport`. `cwd` lets parsers resolve tool targets
/// relative to the working copy.
pub fn parser(transport: Transport, cwd: &Path) -> Box<dyn LineParser> {
    match transport {
        Transport::ClaudeStreamJson => {
            Box::new(crate::parse::claude::ClaudeParser::new(cwd.to_path_buf()))
        }
        Transport::CodexExecJson => {
            Box::new(crate::parse::codex::CodexParser::new(cwd.to_path_buf()))
        }
        Transport::OpencodeRunJson => Box::new(crate::parse::opencode::OpencodeParser::new(
            cwd.to_path_buf(),
        )),
    }
}
