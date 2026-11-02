//! The account gate at the command boundary (plan §6.12 "Sign-in
//! required"). Where signing in is required and nobody is, only the
//! commands the sign-in screen needs run; every other one answers
//! `{code: "account_required"}` without reaching the core.
//!
//! ```text
//! invoke ─▶ ALLOWED_SIGNED_OUT? ──yes──▶ command
//!               │ no
//!               ▼
//!         core.account_gate() ──open──▶ command
//!               └─ closed ─▶ reject {code: "account_required"}
//! ```

use std::future::Future;
use std::sync::Arc;

use tauri::Runtime;
use tauri::ipc::Invoke;
use theodore_contract::CommandError;

use crate::core_api::CoreApi;

/// Commands that run while signed out: the sync stream (local data the
/// window already has), app info and updates, the account itself, and
/// bringing the window forward after a browser sign-in. Everything else —
/// host reads and writes, agent runs, spaces, projects, settings and other
/// local writes — waits for sign-in. Deny by default: a new command is
/// gated until it's listed here.
pub const ALLOWED_SIGNED_OUT: &[&str] = &[
    "bootstrap",
    "catch_up",
    "app_info",
    "check_update",
    "install_update",
    "account_status",
    "account_sign_in_start",
    "account_sign_in_poll",
    "account_sign_in_cancel",
    "account_sign_out",
    "focus_window",
    // Telemetry: UI errors on the sign-in screen still reach the log, and
    // a broken sign-in can still be reported (docs/telemetry.md), also as
    // a GitHub issue: that posts with the GitHub token, not the account.
    "log_client",
    "track_ui",
    "telemetry_status",
    "report_problem",
    "reveal_report",
    "issue_target",
    "preview_issue",
    "submit_issue",
];

pub fn allowed_signed_out(command: &str) -> bool {
    ALLOWED_SIGNED_OUT.contains(&command)
}

/// `Ok` when `command` may run now; `gate` (the core's
/// `CoreApi::account_gate`) is only asked for a command off the list.
pub async fn check<F, Fut>(command: &str, gate: F) -> Result<(), CommandError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<(), CommandError>>,
{
    if allowed_signed_out(command) {
        return Ok(());
    }
    gate().await
}

/// Wraps the generated invoke handler with [`check`] against `core`.
pub fn guard_core<R: Runtime>(
    core: Arc<dyn CoreApi>,
    inner: impl Fn(Invoke<R>) -> bool + Send + Sync + 'static,
) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    guard(
        move || {
            let core = core.clone();
            async move { core.account_gate().await }
        },
        inner,
    )
}

