//! Background indexing of one workspace, most useful files first
//! (docs/code-intel.md §5.2).
//!
//! ```text
//! start(snapshot) ─▶ every file with a language ─▶ queue 3 (rest)
//! prioritize_paths(diff)        ─▶ queue 1
//! prioritize_mentions(names)    ─▶ text scan ─▶ queue 2
//! update_snapshot(new)          ─▶ changed paths ─▶ queue 1
//!
//! workers (N per workspace, one global permit pool):
//!   pop 1 ▸ 2 ▸ 3 ─▶ grammar? ── ready ───▶ spawn_blocking: read + parse + store
//!                             ├─ pending ─▶ parked until the provider answers
//!                             └─ missing ─▶ dropped (not indexable)
//! close / drop ─▶ cancel token + tree-sitter abort flag
//! ```
//!
//! `index_percent` = indexed files / indexable files, where a file is
//! indexable when it has a language whose grammar is ready or still being
//! fetched. A language the provider has no grammar for does not count:
//! its files are the text fallback's, and `Progress::missing_languages`
//! names them for the answer's note.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use futures::future::BoxFuture;
use memchr::memmem;
use theodore_ports::code_intel::LanguageId;
use tokio::sync::{Notify, Semaphore, watch};
use tokio_util::sync::CancellationToken;

use crate::DEFAULT_MAX_FILE_BYTES;
use crate::error::{Result, SyntaxError};
use crate::grammar::{CompiledGrammar, Grammar};
use crate::index::{BlobStatus, SyntaxIndex};
use crate::snapshot::{FileEntry, Snapshot};
use crate::text::mentions;

/// What a grammar provider returns: `None` when the language has no
/// grammar (not in the manifest, download declined or failed).
pub type GrammarFuture = BoxFuture<'static, Option<Grammar>>;

/// Loads a language's grammar (`code-intel-toolchain`). Called at most once
/// per language per workspace, unless [`WorkspaceIndex::refresh_grammar`].
pub type GrammarProvider = Arc<dyn Fn(LanguageId) -> GrammarFuture + Send + Sync>;

#[derive(Debug, Clone)]
pub struct IndexerConfig {
    /// Larger files are recorded as skipped, not parsed.
    pub max_file_bytes: usize,
    /// Files parsed at once, across all workspaces of one [`Indexer`].
    pub workers: usize,
}

impl Default for IndexerConfig {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            workers: (cpus / 2).clamp(1, 4),
        }
    }
}

/// Queue order. Lower runs first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// Files in the diff (head and base).
    Diff,
    /// Files mentioning the changed symbols' names.
    Mentions,
    /// Everything else.
    Rest,
}

impl Priority {
    const ALL: [Priority; 3] = [Priority::Diff, Priority::Mentions, Priority::Rest];

    fn slot(self) -> usize {
        self as usize
    }
}

/// How far a workspace's index is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub indexed: usize,
    pub indexable: usize,
    /// Languages whose grammar is still being fetched.
    pub pending_languages: Vec<LanguageId>,
    /// Languages in the worktree without a grammar.
    pub missing_languages: Vec<LanguageId>,
}

impl Progress {
    /// `indexed / indexable` as 0–100, rounded down (100 only when done);
    /// 100 when nothing is indexable.
    pub fn percent(&self) -> u8 {
        if self.indexable == 0 {
            return 100;
        }
        (self.indexed.min(self.indexable) * 100 / self.indexable) as u8
    }
}

/// Shared by every workspace: the index, the grammar provider and the cap
/// on parses running at once.
#[derive(Clone)]
pub struct Indexer {
    index: SyntaxIndex,
    provider: GrammarProvider,
    config: IndexerConfig,
    permits: Arc<Semaphore>,
}

impl Indexer {
    pub fn new(index: SyntaxIndex, provider: GrammarProvider, config: IndexerConfig) -> Self {
        let permits = Arc::new(Semaphore::new(config.workers.max(1)));
        Self {
            index,
            provider,
            config,
            permits,
        }
    }

