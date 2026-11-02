//! The single event vocabulary every transport parser normalizes into.

/// One normalized agent-stream event (spec §6.2).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// The agent's own session id, captured from the stream.
    SessionStarted {
        id: String,
    },
    /// Assistant text (whole message or delta).
    Text {
        text: String,
    },
    /// A tool call, e.g. `("Edit", Some("SKILL.md"))`.
    ToolCall {
        name: String,
        target: Option<String>,
    },
    Error {
        message: String,
    },
    NeedsLogin {
        message: String,
    },
    TurnEnd {
        ok: bool,
    },
}

/// Does this agent message mean "not logged in"? Shared by the transport
/// parsers and the EOF path of `run_turn`. Case-insensitive substring
/// match on the phrases below; a bare "auth" (e.g. "author") is NOT a
/// login problem.
pub fn is_login_message(text: &str) -> bool {
    const NEEDLES: [&str; 9] = [
        "not logged in",
        "please login",
        "/login",
        "log in",
        "login required",
        "unauthorized",
        "401",
        "authentication",
        "auth login",
    ];
    let lower = text.to_ascii_lowercase();
    NEEDLES.iter().any(|needle| lower.contains(needle))
}
