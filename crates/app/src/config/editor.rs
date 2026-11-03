//! The config editor's pure half (docs/config.md "Creating and saving the
//! config"): checking a file's text with line numbers, and writing YAML
//! from values with a commented template for the keys left unset.
//!
//! ```text
//! text ─▶ YAML parse ──fail──▶ error at the parser's line/column
//!            └─ok─▶ normalize ─▶ unknown keys ─▶ warning at the key's line
//!                        └─▶ each section key typed alone ─▶ error at its line
//!
//! values ─▶ per known key: set ─▶ YAML · unset ─▶ "# example" lines
//! ```

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use theodore_contract::{ConfigProblem, ConfigProblemLevel};

use super::{CodeIntelConfig, ReviewConfig, SpacesConfig, flatten, is_known, normalize};

/// The keys of each section in the order the editor writes them, each with
/// the commented example used when it is unset (the global template's
/// wording, `GLOBAL_TEMPLATE`).
const REVIEW_KEYS: &[(&str, &str)] = &[
    (
        "agents",
        "agents:\n  chat: claude          # claude | codex | gemini\n  grouping: claude",
    ),
    (
        "instructions",
        "instructions: Prefer small functions. Flag any new dependency.",
    ),
    (
        "context",
        "context: [docs/architecture.md, \"docs/adr/*.md\"]  # read first",
    ),
    (
        "hide",
        "hide: [\"gen/**\"]        # never sent to the agent",
    ),
    ("skim", "skim: [\"**/*.snap\"]     # flagged as skimmable"),
    (
        "checklist",
        "checklist:\n  \"migrations/**\": Is there a down migration?",
    ),
    ("auto_skills", "auto_skills: [security-review]"),
    ("ignore_findings", "ignore_findings: [\"vendor/**\"]"),
];

const SPACES_KEYS: &[(&str, &str)] = &[
    ("copy", "copy: [.env, .env.local]"),
    (
        "ports",
        "ports: 1                # $THEODORE_PORT, $THEODORE_PORT_1, …",
    ),
    (
        "env",
        "env:\n  APP_URL: \"http://localhost:$THEODORE_PORT\"",
    ),
    ("setup", "setup: pnpm install"),
    ("run", "run: pnpm dev --port $THEODORE_PORT"),
    ("check", "check: pnpm lint && pnpm test"),
    ("archive", "archive: docker compose -p $THEODORE_SPACE down"),
    (
        "timeout_s",
        "timeout_s: { setup: 600, check: 600, archive: 120 }",
    ),
];

const CODE_INTEL_KEYS: &[(&str, &str)] = &[
    ("enabled", "enabled: true"),
    (
        "install",
        "install: ask            # ask | always | never: download language servers",
    ),
    (
        "languages",
        "languages: [typescript, rust]   # empty: detected",
    ),
    ("servers", "servers: { python: pyright }"),
    (
        "max_servers",
        "max_servers: 4          # these four: global file only",
    ),
    ("idle_minutes", "idle_minutes: 10"),
    ("max_file_kb", "max_file_kb: 1024"),
    ("cache_mb", "cache_mb: 512"),
    (
        "lsp",
        "lsp:                    # global / local files only, never the repo's\n  spaces: true\n  review: false         # true: a PR's code may run during reviews",
    ),
];

/// The header of a generated file.
pub fn header(project: &str) -> String {
    format!(
        "# Theodore config for {project} (docs/config.md).\n\
         # Layers, later wins key by key (lists replace):\n\
         #   ~/.theodore/config.yaml               global: your defaults\n\
         #   <repo>/.theodore/config.yaml          repo: shared with the team\n\
         #   <repo>/.theodore/config.local.yaml    local: yours, gitignored\n\
         # Commands set here or in config.local.yaml run without asking; commands\n\
         # from a repo's file run only after you allow them.\n"
    )
}

