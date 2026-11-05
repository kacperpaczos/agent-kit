//! Parsers for `npx skills` output: `list --json`, `add --json`, and the
//! human text of `find`, `check` and `update` (those have no JSON mode).

use std::collections::HashSet;
use std::path::PathBuf;

use serde::Deserialize;

use super::agent_id_for_display;
use crate::runner::Output;
use crate::types::{
    ChannelId, InstalledSkill, Outcome, Scope, SearchHit, SearchResults, SourceKind, SourceRef,
    UpdateInfo,
};

/// One row of `skills list --json` (src/list.ts).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListRow {
    name: String,
    path: Option<String>,
    #[serde(default)]
    agents: Vec<String>,
    source: Option<String>,
    source_url: Option<String>,
    source_type: Option<String>,
    status: Option<String>,
}

/// Parse `skills list [-g] --json`. Rows the lock file knows but that are
/// not on disk (`status: "missing"`) are skipped. Agent display names
/// (`Claude Code`) become agent ids where known.
pub fn parse_list_json(
    stdout: &str,
    scope: &Scope,
) -> std::result::Result<Vec<InstalledSkill>, String> {
    let rows: Vec<ListRow> = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("unrecognised `skills list --json` output: {e}"))?;
    Ok(rows
        .into_iter()
        .filter(|row| row.status.as_deref() != Some("missing"))
        .filter_map(|row| {
            let path = PathBuf::from(row.path.as_deref()?);
            let source = lock_source(&row);
            Some(InstalledSkill {
                agents: row
                    .agents
                    .iter()
                    .map(|a| agent_id_for_display(a).unwrap_or(a).to_string())
                    .collect(),
                name: row.name,
                path,
                scope: scope.clone(),
                channel: ChannelId::NpxSkills,
                source,
            })
        })
        .collect())
}

/// Map a lock entry's `source`/`sourceUrl`/`sourceType` to a [`SourceRef`].
fn lock_source(row: &ListRow) -> Option<SourceRef> {
    let source = row.source.as_deref();
    let url = row.source_url.as_deref().or(source)?;
    let kind = match row.source_type.as_deref() {
        Some("local") => SourceKind::Local {
            path: PathBuf::from(source.unwrap_or(url)),
        },
        Some("github") => match source.and_then(owner_repo) {
            Some((owner, repo)) => SourceKind::Github { owner, repo },
            None => SourceKind::Url {
                url: url.to_string(),
            },
        },
        _ => SourceKind::Url {
            url: url.to_string(),
        },
    };
    Some(SourceRef {
        kind,
        skill: Some(row.name.clone()),
    })
}

/// `owner/repo` with exactly one slash and no spaces.
fn owner_repo(text: &str) -> Option<(String, String)> {
    let (owner, repo) = text.split_once('/')?;
    let plain = |s: &str| !s.is_empty() && !s.contains('/') && !s.contains(char::is_whitespace);
    (plain(owner) && plain(repo)).then(|| (owner.to_string(), repo.to_string()))
}

/// One entry of `skills add --json -y` (src/add.ts `AddJsonResult`).
#[derive(Deserialize)]
struct AddRow {
    name: Option<String>,
    status: String,
    reason: Option<String>,
    error: Option<String>,
}

/// Parse `skills add --json` output; `None` when stdout is not that array
/// (e.g. an older CLI without `--json`).
pub fn parse_add_json(stdout: &str) -> Option<Outcome> {
    let rows: Vec<AddRow> = serde_json::from_str(stdout.trim()).ok()?;
    let mut outcome = Outcome::default();
    for row in rows {
        let name = row.name.unwrap_or_else(|| "(unnamed)".to_string());
        match row.status.as_str() {
            "installed" => outcome.changed.push(name),
            other => {
                let detail = row.error.or(row.reason).unwrap_or_default();
                outcome
                    .warnings
                    .push(format!("{name}: {other} {detail}").trim_end().to_string());
            }
        }
    }
    Some(outcome)
}

/// Parse the non-interactive text of `skills find <query>` (src/find.ts):
/// `<owner/repo>@<skill> [<N> installs]` followed by `└ <url>`.
pub fn parse_find(stdout: &str) -> SearchResults {
    let lines = plain_lines(stdout);
    let mut results = SearchResults::default();
    let mut recognised = false;
    let mut iter = lines.iter().map(|l| l.trim()).peekable();
    while let Some(line) = iter.next() {
        if line.starts_with("No skills found") || line.starts_with("Install with") {
            recognised = true;
            continue;
        }
        let Some(mut hit) = find_hit(line) else {
            continue;
        };
        if let Some(url) = iter.peek().and_then(|next| next.strip_prefix("└ ")) {
            hit.url = Some(url.trim().to_string());
            iter.next();
        }
        results.hits.push(hit);
    }
    if results.hits.is_empty() && !recognised {
        results
            .warnings
            .push("unrecognised `skills find` output; no results parsed".to_string());
    }
    results
}

