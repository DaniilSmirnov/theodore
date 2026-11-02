//! The running servers, a few per repository.
//!
//! ```text
//! key: (repo_key, server)             workspace_folders: one session, worktrees
//!      (repo_key, server, root)       join as folders; else one per worktree
//!
//! get_or_spawn ─▶ slot alive ─────────────▶ session (joins as a folder if new)
//!              ├▶ slot crashed ─ 1st ─────▶ respawn
//!              │               └ 2nd ─────▶ Err, remembered for the root
//!              └▶ none ─▶ full? evict LRU idle ─▶ spawn
//! reaper: idle longer than `idle` ─▶ shutdown
//! ```
//!
//! A crashed session is restarted lazily, by the next `get_or_spawn`; until
//! then `status` reports it `NotRunning`, so a caller routes the next query
//! here and it restarts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use theodore_ports::{CodeIntelError, CodeIntelResult, ServerState};
use tokio::sync::Mutex;
use tokio::task::JoinSet;

use crate::LaunchConfig;
use crate::session::{LspSession, SessionOptions};

/// Crashes after which a server is not restarted for a root.
const MAX_CRASHES: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SlotKey {
    repo_key: String,
    server: String,
    /// `None`: shared by the repo's worktrees as workspace folders.
    root: Option<PathBuf>,
}

struct Slot {
    session: Arc<LspSession>,
    /// The worktrees it serves (canonical).
    roots: Vec<PathBuf>,
}

#[derive(Default)]
struct Slots {
    map: HashMap<SlotKey, Slot>,
    /// Crashes per (root, server).
    crashes: HashMap<(PathBuf, String), u32>,
    /// Shared keys whose server turned out to take one root only.
    single_root: Vec<(String, String)>,
}

impl Slots {
    /// Drop crashed sessions, counting a crash for each root they served.
    fn reap_failed(&mut self) {
        let failed: Vec<SlotKey> = self
            .map
            .iter()
            .filter(|(_, s)| s.session.state() == ServerState::Failed)
            .map(|(k, _)| k.clone())
            .collect();
        for key in failed {
            let slot = self.map.remove(&key).expect("listed above");
            for root in slot.roots {
                *self.crashes.entry((root, key.server.clone())).or_default() += 1;
            }
        }
    }

    fn crashes(&self, root: &Path, server: &str) -> u32 {
        self.crashes
            .get(&(root.to_path_buf(), server.to_string()))
            .copied()
            .unwrap_or(0)
    }

    /// The slot serving `root` with `server` for `repo_key`, if any.
    fn find(&self, repo_key: &str, root: &Path, server: &str) -> Option<&Slot> {
        let per_root = SlotKey {
            repo_key: repo_key.to_string(),
            server: server.to_string(),
            root: Some(root.to_path_buf()),
        };
        let shared = SlotKey {
            root: None,
            ..per_root.clone()
        };
        self.map.get(&per_root).or_else(|| {
            self.map
                .get(&shared)
                .filter(|s| s.roots.iter().any(|r| r == root))
        })
    }
}

/// Language servers by repository and worktree, with a cap, an idle
/// timeout, and one restart after a crash.
pub struct LspPool {
    inner: Arc<Inner>,
}

struct Inner {
    max_servers: usize,
    idle: Duration,
    options: SessionOptions,
    slots: Mutex<Slots>,
}

impl LspPool {
    /// At most `max_servers` alive; one unused for `idle` is shut down.
    /// Must be called inside a tokio runtime (the idle reaper is a task).
    pub fn new(max_servers: usize, idle: Duration) -> Self {
        Self::with_options(max_servers, idle, SessionOptions::default())
    }

    pub fn with_options(max_servers: usize, idle: Duration, options: SessionOptions) -> Self {
        let inner = Arc::new(Inner {
            max_servers: max_servers.max(1),
            idle,
            options,
            slots: Mutex::default(),
        });
        tokio::spawn(reaper(Arc::downgrade(&inner)));
        Self { inner }
    }

    /// The session serving `root` with `config.server`, started if needed.
    /// Returns while it is still warming up.
    pub async fn get_or_spawn(
        &self,
        repo_key: &str,
        root: &Path,
        config: &LaunchConfig,
    ) -> CodeIntelResult<Arc<LspSession>> {
        let root = canonical(root);
        let server = config.server.clone();
        let mut slots = self.inner.slots.lock().await;
        slots.reap_failed();
        if let Some(slot) = slots.find(repo_key, &root, &server) {
            return Ok(slot.session.clone());
        }
        if slots.crashes(&root, &server) >= MAX_CRASHES {
            return Err(CodeIntelError::Server {
                server,
                message: format!("crashed {MAX_CRASHES} times; not restarting"),
            });
        }
        let shareable = config.workspace_folders
            && !slots
                .single_root
                .contains(&(repo_key.to_string(), server.clone()));
        let shared_key = SlotKey {
            repo_key: repo_key.to_string(),
            server: server.clone(),
            root: None,
        };

        if shareable && let Some(slot) = slots.map.get_mut(&shared_key) {
            let session = slot.session.clone();
            slot.roots.push(root.clone());
            drop(slots);
            match session.add_folder(&root).await {
                Ok(()) => return Ok(session),
                Err(CodeIntelError::Unsupported(_)) => {
                    let mut slots = self.inner.slots.lock().await;
                    if let Some(slot) = slots.map.get_mut(&shared_key) {
                        slot.roots.retain(|r| *r != root);
                    }
                    slots
                        .single_root
                        .push((repo_key.to_string(), server.clone()));
                    drop(slots);
                    return Box::pin(self.get_or_spawn(repo_key, &root, config)).await;
                }
                Err(e) => {
                    let mut slots = self.inner.slots.lock().await;
                    if let Some(slot) = slots.map.get_mut(&shared_key) {
                        slot.roots.retain(|r| *r != root);
                    }
                    return Err(e);
                }
            }
        }

        self.make_room(&mut slots, &server)?;
        let session = LspSession::start(&root, config.clone(), self.inner.options.clone())?;
        let key = SlotKey {
            root: (!shareable).then(|| root.clone()),
            ..shared_key
        };
        slots.map.insert(
            key,
            Slot {
                session: session.clone(),
                roots: vec![root],
            },
        );
        Ok(session)
    }

