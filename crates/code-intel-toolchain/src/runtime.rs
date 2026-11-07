//! The runtimes a language server runs on and Theodore does not ship
//! (docs/code-intel.md §4.3). `requires: { runtime: java, min: 21 }` in the
//! manifest is checked by running the runtime: `which` alone lies, and macOS
//! has a `/usr/bin/java` that exists with no JDK behind it.
//!
//! ```text
//! requires ─▶ candidates ─▶ inside the worktree and Review? ─▶ skip
//!    java: JAVA_HOME · java_home -v <min>+ · PATH · Homebrew's keg-only openjdk*
//!    else: PATH
//!          ─▶ run it (timeout, neutral cwd in Review) ─▶ parse the version
//!                ├─ ≥ min ─▶ Found { exe, env }  first one wins
//!                ├─ < min ─▶ remembered: TooOld { found }
//!                └─ fails ─▶ next
//! none ─▶ Missing
//! ```
//!
//! The version check happens where the server will not: in a review run the
//! worktree's `.tool-versions` must not steer it. So [`Runtime::exe`] is the
//! real binary the probe ran (`process.execPath`, `RbConfig.ruby`,
//! `PHP_BINARY`, `java.home`), not a version manager's shim that would pick
//! again from the server's cwd.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use theodore_ports::code_intel::TrustMode;

use crate::server::{ProbeEnv, Run, is_inside, run};

/// Runtime ids a manifest may name. `dotnet-sdk` is a `dotnet` runtime of at
/// least `min` plus any SDK: servers that load projects through MSBuild need
/// the SDK, and a machine can have the runtime alone.
pub(crate) const KNOWN_RUNTIMES: &[&str] = &["node", "java", "ruby", "php", "dotnet", "dotnet-sdk"];

/// `min:` of a requirement: `21` or `"8.2"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MinVersion {
    pub major: u32,
    pub minor: u32,
}

impl fmt::Display for MinVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.minor {
            0 => write!(f, "{}", self.major),
            m => write!(f, "{}.{m}", self.major),
        }
    }
}

impl Serialize for MinVersion {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for MinVersion {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl serde::de::Visitor<'_> for V {
            type Value = MinVersion;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a major version (21) or a quoted major.minor (\"8.2\")")
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<MinVersion, E> {
                u32::try_from(v)
                    .map(|major| MinVersion { major, minor: 0 })
                    .map_err(E::custom)
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<MinVersion, E> {
                u64::try_from(v)
                    .map_err(E::custom)
                    .and_then(|v| self.visit_u64(v))
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<MinVersion, E> {
                parse_dotted(v)
                    .map(|(major, minor)| MinVersion { major, minor })
                    .ok_or_else(|| E::custom(format!("bad version {v:?}")))
            }
        }
        d.deserialize_any(V)
    }
}

/// A runtime a server needs (`requires:` in the manifest).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRequirement {
    /// One of [`KNOWN_RUNTIMES`].
    pub runtime: String,
    pub min: MinVersion,
}

/// A runtime that passed the check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
    /// What `{runtime}` in `launch` becomes.
    pub exe: PathBuf,
    /// As the probe printed it: `25.0.1`.
    pub version: String,
    /// `JAVA_HOME` of a JVM, `DOTNET_ROOT` of .NET.
    pub(crate) home: Option<(&'static str, PathBuf)>,
}

impl Runtime {
    /// Environment for a server on this runtime: its home, and `PATH` with
    /// its directory first (so a script's `#!/usr/bin/env node` finds it).
    pub(crate) fn env(&self, path: Option<&OsStr>) -> Vec<(OsString, OsString)> {
        let mut out = Vec::new();
        if let Some((key, home)) = &self.home {
            out.push((OsString::from(key), home.as_os_str().to_owned()));
        }
        if let Some(dir) = self.exe.parent() {
            let mut dirs = vec![dir.to_path_buf()];
            if let Some(path) = path {
                dirs.extend(std::env::split_paths(path).filter(|p| p != dir));
            }
            if let Ok(joined) = std::env::join_paths(dirs) {
                out.push((OsString::from("PATH"), joined));
            }
        }
        out
    }
}

