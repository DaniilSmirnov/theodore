//! "Move to Applications?" (docs/releasing.md "First launch"). An app run
//! from the disk image, or from a folder macOS translocates (a quarantined
//! app opened where it was downloaded), sits on a read-only path: the
//! updater can't replace it there and fails on every check. The first
//! launch outside an Applications folder offers to move it, once per
//! install.
//!
//! ```text
//! launch ──▶ a bundle outside Applications, never asked ──▶ dialog
//!   Not Now ──▶ marker written, never asked again
//!   Move ──▶ ditto bundle → /Applications (~/Applications if not writable)
//!        ──▶ quarantine stripped, old copy in Applications to the Trash
//!        ──▶ the original to the Trash (a writable folder only)
//!        ──▶ helper: waits for this pid, opens the copy, ejects the image
//!        ──▶ this process exits
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

use tauri::{AppHandle, Runtime};
use theodore_telemetry::ProductEvent;

/// Written to the data dir once the question has been asked.
const MARKER: &str = "asked-move-to-applications";

/// Where the running bundle is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// `/Applications` or `~/Applications`, at any depth.
    Applications,
    /// Anywhere else.
    Elsewhere { bundle: PathBuf, from: Source },
}

/// Where an app outside Applications runs from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A mounted volume under `/Volumes` (a disk image, most likely).
    Volume(PathBuf),
    /// A read-only copy macOS made of a quarantined app (App Translocation);
    /// where the original is isn't known.
    Translocated,
    /// A plain folder (Downloads, Desktop, …).
    Folder,
}

impl Source {
    /// The value `move_to_applications.from` reports.
    fn as_str(&self) -> &'static str {
        match self {
            Source::Volume(_) => "volume",
            Source::Translocated => "translocated",
            Source::Folder => "folder",
        }
    }
}

/// The `.app` bundle holding `exe` (`X.app/Contents/MacOS/<exe>`); `None`
/// for a bare binary (a dev run).
pub fn bundle_path(exe: &Path) -> Option<PathBuf> {
    let bundle = exe.parent()?.parent()?.parent()?;
    (bundle.extension().is_some_and(|e| e == "app")
        && exe.parent()?.file_name()? == "MacOS"
        && exe.parent()?.parent()?.file_name()? == "Contents")
        .then(|| bundle.to_path_buf())
}

pub fn locate(bundle: &Path, home: &Path) -> Location {
    if bundle.starts_with("/Applications") || bundle.starts_with(home.join("Applications")) {
        return Location::Applications;
    }
    let from = if bundle
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
    {
        Source::Translocated
    } else if let Ok(rest) = bundle.strip_prefix("/Volumes") {
        match rest.components().next() {
            Some(name) => Source::Volume(Path::new("/Volumes").join(name)),
            None => Source::Folder,
        }
    } else {
        Source::Folder
    };
    Location::Elsewhere {
        bundle: bundle.to_path_buf(),
        from,
    }
}

/// The running app's location; `None` outside an app bundle.
pub fn current() -> Option<Location> {
    let bundle = bundle_path(&std::env::current_exe().ok()?)?;
    Some(locate(&bundle, &home()?))
}