/// `header`, then `review:`, `spaces:` and `code_intel:`: each set key
/// (`values` is the normalized tree, `{review: {...}, spaces: {...}}`) as
/// YAML, each unset key as its commented example. A section with nothing
/// set is commented out whole, so it doesn't parse as `null`.
pub fn render(header: &str, values: &Value) -> String {
    let mut out = header.trim_end().to_string();
    out.push('\n');
    for (section, keys) in [
        ("review", REVIEW_KEYS),
        ("spaces", SPACES_KEYS),
        ("code_intel", CODE_INTEL_KEYS),
    ] {
        let set = values.get(section).and_then(Value::as_object);
        out.push('\n');
        out.push_str(&render_section(section, keys, set));
    }
    out
}

fn render_section(
    section: &str,
    keys: &[(&str, &str)],
    set: Option<&Map<String, Value>>,
) -> String {
    let any = set.is_some_and(|m| keys.iter().any(|(k, _)| has_value(m.get(*k))));
    let mut out = String::new();
    out.push_str(if any { "" } else { "# " });
    out.push_str(section);
    out.push_str(":\n");
    for (key, example) in keys {
        match set.and_then(|m| m.get(*key)).filter(|v| has_value(Some(v))) {
            Some(v) => {
                let mut one = Map::new();
                one.insert((*key).to_string(), v.clone());
                let yaml = serde_yaml::to_string(&Value::Object(one)).unwrap_or_default();
                for line in yaml.lines() {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
            None => {
                for line in example.lines() {
                    out.push_str(if any { "  # " } else { "#   " });
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    out
}

fn has_value(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Object(m)) => !m.is_empty(),
        Some(_) => true,
    }
}

/// `validate`, plus a warning for each key `layer` may not set (the repo's
/// file can't turn on language servers, `config::repo_may_not_set`; only
/// the global file sets the machine-wide limits, `config::MACHINE_WIDE`).
pub fn validate_layer(text: &str, layer: theodore_contract::ConfigLayer) -> Vec<ConfigProblem> {
    use theodore_contract::ConfigLayer;
    let mut problems = validate(text);
    if layer == ConfigLayer::Global {
        return problems;
    }
    let Ok(value @ Value::Object(_)) = serde_yaml::from_str::<Value>(text) else {
        return problems;
    };
    let lines = key_lines(text);
    let mut leaves = Vec::new();
    flatten(String::new(), &normalize(value), &mut leaves);
    for (key, _) in leaves {
        if super::MACHINE_WIDE.contains(&key.as_str()) {
            problems.push(ConfigProblem {
                level: ConfigProblemLevel::Warning,
                line: line_of(&lines, &key),
                column: None,
                message: format!(
                    "`{key}`: ignored here; it is one setting for the whole app, set in your \
                     global file"
                ),
            });
        } else if layer == ConfigLayer::Repo && super::repo_may_not_set(&key) {
            problems.push(ConfigProblem {
                level: ConfigProblemLevel::Warning,
                line: line_of(&lines, &key),
                column: None,
                message: format!(
                    "`{key}`: ignored in the repo's file; set it in your global or local file"
                ),
            });
        }
    }
    problems.sort_by_key(|p| {
        (
            p.line.unwrap_or(u32::MAX),
            p.level == ConfigProblemLevel::Warning,
        )
    });
    problems
}

/// Problems in one file's `text`, with 1-based line numbers where the
/// parser or the key's position says.
pub fn validate(text: &str) -> Vec<ConfigProblem> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let value = match serde_yaml::from_str::<Value>(text) {
        Ok(Value::Null) => return Vec::new(),
        Ok(v @ Value::Object(_)) => v,
        Ok(_) => {
            return vec![error(
                Some(first_content_line(text)),
                None,
                "the top level must be a mapping (key: value)".into(),
            )];
        }
        Err(e) => {
            let loc = e.location();
            return vec![error(
                loc.as_ref().map(|l| l.line() as u32),
                loc.as_ref().map(|l| l.column() as u32),
                strip_location(&e.to_string()),
            )];
        }
    };
    let lines = key_lines(text);
    let value = normalize(value);
    let mut problems = Vec::new();
    let mut leaves = Vec::new();
    flatten(String::new(), &value, &mut leaves);
    for (key, _) in &leaves {
        if !is_known(key) {
            problems.push(ConfigProblem {
                level: ConfigProblemLevel::Warning,
                line: line_of(&lines, key),
                column: None,
                message: format!("unknown key `{key}`: ignored"),
            });
        }
    }
    for section in ["review", "spaces", "code_intel"] {
        let Some(v) = value.get(section) else {
            continue;
        };
        let Some(map) = v.as_object() else {
            problems.push(error(
                line_of(&lines, section),
                None,
                format!("`{section}:` must be a mapping"),
            ));
            continue;
        };
        for (key, v) in map {
            let mut one = Map::new();
            one.insert(key.clone(), v.clone());
            let one = Value::Object(one);
            let typed = match section {
                "review" => serde_json::from_value::<ReviewConfig>(one).err(),
                "spaces" => serde_json::from_value::<SpacesConfig>(one).err(),
                _ => serde_json::from_value::<CodeIntelConfig>(one).err(),
            };
            if let Some(e) = typed {
                let dotted = format!("{section}.{key}");
                problems.push(error(
                    line_of(&lines, &dotted),
                    None,
                    format!("`{dotted}`: {e}"),
                ));
            }
        }
    }
    problems.sort_by_key(|p| {
        (
            p.line.unwrap_or(u32::MAX),
            p.level == ConfigProblemLevel::Warning,
        )
    });
    problems
}

fn error(line: Option<u32>, column: Option<u32>, message: String) -> ConfigProblem {
    ConfigProblem {
        level: ConfigProblemLevel::Error,
        line,
        column,
        message,
    }
}

/// The parser's message without its ` at line 3 column 5` (the position is
/// shown on its own).
fn strip_location(message: &str) -> String {
    let mut out = message.to_string();
    while let Some(start) = out.find(" at line ") {
        let rest = &out[start + " at line ".len()..];
        let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit());
        let rest = match rest.strip_prefix(" column ") {
            Some(r) => r.trim_start_matches(|c: char| c.is_ascii_digit()),
            None => rest,
        };
        out = format!("{}{rest}", &out[..start]);
    }
    out
}

fn first_content_line(text: &str) -> u32 {
    text.lines()
        .position(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#') && t != "---"
        })
        .map_or(1, |i| i as u32 + 1)
}

/// Each block-style key's dotted path → its first line (1-based), from
/// indentation. Keys inside a flow mapping (`{a: 1}`) and list items are
/// not tracked; `line_of` falls back to the nearest parent.
fn key_lines(text: &str) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim_start();
        if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
            continue;
        }
        let Some(key) = key_of(line) else {
            continue;
        };
        while stack.last().is_some_and(|(d, _)| *d >= indent) {
            stack.pop();
        }
        let mut path: Vec<&str> = stack.iter().map(|(_, k)| k.as_str()).collect();
        path.push(&key);
        let dotted = path.join(".");
        out.entry(dotted).or_insert(i as u32 + 1);
        stack.push((indent, key));
    }
    out
}