/// The outcome of checking a requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeCheck {
    Found(Runtime),
    /// One ran, but older than `min` (or a .NET runtime without an SDK).
    TooOld {
        found: String,
    },
    Missing,
}

/// Where Java lives besides `PATH`; other runtimes have only `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JavaLookup {
    /// `$JAVA_HOME`.
    pub java_home_env: Option<PathBuf>,
    /// `/usr/libexec/java_home`.
    pub java_home_tool: Option<PathBuf>,
    /// Directories holding Homebrew's keg-only `openjdk*/bin/java`.
    pub keg_roots: Vec<PathBuf>,
}

impl JavaLookup {
    /// This machine's: `$JAVA_HOME`, `java_home`, Homebrew's prefixes.
    pub fn system() -> Self {
        Self {
            java_home_env: std::env::var_os("JAVA_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            java_home_tool: Some(PathBuf::from("/usr/libexec/java_home")),
            keg_roots: vec![
                PathBuf::from("/opt/homebrew/opt"),
                PathBuf::from("/usr/local/opt"),
            ],
        }
    }

    /// `PATH` only (tests).
    pub fn none() -> Self {
        Self {
            java_home_env: None,
            java_home_tool: None,
            keg_roots: Vec::new(),
        }
    }
}

/// Check `req` on this machine. Probes run in `env.neutral_dir` in a review
/// run, in the worktree otherwise.
pub(crate) async fn check(
    req: &RuntimeRequirement,
    worktree: &Path,
    trust: TrustMode,
    env: &ProbeEnv,
) -> RuntimeCheck {
    let cwd = match trust {
        TrustMode::Trusted => worktree,
        TrustMode::Review => env.neutral_dir.as_path(),
    };
    let mut too_old = None;
    for path in candidates(req, cwd, env).await {
        if trust == TrustMode::Review && is_inside(&path, worktree) {
            tracing::info!(
                target: "code_intel", runtime = %req.runtime, path = %path.display(),
                "runtime inside the worktree ignored in a review run"
            );
            continue;
        }
        let Some(found) = probe(&req.runtime, &path, cwd, env).await else {
            continue;
        };
        if found.meets(req.min) {
            return RuntimeCheck::Found(found.runtime);
        }
        too_old.get_or_insert(found.described());
    }
    match too_old {
        Some(found) => RuntimeCheck::TooOld { found },
        None => RuntimeCheck::Missing,
    }
}

/// Where to look, best first, each path once.
async fn candidates(req: &RuntimeRequirement, cwd: &Path, env: &ProbeEnv) -> Vec<PathBuf> {
    let on_path = |name: &str| which::which_in(name, env.path_var().as_ref(), cwd).ok();
    let mut out: Vec<PathBuf> = Vec::new();
    match req.runtime.as_str() {
        "java" => {
            let java = &env.java;
            out.extend(java.java_home_env.iter().map(|h| h.join("bin/java")));
            if let Some(tool) = &java.java_home_tool {
                let wanted = format!("{}+", req.min.major);
                if let Some(home) = java_home(tool, &["-v", &wanted], cwd, env).await {
                    out.push(home.join("bin/java"));
                }
            }
            if let Some(p) = on_path("java")
                // macOS's wrapper: with no JDK it fails, and may open an
                // "install Java" dialog. Ask quietly first.
                && !(p == Path::new("/usr/bin/java")
                    && java.java_home_tool.is_some()
                    && !any_jvm(java, cwd, env).await)
            {
                out.push(p);
            }
            out.extend(keg_javas(&java.keg_roots));
        }
        "dotnet-sdk" => out.extend(on_path("dotnet")),
        other => out.extend(on_path(other)),
    }
    let mut seen = Vec::new();
    out.retain(|p| {
        let new = !seen.contains(p);
        if new {
            seen.push(p.clone());
        }
        new && p.is_file()
    });
    out
}

/// `java_home <args>`: the home it prints, if it succeeds.
async fn java_home(tool: &Path, args: &[&str], cwd: &Path, env: &ProbeEnv) -> Option<PathBuf> {
    if !tool.is_file() {
        return None;
    }
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let Run::Ok(out) = run(tool, &args, cwd, env).await else {
        return None;
    };
    let line = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()?
        .trim()
        .to_string();
    let home = PathBuf::from(line);
    home.is_absolute().then_some(home)
}

async fn any_jvm(java: &JavaLookup, cwd: &Path, env: &ProbeEnv) -> bool {
    match &java.java_home_tool {
        Some(tool) => java_home(tool, &[], cwd, env).await.is_some(),
        None => true,
    }
}

/// `<root>/openjdk*/bin/java`, newest-looking name first.
fn keg_javas(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        let mut names: Vec<OsString> = std::fs::read_dir(root)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name())
                    .filter(|n| n.to_string_lossy().starts_with("openjdk"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort_by_key(|n| std::cmp::Reverse(keg_rank(n)));
        out.extend(names.into_iter().map(|n| root.join(n).join("bin/java")));
    }
    out
}

/// `openjdk` is Homebrew's newest, `openjdk@21` a pinned one.
fn keg_rank(name: &OsStr) -> u32 {
    let name = name.to_string_lossy();
    match name.strip_prefix("openjdk@") {
        Some(v) => v.parse().unwrap_or(0),
        None => u32::MAX,
    }
}

struct Probed {
    runtime: Runtime,
    major: u32,
    minor: u32,
    /// `dotnet-sdk`: a runtime answered but no SDK did.
    no_sdk: bool,
}

impl Probed {
    fn meets(&self, min: MinVersion) -> bool {
        !self.no_sdk
            && MinVersion {
                major: self.major,
                minor: self.minor,
            } >= min
    }

