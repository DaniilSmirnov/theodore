//! Updater (plan §11.1): `tauri-plugin-updater` + `tauri-plugin-process`.
//! Checks on launch and every 6 h, downloads and installs in the background,
//! emits `update` (`downloaded: false` as the download starts, `true` once
//! installed). The actual restart is gated by `CoreApi::is_idle` and only
//! happens when the UI calls the `install_update` command; never forced.
//!
//! ```text
//! tick every 10 min (wall clock: a sleeping Mac doesn't stall it)
//!   ├─ 6 h since the last check ──▶ check ──▶ download + install on disk
//!   │                                          └─ notification, `update_installed`
//!   └─ installed, not restarted, 24 h since the last notice
//!        ──▶ notification again (window in the background)
//! ```

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_updater::UpdaterExt;
use theodore_contract::UpdateInfo;
use url::Url;

use crate::build_info;
use crate::core_api::CoreApi;
use crate::events;

const STABLE_ENDPOINT: &str =
    "https://github.com/JeanIsahakyan/theodore/releases/latest/download/latest.json";
const BETA_ENDPOINT: &str =
    "https://github.com/JeanIsahakyan/theodore/releases/download/channel-beta/latest.json";
const STAGING_ENDPOINT: &str =
    "https://github.com/JeanIsahakyan/theodore/releases/download/channel-staging/latest.json";
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// How often an installed update that waits for a restart is announced again.
const REMIND_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// The periodic loop's tick. Short, and compared against the wall clock:
/// a monotonic 6 h sleep stops while the Mac sleeps, so a laptop that is
/// closed most of the day would check every few days.
const TICK: Duration = Duration::from_secs(10 * 60);

/// Last update this process downloaded and installed on disk (macOS/Linux
/// need a relaunch to run it; see `CoreApi::install_update`).
#[derive(Default)]
pub struct UpdateState(pub Mutex<Option<UpdateInfo>>);

impl UpdateState {
    pub fn get(&self) -> Option<UpdateInfo> {
        self.0.lock().expect("UpdateState lock").clone()
    }
}

/// What the periodic loop does on a tick.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Due {
    check: bool,
    remind: bool,
}

/// `last_check` / `last_notice`: when the loop last checked, last told
/// about the installed update (`None`: never). `pending`: an installed
/// update waits for a restart.
fn due(
    now: SystemTime,
    last_check: Option<SystemTime>,
    last_notice: Option<SystemTime>,
    pending: bool,
) -> Due {
    let elapsed = |t: Option<SystemTime>, every: Duration| {
        // A clock set back counts as due rather than waiting it out.
        t.is_none_or(|t| now.duration_since(t).map_or(true, |d| d >= every))
    };
    Due {
        check: elapsed(last_check, CHECK_INTERVAL),
        remind: pending && last_notice.is_some() && elapsed(last_notice, REMIND_INTERVAL),
    }
}

/// Why a check failed, as `update_failed.reason`.
fn failure_reason(err: &tauri_plugin_updater::Error) -> &'static str {
    use tauri_plugin_updater::Error as E;
    match err {
        // EROFS, EACCES, EPERM: the bundle's folder can't be written.
        E::Io(e) if matches!(e.raw_os_error(), Some(30 | 13 | 1)) => "read_only",
        E::Reqwest(_) | E::Network(_) => "network",
        E::Minisign(_)
        | E::Base64(_)
        | E::SignatureUtf8(_)
        | E::SignedVersionMismatch { .. }
        | E::MissingSignedVersion => "signature",
        _ => "other",
    }
}

/// A failed check: the message the UI shows, the reason telemetry gets.
struct Failure {
    reason: &'static str,
    message: String,
}

/// Whether a build updates itself. Not a nightly: it is built by hand from a
/// commit and no feed publishes one, so the stable feed would replace it with
/// a production app (docs/releasing.md "Local nightly builds").
pub fn self_updates(build_env: &str) -> bool {
    build_env != "nightly"
}

/// The channel a build updates from: a staging build only ever from
/// `staging`, whatever the setting says; any other build from the
/// `updates.channel` setting, `stable` or `beta`, never `staging`
/// (docs/releasing.md "Staging channel").
pub fn effective_channel(build_env: &str, setting: &str) -> &'static str {
    match (build_env, setting) {
        ("staging", _) => "staging",
        (_, "beta") => "beta",
        _ => "stable",
    }
}

fn endpoint_for_channel(channel: &str) -> Url {
    let url = match channel {
        "beta" => BETA_ENDPOINT,
        "staging" => STAGING_ENDPOINT,
        _ => STABLE_ENDPOINT,
    };
    Url::parse(url).expect("static updater endpoint is valid")
}

