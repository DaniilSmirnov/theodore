//! The operation id (`op_id`) a command runs under (docs/telemetry.md):
//! the UI sends one per call as the `x-theodore-op` invoke header; every
//! log record of that command — in the core, its agent runs, its host
//! requests — carries it, so one failure stitches together.
//!
//! ```text
//! api.ts call ─(header x-theodore-op)─▶ Op ─▶ span "op"{op_id, cmd}
//!                                              └─▶ core ─▶ agent run / host request records
//! ```

use std::future::Future;

use tauri::Runtime;
use tauri::ipc::{CommandArg, CommandItem, InvokeError};
use theodore_contract::CommandError;
use tracing::Instrument as _;

pub const OP_HEADER: &str = "x-theodore-op";

/// Commands the UI calls all the time (sync, polls, passive reads): their
/// spans still carry the op id but aren't exported as traces.
const CHATTY: &[&str] = &[
    "bootstrap",
    "catch_up",
    "transact",
    "get_revision",
    "get_file",
    "get_blob",
    "attachment_data",
    "interdiff",
    "find_symbol",
    "space_logs",
    "space_config",
    "code_intel_status",
    "list_spaces",
    "list_skills",
    "space_skills",
    "login_poll",
    "account_sign_in_poll",
    "account_status",
    "check_config",
    "telemetry_status",
    "log_client",
    "track_ui",
    "project_working_state",
    "linear_status",
    "linear_issues",
    "host_status",
    "host_status_for",
    "app_info",
    "merge_info",
    "conflicts",
    "sandbox_status",
    "detect_agents",
    "agent_models",
    "viewer",
];

/// A command's operation: the UI's id, or a fresh one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub id: String,
    pub cmd: &'static str,
}

/// Ids the UI makes are 16 hex chars; anything else odd is replaced.
fn valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}

impl Op {
    pub fn from_header(cmd: &'static str, header: Option<&str>) -> Self {
        let id = header
            .map(str::trim)
            .filter(|h| valid(h))
            .map(str::to_string)
            .unwrap_or_else(new_id);
        Self { id, cmd }
    }

    /// The operation's span: `op_id`, `cmd`, and whether to export it as a
    /// trace.
    pub fn span(&self) -> tracing::Span {
        tracing::info_span!(
            "op",
            op_id = %self.id,
            cmd = self.cmd,
            trace = !self.chatty(),
            ok = tracing::field::Empty,
            error_code = tracing::field::Empty,
            ms = tracing::field::Empty,
        )
    }

    fn chatty(&self) -> bool {
        CHATTY.contains(&self.cmd)
    }

    /// Runs `fut` in this operation's span, then records how it went: on
    /// the span (`ok`, `error_code`, `ms`, so the exported trace says it)
    /// and as one "command done" line — info for a command that matters,
    /// debug for a chatty one. Why it failed is logged where the error is
    /// mapped (`app_core::err`), once.
    pub async fn run<T>(
        self,
        fut: impl Future<Output = Result<T, CommandError>>,
    ) -> Result<T, CommandError> {
        let span = self.span();
        let started = std::time::Instant::now();
        let out = fut.instrument(span.clone()).await;
        let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        span.record("ms", ms);
        span.record("ok", out.is_ok());
        if let Err(e) = &out {
            span.record("error_code", e.code.as_str());
        }
        let ok = out.is_ok();
        let _g = span.enter();
        if self.chatty() {
            tracing::debug!(ok, ms, "command done");
        } else {
            tracing::info!(ok, ms, "command done");
        }
        out
    }
}

impl<'de, R: Runtime> CommandArg<'de, R> for Op {
    fn from_command(command: CommandItem<'de, R>) -> Result<Self, InvokeError> {
        let header = command
            .message
            .headers()
            .get(OP_HEADER)
            .and_then(|v| v.to_str().ok());
        Ok(Op::from_header(command.name, header))
    }
}

/// Not part of the TS signature: it rides in a header.
impl specta::function::FunctionArg for Op {
    fn to_datatype(_: &mut specta::Types) -> Option<specta::datatype::DataType> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ui_id_is_kept_and_a_bad_one_replaced() {
        assert_eq!(
            Op::from_header("regroup", Some("0123456789abcdef")).id,
            "0123456789abcdef"
        );
        for bad in [None, Some(""), Some("a b"), Some("x\ny")] {
            let op = Op::from_header("regroup", bad);
            assert_eq!(op.id.len(), 16, "{bad:?}");
            assert!(valid(&op.id));
        }
        assert_ne!(Op::from_header("a", None).id, Op::from_header("a", None).id);
    }