/// One `<pkg>@<skill> [<count> install(s)]` line.
fn find_hit(line: &str) -> Option<SearchHit> {
    let mut words = line.split_whitespace();
    let target = words.next()?;
    let rest: Vec<&str> = words.collect();
    let installs = match rest.as_slice() {
        [] => None,
        [count, "installs" | "install"] => Some(parse_count(count)?),
        _ => return None,
    };
    let (pkg, name) = target.rsplit_once('@')?;
    if pkg.is_empty() || name.is_empty() || pkg.starts_with('<') {
        return None;
    }
    let kind = match owner_repo(pkg) {
        Some((owner, repo)) => SourceKind::Github { owner, repo },
        None => SourceKind::Url {
            url: pkg.to_string(),
        },
    };
    Some(SearchHit {
        name: name.to_string(),
        source: SourceRef {
            kind,
            skill: Some(name.to_string()),
        },
        description: None,
        installs,
        url: None,
    })
}

/// `42`, `1.2K`, `3M` (src/find.ts `formatInstalls`).
fn parse_count(text: &str) -> Option<u64> {
    let (number, scale) = match text.chars().last()? {
        'K' => (&text[..text.len() - 1], 1_000.0),
        'M' => (&text[..text.len() - 1], 1_000_000.0),
        _ => (text, 1.0),
    };
    let value: f64 = number.parse().ok()?;
    (value >= 0.0).then(|| (value * scale).round() as u64)
}

/// Read `skills check` output (src/update.ts, check mode) for `names`, in
/// order. `↑ <name> has an update` is an update; names listed under
/// "cannot be checked automatically" are unknown. Exit 1 without any
/// update line, any other non-zero exit, or a source that failed to check
/// leaves the unlisted names unknown too.
pub fn parse_check(output: &Output, names: &[String]) -> Vec<UpdateInfo> {
    let lines = plain_lines(&output.stdout);
    let mut updates = HashSet::new();
    let mut unknown = HashSet::new();
    let mut failed = false;
    let mut in_skipped = false;
    for line in &lines {
        let line = line.trim();
        if let Some(name) = line
            .strip_prefix("↑ ")
            .and_then(|l| l.strip_suffix(" has an update"))
        {
            updates.insert(name.to_lowercase());
        } else if line.contains("cannot be checked automatically") {
            in_skipped = true;
        } else if in_skipped && let Some(item) = line.strip_prefix("• ") {
            let names = item.split(" (").next().unwrap_or(item);
            unknown.extend(names.split(", ").map(|n| n.trim().to_lowercase()));
        } else if line.starts_with("✗ ") {
            failed = true;
        } else if line.is_empty() {
            in_skipped = false;
        }
    }
    let completed = output.status == Some(0) || (output.status == Some(1) && !updates.is_empty());
    names
        .iter()
        .map(|name| {
            let key = name.to_lowercase();
            let available = if !completed || unknown.contains(&key) {
                None
            } else if updates.contains(&key) {
                Some(true)
            } else if failed {
                None
            } else {
                Some(false)
            };
            UpdateInfo {
                name: name.clone(),
                available,
            }
        })
        .collect()
}

/// Names from `✓ Updated <name>` lines of `skills update` (not the
/// `✓ Updated N skill(s)` total).
pub(crate) fn parse_updated(stdout: &str) -> Vec<String> {
    plain_lines(stdout)
        .iter()
        .filter_map(|line| line.trim().strip_prefix("✓ Updated "))
        .filter(|rest| !rest.ends_with(" skill(s)"))
        .map(|name| name.trim().to_string())
        .collect()
}

/// Output lines with ANSI escapes removed; a carriage return keeps only
/// what was written after it (spinner overwrites).
fn plain_lines(text: &str) -> Vec<String> {
    strip_ansi(text)
        .lines()
        .map(|line| line.rsplit('\r').next().unwrap_or("").to_string())
        .collect()
}

/// Remove CSI escape sequences (`ESC [ … final-byte`) and lone escapes.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if ('\u{40}'..='\u{7e}').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}