/// Checks for an update, and if one exists downloads and installs it (does
/// not restart). Updates `UpdateState` and emits `update` on success.
/// Never downloads twice.
pub async fn check_and_install<R: Runtime>(
    app: &AppHandle<R>,
    core: &Arc<dyn CoreApi>,
) -> Option<UpdateInfo> {
    match try_check(app, core).await {
        Ok(info) => info,
        Err(Failure { reason, message }) => {
            // Known and only fixed by moving the app; the event counts it.
            if reason == "read_only" {
                tracing::info!(err = %message, "updater: keeping current version");
            } else {
                tracing::warn!(err = %message, "updater: keeping current version");
            }
            theodore_telemetry::product(theodore_telemetry::ProductEvent::UpdateFailed {
                reason: reason.into(),
            });
            None
        }
    }
}

/// Like `check_and_install`, but says why a check failed (unreachable
/// endpoint, no release yet, bad signature) instead of reading as "no
/// update" — for the Settings "Check now" button.
pub async fn check<R: Runtime>(
    app: &AppHandle<R>,
    core: &Arc<dyn CoreApi>,
) -> Result<Option<UpdateInfo>, String> {
    try_check(app, core).await.map_err(|f| f.message)
}

async fn try_check<R: Runtime>(
    app: &AppHandle<R>,
    core: &Arc<dyn CoreApi>,
) -> Result<Option<UpdateInfo>, Failure> {
    let other = |message: String| Failure {
        reason: "other",
        message,
    };
    // A sandbox window never reaches the network (docs/sandbox.md).
    if !core.updates_enabled().await {
        return Ok(None);
    }
    if !self_updates(build_info::BUILD_ENV) {
        return Err(other(
            "a nightly build doesn't update itself; build a new one".into(),
        ));
    }
    // The install would fail after the whole download (relocate.rs).
    if let Some(why) = crate::relocate::blocks_updates() {
        return Err(Failure {
            reason: "read_only",
            message: why.into(),
        });
    }
    // Resolved again here, so no `CoreApi` can point a build at another
    // environment's releases.
    let channel = effective_channel(build_info::BUILD_ENV, &core.update_channel().await);
    let endpoint = endpoint_for_channel(channel);
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|e| other(format!("bad update endpoint: {e}")))?
        .build()
        .map_err(|e| other(format!("updater unavailable: {e}")))?;
    let Some(update) = updater.check().await.map_err(|e| Failure {
        reason: failure_reason(&e),
        message: format!("couldn't check for updates: {e}"),
    })?
    else {
        tracing::info!(channel, "updater: up to date");
        return Ok(None);
    };
    // Already on disk from an earlier check in this process: nothing to
    // download again, the restart is still what's left.
    if let Some(ready) = app
        .state::<UpdateState>()
        .get()
        .filter(|u| u.downloaded && u.version == update.version)
    {
        return Ok(Some(ready));
    }
    tracing::info!(channel, version = %update.version, "updater: downloading");
    // Says "downloading" before the download, which can take a while.
    events::emitter(app.clone()).update(UpdateInfo {
        version: update.version.clone(),
        notes: update.body.clone().unwrap_or_default(),
        downloaded: false,
    });
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| Failure {
            reason: failure_reason(&e),
            message: format!("download or signature check failed: {e}"),
        })?;
    // On disk now; this process keeps running the old build until a restart.
    tracing::info!(version = %update.version, "updater: installed, restart to apply");
    theodore_telemetry::product(theodore_telemetry::ProductEvent::UpdateInstalled {
        version: update.version.clone(),
    });
    notify_installed(app, core, &update.version);

    let info = UpdateInfo {
        version: update.version.clone(),
        notes: update.body.clone().unwrap_or_default(),
        downloaded: true,
    };
    app.state::<UpdateState>()
        .0
        .lock()
        .expect("UpdateState lock")
        .replace(info.clone());
    events::emitter(app.clone()).update(info.clone());
    Ok(Some(info))
}

/// A system notification for an installed update, only while the window is
/// in the background: in front, the "Restart to update" pill says it.
fn notify_installed<R: Runtime>(app: &AppHandle<R>, core: &Arc<dyn CoreApi>, version: &str) {
    // Counts as told either way: in front, the pill said it.
    app.state::<Notices>().set(SystemTime::now());
    let focused = app
        .get_webview_window("main")
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false);
    if focused {
        return;
    }
    let Some(notifier) = app.try_state::<Arc<dyn crate::notify::Notifier>>() else {
        return;
    };
    crate::notify::notify_update(
        notifier.inner().as_ref(),
        UpdateWindow {
            app: app.clone(),
            core: core.clone(),
        },
        version,
    );
}

struct UpdateWindow<R: Runtime> {
    app: AppHandle<R>,
    core: Arc<dyn CoreApi>,
}