/// The key of a `key: …` line, unquoted.
fn key_of(line: &str) -> Option<String> {
    let (key, rest) = if let Some(q) = line.strip_prefix('"') {
        let end = q.find('"')?;
        (q[..end].to_string(), &q[end + 1..])
    } else if let Some(q) = line.strip_prefix('\'') {
        let end = q.find('\'')?;
        (q[..end].to_string(), &q[end + 1..])
    } else {
        let end = line.find(':')?;
        (line[..end].trim_end().to_string(), &line[end..])
    };
    let rest = rest.trim_start();
    (rest.starts_with(':') && (rest.len() == 1 || rest[1..].starts_with([' ', '\t'])))
        .then_some(key)
        .filter(|k| !k.is_empty())
}

/// The line of `dotted`, else of its nearest parent that has one. A legacy
/// top-level `agents:` is found for `review.agents.*` too.
fn line_of(lines: &BTreeMap<String, u32>, dotted: &str) -> Option<u32> {
    let mut key = dotted.to_string();
    loop {
        if let Some(l) = lines.get(&key) {
            return Some(*l);
        }
        if let Some(rest) = key.strip_prefix("review.")
            && rest.starts_with("agents")
            && let Some(l) = lines.get(rest)
        {
            return Some(*l);
        }
        let i = key.rfind('.')?;
        key.truncate(i);
    }
}