    fn described(&self) -> String {
        if self.no_sdk {
            format!("{} without an SDK", self.runtime.version)
        } else {
            self.runtime.version.clone()
        }
    }
}

/// Run the runtime at `path` and read its version. `None`: it does not run
/// (a stub, a broken install), or runs too slowly to trust.
async fn probe(runtime: &str, path: &Path, cwd: &Path, env: &ProbeEnv) -> Option<Probed> {
    match runtime {
        "java" => probe_java(path, cwd, env).await,
        "dotnet" => probe_dotnet(path, false, cwd, env).await,
        "dotnet-sdk" => probe_dotnet(path, true, cwd, env).await,
        "node" => probe_plain(path, "-p", "process.execPath", cwd, env).await,
        "ruby" => probe_plain(path, "-e", "print RbConfig.ruby", cwd, env).await,
        "php" => probe_plain(path, "-r", "echo PHP_BINARY;", cwd, env).await,
        _ => None,
    }
}

/// `<exe> --version` (`v22.14.0`, `ruby 3.4.1 (…)`, `PHP 8.4.3 (cli)…`).
/// The real binary comes from `<exe> <flag> <expr>`; a shim's own path if
/// that fails.
async fn probe_plain(
    path: &Path,
    flag: &str,
    expr: &str,
    cwd: &Path,
    env: &ProbeEnv,
) -> Option<Probed> {
    let out = run_ok(path, &["--version"], cwd, env).await?;
    let (version, major, minor) = version_in(&out)?;
    let exe = match run_ok(path, &[flag, expr], cwd, env).await {
        Some(real) => absolute_file(real.lines().next().unwrap_or_default().trim())
            .unwrap_or_else(|| path.to_path_buf()),
        None => path.to_path_buf(),
    };
    Some(Probed {
        runtime: Runtime {
            exe,
            version,
            home: None,
        },
        major,
        minor,
        no_sdk: false,
    })
}

/// `java -XshowSettings:properties -version` writes to stderr, both the
/// version and `java.home`; the macOS stub fails.
async fn probe_java(path: &Path, cwd: &Path, env: &ProbeEnv) -> Option<Probed> {
    let out = run_ok(path, &["-XshowSettings:properties", "-version"], cwd, env).await?;
    let (version, major, minor) = java_version(&out)?;
    let home = out
        .lines()
        .find_map(|l| l.trim().strip_prefix("java.home = "))
        .and_then(|h| {
            let h = PathBuf::from(h.trim());
            h.join("bin/java").is_file().then_some(h)
        })
        .or_else(|| path.parent()?.parent().map(Path::to_path_buf));
    let exe = home
        .as_ref()
        .map(|h| h.join("bin/java"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| path.to_path_buf());
    Some(Probed {
        runtime: Runtime {
            exe,
            version,
            home: home.map(|h| ("JAVA_HOME", h)),
        },
        major,
        minor,
        no_sdk: false,
    })
}

/// The newest `Microsoft.NETCore.App` of `dotnet --list-runtimes`; the SDK
/// and the runtime are separate installs, `--version` says nothing about
/// which runtimes exist.
async fn probe_dotnet(path: &Path, sdk: bool, cwd: &Path, env: &ProbeEnv) -> Option<Probed> {
    let out = run_ok(path, &["--list-runtimes"], cwd, env).await?;
    let mut best: Option<(u32, u32, String, Option<PathBuf>)> = None;
    for line in out.lines() {
        let Some(rest) = line.trim().strip_prefix("Microsoft.NETCore.App ") else {
            continue;
        };
        let (version, place) = rest.split_once(' ').unwrap_or((rest, ""));
        let Some((major, minor)) = parse_dotted(version) else {
            continue;
        };
        if best.as_ref().is_none_or(|b| (major, minor) > (b.0, b.1)) {
            // `[<root>/shared/Microsoft.NETCore.App]` → `<root>`.
            let root = PathBuf::from(place.trim().trim_matches(['[', ']']))
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf);
            best = Some((major, minor, version.to_string(), root));
        }
    }
    let (major, minor, version, root) = best?;
    let no_sdk = sdk
        && run_ok(path, &["--list-sdks"], cwd, env)
            .await
            .is_none_or(|sdks| sdks.trim().is_empty());
    Some(Probed {
        runtime: Runtime {
            exe: path.to_path_buf(),
            version,
            home: root.filter(|r| r.is_absolute()).map(|r| ("DOTNET_ROOT", r)),
        },
        major,
        minor,
        no_sdk,
    })
}

/// Stdout and stderr of a command that exits 0 in time.
async fn run_ok(path: &Path, args: &[&str], cwd: &Path, env: &ProbeEnv) -> Option<String> {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    match run(path, &args, cwd, env).await {
        Run::Ok(out) => Some(format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )),
        Run::Timeout | Run::Failed => None,
    }
}