    pub fn index(&self) -> &SyntaxIndex {
        &self.index
    }

    /// Starts indexing `snapshot` in the background; every file is queued
    /// at [`Priority::Rest`]. Must be called inside a Tokio runtime.
    pub fn start(&self, snapshot: Snapshot) -> WorkspaceIndex {
        let snapshot = Arc::new(snapshot);
        let shared = Arc::new(Shared {
            index: self.index.clone(),
            provider: self.provider.clone(),
            max_file_bytes: self.config.max_file_bytes,
            permits: self.permits.clone(),
            cancel: CancellationToken::new(),
            abort: Arc::new(AtomicUsize::new(0)),
            wake: Notify::new(),
            changed: watch::Sender::new(0),
            state: Mutex::new(State {
                snapshot: snapshot.clone(),
                queues: Default::default(),
                queued: HashMap::new(),
                parked: HashMap::new(),
                grammars: HashMap::new(),
                processed: HashSet::new(),
                in_flight: 0,
            }),
        });
        {
            let mut st = shared.lock();
            for f in snapshot.files().iter().filter(|f| f.language.is_some()) {
                st.enqueue(&f.path, Priority::Rest);
            }
        }
        shared.resolve(snapshot.languages());
        for _ in 0..self.config.workers.max(1) {
            tokio::spawn(worker(shared.clone()));
        }
        WorkspaceIndex { shared }
    }
}

enum GrammarState {
    Pending,
    Ready(Arc<CompiledGrammar>),
    Missing,
}

struct State {
    snapshot: Arc<Snapshot>,
    /// One deque per priority; an entry is live only while `queued` maps
    /// its path to that priority (promotion leaves a stale copy behind).
    queues: [VecDeque<String>; 3],
    queued: HashMap<String, Priority>,
    /// Popped while their language's grammar was pending.
    parked: HashMap<LanguageId, Vec<(Priority, String)>>,
    grammars: HashMap<LanguageId, GrammarState>,
    /// Blobs done with in this workspace: indexed, cached, skipped or
    /// failed.
    processed: HashSet<String>,
    in_flight: usize,
}

impl State {
    /// Queues `path` at `prio`, or promotes it. `false` when it already
    /// waits at that priority or better.
    fn enqueue(&mut self, path: &str, prio: Priority) -> bool {
        if self.queued.get(path).is_some_and(|p| *p <= prio) {
            return false;
        }
        self.queued.insert(path.to_string(), prio);
        self.queues[prio.slot()].push_back(path.to_string());
        true
    }

    fn pending_grammars(&self) -> bool {
        self.grammars
            .values()
            .any(|g| matches!(g, GrammarState::Pending))
    }

    fn is_idle(&self) -> bool {
        self.queued.is_empty()
            && self.parked.is_empty()
            && self.in_flight == 0
            && !self.pending_grammars()
    }

    /// Whether `path` still has work ahead: it has a language with a
    /// grammar (or one coming) and its blob is not processed.
    fn has_work(&self, f: &FileEntry) -> bool {
        let Some(lang) = &f.language else {
            return false;
        };
        !self.processed.contains(&f.blob_sha)
            && !matches!(self.grammars.get(lang), Some(GrammarState::Missing))
    }
}

struct Job {
    path: String,
    sha: String,
    grammar: Arc<CompiledGrammar>,
}

struct Shared {
    index: SyntaxIndex,
    provider: GrammarProvider,
    max_file_bytes: usize,
    permits: Arc<Semaphore>,
    cancel: CancellationToken,
    /// tree-sitter's cancellation flag: non-zero aborts a running parse.
    abort: Arc<AtomicUsize>,
    state: Mutex<State>,
    /// Workers wait here for new jobs.
    wake: Notify,
    /// Bumped on every state change, for `wait_idle` and `index_now`.
    changed: watch::Sender<u64>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn bump(&self) {
        self.wake.notify_waiters();
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }

