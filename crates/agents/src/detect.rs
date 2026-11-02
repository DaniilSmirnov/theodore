//! `detect`: find each CLI on `PATH` or a common install dir, read `--version`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use theodore_domain::AgentId;
use theodore_ports::agent::AgentInfo;

use crate::config::{AgentBinaryConfig, common_install_dirs, default_binary_name};

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// Locate a CLI's binary: explicit override, else `PATH`, else the common dirs.
/// Pure w.r.t. its inputs; the only I/O is the filesystem checks `which` and
/// `Path::is_file` do.
pub fn locate_binary(id: AgentId, cfg: &AgentBinaryConfig) -> Option<PathBuf> {
    if let Some(path) = &cfg.path {
        return path.is_file().then(|| path.clone());
    }
    let name = default_binary_name(id);
    if let Ok(path) = which::which(name) {
        return Some(path);
    }
    common_install_dirs()
        .into_iter()
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Run `<path> --version` with a timeout; `None` on timeout, spawn failure, or
/// non-UTF8 / empty output.
pub async fn read_version(path: &Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new(path);
    cmd.arg("--version").stdin(std::process::Stdio::null());
    let fut = crate::exit_output::output_after_exit(cmd);
    let output = tokio::time::timeout(VERSION_TIMEOUT, fut)
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// `AgentPort::detect` for one agent id.
pub async fn detect_one(id: AgentId, cfg: &AgentBinaryConfig) -> AgentInfo {
    match locate_binary(id, cfg) {
        Some(path) => {
            let version = read_version(&path).await;
            AgentInfo {
                id,
                path: Some(path),
                version,
                installed: true,
            }
        }
        None => AgentInfo {
            id,
            path: None,
            version: None,
            installed: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_path_must_exist() {
        let cfg = AgentBinaryConfig {
            path: Some(PathBuf::from("/no/such/binary")),
            ..Default::default()
        };
        assert_eq!(locate_binary(AgentId::Claude, &cfg), None);
    }

    #[tokio::test]
    async fn override_path_used_when_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("fake-claude");
        std::fs::write(&bin, "#!/bin/sh\necho test-1.0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cfg = AgentBinaryConfig {
            path: Some(bin.clone()),
            ..Default::default()
        };
        assert_eq!(locate_binary(AgentId::Claude, &cfg), Some(bin.clone()));
        let info = detect_one(AgentId::Claude, &cfg).await;
        assert!(info.installed);
        assert_eq!(info.version.as_deref(), Some("test-1.0"));
    }

    #[tokio::test]
    async fn detect_one_reports_not_installed() {
        let cfg = AgentBinaryConfig {
            path: Some(PathBuf::from("/definitely/not/here/binary")),
            ..Default::default()
        };
        let info = detect_one(AgentId::Claude, &cfg).await;
        assert!(!info.installed);
        assert_eq!(info.path, None);
        assert_eq!(info.version, None);
    }
}
