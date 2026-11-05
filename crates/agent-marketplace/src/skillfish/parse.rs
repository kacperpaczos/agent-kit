//! Parsers for skillfish `--json` output (knoxgraeme/skillfish
//! `src/utils.ts`: every command prints `{success, exit_code, errors}` plus
//! its own fields).

use std::path::PathBuf;

use serde::Deserialize;

use crate::exec::push_unique;
use crate::npx_skills::agent_id_for_display;
use crate::npx_skills::parse::owner_repo;
use crate::provenance::read_skillfish_manifest;
use crate::types::{
    ChannelId, InstalledSkill, Scope, SearchHit, SearchResults, SourceKind, SourceRef,
};

/// One `{skill, agent, path, location}` row (`InstalledSkill` upstream).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Row {
    pub skill: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// `global` or `project`.
    #[serde(default)]
    pub location: Option<String>,
}

/// One `outdated` entry of `update --json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutdatedSkill {
    pub skill: String,
    /// Display name, e.g. `Claude Code`.
    pub agent: String,
    pub path: PathBuf,
    /// `global` or `project`.
    pub location: String,
    #[serde(default)]
    pub local_sha: String,
    #[serde(default)]
    pub remote_sha: String,
    /// `owner/repo`.
    pub source: String,
}

/// `update --json` (check) or `update --yes --json` (apply). `success`,
/// `outdated` and `updated` are always printed upstream and required here,
/// so unrelated JSON (e.g. `{}`) is never read as "nothing outdated".
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct UpdateReport {
    pub success: bool,
    #[serde(default)]
    pub errors: Vec<String>,
    pub outdated: Vec<OutdatedSkill>,
    pub updated: Vec<Row>,
}

/// The common envelope plus the row lists of add/remove/list.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Envelope {
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub installed: Vec<Row>,
    #[serde(default)]
    pub removed: Vec<Row>,
    #[serde(default)]
    pub skipped: Vec<Skipped>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Skipped {
    #[serde(default)]
    pub skill: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Deserialize)]
struct SearchOutput {
    #[serde(default)]
    errors: Vec<String>,
    #[serde(default)]
    results: Vec<SearchRow>,
}

#[derive(Deserialize)]
struct SearchRow {
    name: String,
    #[serde(default)]
    github: String,
    url: Option<String>,
    description: Option<String>,
}

/// The JSON object on stdout (skillfish prints exactly one), only when it
/// carries `success` and the command's own row list `key` (`installed`,
/// `removed`); anything else is unrecognised.
pub(crate) fn envelope(stdout: &str, key: &str) -> Option<Envelope> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    let object = value.as_object()?;
    if !object.contains_key("success") || !object.contains_key(key) {
        return None;
    }
    serde_json::from_value(value).ok()
}

/// Parse `search --json`. `github` is `owner/repo`; anything else is kept
/// as a GitHub URL. Stars are not install counts, so `installs` is `None`.
pub fn parse_search(stdout: &str) -> Result<SearchResults, String> {
    let out: SearchOutput = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("unrecognised `skillfish search --json` output: {e}"))?;
    let hits = out
        .results
        .into_iter()
        .map(|row| {
            let kind = match owner_repo(&row.github) {
                Some((owner, repo)) => SourceKind::Github { owner, repo },
                None => SourceKind::Url {
                    url: format!("https://github.com/{}", row.github),
                },
            };
            SearchHit {
                source: SourceRef {
                    kind,
                    skill: Some(row.name.clone()),
                },
                name: row.name,
                description: row.description.filter(|d| !d.is_empty()),
                installs: None,
                url: row.url,
            }
        })
        .collect();
    Ok(SearchResults {
        hits,
        warnings: out.errors,
    })
}

/// Parse `list --json`. `skillfish list` shows every skill in the agent
/// directories, whoever installed it; only rows whose directory holds a
/// skillfish manifest belong to this channel.
pub fn parse_list(stdout: &str, scope: &Scope) -> Result<Vec<InstalledSkill>, String> {
    let env = envelope(stdout, "installed")
        .ok_or_else(|| "unrecognised `skillfish list --json` output".to_string())?;
    Ok(env
        .installed
        .into_iter()
        .filter_map(|row| {
            let path = row.path?;
            let manifest = read_skillfish_manifest(&path)?;
            Some(InstalledSkill {
                source: Some(manifest.source_ref(&row.skill)),
                agents: vec![
                    agent_id_for_display(&row.agent)
                        .unwrap_or(&row.agent)
                        .to_string(),
                ],
                name: row.skill,
                path,
                scope: scope.clone(),
                channel: ChannelId::Skillfish,
            })
        })
        .collect())
}

/// Parse `update [--yes] --json`.
pub fn parse_update(stdout: &str) -> Result<UpdateReport, String> {
    serde_json::from_str(stdout.trim())
        .map_err(|e| format!("unrecognised `skillfish update --json` output: {e}"))
}

/// Distinct skill names of `rows`, in order.
pub(crate) fn names(rows: &[Row]) -> Vec<String> {
    let mut out = Vec::new();
    for row in rows {
        push_unique(&mut out, &row.skill);
    }
    out
}