fn absolute_file(s: &str) -> Option<PathBuf> {
    let p = PathBuf::from(s);
    (p.is_absolute() && p.is_file()).then_some(p)
}

/// `java version "1.8.0_392"` is Java 8; `"21"` and `"25.0.1"` are what they
/// say.
fn java_version(text: &str) -> Option<(String, u32, u32)> {
    let after = text.split_once("version \"")?.1;
    let version = after.split('"').next()?;
    let (major, minor) = parse_dotted(version)?;
    let (major, minor) = match (major, minor) {
        (1, m) => (m, 0),
        other => other,
    };
    Some((version.to_string(), major, minor))
}

/// The first word that reads as a version in `text`: `v22.14.0`, `4.0.6`.
fn version_in(text: &str) -> Option<(String, u32, u32)> {
    text.split_whitespace().find_map(|word| {
        let word = word.trim_start_matches('v');
        word.starts_with(|c: char| c.is_ascii_digit())
            .then(|| parse_dotted(word))
            .flatten()
            .map(|(major, minor)| (word.to_string(), major, minor))
    })
}

/// `8.2.1` → `(8, 2)`, `21` → `(21, 0)`. Trailing text in a part is ignored
/// (`8.2-rc1`, `0_392`).
fn parse_dotted(s: &str) -> Option<(u32, u32)> {
    let mut parts = s.split('.');
    let number = |part: &str| -> Option<u32> {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    };
    let major = number(parts.next()?)?;
    let minor = parts.next().and_then(number).unwrap_or(0);
    Some((major, minor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn version_parsing() {
        assert_eq!(parse_dotted("8.2.1"), Some((8, 2)));
        assert_eq!(parse_dotted("21"), Some((21, 0)));
        assert_eq!(parse_dotted("8.2-rc1"), Some((8, 2)));
        assert_eq!(parse_dotted("x"), None);
        assert_eq!(
            version_in("v22.14.0\n"),
            Some(("22.14.0".into(), 22, 14)),
            "node"
        );
        assert_eq!(
            version_in("ruby 4.0.6 (2026-07-14 revision 03b6d3f889) +PRISM"),
            Some(("4.0.6".into(), 4, 0)),
            "ruby"
        );
        assert_eq!(
            version_in("PHP 8.4.3 (cli) (built: Jan 15 2025 01:03:17) (NTS)"),
            Some(("8.4.3".into(), 8, 4)),
            "php"
        );
        assert_eq!(version_in("no digits here"), None);
    }

    #[test]
    fn java_versions() {
        assert_eq!(
            java_version("java version \"25.0.1\" 2025-10-21 LTS\nJava(TM) SE"),
            Some(("25.0.1".into(), 25, 0))
        );
        assert_eq!(
            java_version("openjdk version \"1.8.0_392\"\nOpenJDK"),
            Some(("1.8.0_392".into(), 8, 0))
        );
        assert_eq!(
            java_version("openjdk version \"21\" 2023-09-19"),
            Some(("21".into(), 21, 0))
        );
        assert_eq!(java_version("Unable to locate a Java Runtime."), None);
    }

    #[test]
    fn min_version_reads_a_number_or_a_string() {
        let min = |s: &str| serde_yaml::from_str::<MinVersion>(s);
        assert_eq!(
            min("21").unwrap(),
            MinVersion {
                major: 21,
                minor: 0
            }
        );
        assert_eq!(min("\"8.2\"").unwrap(), MinVersion { major: 8, minor: 2 });
        assert!(min("8.2").is_err(), "a float: 8.10 would read as 8.1");
        assert_eq!(min("\"8.2\"").unwrap().to_string(), "8.2");
        assert_eq!(min("21").unwrap().to_string(), "21");
        assert!(MinVersion { major: 8, minor: 4 } >= MinVersion { major: 8, minor: 2 });
        assert!(MinVersion { major: 8, minor: 1 } < MinVersion { major: 8, minor: 2 });
        assert!(MinVersion { major: 9, minor: 0 } > MinVersion { major: 8, minor: 2 });
    }

    #[test]
    fn kegs_newest_name_first() {
        let root = tempfile::tempdir().unwrap();
        for k in ["openjdk@17", "openjdk", "openjdk@21", "node"] {
            std::fs::create_dir(root.path().join(k)).unwrap();
        }
        let got: Vec<String> = keg_javas(&[root.path().to_path_buf()])
            .iter()
            .map(|p| p.strip_prefix(root.path()).unwrap().display().to_string())
            .collect();
        assert_eq!(
            got,
            vec![
                "openjdk/bin/java",
                "openjdk@21/bin/java",
                "openjdk@17/bin/java"
            ]
        );
    }

    #[test]
    fn runtime_env_puts_its_directory_first() {
        let r = Runtime {
            exe: PathBuf::from("/jdk/bin/java"),
            version: "21".into(),
            home: Some(("JAVA_HOME", PathBuf::from("/jdk"))),
        };
        let path = OsString::from("/usr/bin:/jdk/bin:/bin");
        assert_eq!(
            r.env(Some(&path)),
            vec![
                (OsString::from("JAVA_HOME"), OsString::from("/jdk")),
                (
                    OsString::from("PATH"),
                    OsString::from("/jdk/bin:/usr/bin:/bin")
                ),
            ]
        );
    }
}
