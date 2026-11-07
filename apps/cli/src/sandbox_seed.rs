//! `theodore sandbox seed-chat` (docs/sandbox.md): a sandbox space's chat
//! with N stored turns of markdown and tool steps, to try a long chat in the
//! real app without spending tokens. Refuses any data dir that isn't a
//! sandbox's, and a sandbox that is running: the app reads rows when it starts.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use theodore_contract::{ChatMessage, TurnFile, TurnStep, models};
use theodore_domain::{AgentId, Author, PrId};
use theodore_ports::{ModelRow, NewAction, Store, SyncActionCode};
use theodore_sandbox::control;
use theodore_store::SqliteStore;

/// Apart in time, so every seeded turn is older than what you do next.
const TURN_MS: u64 = 60_000;

/// `turns` pairs of your message and a reply, oldest first, ending just
/// before `now_ms`. `tag` keeps the ids of two seedings apart.
pub fn seed_messages(space: &str, turns: usize, now_ms: u64, tag: u64) -> Vec<ChatMessage> {
    let key = PrId(format!("space:{space}"));
    let span = turns as u64 * TURN_MS;
    let start = now_ms.saturating_sub(span);
    let base = |id: String, author: Author, text: String, at: u64| ChatMessage {
        id,
        pr: key.clone(),
        author,
        text,
        refs: vec![],
        at: at as _,
        failed: None,
        attachments: vec![],
        queued: None,
        activity: vec![],
        duration_ms: None,
        changed_files: vec![],
        model: None,
        text_in_steps: false,
    };
    (0..turns)
        .flat_map(|n| {
            let at = start + n as u64 * TURN_MS;
            let ask = base(
                format!("seed-{tag}-{n}-you"),
                Author::Me,
                format!("Turn {n}: tidy up the module and say what changed."),
                at,
            );
            let reply = ChatMessage {
                activity: steps(n),
                duration_ms: Some(5_000 + (n as u64 % 40) * 3_000),
                changed_files: (0..n % 6)
                    .map(|i| TurnFile {
                        path: format!("src/parse_{i}.rs"),
                        additions: 3 + i as u32,
                        deletions: i as u32,
                    })
                    .collect(),
                ..base(
                    format!("seed-{tag}-{n}-reply"),
                    Author::Agent(AgentId::Claude),
                    reply_text(n),
                    at + TURN_MS / 2,
                )
            };
            [ask, reply]
        })
        .collect()
}

/// Some short, some long, so the rows measure differently.
fn reply_text(n: usize) -> String {
    let mut text = format!(
        "## Turn {n}\n\nMoved the parsing into its own module. Three things changed:\n\n\
         - `parse_line` no longer allocates\n- errors carry the line number\n- the tests \
         share one fixture\n"
    );
    if n.is_multiple_of(3) {
        text.push_str(
            "\n```rust\nfn parse_line(line: &str) -> Result<Row, ParseError> {\n    \
             let (key, value) = line.split_once('=').ok_or(ParseError::NoEquals)?;\n    \
             Ok(Row { key, value })\n}\n```\n",
        );
    }
    if n.is_multiple_of(5) {
        text.push_str("\n| File | Lines |\n| --- | --- |\n| `parse.rs` | 42 |\n| `row.rs` | 7 |\n");
    }
    for paragraph in 0..(n % 7) {
        text.push_str(&format!(
            "\nNote {paragraph}: the old path stays until the callers move, which keeps this \
             turn small enough to read in one sitting and to undo on its own if it goes wrong.\n"
        ));
    }
    text
}

fn steps(n: usize) -> Vec<TurnStep> {
    let tool = |i: usize, name: &str, target: String, ok: bool| TurnStep::Tool {
        name: name.into(),
        target: Some(target),
        ok: Some(ok),
        id: Some(format!("seed-{n}-{i}")),
        parent: None,
        detail: None,
        summary: None,
        added: None,
        removed: None,
        background: false,
    };
    let mut steps = vec![TurnStep::Text {
        text: "Reading the module first.".into(),
        parent: None,
    }];
    for i in 0..(2 + n % 12) {
        steps.push(match i % 4 {
            0 => tool(i, "Read", format!("src/parse_{i}.rs"), true),
            1 => TurnStep::Thinking {
                text: "Which callers still use the old path?".into(),
                parent: None,
            },
            2 => tool(i, "Edit", format!("src/parse_{i}.rs"), true),
            _ => tool(i, "Bash", "cargo test".into(), i % 8 != 3),
        });
    }
    steps
}

