//! Adapter for Codex plugins (`codex plugin …`, openai/codex
//! `codex-rs/cli/src/plugin_cmd.rs`, present in 0.150–0.166).
//!
//! - `plugin list --json` prints `{installed, available}`; `--available`
//!   fills `available`. Entries carry `pluginId` (`name@marketplace`),
//!   `name`, `marketplaceName`, `version`, `installed`, `enabled` and a
//!   tagged `source` (`local` with a `path`, `git`, `git-subdir`, `npm`,
//!   `remote`).
//! - `plugin add <PLUGIN@MARKETPLACE> --json` prints `pluginId`, `version`,
//!   `installedPath` after a successful install.
//! - `plugin remove <PLUGIN@MARKETPLACE> --json` prints the selection
//!   whether or not anything was installed, so removal is confirmed by
//!   listing before and after.
//! - There is no plugin update or update check: those are handed to the
//!   `/plugins` browser inside Codex.
//!
//! Plugins are per user (`$CODEX_HOME`); installed copies live in
//! `$CODEX_HOME/plugins/cache/<marketplace>/<plugin>/<version>/` with
//! skills in `skills/<name>/SKILL.md`.

use std::path::PathBuf;

use serde::Deserialize;

use crate::exec::{
    Cli, LIST_TIMEOUT, MUTATE_TIMEOUT, Plan, SEARCH_TIMEOUT, failed, failed_with, matches_query,
    push_unique, query_words, run_each,
};
use crate::provenance::plugin_skill_dirs;
use crate::runner::{CancelToken, CommandRunner};
use crate::types::{
    Capabilities, ChannelId, InstallRequest, InstalledSkill, Marketplace, MarketplaceError,
    Outcome, RemoveRequest, Result, Scope, SearchHit, SearchResults, SourceKind, SourceRef,
    UpdateInfo, UpdateRequest,
};
use crate::validate::{Kind, check};

/// Version directory Codex uses when a plugin declares none.
const DEFAULT_VERSION: &str = "local";
const HANDOFF: &str = "use /plugins in Codex";

/// One plugin entry of `codex plugin list --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexPlugin {
    pub plugin_id: String,
    pub name: String,
    pub marketplace: String,
    pub version: Option<String>,
    pub enabled: bool,
    /// The plugin directory for `local` sources.
    pub local_path: Option<PathBuf>,
}

/// `installed` and `available` plugins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexPluginList {
    pub installed: Vec<CodexPlugin>,
    pub available: Vec<CodexPlugin>,
}

#[derive(Deserialize)]
struct ListOutput {
    installed: Vec<Entry>,
    #[serde(default)]
    available: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    plugin_id: String,
    name: String,
    marketplace_name: String,
    version: Option<String>,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    source: serde_json::Value,
}