/// Why the updater can't replace the running app, if it can't: it runs
/// from a read-only place (a translocated copy, a mounted image).
pub fn blocks_updates() -> Option<&'static str> {
    match current()? {
        Location::Elsewhere {
            from: Source::Translocated,
            ..
        } => Some(
            "Theodore runs from a read-only copy macOS made of the download; \
             move it to Applications to get updates",
        ),
        Location::Elsewhere {
            from: Source::Volume(v),
            ..
        } if is_disk_image(&v) => {
            Some("Theodore runs from the disk image; move it to Applications to get updates")
        }
        _ => None,
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// On the first launch outside Applications, asks to move there (on a
/// background thread: the dialog blocks). `data_dir` keeps the answer.
pub fn offer<R: Runtime>(app: &AppHandle<R>, data_dir: PathBuf) {
    // The question is about updates: a nightly has none to lose.
    let env = crate::build_info::BUILD_ENV;
    if !cfg!(target_os = "macos") || env == "dev" || !crate::updater::self_updates(env) {
        return;
    }
    let Some(Location::Elsewhere { bundle, from }) = current() else {
        return;
    };
    let marker = data_dir.join(MARKER);
    if marker.exists() {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
        let name = crate::build_info::product_name();
        let moved = app
            .dialog()
            .message(format!(
                "{name} updates itself only from the Applications folder. \
                 Move it there now? {name} will reopen from Applications."
            ))
            .title(format!("Move {name} to Applications?"))
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Move to Applications".into(),
                "Not Now".into(),
            ))
            .blocking_show();
        if let Err(err) = std::fs::create_dir_all(&data_dir)
            .and_then(|()| std::fs::write(&marker, crate::build_info::VERSION))
        {
            tracing::warn!(%err, "couldn't record the move-to-Applications answer");
        }
        let choice = if !moved {
            "declined"
        } else {
            match move_to_applications(&bundle, &from) {
                Ok(target) => {
                    relaunch(&target, &from);
                    "moved"
                }
                Err(err) => {
                    tracing::warn!(%err, "couldn't move the app to Applications");
                    app.dialog()
                        .message(format!(
                            "{err}\n\nQuit {name} and drag it to the Applications folder."
                        ))
                        .title(format!("Couldn't move {name}"))
                        .blocking_show();
                    "failed"
                }
            }
        };
        theodore_telemetry::product(ProductEvent::MoveToApplications {
            choice: choice.into(),
            from: from.as_str().into(),
        });
        if choice == "moved" {
            tracing::info!("moved to Applications; reopening from there");
            app.exit(0);
        }
    });
}

/// Copies `bundle` into Applications and returns the copy's path.
fn move_to_applications(bundle: &Path, from: &Source) -> Result<PathBuf, String> {
    let home = home().ok_or("HOME isn't set")?;
    let dir = if writable(Path::new("/Applications")) {
        PathBuf::from("/Applications")
    } else {
        let user = home.join("Applications");
        std::fs::create_dir_all(&user).map_err(|e| format!("{}: {e}", user.display()))?;
        user
    };
    let target = install(bundle, &dir, &home)?;
    if *from == Source::Folder
        && let Err(err) = trash(bundle, &home)
    {
        tracing::info!(%err, "left the original app where it was");
    }
    Ok(target)
}

/// Copies `bundle` into `dir`. The copy is finished before an existing app
/// there goes to the Trash: a failed copy (a full disk) costs nothing.
fn install(bundle: &Path, dir: &Path, home: &Path) -> Result<PathBuf, String> {
    let name = bundle.file_name().ok_or("the app has no name")?;
    let target = dir.join(name);
    let partial = dir.join(format!(".{}.partial", name.to_string_lossy()));
    let _ = std::fs::remove_dir_all(&partial);
    if let Err(err) = run(Command::new("/usr/bin/ditto").arg(bundle).arg(&partial)) {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(err);
    }
    // Opened from Applications by this process, not the browser: the
    // copy needs no second Gatekeeper prompt (docs/releasing.md).
    let _ = run(Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&partial));
    if target.exists()
        && let Err(err) = trash(&target, home)
    {
        let _ = std::fs::remove_dir_all(&partial);
        return Err(format!(
            "{} is already there and couldn't be moved to the Trash: {err}",
            target.display()
        ));
    }
    std::fs::rename(&partial, &target).map_err(|e| format!("{}: {e}", target.display()))?;
    Ok(target)
}

/// Opens `target` once this process is gone, then ejects the disk image
/// the app ran from.
fn relaunch(target: &Path, from: &Source) {
    let volume = match from {
        Source::Volume(v) if is_disk_image(v) => v.display().to_string(),
        _ => String::new(),
    };
    let script = r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done
/usr/bin/open "$2"
[ -n "$3" ] && /usr/bin/hdiutil detach -quiet "$3"
exit 0"#;
    if let Err(err) = Command::new("/bin/sh")
        .args(["-c", script, "relaunch"])
        .arg(std::process::id().to_string())
        .arg(target)
        .arg(volume)
        .spawn()
    {
        tracing::warn!(%err, "couldn't reopen the app from Applications");
    }
}

