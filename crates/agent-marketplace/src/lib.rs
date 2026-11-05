//! Skill marketplaces for AI-agent CLIs behind one blocking API.
//!
//! - [`Marketplace`]: search, list, check, install, remove, update.
//! - [`npx_skills::NpxSkills`]: the vercel-labs/skills adapter.
//! - [`space`]: every installation of a skill shown once, with redundancy.
//!
//! Commands are argv-only and go through an injected [`CommandRunner`].

pub mod npx_skills;
pub mod runner;
pub mod space;
pub mod types;
mod validate;

pub use runner::{CancelToken, Command, CommandRunner, Output, ProcessRunner, RunError};
pub use types::*;