    /// Over a real (mock-runtime) IPC call: the header becomes the op id
    /// of the command's records.
    #[test]
    fn the_invoke_header_reaches_the_commands_records() {
        use tauri::test::{INVOKE_KEY, get_ipc_response, mock_builder, mock_context, noop_assets};
        use tauri::webview::InvokeRequest;

        #[tauri::command]
        fn probe(op: Op) -> Result<String, CommandError> {
            let (cap, _, sub) = theodore_telemetry::layer::capture();
            tracing::subscriber::with_default(sub, || {
                let _e = op.span().entered();
                tracing::warn!("inside the command");
            });
            let recs = cap.records();
            Ok(format!(
                "{}|{}",
                recs[0].op_id().unwrap_or_default(),
                recs[0].field_str("cmd").unwrap_or_default()
            ))
        }

        let app = mock_builder()
            .invoke_handler(tauri::generate_handler![probe])
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("mock webview");
        let mut headers = tauri::http::HeaderMap::new();
        headers.insert(OP_HEADER, "abc123def4567890".parse().unwrap());
        let res = get_ipc_response(
            &webview,
            InvokeRequest {
                cmd: "probe".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: "tauri://localhost".parse().unwrap(),
                body: tauri::ipc::InvokeBody::default(),
                headers,
                invoke_key: INVOKE_KEY.to_string(),
            },
        )
        .expect("probe runs");
        assert_eq!(
            res.deserialize::<String>().unwrap(),
            "abc123def4567890|probe"
        );
    }

    #[test]
    fn records_inside_a_command_carry_its_op_id() {
        let (cap, _, sub) = theodore_telemetry::layer::capture();
        let _g = tracing::subscriber::set_default(sub);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let op = Op::from_header("run_skill", Some("op-from-ui"));
        let r: Result<(), CommandError> = rt.block_on(op.run(async {
            // Work the command spawns keeps the span too.
            tokio::spawn(
                async { tracing::warn!(error.code = "agent.crashed", "agent run failed") }
                    .in_current_span(),
            )
            .await
            .unwrap();
            Ok(())
        }));
        r.unwrap();
        let recs = cap.records();
        let rec = recs
            .iter()
            .find(|r| r.message == "agent run failed")
            .unwrap();
        assert_eq!(rec.op_id(), Some("op-from-ui"));
        assert_eq!(rec.field_str("cmd"), Some("run_skill"));
        assert!(!rec.fields.contains_key("trace"));
    }

    #[test]
    fn every_command_ends_with_one_line_saying_how_it_went() {
        let (cap, _, sub) = theodore_telemetry::layer::capture();
        let _g = tracing::subscriber::set_default(sub);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let ok: Result<(), CommandError> =
            rt.block_on(Op::from_header("regroup", None).run(async { Ok(()) }));
        ok.unwrap();
        let failed: Result<(), CommandError> =
            rt.block_on(Op::from_header("regroup", None).run(async {
                Err(CommandError {
                    code: "host.rate_limited".into(),
                    message: "secret details stay out".into(),
                })
            }));
        assert!(failed.is_err());
        let recs = cap.records();
        let done: Vec<_> = recs
            .iter()
            .filter(|r| r.message == "command done")
            .collect();
        assert_eq!(done.len(), 2);
        for r in &done {
            assert_eq!(r.level, tracing::Level::INFO);
            assert_eq!(r.field_str("cmd"), Some("regroup"));
            assert!(r.fields.contains_key("ms"));
        }
        assert_eq!(done[0].fields.get("ok"), Some(&serde_json::json!(true)));
        assert_eq!(done[1].fields.get("ok"), Some(&serde_json::json!(false)));
        assert_eq!(done[1].field_str("error_code"), Some("host.rate_limited"));
        assert!(
            !serde_json::to_string(&done[1].fields)
                .unwrap()
                .contains("secret details")
        );
    }

    #[test]
    fn a_chatty_command_finishing_well_logs_at_debug() {
        let (cap, _, sub) = theodore_telemetry::layer::capture();
        let _g = tracing::subscriber::set_default(sub);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let r: Result<(), CommandError> =
            rt.block_on(Op::from_header("catch_up", None).run(async { Ok(()) }));
        r.unwrap();
        let recs = cap.records();
        if let Some(done) = recs.iter().find(|r| r.message == "command done") {
            assert_eq!(done.level, tracing::Level::DEBUG);
        }
    }
}
