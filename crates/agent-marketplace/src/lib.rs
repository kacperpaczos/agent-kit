//! Skill marketplaces for AI-agent CLIs behind one blocking API.
//!
//! - [`Marketplace`]: search, list, check, install, remove, update.
//! - Adapters: [`npx_skills::NpxSkills`] (vercel-labs/skills),
//!   [`skillfish::Skillfish`], [`claude_plugins::ClaudePlugins`] (Claude
//!   Code plugin marketplaces), [`codex_plugins::CodexPlugins`].
//! - [`provenance`]: what each channel's own records say is installed,
//!   read without running anything.
//! - [`space`]: every installation of a skill shown once, with redundancy.
//!
//! Commands are argv-only and go through an injected [`CommandRunner`].

pub mod claude_plugins;
pub mod codex_plugins;
mod exec;
pub mod npx_skills;
pub mod provenance;
pub mod runner;
pub mod skillfish;
pub mod space;
pub mod types;
mod validate;

pub use exec::Plan;
pub use provenance::{installed_everywhere, installed_in_project};
pub use runner::{CancelToken, Command, CommandRunner, Output, ProcessRunner, RunError};
pub use types::*;

/// One adapter per channel, in a fixed order: `npx skills`, skillfish,
/// Claude plugins, Codex plugins. Each gets a clone of `runner`; `env`
/// supplies HOME and `CODEX_HOME`. Nothing runs until an adapter is used.
pub fn all_channels<'a, R>(
    runner: R,
    env: &dyn provenance::HomeEnv,
) -> Vec<Box<dyn Marketplace + 'a>>
where
    R: CommandRunner + Clone + 'a,
{
    vec![
        Box::new(npx_skills::NpxSkills::new(runner.clone())),
        Box::new(skillfish::Skillfish::new(runner.clone(), env.home())),
        Box::new(claude_plugins::ClaudePlugins::new(runner.clone())),
        Box::new(codex_plugins::CodexPlugins::new(
            runner,
            provenance::codex_home(env),
        )),
    ]
}