/// The space by id or name.
fn find_space<'a>(rows: &'a [ModelRow], wanted: &str) -> Option<&'a ModelRow> {
    rows.iter()
        .find(|r| r.id == wanted || r.data.get("name").and_then(|n| n.as_str()) == Some(wanted))
}

/// Seeds the chat, returning the space's id.
pub fn seed_chat(data_dir: &Path, space: &str, turns: usize) -> anyhow::Result<String> {
    let root = theodore_sandbox::root_of(data_dir);
    if theodore_sandbox::state::State::load(&root)?.is_none() {
        anyhow::bail!(
            "{} is not a sandbox's data dir: seed-chat only writes to one",
            data_dir.display()
        );
    }
    if control::alive(&root).is_some() {
        anyhow::bail!("quit the sandbox's Theodore first: it reads the chat when it starts");
    }
    let store = SqliteStore::open(data_dir.join("theodore.db"))?;
    let spaces = store.all(models::SPACE)?;
    let Some(row) = find_space(&spaces, space) else {
        let names: Vec<_> = spaces
            .iter()
            .filter_map(|r| r.data.get("name")?.as_str())
            .collect();
        anyhow::bail!("no space {space}; the sandbox has: {}", names.join(", "));
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;
    let actions = seed_messages(&row.id, turns, now, now)
        .into_iter()
        .map(|m| {
            Ok(NewAction {
                model: models::CHAT_MESSAGE.into(),
                model_id: m.id.clone(),
                action: SyncActionCode::I,
                data: Some(serde_json::to_value(&m)?),
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    store.apply(actions)?;
    Ok(row.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_follows_each_message_in_time_with_its_steps() {
        let all = seed_messages("s1", 20, 10_000_000, 7);
        assert_eq!(all.len(), 40);
        assert!(all.windows(2).all(|w| w[0].at < w[1].at));
        assert!(all.iter().all(|m| m.at < 10_000_000 as _));
        for pair in all.chunks(2) {
            assert_eq!(pair[0].author, Author::Me);
            assert_eq!(pair[1].author, Author::Agent(AgentId::Claude));
            assert!(pair[1].activity.len() >= 3);
            assert!(pair[1].text.starts_with("## Turn"));
            assert_eq!(pair[0].pr, PrId("space:s1".into()));
        }
    }

    #[test]
    fn two_seedings_keep_their_ids_apart() {
        let (a, b) = (seed_messages("s", 3, 1, 1), seed_messages("s", 3, 1, 2));
        assert!(a.iter().all(|m| b.iter().all(|n| n.id != m.id)));
    }

    #[test]
    fn refuses_a_data_dir_that_is_not_a_sandboxs() {
        let dir = tempfile::tempdir().unwrap();
        let err = seed_chat(dir.path(), "s", 3).unwrap_err().to_string();
        assert!(err.contains("not a sandbox"), "{err}");
        assert!(!dir.path().join("theodore.db").exists());
    }

    #[test]
    fn writes_the_turns_into_a_sandboxs_store_and_reports_a_wrong_space() {
        let scenario = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        theodore_sandbox::default_scenario::write(scenario.path()).unwrap();
        theodore_sandbox::Sandbox::open(scenario.path(), data.path()).unwrap();
        let store = SqliteStore::open(data.path().join("theodore.db")).unwrap();
        store
            .apply(vec![NewAction {
                model: models::SPACE.into(),
                model_id: "abc".into(),
                action: SyncActionCode::I,
                data: Some(serde_json::json!({ "name": "Rate limits" })),
            }])
            .unwrap();
        drop(store);

        let err = seed_chat(data.path(), "nope", 2).unwrap_err().to_string();
        assert!(err.contains("Rate limits"), "{err}");

        assert_eq!(seed_chat(data.path(), "Rate limits", 2).unwrap(), "abc");
        let store = SqliteStore::open(data.path().join("theodore.db")).unwrap();
        let rows = store.all(models::CHAT_MESSAGE).unwrap();
        assert_eq!(rows.len(), 4);
        let back: ChatMessage = serde_json::from_value(rows[1].data.clone()).unwrap();
        assert_eq!(back.pr, PrId("space:abc".into()));
    }

    #[test]
    fn finds_a_space_by_id_or_name() {
        let rows = vec![ModelRow {
            model: models::SPACE.into(),
            id: "abc".into(),
            data: serde_json::json!({ "name": "Rate limits" }),
            archived: false,
        }];
        assert!(find_space(&rows, "abc").is_some());
        assert!(find_space(&rows, "Rate limits").is_some());
        assert!(find_space(&rows, "other").is_none());
    }
}