    /// Asks the provider for every language not asked about yet.
    fn resolve(self: &Arc<Self>, langs: Vec<LanguageId>) {
        for lang in langs {
            {
                let mut st = self.lock();
                if st.grammars.contains_key(&lang) {
                    continue;
                }
                st.grammars.insert(lang.clone(), GrammarState::Pending);
            }
            let me = self.clone();
            tokio::spawn(async move {
                let fetch = (me.provider)(lang.clone());
                let grammar = tokio::select! {
                    _ = me.cancel.cancelled() => return,
                    g = fetch => g,
                };
                let compiled = match grammar {
                    Some(g) => {
                        let index = me.index.clone();
                        match tokio::task::spawn_blocking(move || index.compile(&g)).await {
                            Ok(Ok(c)) => Some(c),
                            Ok(Err(e)) => {
                                tracing::warn!(language = %lang, error = %e, "grammar unusable");
                                None
                            }
                            Err(_) => None,
                        }
                    }
                    None => None,
                };
                me.grammar_resolved(lang, compiled);
            });
        }
    }

    fn grammar_resolved(&self, lang: LanguageId, compiled: Option<Arc<CompiledGrammar>>) {
        {
            let mut st = self.lock();
            let parked = st.parked.remove(&lang).unwrap_or_default();
            match compiled {
                Some(c) => {
                    st.grammars.insert(lang, GrammarState::Ready(c));
                    for (prio, path) in parked {
                        st.enqueue(&path, prio);
                    }
                }
                None => {
                    st.grammars.insert(lang, GrammarState::Missing);
                }
            }
        }
        self.bump();
    }

    /// The next file to parse, best priority first.
    fn next_job(&self) -> Option<Job> {
        let mut guard = self.lock();
        let st = &mut *guard;
        for prio in Priority::ALL {
            while let Some(path) = st.queues[prio.slot()].pop_front() {
                if st.queued.get(&path) != Some(&prio) {
                    continue;
                }
                st.queued.remove(&path);
                let Some(entry) = st.snapshot.get(&path) else {
                    continue;
                };
                let Some(lang) = entry.language.clone() else {
                    continue;
                };
                if st.processed.contains(&entry.blob_sha) {
                    continue;
                }
                match st.grammars.get(&lang) {
                    Some(GrammarState::Ready(g)) => {
                        let job = Job {
                            sha: entry.blob_sha.clone(),
                            grammar: g.clone(),
                            path,
                        };
                        st.in_flight += 1;
                        return Some(job);
                    }
                    Some(GrammarState::Missing) => {}
                    Some(GrammarState::Pending) | None => {
                        st.parked.entry(lang).or_default().push((prio, path));
                    }
                }
            }
        }
        None
    }

    /// Reads and indexes one file. Blocking.
    fn run(&self, job: &Job) -> Result<()> {
        let status = self.index.status(&job.sha, &job.grammar)?;
        if matches!(status, Some(BlobStatus::Indexed | BlobStatus::Binary)) {
            return Ok(());
        }
        let full = self.lock().snapshot.root().join(&job.path);
        let len = std::fs::metadata(&full)?.len();
        if len > self.max_file_bytes as u64 {
            if status.is_none() {
                self.index
                    .record_skip(&job.sha, &job.grammar, BlobStatus::TooLarge)?;
            }
            return Ok(());
        }
        let source = std::fs::read(&full)?;
        self.index.index_compiled(
            &job.sha,
            &job.grammar,
            &source,
            self.max_file_bytes,
            Some(&self.abort),
        )?;
        Ok(())
    }

    fn finish(&self, sha: String, path: &str, result: Result<()>) {
        {
            let mut st = self.lock();
            st.in_flight = st.in_flight.saturating_sub(1);
            match result {
                Err(SyntaxError::Cancelled) => {}
                Err(e) => {
                    tracing::debug!(path, error = %e, "not indexed");
                    st.processed.insert(sha);
                }
                Ok(()) => {
                    st.processed.insert(sha);
                }
            }
        }
        self.bump();
    }
}

