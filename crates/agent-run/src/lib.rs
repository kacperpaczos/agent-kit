//! Running agent turns: transports, stream parsers, and one-turn execution.
//! Detection of installed agents lives in [`agent_detect`]; this crate turns
//! a detected agent plus a [`turn::TurnSpec`] into a stream of
//! [`event::AgentEvent`]s.

pub mod event;
pub mod parse;
pub mod transport;
pub mod turn;
