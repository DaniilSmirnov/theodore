//! Wire types for the `review-tools` MCP tools: request params (schemars, for
//! the tool's `inputSchema`) and response bodies (plain serde, sent back as
//! JSON tool content).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use theodore_domain::{Commit, FileStatus, Hunk, LineKind, Severity};

use crate::backend::{FileSummary, GroupSummary, GroupWithHunks, NewFinding, NewPatch, PrContext};

// ── get_group ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetGroupParams {
    /// Group id, as returned by `get_pr_context`.
    pub id: String,
}

// ── get_base_file ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetBaseFileParams {
    /// Repo-relative path, as listed by `get_pr_context`.
    pub path: String,
}

// ── add_finding ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SeverityParam {
    Info,
    Warn,
    High,
}

impl From<SeverityParam> for Severity {
    fn from(s: SeverityParam) -> Self {
        match s {
            SeverityParam::Info => Severity::Info,
            SeverityParam::Warn => Severity::Warn,
            SeverityParam::High => Severity::High,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddFindingParams {
    /// Repo-relative path of the finding.
    pub file: String,
    /// 1-based line number on the new side of the diff.
    pub line: u32,
    pub severity: SeverityParam,
    pub title: String,
    pub body: String,
}

impl From<AddFindingParams> for NewFinding {
    fn from(p: AddFindingParams) -> Self {
        NewFinding {
            file: p.file,
            line: p.line,
            severity: p.severity.into(),
            title: p.title,
            body: p.body,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AddFindingResult {
    pub finding_id: String,
}

// ── propose_patch ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ProposePatchParams {
    /// Id of the finding this patch fixes, as returned by `add_finding`.
    pub finding_id: String,
    pub file: String,
    /// Lines being replaced.
    pub before: Vec<String>,
    /// Replacement lines.
    pub after: Vec<String>,
    pub note: String,
}

impl ProposePatchParams {
    pub fn into_new_patch(self) -> NewPatch {
        NewPatch {
            finding_id: theodore_domain::FindingId::new(self.finding_id),
            file: self.file,
            before: self.before,
            after: self.after,
            note: self.note,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ProposePatchResult {
    pub ok: bool,
}

// ── get_pr_context response ─────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct CommitResponse {
    pub sha: String,
    pub title: String,
    pub author: String,
}

impl From<&Commit> for CommitResponse {
    fn from(c: &Commit) -> Self {
        CommitResponse {
            sha: c.sha.to_string(),
            title: c.title.clone(),
            author: c.author.login.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct FileSummaryResponse {
    pub path: String,
    pub status: &'static str,
    pub additions: u32,
    pub deletions: u32,
}

fn status_str(s: FileStatus) -> &'static str {
    match s {
        FileStatus::Added => "added",
        FileStatus::Modified => "modified",
        FileStatus::Deleted => "deleted",
        FileStatus::Renamed => "renamed",
        FileStatus::Copied => "copied",
    }
}

impl From<&FileSummary> for FileSummaryResponse {
    fn from(f: &FileSummary) -> Self {
        FileSummaryResponse {
            path: f.path.clone(),
            status: status_str(f.status),
            additions: f.additions,
            deletions: f.deletions,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct GroupSummaryResponse {
    pub id: String,
    pub title: String,
    pub why: String,
    pub hunk_count: usize,
}

impl From<&GroupSummary> for GroupSummaryResponse {
    fn from(g: &GroupSummary) -> Self {
        GroupSummaryResponse {
            id: g.id.clone(),
            title: g.title.clone(),
            why: g.why.clone(),
            hunk_count: g.hunk_count,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PrContextResult {
    pub title: String,
    pub body: String,
    pub commits: Vec<CommitResponse>,
    pub files: Vec<FileSummaryResponse>,
    pub groups: Vec<GroupSummaryResponse>,
}

impl From<&PrContext> for PrContextResult {
    fn from(ctx: &PrContext) -> Self {
        PrContextResult {
            title: ctx.title.clone(),
            body: ctx.body.clone(),
            commits: ctx.commits.iter().map(CommitResponse::from).collect(),
            files: ctx.files.iter().map(FileSummaryResponse::from).collect(),
            groups: ctx.groups.iter().map(GroupSummaryResponse::from).collect(),
        }
    }
}

// ── get_group response ──────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct GroupResult {
    pub id: String,
    pub title: String,
    pub why: String,
    pub kind: &'static str,
    pub risk: Option<String>,
    pub check: Vec<String>,
    /// The group's hunks, rendered as unified diff text.
    pub diff: String,
}

impl From<&GroupWithHunks> for GroupResult {
    fn from(g: &GroupWithHunks) -> Self {
        let kind = match g.group.kind {
            theodore_domain::GroupKind::Normal => "normal",
            theodore_domain::GroupKind::Skim => "skim",
            theodore_domain::GroupKind::Hidden => "hidden",
        };
        GroupResult {
            id: g.group.id.to_string(),
            title: g.group.title.clone(),
            why: g.group.why.clone(),
            kind,
            risk: g.group.risk.clone(),
            check: g.group.check.clone(),
            diff: render_unified_diff(&g.hunks),
        }
    }
}

/// Renders hunks as unified diff text, grouped under one `---`/`+++` header per
/// file. Presentation only: the hunks themselves already carry the diff.
pub fn render_unified_diff(hunks: &[Hunk]) -> String {
    let mut out = String::new();
    let mut current_path: Option<&str> = None;
    for h in hunks {
        if current_path != Some(h.path.as_str()) {
            out.push_str(&format!("--- a/{p}\n+++ b/{p}\n", p = h.path));
            current_path = Some(h.path.as_str());
        }
        out.push_str(&format!(
            "@@ -{os},{ol} +{ns},{nl} @@ {hdr}\n",
            os = h.old_start,
            ol = h.old_lines,
            ns = h.new_start,
            nl = h.new_lines,
            hdr = h.header,
        ));
        for line in &h.lines {
            let prefix = match line.kind {
                LineKind::Ctx => ' ',
                LineKind::Add => '+',
                LineKind::Del => '-',
            };
            out.push(prefix);
            out.push_str(&line.text);
            out.push('\n');
        }
    }
    out
}

// ── get_base_file response ──────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct BaseFileResult {
    pub path: String,
    pub exists: bool,
    pub content: String,
}

// ── read_logs (space-tools; read-only on a space's PR) ──────────────────────

/// Lines `read_logs` returns when `tail` is left out.
pub const DEFAULT_LOG_TAIL: usize = 100;
/// The most lines one `read_logs` call returns.
pub const MAX_LOG_TAIL: usize = 400;

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LogKindParam {
    Setup,
    Run,
    Check,
    Archive,
}

impl From<LogKindParam> for theodore_ports::SpaceCommandName {
    fn from(k: LogKindParam) -> Self {
        use theodore_ports::SpaceCommandName as N;
        match k {
            LogKindParam::Setup => N::Setup,
            LogKindParam::Run => N::Run,
            LogKindParam::Check => N::Check,
            LogKindParam::Archive => N::Archive,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadLogsParams {
    /// Which command's output: setup, run (the app), check or archive.
    pub kind: LogKindParam,
    /// How many of the last lines (default 100, at most 400).
    pub tail: Option<u32>,
}

impl ReadLogsParams {
    /// `tail`, bounded to `1..=MAX_LOG_TAIL`.
    pub fn bounded_tail(&self) -> usize {
        self.tail
            .map_or(DEFAULT_LOG_TAIL, |t| t as usize)
            .clamp(1, MAX_LOG_TAIL)
    }
}

// ── code tools (docs/code-intel.md §7) ──────────────────────────────────────

/// Items one code tool call returns, at most: an agent never gets a huge
/// payload; `total` says how many there were.
pub const MAX_CODE_ITEMS: usize = 50;
/// Chars of one hit's preview.
pub const MAX_PREVIEW_CHARS: usize = 400;
/// Chars of `get_symbol`'s source.
pub const MAX_BODY_CHARS: usize = 20_000;

/// `text` cut to `max` chars, and whether it was.
fn cap(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((i, _)) => (format!("{}…", &text[..i]), true),
        None => (text.to_string(), false),
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKindParam {
    Function,
    Method,
    Class,
    Struct,
    Enum,
    Interface,
    Trait,
    Type,
    Constant,
    Variable,
    Field,
    Module,
    Macro,
}

impl From<SymbolKindParam> for theodore_ports::CodeSymbolKind {
    fn from(k: SymbolKindParam) -> Self {
        use theodore_ports::CodeSymbolKind as K;
        match k {
            SymbolKindParam::Function => K::Function,
            SymbolKindParam::Method => K::Method,
            SymbolKindParam::Class => K::Class,
            SymbolKindParam::Struct => K::Struct,
            SymbolKindParam::Enum => K::Enum,
            SymbolKindParam::Interface => K::Interface,
            SymbolKindParam::Trait => K::Trait,
            SymbolKindParam::Type => K::Type,
            SymbolKindParam::Constant => K::Constant,
            SymbolKindParam::Variable => K::Variable,
            SymbolKindParam::Field => K::Field,
            SymbolKindParam::Module => K::Module,
            SymbolKindParam::Macro => K::Macro,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindSymbolParams {
    /// The exact, case-sensitive name (`save`, not `Store::save`).
    pub name: String,
    /// Only definitions of this kind.
    pub kind: Option<SymbolKindParam>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetSymbolParams {
    /// Repo-relative path of the file defining it.
    pub path: String,
    pub name: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PositionParams {
    /// Repo-relative path.
    pub path: String,
    /// 1-based line.
    pub line: u32,
    /// 1-based column, in characters.
    pub col: u32,
}

impl PositionParams {
    pub fn at(&self) -> theodore_ports::Position {
        theodore_ports::Position {
            line: self.line,
            col: self.col,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DiagnosticsParams {
    /// One file; left out: every file the language server reported on.
    pub path: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReplaceSymbolBodyParams {
    pub path: String,
    pub name: String,
    /// The symbol's new source: signature and body, as `get_symbol`
    /// returns it.
    pub body: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct InsertAfterSymbolParams {
    pub path: String,
    /// The symbol the text goes after.
    pub name: String,
    /// Inserted on the line after the symbol's end.
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RenameSymbolParams {
    /// Repo-relative path of a use or the definition.
    pub path: String,
    pub line: u32,
    pub col: u32,
    pub new_name: String,
}

/// A query's answer: at most [`MAX_CODE_ITEMS`] items, and how far to
/// trust them.
#[derive(Debug, Serialize)]
pub struct CodeAnswerResult<T> {
    pub items: Vec<T>,
    /// Items before the cap.
    pub total: usize,
    pub truncated: bool,
    /// `lsp` (exact), `syntax` (by name) or `text` (a search).
    pub source: theodore_ports::Source,
    /// Share of the worktree indexed so far, 0–100.
    pub index_percent: u8,
    /// Why the answer may be partial.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl<T> CodeAnswerResult<T> {
    pub fn from_answer<U>(a: theodore_ports::Answer<U>, f: impl Fn(U) -> T) -> Self {
        let total = a.items.len();
        CodeAnswerResult {
            items: a.items.into_iter().take(MAX_CODE_ITEMS).map(f).collect(),
            total,
            truncated: total > MAX_CODE_ITEMS,
            source: a.source,
            index_percent: a.index_percent,
            note: a.note,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SymbolItem {
    pub name: String,
    pub kind: theodore_ports::CodeSymbolKind,
    pub path: String,
    /// Where the name is.
    pub line: u32,
    pub col: u32,
    /// The definition's last line.
    pub end_line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    pub source: theodore_ports::Source,
}

impl From<theodore_ports::CodeSymbol> for SymbolItem {
    fn from(s: theodore_ports::CodeSymbol) -> Self {
        SymbolItem {
            name: s.name,
            kind: s.kind,
            path: s.path,
            line: s.name_range.start.line,
            col: s.name_range.start.col,
            end_line: s.range.end.line,
            container: s.container,
            source: s.source,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct HitItem {
    pub path: String,
    pub line: u32,
    pub col: u32,
    pub preview: String,
    pub is_definition: bool,
    pub source: theodore_ports::Source,
}

impl From<theodore_ports::CodeHit> for HitItem {
    fn from(h: theodore_ports::CodeHit) -> Self {
        HitItem {
            path: h.location.path,
            line: h.location.range.start.line,
            col: h.location.range.start.col,
            preview: cap(&h.preview, MAX_PREVIEW_CHARS).0,
            is_definition: h.is_definition,
            source: h.source,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct DiagnosticItem {
    pub path: String,
    pub line: u32,
    pub col: u32,
    pub severity: theodore_ports::DiagnosticSeverity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub server: String,
}

impl From<theodore_ports::Diagnostic> for DiagnosticItem {
    fn from(d: theodore_ports::Diagnostic) -> Self {
        DiagnosticItem {
            path: d.location.path,
            line: d.location.range.start.line,
            col: d.location.range.start.col,
            severity: d.severity,
            message: cap(&d.message, MAX_PREVIEW_CHARS).0,
            code: d.code,
            server: d.server,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SymbolBodyResult {
    pub symbol: SymbolItem,
    pub start_line: u32,
    /// The symbol's source (signature and body).
    pub body: String,
    /// `body` was cut at [`MAX_BODY_CHARS`].
    pub truncated: bool,
}

impl SymbolBodyResult {
    pub fn new(symbol: theodore_ports::CodeSymbol, body: &str) -> Self {
        let (body, truncated) = cap(body, MAX_BODY_CHARS);
        SymbolBodyResult {
            start_line: symbol.range.start.line,
            symbol: symbol.into(),
            body,
            truncated,
        }
    }
}

/// `code_status`: one language.
#[derive(Debug, Serialize)]
pub struct LanguageItem {
    pub language: String,
    pub files: usize,
    /// `missing`, `installed`, `system`, …
    pub grammar: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// `missing`, `installed`, `system`, `declined`, …
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_install: Option<String>,
    /// `not_running`, `warming`, `ready`, `failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_state: Option<theodore_ports::ServerState>,
    pub index_percent: u8,
}

fn install_name(s: &theodore_ports::InstallState) -> String {
    use theodore_ports::InstallState as S;
    match s {
        S::Missing => "missing",
        S::Downloading => "downloading",
        S::System { .. } => "system",
        S::Installed { .. } => "installed",
        S::Unavailable => "unavailable",
        S::Declined => "declined",
        S::Failed { .. } => "failed",
    }
    .into()
}

impl From<theodore_ports::LanguageStatus> for LanguageItem {
    fn from(l: theodore_ports::LanguageStatus) -> Self {
        LanguageItem {
            language: l.language.0,
            files: l.files,
            grammar: install_name(&l.grammar),
            server_install: l.server.as_ref().map(|s| install_name(&s.install)),
            server_state: l.server.as_ref().map(|s| s.state),
            server: l.server.map(|s| s.name),
            index_percent: l.index_percent,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CodeStatusResult {
    pub languages: Vec<LanguageItem>,
}

#[derive(Debug, Serialize)]
pub struct CodeEditResult {
    /// Files written, repo-relative.
    pub files: Vec<String>,
    pub edits: usize,
}

impl From<crate::backend::CodeEditOutcome> for CodeEditResult {
    fn from(o: crate::backend::CodeEditOutcome) -> Self {
        CodeEditResult {
            files: o.files,
            edits: o.edits,
        }
    }
}

#[cfg(test)]
mod code_tests {
    use super::*;

    #[test]
    fn caps_count_chars() {
        assert_eq!(cap("héllo", 10), ("héllo".to_string(), false));
        assert_eq!(cap("héllo", 2), ("hé…".to_string(), true));
    }
}