/// What a new local file starts with when a switch creates it.
const NEW_LOCAL_HEADER: &str = "# Theodore local config: yours, not committed (docs/config.md).";

/// `text` (a config file; `None` when it doesn't exist yet) with the
/// block-style leaf `path` (`["code_intel", "lsp", "review"]`) set to
/// `value` (a YAML scalar), every other line kept as written: comments,
/// order, the rest of the file. A line edit, checked afterwards: the
/// result must parse and hold exactly the old leaves plus this one. `Err`
/// says why the file can't be edited that way (a flow mapping, a dotted
/// key, a parse error): the caller writes nothing.
///
/// ```text
/// code_intel:            code_intel:
///   install: ask    ─▶     lsp:
///                            review: true
///                          install: ask
/// ```
pub fn set_leaf(text: Option<&str>, path: &[&str], value: &str) -> Result<String, String> {
    let old = text.unwrap_or("");
    if old.lines().any(is_content) && serde_yaml::from_str::<serde_yaml::Value>(old).is_err() {
        return Err("the file doesn't parse; fix it in the config editor first".into());
    }
    let mut lines: Vec<String> = match text {
        Some(t) => t.lines().map(str::to_string).collect(),
        None => vec![NEW_LOCAL_HEADER.to_string()],
    };
    set_in_block(&mut lines, 0, None, path, value)?;
    let mut out = lines.join("\n");
    out.push('\n');
    check_leaf_edit(old, &out, path, value)?;
    Ok(out)
}

