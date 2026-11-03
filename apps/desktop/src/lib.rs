//! Tauri 2 desktop shell (plan §11). Owns windows, the updater and the
//! command/event wiring; delegates all behaviour to `State<Arc<dyn CoreApi>>`.

pub mod app_core;
pub mod build_info;
pub mod build_stamp;
mod chrome;
pub mod commands;
pub mod core_api;
pub mod deep_link;
pub mod events;
pub mod gate;
pub mod menu;
pub mod notify;
pub mod op;
pub mod shell_path;
pub mod updater;

use std::path::Path;
use std::sync::Arc;

use tauri::Manager;
use tauri_specta::{Builder, collect_commands};

pub use core_api::{CoreApi, NotReadyCoreApi};
pub use events::{CoreEvents, emitter};

/// Commands + extra types exported to TS. Shared between the running app
/// (`run`) and the bindings exporter (`export_bindings`) so they never drift.
pub fn specta_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            commands::bootstrap,
            commands::catch_up,
            commands::transact,
            commands::open_pr,
            commands::get_revision,
            commands::get_file,
            commands::regroup,
            commands::run_skill,
            commands::ask,
            commands::save_attachment,
            commands::attachment_data,
            commands::cancel_run,
            commands::submit_review,
            commands::detect_agents,
            commands::agent_models,
            commands::app_info,
            commands::check_update,
            commands::install_update,
            commands::refresh_inbox,
            commands::viewer,
            commands::host_status,
            commands::host_status_for,
            commands::set_host_token,
            commands::reply_thread,
            commands::resolve_thread,
            commands::list_skills,
            commands::delegate_task,
            commands::decide_task,
            commands::list_host_repos,
            commands::add_project,
            commands::remove_project,
            commands::refresh_project,
            commands::list_branches,
            commands::open_branch,
            commands::merge_info,
            commands::merge_pr,
            commands::assignable_users,
            commands::set_reviewers,
            commands::set_draft,
            commands::update_branch,
            commands::react,
            commands::edit_comment,
            commands::commit_suggestions,
            commands::get_skill,
            commands::save_skill,
            commands::pick_folder,
            commands::interdiff,
            commands::login_start,
            commands::login_poll,
            commands::rerun_check,
            commands::triage,
            commands::set_linear_token,
            commands::linear_status,
            commands::linear_issues,
            commands::find_symbol,
            commands::get_blob,
            commands::conflicts,
            commands::sandbox_status,
            commands::sandbox_set_fault,
            commands::sandbox_fire_event,
            commands::project_working_state,
            commands::close_branch_review,
            commands::create_space,
            commands::project_issues,
            commands::list_spaces,
            commands::space_send,
            commands::space_send_now,
            commands::space_queued,
            commands::space_stop,
            commands::space_skills,
            commands::space_review,
            commands::archive_space,
            commands::ship_space,
            commands::merge_space,
            commands::continue_space,
            commands::undo_space_turn,
            commands::rewind_space,
            commands::revert_space_file,
            commands::unarchive_space,
            commands::set_space_pinned,
            commands::space_send_feedback,
            commands::space_base_status,
            commands::update_space_from_base,
            commands::abort_space_update,
            commands::space_fix_ci,
            commands::approve_space_plan,
            commands::answer_space_question,
            commands::mirror_space,
            commands::sync_mirror,
            commands::unmirror_space,
            commands::space_config,
            commands::space_trust,
            commands::space_setup,
            commands::space_setup_assist,
            commands::space_fix_setup,
            commands::space_run,
            commands::space_logs,
            commands::open_space_terminal,
            commands::write_space_terminal,
            commands::resize_space_terminal,
            commands::close_space_terminal,
            commands::list_space_terminals,
            commands::space_terminal_mention,
            commands::project_config,
            commands::code_intel_status,
            commands::code_intel_install,
            commands::code_intel_set_lsp,
            commands::code_intel_decline,
            commands::code_intel_remove,
            commands::code_intel_hover,
            commands::config_draft,
            commands::check_config,
            commands::save_config,
            commands::global_config,
            commands::open_global_config,
            commands::account_status,
            commands::account_sign_in_start,
            commands::account_sign_in_poll,
            commands::account_sign_in_cancel,
            commands::account_sign_out,
            commands::focus_window,
            commands::notify_space,
            commands::log_client,
            commands::track_ui,
            commands::telemetry_status,
            commands::set_telemetry,
            commands::report_problem,
            commands::reveal_report,
            commands::issue_target,
            commands::preview_issue,
            commands::submit_issue,
        ])
        // Not a command return type, but still an event payload the UI needs.
        .typ::<theodore_contract::Ephemeral>()
        // Sync ids and unix-ms timestamps stay far below 2^53 (contract crate
        // makes the same call via its own u64/i64 -> u32/i32 remap).
        .dangerously_cast_bigints_to_number()
}

/// Writes the TS command bindings to `out` (default `ui/src/gen/bindings.ts`,
/// relative to the workspace root). Strips the trailing whitespace Specta
/// leaves after `|`, like `crates/contract/src/bin/export-ts.rs`.
pub fn export_bindings(out: impl AsRef<Path>) -> Result<(), String> {
    let out = out.as_ref();
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    specta_builder()
        .export(specta_typescript::Typescript::default(), out)
        .map_err(|e| e.to_string())?;

    let contents = std::fs::read_to_string(out).map_err(|e| e.to_string())?;
    let mut clean: String = contents
        .lines()
        .map(|l| format!("{}\n", l.trim_end()))
        .collect();
    while clean.ends_with("\n\n") {
        clean.pop();
    }
    std::fs::write(out, clean).map_err(|e| e.to_string())
}