impl<R: Runtime> crate::notify::UpdateWindow for UpdateWindow<R> {
    fn focus(&self) {
        if let Err(err) = crate::commands::focus_main(&self.app) {
            tracing::warn!(?err, "failed to focus the window after an update click");
        }
    }

    fn restart_when_idle(&self) {
        let (app, core) = (self.app.clone(), self.core.clone());
        tauri::async_runtime::spawn(async move {
            if core.is_idle().await {
                app.request_restart();
            }
        });
    }
}

/// When the installed update was last announced (notification or pill).
#[derive(Default)]
pub struct Notices(Mutex<Option<SystemTime>>);

impl Notices {
    fn get(&self) -> Option<SystemTime> {
        *self.0.lock().expect("Notices lock")
    }
    fn set(&self, at: SystemTime) {
        self.0.lock().expect("Notices lock").replace(at);
    }
}

/// Runs `check_and_install` on launch and every 6 hours for the app's
/// lifetime, and announces an installed update again once a day until
/// the app restarts into it.
pub fn spawn_periodic_check<R: Runtime>(app: AppHandle<R>, core: Arc<dyn CoreApi>) {
    tauri::async_runtime::spawn(async move {
        let mut last_check = None;
        loop {
            let now = SystemTime::now();
            let ready = app.state::<UpdateState>().get().filter(|u| u.downloaded);
            let due = due(
                now,
                last_check,
                app.state::<Notices>().get(),
                ready.is_some(),
            );
            if due.check {
                last_check = Some(now);
                check_and_install(&app, &core).await;
            } else if due.remind
                && let Some(update) = ready
            {
                tracing::info!(version = %update.version, "updater: reminding to restart");
                notify_installed(&app, &core, &update.version);
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_and_beta_resolve_to_different_github_releases() {
        let stable = endpoint_for_channel("stable");
        let beta = endpoint_for_channel("beta");
        assert_ne!(stable, beta);
        assert!(stable.as_str().ends_with("latest.json"));
        assert!(beta.as_str().contains("channel-beta"));
    }

    #[test]
    fn checks_every_6h_and_reminds_daily_by_the_wall_clock() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let h = |n: u64| t0 + Duration::from_secs(n * 3600);
        assert!(due(t0, None, None, false).check, "launch checks at once");
        assert!(!due(h(5), Some(t0), None, false).check);
        assert!(due(h(6), Some(t0), None, false).check);
        // A clock set back doesn't stop checks until it catches up.
        assert!(due(t0, Some(h(1)), None, false).check);

        let last = Some(h(5));
        assert!(!due(h(23), last, Some(t0), true).remind);
        assert!(due(h(24), last, Some(t0), true).remind);
        assert!(!due(h(24), last, Some(t0), false).remind, "nothing pending");
        assert!(
            !due(h(24), last, None, true).remind,
            "never announced: the install does"
        );
    }

    #[test]
    fn failures_are_bucketed_for_telemetry() {
        use tauri_plugin_updater::Error as E;
        let io = |code| E::Io(std::io::Error::from_raw_os_error(code));
        assert_eq!(failure_reason(&io(30)), "read_only");
        assert_eq!(failure_reason(&io(13)), "read_only");
        assert_eq!(failure_reason(&io(2)), "other");
        assert_eq!(failure_reason(&E::Network("down".into())), "network");
        assert_eq!(failure_reason(&E::MissingSignedVersion), "signature");
        assert_eq!(failure_reason(&E::ReleaseNotFound), "other");
    }

    #[test]
    fn only_a_nightly_stays_off_the_update_feeds() {
        assert!(!self_updates("nightly"));
        for env in ["production", "staging", "dev"] {
            assert!(self_updates(env), "{env}");
        }
    }

    #[test]
    fn unknown_channel_falls_back_to_stable() {
        assert_eq!(endpoint_for_channel("nope"), endpoint_for_channel("stable"));
    }

    #[test]
    fn a_staging_build_updates_from_staging_only() {
        for setting in ["stable", "beta", "staging", ""] {
            let channel = effective_channel("staging", setting);
            assert_eq!(channel, "staging");
            assert!(
                endpoint_for_channel(channel)
                    .as_str()
                    .ends_with("/releases/download/channel-staging/latest.json")
            );
        }
    }

    #[test]
    fn other_builds_never_see_the_staging_channel() {
        for env in ["production", "nightly", "dev"] {
            assert_eq!(effective_channel(env, "stable"), "stable");
            assert_eq!(effective_channel(env, "beta"), "beta");
            assert_eq!(effective_channel(env, "staging"), "stable");
            assert_eq!(effective_channel(env, "nope"), "stable");
            for setting in ["stable", "beta", "staging", ""] {
                let url = endpoint_for_channel(effective_channel(env, setting));
                assert!(!url.as_str().contains("channel-staging"), "{env}/{setting}");
            }
        }
    }
}
