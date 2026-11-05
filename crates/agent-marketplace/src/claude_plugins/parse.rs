//! Parsers for `claude plugin … --json` output.

use std::path::PathBuf;

use serde::Deserialize;

/// One installation from `plugin list --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInstall {
    /// `name@marketplace` (`@inline`, `@skills-dir`, `@synced` for
    /// non-marketplace plugins).
    pub id: String,
    pub plugin: String,
    pub marketplace: String,
    pub version: String,
    /// `user`, `project`, `local`, `managed`, `session` or `synced`.
    pub scope: String,
    pub enabled: bool,
    /// `installPath`, or `readFromFolder` for plugins loaded in place.
    pub root: Option<PathBuf>,
    pub project_path: Option<PathBuf>,
    pub last_updated: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallRow {
    id: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    scope: String,
    #[serde(default)]
    enabled: bool,
    install_path: Option<PathBuf>,
    read_from_folder: Option<PathBuf>,
    project_path: Option<PathBuf>,
    last_updated: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ListOutput {
    Plain(Vec<InstallRow>),
    Wrapped {
        installed: Vec<InstallRow>,
        #[serde(default)]
        available: Vec<AvailableRow>,
    },
}

/// One uninstalled plugin from `plugin list --json --available`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailablePlugin {
    pub plugin_id: String,
    pub name: String,
    pub marketplace: String,
    /// The marketplace entry's `source` when it is a relative path string
    /// (relative to the marketplace root); `None` for object sources.
    pub source_path: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub install_count: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AvailableRow {
    plugin_id: String,
    name: String,
    marketplace_name: String,
    #[serde(default)]
    source: serde_json::Value,
    description: Option<String>,
    version: Option<String>,
    install_count: Option<u64>,
}

/// One configured marketplace from `plugin marketplace list --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownMarketplace {
    pub name: String,
    /// `github`, `git`, `url`, `directory`, `file` or `claudeai`.
    pub source: String,
    pub install_location: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarketplaceRow {
    name: String,
    #[serde(default)]
    source: String,
    install_location: Option<PathBuf>,
}

/// The JSON result line of install/uninstall/update/enable/disable.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResultLine {
    #[serde(default)]
    pub command: String,
    /// `ok` or `failed`.
    pub outcome: String,
    #[serde(default)]
    pub message: String,
}

fn install(row: InstallRow) -> PluginInstall {
    let (plugin, marketplace) = match row.id.split_once('@') {
        Some((p, m)) => (p.to_string(), m.to_string()),
        None => (row.id.clone(), String::new()),
    };
    PluginInstall {
        id: row.id,
        plugin,
        marketplace,
        version: row.version,
        scope: row.scope,
        enabled: row.enabled,
        root: row.read_from_folder.or(row.install_path),
        project_path: row.project_path,
        last_updated: row.last_updated,
    }
}

/// Parse `plugin list --json` (an array) or its `--available` object.
pub fn parse_plugin_list(stdout: &str) -> Result<Vec<PluginInstall>, String> {
    let rows = match serde_json::from_str::<ListOutput>(stdout.trim())
        .map_err(|e| format!("unrecognised `claude plugin list --json` output: {e}"))?
    {
        ListOutput::Plain(rows)
        | ListOutput::Wrapped {
            installed: rows, ..
        } => rows,
    };
    Ok(rows.into_iter().map(install).collect())
}

/// Parse the `available` part of `plugin list --json --available`.
pub fn parse_available(stdout: &str) -> Result<Vec<AvailablePlugin>, String> {
    match serde_json::from_str::<ListOutput>(stdout.trim()) {
        Ok(ListOutput::Wrapped { available, .. }) => Ok(available
            .into_iter()
            .map(|row| AvailablePlugin {
                source_path: row.source.as_str().map(str::to_string),
                plugin_id: row.plugin_id,
                name: row.name,
                marketplace: row.marketplace_name,
                description: row.description.filter(|d| !d.is_empty()),
                version: row.version,
                install_count: row.install_count,
            })
            .collect()),
        Ok(ListOutput::Plain(_)) => Err(
            "`claude plugin list --json --available` printed no available plugins (older CLI?)"
                .to_string(),
        ),
        Err(e) => Err(format!(
            "unrecognised `claude plugin list --json --available` output: {e}"
        )),
    }
}

/// Parse `plugin marketplace list --json`.
pub fn parse_marketplaces(stdout: &str) -> Result<Vec<KnownMarketplace>, String> {
    let rows: Vec<MarketplaceRow> = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("unrecognised `claude plugin marketplace list --json` output: {e}"))?;
    Ok(rows
        .into_iter()
        .map(|r| KnownMarketplace {
            name: r.name,
            source: r.source,
            install_location: r.install_location,
        })
        .collect())
}

/// The last non-empty stdout line as a JSON result; `None` when it is not
/// one (e.g. an older CLI without `--json`).
pub fn parse_result_line(stdout: &str) -> Option<ResultLine> {
    let line = stdout.lines().rev().find(|l| !l.trim().is_empty())?;
    serde_json::from_str(line.trim()).ok()
}