impl From<Entry> for CodexPlugin {
    fn from(e: Entry) -> CodexPlugin {
        let local_path = (e.source.get("source").and_then(|s| s.as_str()) == Some("local"))
            .then(|| {
                e.source
                    .get("path")
                    .and_then(|p| p.as_str())
                    .map(PathBuf::from)
            })
            .flatten();
        CodexPlugin {
            plugin_id: e.plugin_id,
            name: e.name,
            marketplace: e.marketplace_name,
            version: e.version,
            enabled: e.enabled,
            local_path,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddOutput {
    plugin_id: String,
}

/// Parse `codex plugin list [--available] --json`.
pub fn parse_codex_list(stdout: &str) -> std::result::Result<CodexPluginList, String> {
    let out: ListOutput = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("unrecognised `codex plugin list --json` output: {e}"))?;
    Ok(CodexPluginList {
        installed: out.installed.into_iter().map(Into::into).collect(),
        available: out.available.into_iter().map(Into::into).collect(),
    })
}

/// The `pluginId` of `codex plugin add --json`; `None` when absent.
pub fn parse_add(stdout: &str) -> Option<String> {
    serde_json::from_str::<AddOutput>(stdout.trim())
        .ok()
        .map(|o| o.plugin_id)
}

fn check_segment(value: &str) -> Result<()> {
    check(Kind::Name, value)?;
    if value.contains('@') {
        return Err(MarketplaceError::Invalid(format!(
            "name {value:?}: contains '@'"
        )));
    }
    Ok(())
}

/// `list --json`.
pub fn plan_list() -> Plan {
    let mut plan = Plan::new("list", None);
    plan.push("--json");
    plan
}

/// `list --available --json`.
pub fn plan_available() -> Plan {
    let mut plan = Plan::new("list", None);
    plan.extend(["--available", "--json"]);
    plan
}

/// `add <plugin>@<marketplace> --json`.
pub fn plan_add(plugin: &str, marketplace: &str) -> Result<Plan> {
    check_segment(plugin)?;
    check_segment(marketplace)?;
    let mut plan = Plan::new("add", None);
    plan.extend([&format!("{plugin}@{marketplace}"), "--json"]);
    Ok(plan)
}

/// `remove <plugin>@<marketplace> --json`; the marketplace is required.
pub fn plan_remove(id: &str) -> Result<Plan> {
    let Some((plugin, marketplace)) = id.split_once('@') else {
        return Err(MarketplaceError::Invalid(format!(
            "plugin {id:?}: expected PLUGIN@MARKETPLACE"
        )));
    };
    check_segment(plugin)?;
    check_segment(marketplace)?;
    let mut plan = Plan::new("remove", None);
    plan.extend([id, "--json"]);
    Ok(plan)
}

/// Codex's plugin commands as a [`Marketplace`].
pub struct CodexPlugins<R> {
    cli: Cli<R>,
    codex_home: PathBuf,
}

impl<R: CommandRunner> CodexPlugins<R> {
    /// Runs `codex plugin …`; `codex_home` is `$CODEX_HOME` or `~/.codex`
    /// (see [`crate::provenance::codex_home`]).
    pub fn new(runner: R, codex_home: PathBuf) -> Self {
        Self::with_command(runner, codex_home, "codex", &["plugin"])
    }

    pub fn with_command(runner: R, codex_home: PathBuf, program: &str, prefix: &[&str]) -> Self {
        Self {
            cli: Cli::new(runner, program, prefix),
            codex_home,
        }
    }

    /// Every command shares `cancel`.
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cli.cancel = cancel;
        self
    }

    fn listing(&self, plan: Plan) -> Result<CodexPluginList> {
        let output = self.cli.run_ok(plan, LIST_TIMEOUT)?;
        parse_codex_list(&output.stdout).map_err(MarketplaceError::Parse)
    }

    fn installed_ids(&self) -> Result<Vec<String>> {
        Ok(self
            .listing(plan_list())?
            .installed
            .into_iter()
            .map(|p| p.plugin_id)
            .collect())
    }

    fn per_user(scope: &Scope) -> Result<()> {
        match scope {
            Scope::Global => Ok(()),
            Scope::Project(_) => Err(MarketplaceError::Unsupported(
                "Codex plugins are installed per user, not per project".to_string(),
            )),
        }
    }
}

fn plugin_source(p: &CodexPlugin, skill: Option<&str>) -> SourceRef {
    SourceRef {
        kind: SourceKind::Plugin {
            marketplace: p.marketplace.clone(),
            plugin: p.name.clone(),
        },
        skill: skill.map(str::to_string),
    }
}

impl<R: CommandRunner> Marketplace for CodexPlugins<R> {
    fn id(&self) -> ChannelId {
        ChannelId::CodexPlugins
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            search: true,
            install: true,
            remove: true,
            update: false,
            check: false,
        }
    }

    /// Uninstalled plugins whose name holds every query word, followed by
    /// the skills of local plugins; matching skill names alone also count.
    fn search(&self, query: &str) -> Result<SearchResults> {
        let words = query_words(query)?;
        words.iter().try_for_each(|w| check(Kind::Word, w))?;
        let output = self.cli.run_ok(plan_available(), SEARCH_TIMEOUT)?;
        let list = parse_codex_list(&output.stdout).map_err(MarketplaceError::Parse)?;
        let mut results = SearchResults::default();
        for p in &list.available {
            let skills = p
                .local_path
                .as_deref()
                .map(|dir| plugin_skill_dirs(dir, &p.name))
                .unwrap_or_default();
            let plugin_hit = matches_query(&words, &[Some(&p.name)]);
            if plugin_hit {
                results.hits.push(SearchHit {
                    name: p.name.clone(),
                    source: plugin_source(p, None),
                    description: None,
                    installs: None,
                    url: None,
                });
            }
            for (skill, _) in skills {
                if plugin_hit || matches_query(&words, &[Some(&skill)]) {
                    results.hits.push(SearchHit {
                        source: plugin_source(p, Some(&skill)),
                        name: skill,
                        description: None,
                        installs: None,
                        url: None,
                    });
                }
            }
        }
        Ok(results)
    }

