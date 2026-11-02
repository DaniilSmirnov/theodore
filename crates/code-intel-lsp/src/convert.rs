//! LSP values that need no file text, as contract values.

use lsp_types::{HoverContents, MarkedString, NumberOrString, SymbolKind};
use serde_json::Value;
use theodore_ports::{CodeSymbolKind, DiagnosticSeverity};

pub(crate) fn symbol_kind(kind: SymbolKind) -> CodeSymbolKind {
    match kind {
        SymbolKind::FUNCTION => CodeSymbolKind::Function,
        SymbolKind::METHOD | SymbolKind::CONSTRUCTOR => CodeSymbolKind::Method,
        SymbolKind::CLASS => CodeSymbolKind::Class,
        SymbolKind::STRUCT => CodeSymbolKind::Struct,
        SymbolKind::ENUM => CodeSymbolKind::Enum,
        // rust-analyzer reports traits as interfaces.
        SymbolKind::INTERFACE => CodeSymbolKind::Interface,
        // rust-analyzer reports type aliases as type parameters.
        SymbolKind::TYPE_PARAMETER => CodeSymbolKind::Type,
        SymbolKind::CONSTANT | SymbolKind::ENUM_MEMBER => CodeSymbolKind::Constant,
        SymbolKind::VARIABLE => CodeSymbolKind::Variable,
        SymbolKind::FIELD | SymbolKind::PROPERTY => CodeSymbolKind::Field,
        SymbolKind::MODULE | SymbolKind::NAMESPACE | SymbolKind::PACKAGE => CodeSymbolKind::Module,
        _ => CodeSymbolKind::Other,
    }
}

/// Absent severity is the client's call; the spec suggests error.
pub(crate) fn severity(s: Option<lsp_types::DiagnosticSeverity>) -> DiagnosticSeverity {
    match s {
        Some(lsp_types::DiagnosticSeverity::WARNING) => DiagnosticSeverity::Warning,
        Some(lsp_types::DiagnosticSeverity::INFORMATION) => DiagnosticSeverity::Info,
        Some(lsp_types::DiagnosticSeverity::HINT) => DiagnosticSeverity::Hint,
        _ => DiagnosticSeverity::Error,
    }
}

pub(crate) fn code(c: Option<NumberOrString>) -> Option<String> {
    match c? {
        NumberOrString::Number(n) => Some(n.to_string()),
        NumberOrString::String(s) => Some(s),
    }
}

/// Hover contents as one markdown-ish string; blank → `None`.
pub(crate) fn hover_text(contents: HoverContents) -> Option<String> {
    fn marked(m: MarkedString) -> String {
        match m {
            MarkedString::String(s) => s,
            MarkedString::LanguageString(ls) => format!("```{}\n{}\n```", ls.language, ls.value),
        }
    }
    let text = match contents {
        HoverContents::Scalar(m) => marked(m),
        HoverContents::Array(ms) => ms.into_iter().map(marked).collect::<Vec<_>>().join("\n\n"),
        HoverContents::Markup(m) => m.value,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The answer to one `workspace/configuration` item, from `init_options`:
///
/// | section | answer |
/// | --- | --- |
/// | none | all of `init_options` |
/// | `a.b` present in `init_options` | that value |
/// | `<server>` or `<server>.a.b` | `init_options` itself, or its `a.b` |
/// | anything else | `null` |
///
/// The third row is the common convention: a server's initialization
/// options are its own settings section (`rust-analyzer` asks for section
/// `"rust-analyzer"` and expects `{ "cargo": … }`).
pub(crate) fn config_section(init: &Value, server: &str, section: Option<&str>) -> Value {
    let Some(section) = section.filter(|s| !s.is_empty()) else {
        return init.clone();
    };
    let lookup = |path: &str| -> Option<Value> {
        if path.is_empty() {
            return Some(init.clone());
        }
        path.split('.').try_fold(init, |v, key| v.get(key)).cloned()
    };
    if let Some(v) = lookup(section) {
        return v;
    }
    if section == server {
        return init.clone();
    }
    if let Some(rest) = section
        .strip_prefix(server)
        .and_then(|r| r.strip_prefix('.'))
        && let Some(v) = lookup(rest)
    {
        return v;
    }
    Value::Null
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn configuration_sections_resolve_against_init_options() {
        let init = json!({"cargo": {"buildScripts": {"enable": false}}, "fake": {"x": 1}});
        let ra = "rust-analyzer";
        assert_eq!(config_section(&init, ra, None), init);
        assert_eq!(config_section(&init, ra, Some("fake.x")), json!(1));
        assert_eq!(config_section(&init, ra, Some("rust-analyzer")), init);
        assert_eq!(
            config_section(&init, ra, Some("rust-analyzer.cargo.buildScripts")),
            json!({"enable": false})
        );
        assert_eq!(config_section(&init, ra, Some("editor")), Value::Null);
    }
}