/// The prefix the UI puts on its `localStorage` keys (tabs, onboarding,
/// last-seen heads). Webview storage follows the bundle id, and two runs
/// can share one: a sandbox run and a real one of the same build, or a dev
/// build bundled locally (it keeps production's id) and production. Each
/// of those gets its own prefix, so one never leaks into the other.
/// Staging and nightly bundles have ids of their own.
fn storage_namespace(sandbox: bool, env: theodore_runtime::BuildEnv) -> &'static str {
    match (sandbox, env) {
        (true, _) => "sandbox:",
        (false, theodore_runtime::BuildEnv::Dev) => "dev:",
        (false, _) => "",
    }
}

/// Runs the Theodore desktop shell. `core` is the use-case implementation;
/// `forward` receives the event emitter once the app is up, to pipe the
/// core's sync and ephemeral streams to the UI. `auto_update` starts the
/// launch-time and periodic update check (the menu's "Check for updates"
/// works either way).
pub fn run(
    core: Arc<dyn CoreApi>,
    sandbox: bool,
    auto_update: bool,
    forward: impl FnOnce(CoreEvents<tauri::Wry>) + Send + 'static,
) {
    let builder = specta_builder();
    let storage_ns = tauri::plugin::Builder::<tauri::Wry>::new("storage-ns")
        .js_init_script(format!(
            "window.__THEODORE_STORAGE_NS__ = {:?};",
            storage_namespace(sandbox, build_info::build_env())
        ))
        .build();

    let exit_core = core.clone();
    tauri::Builder::default()
        .plugin(storage_ns)
        // Native menu bar. Its Edit submenu is also what makes ⌘C / ⌘V /
        // ⌘X / ⌘A / ⌘Z work inside text fields on macOS.
        .menu(menu::build)
        .on_menu_event(menu::on_event)
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        // Links out (a PR on its host, the sign-in page): the webview's own
        // `window.open` opens nothing in a desktop window.
        .plugin(tauri_plugin_opener::init())
        // The build's URL scheme (the account sign-in redirect in an app
        // bundle; `deep_link`).
        .plugin(tauri_plugin_deep_link::init())
        // A space's turn finished while the window was in the background
        // (`notify`; the plugin is its fallback where clicks can't come back).
        .plugin(tauri_plugin_notification::init())
        // Signed out where sign-in is required, only the sign-in screen's
        // commands run (plan §6.12, `gate.rs`).
        .invoke_handler(gate::guard_core(core.clone(), builder.invoke_handler()))
        .manage(core.clone())
        .manage(updater::UpdateState::default())
        // The title bar's size reaches the page as CSS variables (`chrome`).
        .on_page_load(|webview, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Finished
                && let Some(window) = webview.app_handle().get_webview_window(webview.label())
            {
                chrome::sync_insets(&window);
            }
        })
        .setup(move |app| {
            builder.mount_events(app);
            let handle = app.handle().clone();
            // `Theodore Dev` and the rest: the title Mission Control and the
            // Window menu show (`tauri.conf.json` has production's).
            if let Some(main) = app.get_webview_window("main") {
                let _ = main.set_title(&build_info::product_name());
                chrome::unified_toolbar(&main);
                let window = main.clone();
                main.on_window_event(move |e| {
                    if let tauri::WindowEvent::Resized(_) = e {
                        chrome::sync_insets(&window);
                    }
                });
            }
            app.manage(notify::for_app(&handle));
            forward(emitter(handle.clone()));
            deep_link::listen(&handle, core.clone());
            if auto_update {
                updater::spawn_periodic_check(handle, core.clone());
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the Theodore app")
        .run(move |_handle, event| {
            // A clean quit stops every space's `run` command first: left to
            // `kill_on_drop` alone, a SIGTERM'd process never runs its
            // destructors, so its process group would be orphaned to PID 1
            // (docs/spaces.md "Crash recovery") — the same way an unclean
            // exit does, just needlessly.
            if let tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit = event {
                tauri::async_runtime::block_on(exit_core.stop_all_space_commands());
            }
            // Last: send what the remote sinks still hold, flush the file.
            if let tauri::RunEvent::Exit = event
                && let Some(t) = theodore_telemetry::global()
            {
                tracing::info!("Theodore quitting");
                t.shutdown();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_is_namespaced_where_webview_storage_is_shared() {
        use theodore_runtime::BuildEnv;
        assert_eq!(storage_namespace(true, BuildEnv::Production), "sandbox:");
        assert_eq!(storage_namespace(true, BuildEnv::Dev), "sandbox:");
        assert_eq!(storage_namespace(false, BuildEnv::Dev), "dev:");
        for env in [BuildEnv::Production, BuildEnv::Staging, BuildEnv::Nightly] {
            assert_eq!(storage_namespace(false, env), "", "{env:?}");
        }
    }

    #[test]
    fn export_bindings_writes_a_commands_object() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("bindings.ts");
        export_bindings(&out).expect("export bindings");
        let written = std::fs::read_to_string(&out).expect("read generated bindings");
        assert!(written.contains("export const commands"));
        assert!(written.contains("bootstrap"));
        // No trailing whitespace on any line (the commit hook rejects it).
        assert!(written.lines().all(|l| l == l.trim_end()));
    }
}
