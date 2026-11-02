//! One open worktree: its index, its languages and their servers, and what
//! the text fallback still has to cover.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use theodore_code_intel_syntax::{Snapshot, SyntaxIndex, WorkspaceIndex};
use theodore_code_intel_toolchain::Manifest;
use theodore_ports::code_intel::{LanguageId, LspPolicy, TrustMode};
use tokio::task::JoinHandle;

use crate::servers::ServerPlan;

/// Path → language, honouring the workspace's language list.
pub(crate) type LanguageOf = Arc<dyn Fn(&str) -> Option<LanguageId> + Send + Sync>;

pub(crate) fn language_of(manifest: Manifest, only: Option<HashSet<LanguageId>>) -> LanguageOf {
    Arc::new(move |path: &str| {
        let id = &manifest.language_for_path(path)?.id;
        only.as_ref()
            .is_none_or(|o| o.contains(id))
            .then(|| id.clone())
    })
}

/// A language code intelligence knows something about: it has a grammar or
/// a server in the manifest. JSON, YAML, Markdown are not; the text
/// fallback leaves them out, and notes do not mention them.
pub(crate) fn is_code(manifest: &Manifest, language: &LanguageId) -> bool {
    manifest
        .language(language)
        .is_some_and(|l| l.grammar.is_some() || !l.servers.is_empty())
}

pub(crate) struct Ws {
    pub root: PathBuf,
    pub repo_key: String,
    pub trust: TrustMode,
    /// `Workspace::lsp`: this workspace's own policy, over the default.
    pub lsp: Option<LspPolicy>,
    /// `Workspace::languages` as given (empty: detected).
    pub requested: Vec<LanguageId>,
    pub pool_key: String,
    pub language_of: LanguageOf,
    pub index: WorkspaceIndex,
    pub servers: Mutex<BTreeMap<LanguageId, ServerPlan>>,
    /// Background work (mention scans), aborted on close.
    pub tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Ws {
    pub fn language(&self, path: &str) -> Option<LanguageId> {
        (self.language_of)(path)
    }

    pub fn plan(&self, language: &LanguageId) -> Option<ServerPlan> {
        lock(&self.servers).get(language).cloned()
    }

    /// Each distinct server and the languages it serves here.
    pub fn servers_by_name(&self) -> BTreeMap<String, (ServerPlan, Vec<LanguageId>)> {
        let mut out: BTreeMap<String, (ServerPlan, Vec<LanguageId>)> = BTreeMap::new();
        for (lang, plan) in lock(&self.servers).iter() {
            out.entry(plan.spec.name.clone())
                .or_insert_with(|| (plan.clone(), Vec::new()))
                .1
                .push(lang.clone());
        }
        out
    }

    /// Languages with files now, plus requested ones without files.
    pub fn languages(&self) -> Vec<LanguageId> {
        let mut langs = self.index.snapshot().languages();
        for l in &self.requested {
            if !langs.contains(l) {
                langs.push(l.clone());
            }
        }
        langs.sort();
        langs
    }

    pub fn track(&self, task: JoinHandle<()>) {
        let mut tasks = lock(&self.tasks);
        tasks.retain(|t| !t.is_finished());
        tasks.push(task);
    }

    pub fn shut(&self) {
        for t in lock(&self.tasks).drain(..) {
            t.abort();
        }
        self.index.close();
    }

    /// What the syntax layer does not cover yet, for the text fallback.
    /// `skip`: languages a language server already answered for.
    pub fn gap(&self, manifest: &Manifest, index: &SyntaxIndex, skip: &HashSet<LanguageId>) -> Gap {
        let progress = self.index.progress();
        let snap = self.index.snapshot();
        let not_ready: HashSet<LanguageId> = progress
            .pending_languages
            .iter()
            .chain(&progress.missing_languages)
            .cloned()
            .collect();
        let code: HashSet<LanguageId> = snap
            .languages()
            .into_iter()
            .filter(|l| is_code(manifest, l) && !skip.contains(l))
            .collect();
        let complete = progress.indexed >= progress.indexable;
        Gap {
            snap,
            index: index.clone(),
            empty: complete && code.iter().all(|l| !not_ready.contains(l)),
            not_ready,
            code,
            complete,
        }
    }
}

/// Files the text search covers: code files whose language has no grammar
/// (yet), and, while indexing runs, files not indexed so far.
pub(crate) struct Gap {
    snap: Arc<Snapshot>,
    index: SyntaxIndex,
    code: HashSet<LanguageId>,
    not_ready: HashSet<LanguageId>,
    complete: bool,
    empty: bool,
}

impl Gap {
    /// Nothing to cover: the walk can be skipped.
    pub fn is_empty(&self) -> bool {
        self.empty
    }

    /// Blocking (an index lookup while indexing runs).
    pub fn includes(&self, path: &str) -> bool {
        let Some(lang) = self.snap.get(path).and_then(|f| f.language.as_ref()) else {
            return false;
        };
        if !self.code.contains(lang) {
            return false;
        }
        if self.not_ready.contains(lang) {
            return true;
        }
        !self.complete
            && self
                .snap
                .get(path)
                .is_some_and(|f| !self.index.is_indexed(&f.blob_sha).unwrap_or(true))
    }
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