fn is_disk_image(volume: &Path) -> bool {
    Command::new("/usr/bin/hdiutil")
        .arg("info")
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.split('\t').next_back() == Some(&*volume.to_string_lossy()))
        })
        .unwrap_or(false)
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".theodore-write-test-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Moves `path` into `~/.Trash`, under a new name if one is taken.
fn trash(path: &Path, home: &Path) -> std::io::Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let trash = home.join(".Trash");
    let mut to = trash.join(&*name);
    let mut n = 1;
    while to.exists() {
        n += 1;
        to = trash.join(format!("{name} {n}"));
    }
    std::fs::rename(path, to)
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/Users/me";

    fn at(p: &str) -> Location {
        locate(Path::new(p), Path::new(HOME))
    }

    #[test]
    fn the_bundle_is_three_levels_above_the_binary() {
        assert_eq!(
            bundle_path(Path::new(
                "/Applications/Theodore.app/Contents/MacOS/theodore-app"
            )),
            Some(PathBuf::from("/Applications/Theodore.app"))
        );
        assert_eq!(
            bundle_path(Path::new("/repo/target/debug/theodore-app")),
            None
        );
        assert_eq!(
            bundle_path(Path::new("/x/Theodore.app/Contents/bin/t")),
            None
        );
    }

    #[test]
    fn applications_folders_need_no_move() {
        assert_eq!(at("/Applications/Theodore.app"), Location::Applications);
        assert_eq!(at("/Applications/Dev/Theodore.app"), Location::Applications);
        assert_eq!(
            at("/Users/me/Applications/Theodore.app"),
            Location::Applications
        );
    }

    #[test]
    fn elsewhere_says_where_from() {
        let from = |p| match at(p) {
            Location::Elsewhere { from, .. } => from,
            Location::Applications => panic!("{p} is not in Applications"),
        };
        assert_eq!(
            from("/Volumes/Theodore/Theodore.app"),
            Source::Volume("/Volumes/Theodore".into())
        );
        assert_eq!(
            from("/private/var/folders/x/T/AppTranslocation/1A2B/d/Theodore.app"),
            Source::Translocated
        );
        assert_eq!(from("/Users/me/Downloads/Theodore.app"), Source::Folder);
        assert_eq!(
            from("/Users/other/Applications/Theodore.app"),
            Source::Folder
        );
    }

    #[test]
    fn trash_keeps_both_when_a_name_is_taken() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".Trash")).unwrap();
        for _ in 0..2 {
            let app = home.path().join("Theodore.app");
            std::fs::create_dir(&app).unwrap();
            trash(&app, home.path()).unwrap();
        }
        assert!(home.path().join(".Trash/Theodore.app").is_dir());
        assert!(home.path().join(".Trash/Theodore.app 2").is_dir());
    }

    /// A bundle with a binary inside, flagged as downloaded.
    #[cfg(target_os = "macos")]
    fn fake_bundle(at: &Path, content: &str) -> PathBuf {
        let app = at.join("Theodore.app");
        let bin = app.join("Contents/MacOS");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("theodore-app"), content).unwrap();
        run(Command::new("/usr/bin/xattr")
            .args(["-w", "com.apple.quarantine", "0081;0;Safari;"])
            .arg(&app))
        .unwrap();
        app
    }

    #[cfg(target_os = "macos")]
    fn quarantined(path: &Path) -> bool {
        let out = Command::new("/usr/bin/xattr").arg(path).output().unwrap();
        String::from_utf8_lossy(&out.stdout).contains("com.apple.quarantine")
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn install_replaces_an_old_copy_and_strips_quarantine() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".Trash")).unwrap();
        let (src, apps) = (home.path().join("src"), home.path().join("Applications"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&apps).unwrap();
        let bundle = fake_bundle(&src, "new");
        fake_bundle(&apps, "old");

        let target = install(&bundle, &apps, home.path()).unwrap();

        assert_eq!(target, apps.join("Theodore.app"));
        let read =
            |app: &Path| std::fs::read_to_string(app.join("Contents/MacOS/theodore-app")).unwrap();
        assert_eq!(read(&target), "new");
        assert!(!quarantined(&target));
        assert_eq!(read(&home.path().join(".Trash/Theodore.app")), "old");
        assert!(quarantined(&bundle), "the source isn't touched");
        assert!(!apps.join(".Theodore.app.partial").exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_failed_copy_keeps_the_existing_app() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".Trash")).unwrap();
        let apps = home.path().join("Applications");
        std::fs::create_dir_all(&apps).unwrap();
        fake_bundle(&apps, "old");

        let missing = home.path().join("gone/Theodore.app");
        assert!(install(&missing, &apps, home.path()).is_err());

        assert!(apps.join("Theodore.app").is_dir());
        assert!(!home.path().join(".Trash/Theodore.app").exists());
        assert!(!apps.join(".Theodore.app.partial").exists());
    }
}
