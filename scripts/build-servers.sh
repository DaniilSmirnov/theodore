#!/usr/bin/env bash
# Builds language servers that have no macOS binary upstream and so are
# built by us (docs/code-intel.md §4.2 "No macOS binary upstream"). Today
# that's one server: gopls (Go, cross-compiles, BSD-licensed).
#
#   go install -trimpath golang.org/x/tools/gopls@<pinned version>
#     (GOOS=darwin GOARCH=arm64 CGO_ENABLED=0, into a throwaway GOBIN)
#   ─▶ tar.gz ─▶ sha256
#   ─▶ release "servers-<content hash of this script>" (prerelease)
#
# No codesign step here, unlike build-grammars.sh: a dylib is dlopen'd
# in-process and the OS checks its signature before loading it, but a
# server runs as a child process, and `code-intel-toolchain` ad-hoc signs
# any Mach-O binary itself right after unpacking it
# (crates/code-intel-toolchain/src/lib.rs `sign_adhoc`, docs/code-intel.md
# §4.2 "Signing"). Pre-signing the archive here would just be redone.
#
# `-trimpath` drops build-machine paths from the binary so CI's build and a
# local one are close to reproducible; they are not guaranteed byte-for-byte
# identical (toolchain version, embedded build id), so the manifest's
# sha256 always comes from whichever build actually gets published — a
# local run here is for checking the pin builds, not for the final hash.
#
# Everything lands in a gitignored output dir (default target/servers/);
# nothing here is committed by this script. .github/workflows/servers.yml
# runs this in CI and publishes the archive as a release asset; a human
# then pastes its sha256 into
# crates/code-intel-toolchain/manifest/languages.yaml's gopls entry.
#
# Bash 3.2 compatible (macOS's shipped /bin/bash), matching build-grammars.sh.
#
# Env overrides (all optional):
#   SERVERS_OUT_DIR        output dir                 (default: target/servers)
#   SERVERS_GOPLS_VERSION  gopls version to build      (default: v0.24.0 —
#                                                        keep in sync with
#                                                        manifest/languages.yaml's
#                                                        gopls `version`, without
#                                                        the leading "v")
#   SERVERS_RELEASE_BASE   release assets base URL    (default:
#                                                       https://github.com/<repo>/releases/download)
#   SERVERS_RELEASE_TAG    the release tag assets are published under
#                                                      (default: servers-<12 hex chars
#                                                       of sha256(this script)> —
#                                                       content-addressed, so a
#                                                       re-run with the same pin
#                                                       reuses the same tag)
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
self="$root/scripts/build-servers.sh"

out_dir="${SERVERS_OUT_DIR:-$root/target/servers}"
work_dir="$out_dir/_work"
gopls_version="${SERVERS_GOPLS_VERSION:-v0.24.0}"

gh_repo="${GITHUB_REPOSITORY:-JeanIsahakyan/theodore}"
release_base="${SERVERS_RELEASE_BASE:-https://github.com/$gh_repo/releases/download}"

if [ -z "${SERVERS_RELEASE_TAG:-}" ]; then
  self_hash=$(shasum -a 256 "$self" | awk '{print substr($1, 1, 12)}')
  release_tag="servers-$self_hash"
else
  release_tag="$SERVERS_RELEASE_TAG"
fi

echo "output dir:    $out_dir"
echo "gopls version: $gopls_version"
echo "release:       $release_base/$release_tag"
echo

mkdir -p "$out_dir" "$work_dir"

if ! command -v go >/dev/null 2>&1; then
  echo "✗ go is not installed; nothing to build" >&2
  exit 1
fi

# --- gopls -----------------------------------------------------------------
echo "=== gopls $gopls_version (darwin-arm64) ==="
gobin="$work_dir/gopls-gobin"
rm -rf "$gobin"
mkdir -p "$gobin"
GOOS=darwin GOARCH=arm64 CGO_ENABLED=0 GOBIN="$gobin" \
  go install -trimpath "golang.org/x/tools/gopls@$gopls_version"

gopls_bin="$gobin/gopls"
if [ ! -f "$gopls_bin" ]; then
  echo "✗ gopls: build did not produce $gopls_bin" >&2
  exit 1
fi

archive_name="gopls-${gopls_version#v}-darwin-arm64.tar.gz"
archive_path="$out_dir/$archive_name"
# `gopls` only, flat inside the archive: the manifest's `bin` is just
# "gopls" (matches ty / rust-analyzer's single-binary archives).
tar -czf "$archive_path" -C "$gobin" gopls

archive_sha=$(shasum -a 256 "$archive_path" | awk '{print $1}')
archive_size=$(python3 -c "import os,sys; print(os.path.getsize(sys.argv[1]))" "$archive_path")
archive_url="$release_base/$release_tag/$archive_name"

echo "  -> $archive_name ($archive_size bytes, sha256 ${archive_sha:0:12}…)"
echo

echo "$release_tag" > "$out_dir/RELEASE_TAG"

cat > "$out_dir/gopls-manifest-snippet.yaml" <<EOF
# Paste into crates/code-intel-toolchain/manifest/languages.yaml's gopls
# server entry (docs/code-intel.md §4.2 "No macOS binary upstream").
  - name: gopls
    version: "${gopls_version#v}"
    detect: [["gopls", "version"]]
    download:
      darwin-arm64:
        url: $archive_url
        sha256: $archive_sha
        size: $archive_size
        format: tar.gz
        bin: gopls
    launch: ["{bin}"]
    workspace_folders: true
EOF

echo "wrote $out_dir/$archive_name"
echo "wrote $out_dir/gopls-manifest-snippet.yaml"
echo "wrote $out_dir/RELEASE_TAG ($release_tag)"
