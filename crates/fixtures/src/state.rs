//! Shared in-memory state behind every fixture adapter.
//!
//! Loaded once from `data_dir` by [`crate::FixtureSet::load`] and handed to
//! `FixtureHost`, `FixtureGit`, `ReplayAgents` and `Triggers` as one
//! `Arc<Mutex<_>>` so a trigger fired through one of them is visible to all
//! the others — a `Triggers::author_push` moves the head `FixtureGit` serves
//! and the PR `FixtureHost` serves, in the same state.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use theodore_domain::{AgentId, PrDetail, PrId, PrRef, Revision, Sha};
use theodore_ports::HostError;

use crate::error::FixtureError;
use crate::host::HostCall;

#[derive(Debug)]
pub(crate) struct SharedState {
    pub data_dir: PathBuf,
    pub prs: HashMap<PrId, PrDetail>,
    /// Current head per PR; moved by `Triggers::author_push`.
    pub heads: HashMap<PrId, Sha>,
    /// Every revision found under `git/*/*.json`, keyed by its head sha.
    pub revisions_by_head: HashMap<Sha, Revision>,
    /// File blobs found under `git/*/files/<rev>/<path>`, keyed by (rev, path).
    pub files: HashMap<(Sha, String), Vec<u8>>,
    pub calls: Vec<HostCall>,
    /// PR refs that changed, in order; `changed_since` slices into this.
    pub change_log: Vec<PrRef>,
    /// Agents whose next `run()` must fail, set by `Triggers::agent_crash`.
    pub crash_next: HashSet<AgentId>,
    /// Errors the host returns for the next calls of a method, oldest first;
    /// set by `Triggers::fail_host`.
    pub host_faults: HashMap<String, VecDeque<HostError>>,
}

impl SharedState {
    /// Pops the next injected error for `method`, if any.
    pub fn take_fault(&mut self, method: &str) -> Option<HostError> {
        self.host_faults.get_mut(method)?.pop_front()
    }
}

impl SharedState {
    pub fn load(data_dir: &Path) -> Result<Self, FixtureError> {
        let mut prs = HashMap::new();
        let mut heads = HashMap::new();

        let host_file = data_dir.join("host").join("prs.json");
        if host_file.exists() {
            let list: Vec<PrDetail> = read_json(&host_file)?;
            for detail in list {
                let id = detail.summary.pr.id();
                heads.insert(id.clone(), detail.summary.head_sha.clone());
                prs.insert(id, detail);
            }
        }

        let mut revisions_by_head = HashMap::new();
        let mut files = HashMap::new();
        let git_dir = data_dir.join("git");
        if git_dir.exists() {
            for pr_dir in read_dir_sorted(&git_dir)? {
                if !pr_dir.is_dir() {
                    continue;
                }
                for entry in read_dir_sorted(&pr_dir)? {
                    if entry.is_dir() {
                        continue;
                    }
                    if entry.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    let revision: Revision = read_json(&entry)?;
                    revisions_by_head.insert(revision.head.clone(), revision);
                }

                let files_dir = pr_dir.join("files");
                if files_dir.exists() {
                    for rev_dir in read_dir_sorted(&files_dir)? {
                        if !rev_dir.is_dir() {
                            continue;
                        }
                        let rev = Sha::new(rev_dir.file_name().unwrap().to_string_lossy());
                        collect_files(&rev_dir, &rev_dir, &rev, &mut files)?;
                    }
                }
            }
        }

        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            prs,
            heads,
            revisions_by_head,
            files,
            calls: Vec::new(),
            change_log: Vec::new(),
            crash_next: HashSet::new(),
            host_faults: HashMap::new(),
        })
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, FixtureError> {
    let data = std::fs::read_to_string(path).map_err(|source| FixtureError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&data).map_err(|source| FixtureError::Json {
        path: path.to_path_buf(),
        source,
    })
}

fn read_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>, FixtureError> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|source| FixtureError::Io {
            path: dir.to_path_buf(),
            source,
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    Ok(entries)
}

fn collect_files(
    root: &Path,
    dir: &Path,
    rev: &Sha,
    out: &mut HashMap<(Sha, String), Vec<u8>>,
) -> Result<(), FixtureError> {
    for entry in read_dir_sorted(dir)? {
        if entry.is_dir() {
            collect_files(root, &entry, rev, out)?;
        } else {
            let rel = entry
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = std::fs::read(&entry).map_err(|source| FixtureError::Io {
                path: entry.clone(),
                source,
            })?;
            out.insert((rev.clone(), rel), bytes);
        }
    }
    Ok(())
}