    /// Skills of installed plugins, read from the plugin cache. Project
    /// scope has none.
    fn list(&self, scope: &Scope) -> Result<Vec<InstalledSkill>> {
        if Self::per_user(scope).is_err() {
            return Ok(vec![]);
        }
        let list = self.listing(plan_list())?;
        let cache = self.codex_home.join("plugins").join("cache");
        Ok(list
            .installed
            .iter()
            .flat_map(|p| {
                let root = cache
                    .join(&p.marketplace)
                    .join(&p.name)
                    .join(p.version.as_deref().unwrap_or(DEFAULT_VERSION));
                plugin_skill_dirs(&root, &p.name)
                    .into_iter()
                    .map(move |(name, path)| InstalledSkill {
                        source: Some(plugin_source(p, Some(&name))),
                        name,
                        path,
                        agents: vec!["codex".to_string()],
                        scope: Scope::Global,
                        channel: ChannelId::CodexPlugins,
                    })
            })
            .collect())
    }

    /// Unknown: Codex has no update check.
    fn check(&self, installed: &[InstalledSkill]) -> Result<Vec<UpdateInfo>> {
        Ok(installed
            .iter()
            .map(|s| UpdateInfo {
                name: s.name.clone(),
                available: None,
            })
            .collect())
    }

    /// Installs the whole plugin; confirmed by `add --json`.
    fn install(&self, req: &InstallRequest) -> Result<Outcome> {
        let SourceKind::Plugin {
            marketplace,
            plugin,
        } = &req.source.kind
        else {
            return Err(MarketplaceError::Unsupported(
                "Codex installs plugins from its marketplaces only".to_string(),
            ));
        };
        Self::per_user(&req.scope)?;
        let output = self
            .cli
            .run(plan_add(plugin, marketplace)?, MUTATE_TIMEOUT)?;
        let mut outcome = Outcome::default();
        if !req.skills.is_empty() || req.source.skill.is_some() {
            outcome
                .warnings
                .push("Codex installs the whole plugin; skill selection is ignored".to_string());
        }
        match parse_add(&output.stdout) {
            Some(id) if output.success() => outcome.changed.push(id),
            _ if !output.success() => return Err(failed(&output)),
            _ => outcome
                .warnings
                .push("unrecognised `codex plugin add` output; install not confirmed".to_string()),
        }
        Ok(outcome)
    }

    /// `skills` are plugin ids `name@marketplace`.
    fn remove(&self, req: &RemoveRequest) -> Result<Outcome> {
        Self::per_user(&req.scope)?;
        if req.skills.is_empty() || req.skills.iter().any(|s| s == "*") {
            return Err(MarketplaceError::Invalid(
                "remove needs plugin ids (name@marketplace)".to_string(),
            ));
        }
        let plans: Vec<Plan> = req
            .skills
            .iter()
            .map(|id| plan_remove(id))
            .collect::<Result<_>>()?;
        let mut outcome = Outcome::default();
        run_each(
            req.skills.iter().zip(plans),
            &mut outcome,
            |(id, plan), out| {
                let before = self.installed_ids()?;
                let output = self.cli.run(plan, MUTATE_TIMEOUT)?;
                let gone = match self.installed_ids() {
                    Ok(after) => Some(before.contains(id) && !after.contains(id)),
                    Err(e) => {
                        out.warnings
                            .push(format!("removal not confirmed: listing failed ({e})"));
                        None
                    }
                };
                if gone == Some(true) {
                    push_unique(&mut out.changed, id);
                } else if !output.success() {
                    return Err(failed_with(&output, ""));
                } else if gone == Some(false) {
                    out.warnings.push(format!(
                        "`codex plugin remove {id}` changed no installed plugin"
                    ));
                }
                Ok(())
            },
        )?;
        Ok(outcome)
    }

    fn update(&self, _req: &UpdateRequest) -> Result<Outcome> {
        Err(MarketplaceError::Unsupported(format!(
            "Codex has no non-interactive plugin update; {HANDOFF}"
        )))
    }
}
