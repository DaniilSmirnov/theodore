//! The app's own URL scheme, one per build (`identity::url_scheme`:
//! `theodore://` for production, `theodore-staging://`,
//! `theodore-nightly://`), registered in the bundle's `Info.plist` from the
//! bundle config (`plugins.deep-link`: `tauri.conf.json`, with
//! `tauri.<env>.conf.json` merged in for staging and nightly). It carries
//! the Theodore account sign-in redirect, and the link back to a space a
//! space's PR body ends with (docs/spaces.md §6).
//!
//! ```text
//! browser ─▶ theodore[-<env>]://callback?code=…&state=… ─▶ macOS "Open …?"
//!   ─▶ on_open_url ─▶ core.account_sign_in_callback ─▶ state check, exchange
//!        └─ handled ─▶ main window to the front
//! PR body ─▶ theodore[-<env>]://space/<id> ─▶ main window to the front
//!   ─▶ emit `open-space` <id> ─▶ the UI opens the space
//! ```
//!
//! Only an app bundle can receive the scheme; a dev run or a bare binary
//! keeps the loopback redirect (`app_redirect`), and so does a dev build
//! even when bundled: it has no scheme of its own.

use std::path::Path;
use std::sync::Arc;

use tauri::{AppHandle, Runtime};
use tauri_plugin_deep_link::DeepLinkExt;
use theodore_runtime::identity;
use url::Url;

use crate::CoreApi;
use crate::build_info;

/// This build's scheme; `None` for a dev build.
pub fn scheme() -> Option<String> {
    identity::url_scheme(build_info::build_env())
}

/// The account redirect for this process: the build's URL scheme
/// redirect when running from an app bundle (the only place macOS routes
/// the scheme to), else `None` (the core uses the loopback).
pub fn app_redirect() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    if !(cfg!(target_os = "macos") && in_app_bundle(&exe)) {
        return None;
    }
    identity::app_redirect_uri(build_info::build_env())
}

/// `…/Name.app/Contents/MacOS/<exe>`.
pub fn in_app_bundle(exe: &Path) -> bool {
    let Some(macos) = exe.parent() else {
        return false;
    };
    let Some(contents) = macos.parent() else {
        return false;
    };
    let Some(bundle) = contents.parent() else {
        return false;
    };
    macos.file_name().is_some_and(|n| n == "MacOS")
        && contents.file_name().is_some_and(|n| n == "Contents")
        && bundle.extension().is_some_and(|e| e == "app")
}

/// Routes URLs the OS opens with the app: while running (`on_open_url`)
/// and the one it was launched with, if any (`get_current`).
pub fn listen<R: Runtime>(app: &AppHandle<R>, core: Arc<dyn CoreApi>) {
    let handle = app.clone();
    let on_open_core = core.clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            open(&handle, on_open_core.clone(), url);
        }
    });
    if let Ok(Some(urls)) = app.deep_link().get_current() {
        for url in urls {
            open(app, core.clone(), url);
        }
    }
}

fn open<R: Runtime>(app: &AppHandle<R>, core: Arc<dyn CoreApi>, url: Url) {
    if scheme().as_deref() != Some(url.scheme()) {
        return;
    }
    if let Some(space) = space_route(&url) {
        use crate::notify::SpaceWindow;
        let window = crate::notify::MainWindow(app.clone());
        window.focus();
        window.open_space(&space);
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Never log the URL: it carries the authorization code.
        if core.account_sign_in_callback(url.into()).await {
            tracing::info!("account sign-in redirect handled");
            let _ = crate::commands::focus_main(&app);
        }
    });
}

/// `<scheme>://space/<id>` → the space id; `None` for any other URL.
pub fn space_route(url: &Url) -> Option<String> {
    if url.host_str() != Some("space") {
        return None;
    }
    let path = url.path().trim_matches('/');
    let ok = !path.is_empty()
        && path.len() <= 64
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    ok.then(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_space_link_routes_to_its_space() {
        let route = |s: &str| space_route(&Url::parse(s).unwrap());
        assert_eq!(
            route("theodore://space/3f2a9c1b0d4e").as_deref(),
            Some("3f2a9c1b0d4e")
        );
        assert_eq!(
            route("theodore-staging://space/abc/").as_deref(),
            Some("abc")
        );
        assert_eq!(route("theodore://space/"), None);
        assert_eq!(route("theodore://space/a/b"), None);
        assert_eq!(route("theodore://space/..%2Fx"), None);
        assert_eq!(route("theodore://callback?code=1"), None);
    }

    #[test]
    fn only_an_executable_inside_an_app_bundle_counts() {
        assert!(in_app_bundle(Path::new(
            "/Applications/Theodore.app/Contents/MacOS/theodore-app"
        )));
        for bare in [
            "/repo/target/release/theodore-app",
            "/repo/target/debug/theodore-app",
            "/x/Contents/MacOS/Theodore",
            "/x/Theodore.app/MacOS/Theodore",
            "Theodore",
        ] {
            assert!(!in_app_bundle(Path::new(bare)), "{bare}");
        }
    }

    /// `base` with `overlay` merged in, as `cargo tauri build --config`
    /// does (JSON merge patch: objects merge, anything else replaces).
    fn merged(base: &str, overlay: Option<&str>) -> serde_json::Value {
        fn merge(a: &mut serde_json::Value, b: serde_json::Value) {
            match (a, b) {
                (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
                    for (k, v) in b {
                        merge(a.entry(k).or_insert(serde_json::Value::Null), v);
                    }
                }
                (a, b) => *a = b,
            }
        }
        let mut conf: serde_json::Value = serde_json::from_str(base).expect("tauri.conf.json");
        if let Some(o) = overlay {
            merge(&mut conf, serde_json::from_str(o).expect("overlay config"));
        }
        conf
    }

    /// Each bundled build's config registers the names `identity` gives
    /// it: product name, bundle id, and the scheme of its sign-in redirect.
    #[test]
    fn each_bundle_config_registers_its_builds_names() {
        use theodore_runtime::BuildEnv;
        let base = include_str!("../tauri.conf.json");
        let prod = merged(base, None);
        for (env, overlay) in [
            (BuildEnv::Production, None),
            (
                BuildEnv::Staging,
                Some(include_str!("../tauri.staging.conf.json")),
            ),
            (
                BuildEnv::Nightly,
                Some(include_str!("../tauri.nightly.conf.json")),
            ),
        ] {
            let conf = merged(base, overlay);
            assert_eq!(conf["productName"], identity::product_name(env), "{env:?}");
            assert_eq!(
                conf["identifier"],
                identity::bundle_identifier(env),
                "{env:?}"
            );
            let scheme = identity::url_scheme(env).expect("a bundled build has a scheme");
            assert_eq!(
                conf["plugins"]["deep-link"]["desktop"]["schemes"],
                serde_json::json!([scheme]),
                "{env:?}"
            );
            let redirect = identity::app_redirect_uri(env).expect("redirect");
            assert_eq!(Url::parse(&redirect).unwrap().scheme(), scheme);
            // Only the names differ: same updater key, windows, CSP.
            assert_eq!(conf["plugins"]["updater"], prod["plugins"]["updater"]);
            assert_eq!(conf["app"], prod["app"]);
            assert_eq!(conf["version"], prod["version"]);
        }
    }
}