async fn worker(shared: Arc<Shared>) {
    loop {
        let job = loop {
            let notified = shared.wake.notified();
            tokio::pin!(notified);
            // Registered before looking, so a job queued in between still
            // wakes this worker.
            notified.as_mut().enable();
            if shared.cancel.is_cancelled() {
                return;
            }
            if let Some(job) = shared.next_job() {
                break job;
            }
            tokio::select! {
                _ = shared.cancel.cancelled() => return,
                _ = &mut notified => {}
            }
        };
        let permit = tokio::select! {
            _ = shared.cancel.cancelled() => {
                shared.finish(job.sha, &job.path, Err(SyntaxError::Cancelled));
                return;
            }
            p = shared.permits.clone().acquire_owned() => p,
        };
        let (sha, path) = (job.sha.clone(), job.path.clone());
        let me = shared.clone();
        let result = tokio::task::spawn_blocking(move || me.run(&job)).await;
        drop(permit);
        let result = result.unwrap_or_else(|e| {
            Err(SyntaxError::Io(std::io::Error::other(format!(
                "indexing panicked: {e}"
            ))))
        });
        shared.finish(sha, &path, result);
    }
}

/// One workspace being indexed. Dropping it (or [`close`](Self::close))
/// stops its workers and aborts a parse in progress.
pub struct WorkspaceIndex {
    shared: Arc<Shared>,
}

