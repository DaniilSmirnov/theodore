//! Verified downloads (docs/code-intel.md §4.2).
//!
//! ```text
//! GET url ─▶ 3xx? ─▶ next host allowed? ── no ─▶ Download error
//!    │                └─ yes ─▶ GET next (≤ MAX_REDIRECTS)
//!    └─ 200 ─▶ stream to <root>/tmp/.download-* ─▶ sha256 == manifest? ── no ─▶ Checksum, temp removed
//!                                                  └─ yes ─▶ temp file
//! ```
//! Redirects are followed by hand (the client has none) so every hop is
//! checked against the manifest entry's hosts.

use std::path::Path;
use std::time::Duration;

use reqwest::header::LOCATION;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use theodore_ports::code_intel::{CodeIntelError, CodeIntelResult};
use tokio::io::AsyncWriteExt;
use url::Url;

use crate::manifest::{Artifact, url_allowed};

pub(crate) const MAX_REDIRECTS: usize = 5;
/// No artifact is bigger; a body past it is cut off.
pub(crate) const MAX_DOWNLOAD_BYTES: u64 = 1 << 30;

/// The client for every download: no automatic redirects, rustls.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!("theodore/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Download `artifact` into a temp file under `tmp_dir`, checked by sha256.
/// The file is removed when the returned handle drops.
pub(crate) async fn fetch_verified(
    client: &reqwest::Client,
    artifact: &Artifact,
    tmp_dir: &Path,
) -> CodeIntelResult<NamedTempFile> {
    let first = Url::parse(&artifact.url)
        .map_err(|e| CodeIntelError::Download(format!("{}: {e}", artifact.url)))?;
    let mut allowed: Vec<&str> = artifact.redirect_hosts.iter().map(String::as_str).collect();
    let first_host = first.host_str().unwrap_or_default().to_string();
    allowed.push(&first_host);

    let mut url = first.clone();
    for _ in 0..=MAX_REDIRECTS {
        let mut resp = client
            .get(url.clone())
            .send()
            .await
            .map_err(|e| CodeIntelError::Download(format!("{}: {e}", artifact.url)))?;
        let status = resp.status();
        if status.is_redirection() {
            let next = resp
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|loc| url.join(loc).ok())
                .ok_or_else(|| {
                    CodeIntelError::Download(format!("{}: {status} without Location", artifact.url))
                })?;
            let host = next.host_str().unwrap_or_default();
            if !allowed.contains(&host) || !url_allowed(&next) {
                return Err(CodeIntelError::Download(format!(
                    "{}: redirect to {} refused (allowed: {})",
                    artifact.url,
                    next.origin().ascii_serialization(),
                    allowed.join(", ")
                )));
            }
            url = next;
            continue;
        }
        if !status.is_success() {
            return Err(CodeIntelError::Download(format!(
                "{}: HTTP {status}",
                artifact.url
            )));
        }

        std::fs::create_dir_all(tmp_dir).map_err(|e| io_err(tmp_dir, e))?;
        let tmp = tempfile::Builder::new()
            .prefix(".download-")
            .tempfile_in(tmp_dir)
            .map_err(|e| io_err(tmp_dir, e))?;
        let file = tmp
            .as_file()
            .try_clone()
            .map_err(|e| io_err(tmp.path(), e))?;
        let mut out = tokio::fs::File::from_std(file);
        let limit = artifact
            .size
            .unwrap_or(MAX_DOWNLOAD_BYTES)
            .min(MAX_DOWNLOAD_BYTES);
        let mut hasher = Sha256::new();
        let mut read: u64 = 0;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| CodeIntelError::Download(format!("{}: {e}", artifact.url)))?
        {
            read += chunk.len() as u64;
            if read > limit {
                return Err(CodeIntelError::Download(format!(
                    "{}: larger than {limit} bytes",
                    artifact.url
                )));
            }
            hasher.update(&chunk);
            out.write_all(&chunk)
                .await
                .map_err(|e| io_err(tmp.path(), e))?;
        }
        out.flush().await.map_err(|e| io_err(tmp.path(), e))?;
        drop(out);

        let got = hex::encode(hasher.finalize());
        if !got.eq_ignore_ascii_case(&artifact.sha256) {
            tracing::warn!(
                target: "code_intel",
                url = %artifact.url, expected = %artifact.sha256, got = %got,
                "checksum mismatch"
            );
            return Err(CodeIntelError::Checksum(artifact.url.clone()));
        }
        return Ok(tmp);
    }
    Err(CodeIntelError::Download(format!(
        "{}: more than {MAX_REDIRECTS} redirects",
        artifact.url
    )))
}

/// sha256 of a file on disk, lowercase hex.
pub(crate) fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut f, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn io_err(path: &Path, e: std::io::Error) -> CodeIntelError {
    CodeIntelError::Other(format!("{}: {e}", path.display()))
}