    /// Evict the least recently used idle session while at the cap.
    fn make_room(&self, slots: &mut Slots, server: &str) -> CodeIntelResult<()> {
        while slots.map.len() >= self.inner.max_servers {
            let Some(key) = slots
                .map
                .iter()
                .filter(|(_, s)| !s.session.is_busy())
                .min_by_key(|(_, s)| s.session.last_used())
                .map(|(k, _)| k.clone())
            else {
                return Err(CodeIntelError::Server {
                    server: server.to_string(),
                    message: format!("all {} language servers are busy", self.inner.max_servers),
                });
            };
            let slot = slots.map.remove(&key).expect("found above");
            tracing::debug!(server = %key.server, repo = %key.repo_key, "lsp: evicted (LRU)");
            tokio::spawn(async move { slot.session.shutdown().await });
        }
        Ok(())
    }

    /// The live session serving `root` with `server`, without starting one.
    pub async fn get(&self, repo_key: &str, root: &Path, server: &str) -> Option<Arc<LspSession>> {
        let root = canonical(root);
        let slots = self.inner.slots.lock().await;
        slots
            .find(repo_key, &root, server)
            .map(|s| s.session.clone())
            .filter(|s| matches!(s.state(), ServerState::Warming | ServerState::Ready))
    }

    /// A worktree closed: it leaves shared sessions, its own sessions shut
    /// down, and its crash record is forgotten.
    pub async fn release_root(&self, root: &Path) {
        let root = canonical(root);
        let mut leave = Vec::new();
        let mut stop = Vec::new();
        {
            let mut slots = self.inner.slots.lock().await;
            slots.crashes.retain(|(r, _), _| *r != root);
            let keys: Vec<SlotKey> = slots
                .map
                .iter()
                .filter(|(_, s)| s.roots.contains(&root))
                .map(|(k, _)| k.clone())
                .collect();
            for key in keys {
                let slot = slots.map.get_mut(&key).expect("listed above");
                slot.roots.retain(|r| *r != root);
                if slot.roots.is_empty() {
                    stop.push(slots.map.remove(&key).expect("listed above").session);
                } else {
                    leave.push(slot.session.clone());
                }
            }
        }
        for s in leave {
            s.remove_folder(&root).await;
        }
        shutdown_each(stop).await;
    }

    /// Each server serving `root` and its state. A server that crashed
    /// and will be restarted on demand is `NotRunning`; one that crashed
    /// for the last time is `Failed`.
    pub async fn status(&self, root: &Path) -> Vec<(String, ServerState)> {
        let root = canonical(root);
        let mut slots = self.inner.slots.lock().await;
        slots.reap_failed();
        let mut out: Vec<(String, ServerState)> = slots
            .map
            .iter()
            .filter(|(_, s)| s.roots.contains(&root))
            .map(|(k, s)| (k.server.clone(), s.session.state()))
            .collect();
        for ((r, server), n) in &slots.crashes {
            if *r == root && !out.iter().any(|(s, _)| s == server) {
                let state = if *n >= MAX_CRASHES {
                    ServerState::Failed
                } else {
                    ServerState::NotRunning
                };
                out.push((server.clone(), state));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Shut every server down.
    pub async fn shutdown_all(&self) {
        let sessions: Vec<Arc<LspSession>> = {
            let mut slots = self.inner.slots.lock().await;
            slots.map.drain().map(|(_, s)| s.session).collect()
        };
        shutdown_each(sessions).await;
    }

    /// Sessions alive now.
    pub async fn len(&self) -> usize {
        self.inner.slots.lock().await.map.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

async fn shutdown_each(sessions: Vec<Arc<LspSession>>) {
    let mut set = JoinSet::new();
    for s in sessions {
        set.spawn(async move { s.shutdown().await });
    }
    while set.join_next().await.is_some() {}
}

fn canonical(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

/// Shut down sessions idle longer than `idle`; ends with the pool.
async fn reaper(inner: Weak<Inner>) {
    let period = match inner.upgrade() {
        Some(i) => (i.idle / 4).clamp(Duration::from_millis(10), Duration::from_secs(30)),
        None => return,
    };
    loop {
        tokio::time::sleep(period).await;
        let Some(inner) = inner.upgrade() else { return };
        let idle: Vec<Arc<LspSession>> = {
            let mut slots = inner.slots.lock().await;
            slots.reap_failed();
            let keys: Vec<SlotKey> = slots
                .map
                .iter()
                .filter(|(_, s)| {
                    !s.session.is_busy() && s.session.last_used().elapsed() >= inner.idle
                })
                .map(|(k, _)| k.clone())
                .collect();
            keys.iter()
                .filter_map(|k| slots.map.remove(k))
                .map(|s| s.session)
                .collect()
        };
        drop(inner);
        for s in idle {
            tracing::debug!(server = %s.server(), "lsp: idle, shutting down");
            tokio::spawn(async move { s.shutdown().await });
        }
    }
}