/// Wraps an invoke handler with [`check`]. An allowed command goes straight
/// through; any other one first asks `gate`, off the IPC thread, then runs
/// or is rejected. (Every command here is `async`, so it runs on the async
/// runtime either way.)
pub fn guard<R, G, Fut>(
    gate: G,
    inner: impl Fn(Invoke<R>) -> bool + Send + Sync + 'static,
) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static
where
    R: Runtime,
    G: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), CommandError>> + Send + 'static,
{
    let inner = Arc::new(inner);
    let gate = Arc::new(gate);
    move |invoke: Invoke<R>| {
        if allowed_signed_out(invoke.message.command()) {
            return inner(invoke);
        }
        let (gate, inner) = (gate.clone(), inner.clone());
        tauri::async_runtime::spawn(async move {
            match check(invoke.message.command(), || gate()).await {
                Ok(()) => {
                    inner(invoke);
                }
                Err(e) => invoke.resolver.reject(e),
            }
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::core_api::NotReadyCoreApi;

    /// Every command the shell registers, read back from the generated
    /// bindings (`__TAURI_INVOKE("<name>"`).
    fn registered_commands() -> BTreeSet<String> {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("bindings.ts");
        crate::export_bindings(&out).expect("export bindings");
        let text = std::fs::read_to_string(&out).expect("read bindings");
        text.split("__TAURI_INVOKE(\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn the_allow_list_is_exactly_what_the_sign_in_screen_needs() {
        let allowed: BTreeSet<&str> = ALLOWED_SIGNED_OUT.iter().copied().collect();
        let expected: BTreeSet<&str> = [
            "bootstrap",
            "catch_up",
            "app_info",
            "check_update",
            "install_update",
            "account_status",
            "account_sign_in_start",
            "account_sign_in_poll",
            "account_sign_in_cancel",
            "account_sign_out",
            "focus_window",
            "log_client",
            "track_ui",
            "telemetry_status",
            "report_problem",
            "reveal_report",
            "issue_target",
            "preview_issue",
            "submit_issue",
        ]
        .into_iter()
        .collect();
        assert_eq!(allowed, expected);
    }

    #[test]
    fn every_allowed_command_is_registered() {
        let registered = registered_commands();
        assert!(registered.len() > ALLOWED_SIGNED_OUT.len());
        for name in ALLOWED_SIGNED_OUT {
            assert!(registered.contains(*name), "{name} isn't a command");
        }
    }

    #[test]
    fn commands_that_do_work_are_gated() {
        let registered = registered_commands();
        for name in [
            // host reads and writes
            "open_pr",
            "refresh_inbox",
            "viewer",
            "host_status",
            "set_host_token",
            "list_host_repos",
            "submit_review",
            "reply_thread",
            "merge_pr",
            // local writes
            "transact",
            "save_skill",
            "save_config",
            // agents
            "run_skill",
            "ask",
            "save_attachment",
            "delegate_task",
            "detect_agents",
            "agent_models",
            // projects and spaces
            "add_project",
            "remove_project",
            "list_spaces",
            "create_space",
            "space_send",
            "space_send_now",
            "space_queued",
            "space_skills",
            "space_run",
            // shell-side
            "pick_folder",
            "notify_space",
            "open_global_config",
        ] {
            assert!(registered.contains(name), "{name} isn't a command");
            assert!(!allowed_signed_out(name), "{name} must be gated");
        }
    }

    fn required() -> CommandError {
        CommandError {
            code: "account_required".into(),
            message: "Sign in to your Theodore account to continue".into(),
        }
    }

    #[tokio::test]
    async fn signed_out_only_the_allow_list_runs() {
        let closed = || async { Err(required()) };
        for name in ALLOWED_SIGNED_OUT {
            check(name, closed).await.unwrap();
        }
        for name in ["open_pr", "transact", "list_spaces", "not_a_command"] {
            let err = check(name, closed).await.unwrap_err();
            assert_eq!(err.code, "account_required", "{name}");
        }
    }

    #[tokio::test]
    async fn signed_in_everything_runs_and_the_list_skips_the_gate() {
        let asked = AtomicBool::new(false);
        let open = || async {
            asked.store(true, Ordering::SeqCst);
            Ok(())
        };
        check("account_status", open).await.unwrap();
        assert!(!asked.load(Ordering::SeqCst), "allowed: gate not asked");
        for name in ["open_pr", "transact", "list_spaces"] {
            check(name, open).await.unwrap();
        }
        assert!(asked.load(Ordering::SeqCst));
    }

    /// The guard over a real (mock-runtime) IPC round trip: an allowed
    /// command runs, a gated one is rejected while the gate is closed and
    /// runs once it opens.
    #[test]
    fn the_guard_answers_over_ipc() {
        use std::sync::Arc;
        use tauri::test::{INVOKE_KEY, get_ipc_response, mock_builder, mock_context, noop_assets};
        use tauri::webview::InvokeRequest;

        #[tauri::command]
        async fn account_status() -> Result<String, CommandError> {
            Ok("status".into())
        }
        #[tauri::command]
        async fn list_spaces() -> Result<String, CommandError> {
            Ok("spaces".into())
        }

        let open = Arc::new(AtomicBool::new(false));
        let gate_open = open.clone();
        let app = mock_builder()
            .invoke_handler(guard(
                move || {
                    let open = gate_open.load(Ordering::SeqCst);
                    async move { if open { Ok(()) } else { Err(required()) } }
                },
                tauri::generate_handler![account_status, list_spaces],
            ))
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .expect("mock webview");
        let call = |cmd: &str| {
            get_ipc_response(
                &webview,
                InvokeRequest {
                    cmd: cmd.into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: "tauri://localhost".parse().unwrap(),
                    body: tauri::ipc::InvokeBody::default(),
                    headers: Default::default(),
                    invoke_key: INVOKE_KEY.to_string(),
                },
            )
        };

        let ok = call("account_status").expect("allowed");
        assert_eq!(ok.deserialize::<String>().unwrap(), "status");
        let rejected = call("list_spaces").expect_err("gated");
        assert_eq!(rejected["code"], "account_required");

        open.store(true, Ordering::SeqCst);
        let ok = call("list_spaces").expect("open");
        assert_eq!(ok.deserialize::<String>().unwrap(), "spaces");
    }

    #[tokio::test]
    async fn the_default_gate_is_open() {
        let core = NotReadyCoreApi;
        check("open_pr", || core.account_gate()).await.unwrap();
    }
}