impl WorkspaceIndex {
    /// The snapshot queries should use: the latest one given.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.shared.lock().snapshot.clone()
    }

    /// Moves `paths` (the diff's files) to the front.
    pub fn prioritize_paths(&self, paths: &[String]) {
        {
            let mut st = self.shared.lock();
            for p in paths {
                let Some(f) = st.snapshot.get(p).cloned() else {
                    continue;
                };
                if st.has_work(&f) {
                    st.enqueue(p, Priority::Diff);
                }
            }
        }
        self.shared.bump();
    }

    /// Moves files that mention any of `names` as a whole word (the
    /// changed symbols) ahead of the rest. Scans the still-queued files on
    /// a blocking thread; returns how many were promoted.
    pub async fn prioritize_mentions(&self, names: &[String]) -> usize {
        let names: Vec<String> = names.iter().filter(|n| !n.is_empty()).cloned().collect();
        if names.is_empty() {
            return 0;
        }
        let (root, candidates) = {
            let st = self.shared.lock();
            // Waiting at `Rest`: queued, or parked until a grammar arrives.
            let mut candidates: Vec<String> = st
                .queued
                .iter()
                .filter(|(_, p)| **p == Priority::Rest)
                .map(|(path, _)| path.clone())
                .chain(
                    st.parked
                        .values()
                        .flatten()
                        .filter(|(p, _)| *p == Priority::Rest)
                        .map(|(_, path)| path.clone()),
                )
                .collect();
            candidates.sort();
            candidates.dedup();
            (st.snapshot.root().to_path_buf(), candidates)
        };
        let max = self.shared.max_file_bytes as u64;
        let cancel = self.shared.cancel.clone();
        let matched = tokio::task::spawn_blocking(move || {
            let finders: Vec<memmem::Finder<'_>> = names
                .iter()
                .map(|n| memmem::Finder::new(n.as_bytes()))
                .collect();
            let mut matched = Vec::new();
            for path in candidates {
                if cancel.is_cancelled() {
                    break;
                }
                let full = root.join(&path);
                if std::fs::metadata(&full).map_or(true, |m| m.len() > max) {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&full) else {
                    continue;
                };
                if finders.iter().any(|f| mentions(&bytes, f)) {
                    matched.push(path);
                }
            }
            matched
        })
        .await
        .unwrap_or_default();
        let promoted = {
            let mut st = self.shared.lock();
            matched
                .iter()
                .filter(|p| st.enqueue(p, Priority::Mentions))
                .count()
        };
        self.shared.bump();
        promoted
    }

    /// Switches to a newer snapshot of the same worktree (an edit, a new
    /// revision). Paths whose blob changed, and new paths, go to the
    /// front; new languages are fetched.
    pub fn update_snapshot(&self, snapshot: Snapshot) {
        let snapshot = Arc::new(snapshot);
        {
            let mut st = self.shared.lock();
            let old = std::mem::replace(&mut st.snapshot, snapshot.clone());
            for f in snapshot.files() {
                if !st.has_work(f) {
                    continue;
                }
                let changed = old.get(&f.path).map(|o| &o.blob_sha) != Some(&f.blob_sha);
                let prio = if changed {
                    Priority::Diff
                } else {
                    Priority::Rest
                };
                st.enqueue(&f.path, prio);
            }
        }
        self.shared.resolve(snapshot.languages());
        self.shared.bump();
    }

    /// One file added or changed on disk (the spaces watcher).
    pub fn update_file(&self, entry: FileEntry) {
        let next = self.snapshot().with_file(entry);
        self.update_snapshot(next);
    }

    /// One file removed.
    pub fn remove_file(&self, path: &str) {
        let next = self.snapshot().without(path);
        self.update_snapshot(next);
    }

    /// Asks the provider again for `lang` (a grammar was just installed or
    /// upgraded) and requeues its files.
    pub fn refresh_grammar(&self, lang: &LanguageId) {
        {
            let mut st = self.shared.lock();
            st.grammars.remove(lang);
            let files: Vec<FileEntry> = st
                .snapshot
                .files()
                .iter()
                .filter(|f| f.language.as_ref() == Some(lang))
                .cloned()
                .collect();
            for f in files {
                st.processed.remove(&f.blob_sha);
                st.enqueue(&f.path, Priority::Rest);
            }
        }
        self.shared.resolve(vec![lang.clone()]);
        self.shared.bump();
    }

    pub fn progress(&self) -> Progress {
        let st = self.shared.lock();
        let mut indexed = 0;
        let mut indexable = 0;
        for f in st.snapshot.files() {
            let Some(lang) = &f.language else {
                continue;
            };
            if matches!(st.grammars.get(lang), Some(GrammarState::Missing)) {
                continue;
            }
            indexable += 1;
            if st.processed.contains(&f.blob_sha) {
                indexed += 1;
            }
        }
        let mut pending = Vec::new();
        let mut missing = Vec::new();
        for (lang, g) in &st.grammars {
            match g {
                GrammarState::Pending => pending.push(lang.clone()),
                GrammarState::Missing => missing.push(lang.clone()),
                GrammarState::Ready(_) => {}
            }
        }
        pending.sort();
        missing.sort();
        Progress {
            indexed,
            indexable,
            pending_languages: pending,
            missing_languages: missing,
        }
    }

    /// Indexes `paths` ahead of everything and waits until each is done
    /// (or turns out not indexable). For a query about a file the
    /// background has not reached.
    pub async fn index_now(&self, paths: &[String]) {
        self.prioritize_paths(paths);
        let mut rx = self.shared.changed.subscribe();
        loop {
            let waiting = {
                let st = self.shared.lock();
                paths
                    .iter()
                    .any(|p| st.snapshot.get(p).is_some_and(|f| st.has_work(f)))
            };
            if !waiting || self.shared.cancel.is_cancelled() || rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Resolves once the queue is empty and nothing is parsing or waiting
    /// for a grammar (or the workspace closed).
    pub async fn wait_idle(&self) {
        let mut rx = self.shared.changed.subscribe();
        loop {
            if self.shared.cancel.is_cancelled() || self.shared.lock().is_idle() {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    /// Stops the workers and aborts a parse in progress. Queries keep
    /// working: the index stays.
    pub fn close(&self) {
        self.shared.abort.store(1, Ordering::SeqCst);
        self.shared.cancel.cancel();
        self.shared.bump();
    }

    pub fn is_closed(&self) -> bool {
        self.shared.cancel.is_cancelled()
    }
}

impl Drop for WorkspaceIndex {
    fn drop(&mut self) {
        self.close();
    }
}