fn is_content(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && !t.starts_with('#') && t != "---"
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Sets `path` in the block that starts at line `start` (after the
/// parent's key line; the whole file when `parent_indent` is `None`) and
/// whose lines are indented deeper than the parent.
fn set_in_block(
    lines: &mut Vec<String>,
    start: usize,
    parent_indent: Option<usize>,
    path: &[&str],
    value: &str,
) -> Result<(), String> {
    const STEP: usize = 2;
    let (key, rest) = path.split_first().ok_or("an empty key")?;
    let mut child: Option<usize> = None;
    let mut found: Option<usize> = None;
    for (i, line) in lines.iter().enumerate().skip(start) {
        if !is_content(line) {
            continue;
        }
        let indent = indent_of(line);
        if parent_indent.is_some_and(|p| indent <= p) {
            break;
        }
        if indent != *child.get_or_insert(indent) {
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.starts_with(&format!("{key}.")) {
            return Err(format!("`{key}` is written as a dotted key"));
        }
        if key_of(trimmed).as_deref() == Some(*key) {
            if found.is_some() {
                return Err(format!("`{key}` appears twice"));
            }
            found = Some(i);
        }
    }
    let Some(at) = found else {
        // Missing: right under the parent's line, or at the file's end.
        let child = child.unwrap_or_else(|| parent_indent.map_or(0, |p| p + STEP));
        let pos = if parent_indent.is_some() {
            start
        } else {
            lines.len()
        };
        for (depth, k) in path.iter().enumerate() {
            let pad = " ".repeat(child + depth * STEP);
            let line = if depth + 1 == path.len() {
                format!("{pad}{k}: {value}")
            } else {
                format!("{pad}{k}:")
            };
            lines.insert(pos + depth, line);
        }
        return Ok(());
    };
    let line = lines[at].clone();
    let indent = indent_of(&line);
    let colon = indent + line[indent..].find(':').ok_or("no colon")?;
    let after = &line[colon + 1..];
    // The comment keeps the spaces before its `#`.
    let body = after.find(" #").map_or(after, |i| &after[..i]).trim_end();
    let (scalar, comment) = (body.trim_start(), &after[body.len()..]);
    if rest.is_empty() {
        if scalar.starts_with(['{', '[', '|', '>', '&', '*', '!']) {
            return Err(format!("`{key}` isn't a plain value"));
        }
        lines[at] = format!("{}: {value}{comment}", &line[..colon]);
        return Ok(());
    }
    if !scalar.is_empty() {
        return Err(format!("`{key}` is written inline"));
    }
    set_in_block(lines, at + 1, Some(indent), rest, value)
}

/// The edit changed exactly `path`: the same leaves as before, with it set.
fn check_leaf_edit(old: &str, new: &str, path: &[&str], value: &str) -> Result<(), String> {
    let leaves = |text: &str| -> Result<BTreeMap<String, Value>, String> {
        let v = if text.lines().any(is_content) {
            serde_yaml::from_str::<Value>(text).map_err(|e| e.to_string())?
        } else {
            Value::Null
        };
        let v = if v.is_null() {
            Value::Object(Map::new())
        } else {
            v
        };
        let mut out = Vec::new();
        flatten(String::new(), &normalize(v), &mut out);
        Ok(out.into_iter().collect())
    };
    let mut want = leaves(old)?;
    let set: Value = serde_yaml::from_str(value).map_err(|e| format!("{value}: {e}"))?;
    want.insert(path.join("."), set);
    let got = leaves(new).map_err(|e| format!("the edit wouldn't parse: {e}"))?;
    if got == want {
        Ok(())
    } else {
        Err(format!(
            "`{}` can't be set by a line edit here",
            path.join(".")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LayerText, merge};
    use theodore_contract::ConfigLayer;

    fn parsed(text: &str) -> crate::config::Merged {
        merge(&[LayerText {
            layer: ConfigLayer::Repo,
            path: "/r".into(),
            legacy: false,
            text: Some(text.into()),
        }])
    }

    #[test]
    fn a_syntax_error_has_the_parsers_line_and_column() {
        let p = validate("review:\n  hide: [\"gen/**\"\n  skim: []\n");
        assert_eq!(p.len(), 1, "{p:?}");
        assert_eq!(p[0].level, ConfigProblemLevel::Error);
        assert!(p[0].line.is_some_and(|l| l >= 2), "{p:?}");
        assert!(p[0].column.is_some());
        assert!(!p[0].message.contains(" at line "), "{}", p[0].message);
    }

    #[test]
    fn a_wrong_type_is_an_error_on_its_keys_line() {
        let text =
            "# hi\nreview:\n  instructions: Be brief.\n  hide: gen/**\nspaces:\n  ports: many\n";
        let p = validate(text);
        assert_eq!(p.len(), 2, "{p:?}");
        assert_eq!(p[0].line, Some(4));
        assert!(
            p[0].message.starts_with("`review.hide`"),
            "{}",
            p[0].message
        );
        assert_eq!(p[1].line, Some(6));
        assert!(p[1].message.starts_with("`spaces.ports`"));
        assert!(p.iter().all(|p| p.level == ConfigProblemLevel::Error));
    }

    #[test]
    fn unknown_keys_are_warnings_at_their_line() {
        let text = "review:\n  hidee: [x]\nspaces:\n  timeout_s:\n    build: 3\n";
        let p = validate(text);
        assert_eq!(
            p.iter().map(|p| (p.line, p.level)).collect::<Vec<_>>(),
            vec![
                (Some(2), ConfigProblemLevel::Warning),
                (Some(5), ConfigProblemLevel::Warning)
            ]
        );
        assert!(p[1].message.contains("spaces.timeout_s.build"));
    }

    #[test]
    fn a_top_level_list_and_a_scalar_section_are_errors() {
        let p = validate("# c\n- a\n- b\n");
        assert_eq!(p[0].line, Some(2));
        assert_eq!(p[0].level, ConfigProblemLevel::Error);
        let p = validate("review: yes\n");
        assert_eq!(p[0].line, Some(1));
        assert!(p[0].message.contains("must be a mapping"));
    }

    #[test]
    fn a_clean_file_has_no_problems() {
        assert!(validate("").is_empty());
        assert!(validate("# only comments\n").is_empty());
        assert!(
            validate(
                "review:\n  checklist:\n    \"**/*.sql\": Safe twice?\nagents:\n  chat: codex\n"
            )
            .is_empty()
        );
    }

    #[test]
    fn rendered_values_round_trip_through_the_parser() {
        let values = serde_json::json!({
            "review": {
                "agents": {"chat": "codex", "skills": ["claude", "codex"]},
                "instructions": "Prefer small functions.\nFlag any new dependency.",
                "hide": ["gen/**", "*.min.js"],
                "checklist": {"**/*.sql": "Is it safe to run twice?", "api/**": ["Versioned?", "Docs?"]},
            },
            "spaces": {
                "env": {"APP_URL": "http://localhost:$THEODORE_PORT", "A.B": "x"},
                "ports": 2,
                "setup": "pnpm install\ndocker compose up -d\n",
            }
        });
        let text = render(&header("acme/widgets"), &values);
        assert!(validate(&text).is_empty(), "{:?}\n{text}", validate(&text));
        let m = parsed(&text);
        assert!(m.files[0].error.is_none(), "{text}");
        assert!(m.unknown.is_empty(), "{:?}", m.unknown);
        assert_eq!(m.value, values, "{text}");
        // Unset keys stay as commented examples.
        assert!(text.contains("  # skim: [\"**/*.snap\"]"), "{text}");
        assert!(text.contains("  # run: pnpm dev --port $THEODORE_PORT"));
    }

    #[test]
    fn a_section_with_nothing_set_is_commented_out_whole() {
        let text = render(
            &header("x"),
            &serde_json::json!({"review": {"hide": ["gen/**"]}}),
        );
        assert!(
            text.contains("\n# spaces:\n#   copy: [.env, .env.local]\n"),
            "{text}"
        );
        assert!(text.contains("\nreview:\n"));
        let m = parsed(&text);
        assert_eq!(m.value, serde_json::json!({"review": {"hide": ["gen/**"]}}));
        // Nothing at all: still a valid (empty) config.
        let text = render(&header("x"), &serde_json::json!({}));
        assert!(validate(&text).is_empty());
        assert_eq!(parsed(&text).value, serde_json::json!({}));
    }

    const REVIEW: &[&str] = &["code_intel", "lsp", "review"];

    #[test]
    fn a_missing_file_gets_a_header_and_the_leaf() {
        let out = set_leaf(None, REVIEW, "true").unwrap();
        assert!(out.starts_with("# Theodore local config"), "{out}");
        assert!(
            out.ends_with("code_intel:\n  lsp:\n    review: true\n"),
            "{out}"
        );
        let out = set_leaf(Some(""), REVIEW, "false").unwrap();
        assert_eq!(out, "code_intel:\n  lsp:\n    review: false\n");
    }

    #[test]
    fn an_existing_value_is_replaced_in_place_with_its_comment() {
        let text = "# mine\nreview:\n  hide: [\"gen/**\"]\ncode_intel:\n  install: ask\n  \
                    lsp:\n    spaces: true\n    review: false   # careful\n";
        let out = set_leaf(Some(text), REVIEW, "true").unwrap();
        assert_eq!(out, text.replace("review: false", "review: true"));
    }

    #[test]
    fn missing_levels_go_under_their_parent_at_its_indent() {
        let text = "code_intel:\n    install: ask\nspaces:\n    setup: make\n";
        let out = set_leaf(Some(text), &["code_intel", "lsp", "spaces"], "false").unwrap();
        assert_eq!(
            out,
            "code_intel:\n    lsp:\n      spaces: false\n    install: ask\nspaces:\n    setup: make\n"
        );
        let out = set_leaf(Some("spaces:\n  setup: make"), REVIEW, "true").unwrap();
        assert_eq!(
            out,
            "spaces:\n  setup: make\ncode_intel:\n  lsp:\n    review: true\n"
        );
    }

    #[test]
    fn shapes_a_line_edit_cannot_handle_are_refused() {
        for text in [
            "code_intel: {lsp: {review: true}}\n",
            "code_intel:\n  lsp: {review: true}\n",
            "code_intel.lsp.review: false\n",
            "code_intel:\n  lsp.review: false\n",
            "code_intel: [\n",
        ] {
            assert!(set_leaf(Some(text), REVIEW, "true").is_err(), "{text}");
        }
    }
}
