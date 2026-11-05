//! One skill space: every installed skill shown once with all its
//! installations, whatever channel or agent they came through.
//!
//! Redundancy is allowed (a deliberate backup copy is fine); the space only
//! tells identical copies from diverged ones.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::types::{ChannelId, InstalledSkill, SourceRef};

/// How many installations a skill has and whether they agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpaceStatus {
    Single,
    /// `same` when every installation resolves to the same directory or has
    /// the same content hash; `false` when contents differ or cannot be
    /// confirmed equal.
    Redundant {
        same: bool,
    },
}

/// One skill name with all its installations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceEntry {
    pub name: String,
    pub installs: Vec<InstalledSkill>,
    /// Distinct known sources, in installation order.
    pub sources: Vec<SourceRef>,
    pub status: SpaceStatus,
}

/// Group installations by skill name (case-insensitive), sorted by name.
///
/// `hashes` returns a content hash for an installation directory (called
/// once per distinct real path; `None` when unknown). Installations whose
/// real paths match, or whose hashes match, count as the same copy.
pub fn build_space(
    installs: &[InstalledSkill],
    hashes: &dyn Fn(&Path) -> Option<String>,
) -> Vec<SpaceEntry> {
    let mut groups: Vec<(String, Vec<&InstalledSkill>)> = Vec::new();
    for install in installs {
        let key = install.name.to_lowercase();
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, members)) => members.push(install),
            None => groups.push((key, vec![install])),
        }
    }
    groups.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hash_cache: HashMap<PathBuf, Option<String>> = HashMap::new();
    groups
        .into_iter()
        .map(|(_, members)| {
            let status = if members.len() == 1 {
                SpaceStatus::Single
            } else {
                SpaceStatus::Redundant {
                    same: all_same(&members, hashes, &mut hash_cache),
                }
            };
            let mut sources: Vec<SourceRef> = Vec::new();
            for source in members.iter().filter_map(|m| m.source.as_ref()) {
                if !sources.contains(source) {
                    sources.push(source.clone());
                }
            }
            SpaceEntry {
                name: members[0].name.clone(),
                installs: members.into_iter().cloned().collect(),
                sources,
                status,
            }
        })
        .collect()
}

/// True when every installation is linked to the first by a shared real
/// path or an equal known hash (transitively).
fn all_same(
    members: &[&InstalledSkill],
    hashes: &dyn Fn(&Path) -> Option<String>,
    cache: &mut HashMap<PathBuf, Option<String>>,
) -> bool {
    let keys: Vec<(PathBuf, Option<String>)> = members
        .iter()
        .map(|m| {
            let real = std::fs::canonicalize(&m.path).unwrap_or_else(|_| m.path.clone());
            let hash = cache
                .entry(real.clone())
                .or_insert_with(|| hashes(&m.path))
                .clone();
            (real, hash)
        })
        .collect();
    // Union-find over "same path or same hash".
    let mut parent: Vec<usize> = (0..keys.len()).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for i in 0..keys.len() {
        for j in i + 1..keys.len() {
            let (pi, hi) = &keys[i];
            let (pj, hj) = &keys[j];
            if pi == pj || (hi.is_some() && hi == hj) {
                let (ri, rj) = (root(&mut parent, i), root(&mut parent, j));
                parent[ri] = rj;
            }
        }
    }
    let first = root(&mut parent, 0);
    (1..keys.len()).all(|i| root(&mut parent, i) == first)
}

/// Summary of one redundant skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedundancyLine {
    pub name: String,
    pub installs: usize,
    pub same: bool,
    /// Distinct channels, sorted.
    pub channels: Vec<ChannelId>,
    /// Distinct agents, sorted.
    pub agents: Vec<String>,
}

impl fmt::Display for RedundancyLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let channels: Vec<&str> = self.channels.iter().map(|c| c.as_str()).collect();
        write!(
            f,
            "{}: {} installs ({}) via {} for {}",
            self.name,
            self.installs,
            if self.same { "identical" } else { "diverged" },
            channels.join(", "),
            self.agents.join(", ")
        )
    }
}

/// One line per skill installed more than once.
pub fn redundancy_report(space: &[SpaceEntry]) -> Vec<RedundancyLine> {
    space
        .iter()
        .filter_map(|entry| {
            let SpaceStatus::Redundant { same } = entry.status else {
                return None;
            };
            let mut channels: Vec<ChannelId> = entry.installs.iter().map(|i| i.channel).collect();
            channels.sort();
            channels.dedup();
            let mut agents: Vec<String> = entry
                .installs
                .iter()
                .flat_map(|i| i.agents.iter().cloned())
                .collect();
            agents.sort();
            agents.dedup();
            Some(RedundancyLine {
                name: entry.name.clone(),
                installs: entry.installs.len(),
                same,
                channels,
                agents,
            })
        })
        .collect()
}
