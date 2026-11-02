#!/usr/bin/env bash
# Builds one dylib per tree-sitter grammar package pinned in grammars.toml
# (docs/code-intel.md §4.1), compiled for this Mac's arch, and the generated
# manifest fragment (grammars.json) that lists them with their download
# URLs and checksums.
#
#   grammars.toml ─▶ shallow fetch at rev ─▶ cc parser.c (+scanner.c) ─▶
#     codesign ─▶ sha256 ─▶ <package>-<version>.dylib
#                         + <package>-<version>-<lang>.tags.scm
#   ─▶ grammars.json (generated; crates/code-intel-toolchain/manifest/)
#
# Everything lands in a gitignored output dir (default target/grammars/);
# nothing here is committed by this script. .github/workflows/grammars.yml
# runs this in CI and publishes the dylibs + queries as release assets;
# grammars.json is committed by hand in a follow-up PR once that release
# exists.
#
# The shipped app is macOS arm64 only today (build.yml targets
# aarch64-apple-darwin); GRAMMARS_TARGETS below is the one line to touch to
# add x86_64 (lipo'd into a universal dylib automatically once there is more
# than one target).
#
# Bash 3.2 compatible (macOS's shipped /bin/bash): indexed arrays only, no
# associative arrays, no mapfile/readarray. grammars.toml is parsed by a
# tiny inline python3 snippet (python3 is on every macOS runner) rather
# than a shell TOML parser.
#
# Env overrides (all optional):
#   GRAMMARS_OUT_DIR       output dir                 (default: target/grammars)
#   GRAMMARS_TARGETS       space-separated clang -arch values
#                                                      (default: arm64)
#   GRAMMARS_MACOS_MIN     -mmacosx-version-min       (default: from
#                                                       apps/desktop/tauri.conf.json,
#                                                       else 11.0)
#   GRAMMARS_RELEASE_BASE  release assets base URL    (default:
#                                                       https://github.com/<repo>/releases/download)
#   GRAMMARS_RELEASE_TAG   the release tag assets are published under
#                                                      (default: grammars-<12 hex chars
#                                                       of sha256(grammars.toml)> —
#                                                       content-addressed, so re-running
#                                                       on an unchanged pin reuses the
#                                                       same tag)
#   CODESIGN_IDENTITY      a Developer ID to sign dylibs with
#                          (--options runtime); unset: ad-hoc `codesign -s -`
#   CODESIGN_KEYCHAIN      keychain to find that identity in (`codesign
#                          --keychain`), when it's not on the default
#                          search list — e.g. a throwaway keychain a CI job
#                          imported a certificate into. Ignored without
#                          CODESIGN_IDENTITY.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
toml="$root/grammars.toml"

out_dir="${GRAMMARS_OUT_DIR:-$root/target/grammars}"
work_dir="$out_dir/_work"
checkout_dir="$work_dir/checkout"
manifest_out="$out_dir/grammars.json"

targets="${GRAMMARS_TARGETS:-arm64}"

if [ -z "${GRAMMARS_MACOS_MIN:-}" ]; then
  tauri_conf="$root/apps/desktop/tauri.conf.json"
  if [ -f "$tauri_conf" ]; then
    macos_min=$(python3 -c "
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
print(d.get('bundle', {}).get('macOS', {}).get('minimumSystemVersion', '11.0'))
" "$tauri_conf")
  else
    macos_min="11.0"
  fi
else
  macos_min="$GRAMMARS_MACOS_MIN"
fi

gh_repo="${GITHUB_REPOSITORY:-JeanIsahakyan/theodore}"
release_base="${GRAMMARS_RELEASE_BASE:-https://github.com/$gh_repo/releases/download}"

if [ -z "${GRAMMARS_RELEASE_TAG:-}" ]; then
  toml_hash=$(shasum -a 256 "$toml" | awk '{print substr($1, 1, 12)}')
  release_tag="grammars-$toml_hash"
else
  release_tag="$GRAMMARS_RELEASE_TAG"
fi

echo "output dir:    $out_dir"
echo "targets:       $targets"
echo "macOS min:     $macos_min"
echo "release:       $release_base/$release_tag"
echo

mkdir -p "$out_dir" "$work_dir" "$checkout_dir"

# --- 1. flatten grammars.toml into one JSON object per line (ndjson). ----
# A tiny hand-rolled parser: the file only ever uses `[[grammar]]`,
# `[[grammar.languages]]` and `key = "value"` lines, so a full TOML library
# is not needed (and not guaranteed present on every python3).
plan_file="$work_dir/plan.ndjson"
python3 - "$toml" > "$plan_file" <<'PY'
import json
import re
import sys

path = sys.argv[1]
grammars = []
cur = None
cur_lang = None
section = None

kv_re = re.compile(r'^(\w+)\s*=\s*"(.*)"\s*$')

with open(path) as f:
    for raw in f:
        line = raw.strip()
        if not line or line.startswith('#'):
            continue
        if line == '[[grammar]]':
            if cur is not None:
                if cur_lang is not None:
                    cur['languages'].append(cur_lang)
                    cur_lang = None
                grammars.append(cur)
            cur = {'languages': []}
            section = 'grammar'
            continue
        if line == '[[grammar.languages]]':
            if cur_lang is not None:
                cur['languages'].append(cur_lang)
            cur_lang = {}
            section = 'languages'
            continue
        m = kv_re.match(line)
        if not m:
            raise SystemExit(f"grammars.toml: can't parse line: {line!r}")
        key, val = m.group(1), m.group(2)
        if section == 'languages':
            cur_lang[key] = val
        else:
            cur[key] = val

if cur is not None:
    if cur_lang is not None:
        cur['languages'].append(cur_lang)
    grammars.append(cur)

for g in grammars:
    g.setdefault('tags_query', None)
    print(json.dumps(g))
PY

# --- 2. one package at a time: fetch, compile, link, sign, checksum. -----
languages_manifest="$work_dir/languages.ndjson"
: > "$languages_manifest"

while IFS= read -r grammar_json; do
  package=$(python3 -c "import json,sys; print(json.loads(sys.argv[1])['package'])" "$grammar_json")
  repo=$(python3 -c "import json,sys; print(json.loads(sys.argv[1])['repo'])" "$grammar_json")
  tag=$(python3 -c "import json,sys; print(json.loads(sys.argv[1])['tag'])" "$grammar_json")
  rev=$(python3 -c "import json,sys; print(json.loads(sys.argv[1])['rev'])" "$grammar_json")
  version=$(python3 -c "import json,sys; print(json.loads(sys.argv[1])['version'])" "$grammar_json")
  tags_query_rel=$(python3 -c "import json,sys; v=json.loads(sys.argv[1])['tags_query']; print(v if v else '')" "$grammar_json")
  n_langs=$(python3 -c "import json,sys; print(len(json.loads(sys.argv[1])['languages']))" "$grammar_json")

  echo "=== $package $version (rev ${rev:0:12}) ==="

  src_checkout="$checkout_dir/$package"
  need_clone=1
  if [ -d "$src_checkout/.git" ]; then
    have_rev=$(git -C "$src_checkout" rev-parse HEAD 2>/dev/null || echo "")
    if [ "$have_rev" = "$rev" ]; then
      need_clone=0
      echo "  already fetched at $rev"
    fi
  fi
  if [ "$need_clone" = 1 ]; then
    rm -rf "$src_checkout"
    echo "  fetching $repo @ $tag"
    git clone --quiet --depth 1 --branch "$tag" "$repo" "$src_checkout"
  fi
  have_rev=$(git -C "$src_checkout" rev-parse HEAD)
  if [ "$have_rev" != "$rev" ]; then
    echo "✗ $package: tag $tag now points at $have_rev, pinned rev is $rev (grammars.toml is out of date or the tag moved upstream)" >&2
    exit 1
  fi

  pkg_work="$work_dir/$package"
  mkdir -p "$pkg_work"

  # Resolve the tags query once per package (shared by its languages).
  tags_src=""
  if [ -n "$tags_query_rel" ] && [ -f "$src_checkout/$tags_query_rel" ]; then
    tags_src="$src_checkout/$tags_query_rel"
  fi

  lang_idx=0
  obj_files=()
  while [ "$lang_idx" -lt "$n_langs" ]; do
    lang_id=$(python3 -c "import json,sys; print(json.loads(sys.argv[1])['languages'][int(sys.argv[2])]['id'])" "$grammar_json" "$lang_idx")
    lang_symbol=$(python3 -c "import json,sys; print(json.loads(sys.argv[1])['languages'][int(sys.argv[2])]['symbol'])" "$grammar_json" "$lang_idx")
    lang_src=$(python3 -c "import json,sys; print(json.loads(sys.argv[1])['languages'][int(sys.argv[2])]['src'])" "$grammar_json" "$lang_idx")

    src_dir="$src_checkout/$lang_src"
    if [ ! -f "$src_dir/parser.c" ]; then
      echo "✗ $package/$lang_id: no $lang_src/parser.c in $repo @ $tag (generated parser not committed upstream — see grammars.toml's comment for this package)" >&2
      exit 1
    fi

    echo "  compiling $lang_id ($lang_symbol)"
    parser_obj="$pkg_work/$lang_id-parser.o"
    cc -std=c11 -O2 -fPIC -mmacosx-version-min="$macos_min" \
      -I "$src_dir" -c "$src_dir/parser.c" -o "$parser_obj"
    obj_files+=("$parser_obj")

    if [ -f "$src_dir/scanner.c" ]; then
      scanner_obj="$pkg_work/$lang_id-scanner.o"
      cc -std=c11 -O2 -fPIC -mmacosx-version-min="$macos_min" \
        -I "$src_dir" -c "$src_dir/scanner.c" -o "$scanner_obj"
      obj_files+=("$scanner_obj")
    fi

    # Per-language tags query, falling back to our own hand-written one
    # when the grammar ships none (listed in grammars.toml's comments).
    lang_tags_src="$tags_src"
    if [ -z "$lang_tags_src" ]; then
      own_tags="$root/crates/code-intel-toolchain/queries/$lang_id-tags.scm"
      if [ -f "$own_tags" ]; then
        lang_tags_src="$own_tags"
      fi
    fi

    tags_entry="null"
    if [ -n "$lang_tags_src" ]; then
      tags_name="$package-$version-$lang_id.tags.scm"
      cp "$lang_tags_src" "$out_dir/$tags_name"
      tags_sha=$(shasum -a 256 "$out_dir/$tags_name" | awk '{print $1}')
      tags_url="$release_base/$release_tag/$tags_name"
      tags_entry=$(python3 -c "
import json, sys
print(json.dumps({'url': sys.argv[1], 'sha256': sys.argv[2]}))
" "$tags_url" "$tags_sha")
    else
      echo "  note: $lang_id has no tags query (no upstream tags.scm, no crates/code-intel-toolchain/queries/$lang_id-tags.scm yet)"
    fi

    echo "$lang_id|$lang_symbol|$package|$version|$tags_entry" >> "$languages_manifest.$package.$lang_id.pending"
    lang_idx=$((lang_idx + 1))
  done

  # --- link: one arch at a time, lipo together if there's more than one. -
  dylib_name="$package-$version.dylib"
  per_arch_dylibs=()
  # shellcheck disable=SC2086 # $targets is intentionally word-split: "arm64" or "arm64 x86_64"
  set -- $targets
  n_targets=$#
  for arch in "$@"; do
    arch_dylib="$pkg_work/$package.$arch.dylib"
    cc -dynamiclib -arch "$arch" -mmacosx-version-min="$macos_min" \
      -o "$arch_dylib" "${obj_files[@]}"
    per_arch_dylibs+=("$arch_dylib")
  done

  final_dylib="$out_dir/$dylib_name"
  if [ "$n_targets" -gt 1 ]; then
    echo "  lipo: $targets"
    lipo -create "${per_arch_dylibs[@]}" -output "$final_dylib"
  else
    cp "${per_arch_dylibs[0]}" "$final_dylib"
  fi

  if [ -n "${CODESIGN_IDENTITY:-}" ]; then
    if [ -n "${CODESIGN_KEYCHAIN:-}" ]; then
      codesign --force --keychain "$CODESIGN_KEYCHAIN" --sign "$CODESIGN_IDENTITY" --options runtime "$final_dylib"
    else
      codesign --force --sign "$CODESIGN_IDENTITY" --options runtime "$final_dylib"
    fi
  else
    codesign --force -s - "$final_dylib"
  fi

  dylib_sha=$(shasum -a 256 "$final_dylib" | awk '{print $1}')
  dylib_size=$(python3 -c "import os,sys; print(os.path.getsize(sys.argv[1]))" "$final_dylib")
  dylib_url="$release_base/$release_tag/$dylib_name"
  downloads=$(python3 -c "
import json, sys
print(json.dumps({'darwin-arm64': {'url': sys.argv[1], 'sha256': sys.argv[2], 'size': int(sys.argv[3])}}))
" "$dylib_url" "$dylib_sha" "$dylib_size")

  # Now that the dylib's checksum is known, finish each language's entry.
  lang_idx=0
  while [ "$lang_idx" -lt "$n_langs" ]; do
    lang_id=$(python3 -c "import json,sys; print(json.loads(sys.argv[1])['languages'][int(sys.argv[2])]['id'])" "$grammar_json" "$lang_idx")
    pending="$languages_manifest.$package.$lang_id.pending"
    IFS='|' read -r p_id p_symbol p_package p_version p_tags < "$pending"
    rm -f "$pending"
    python3 -c "
import json, sys
entry = {
    'id': sys.argv[1],
    'package': sys.argv[2],
    'version': sys.argv[3],
    'symbol': sys.argv[4],
    'downloads': json.loads(sys.argv[5]),
    'tags_query': json.loads(sys.argv[6]),
}
print(json.dumps(entry))
" "$p_id" "$p_package" "$p_version" "$p_symbol" "$downloads" "$p_tags" >> "$languages_manifest"
    lang_idx=$((lang_idx + 1))
  done

  echo "  -> $dylib_name ($dylib_size bytes, sha256 ${dylib_sha:0:12}…)"
  echo
done < "$plan_file"

# --- 3. assemble grammars.json -------------------------------------------
# Schema 1 of crates/code-intel-toolchain/src/manifest.rs: one entry per
# package, its languages inside. A language without a tags query has
# nothing to index, so it is left out.
python3 - "$languages_manifest" "$release_tag" "$manifest_out" <<'PY'
import json, sys

src, release_tag, out = sys.argv[1:4]
redirect = ["release-assets.githubusercontent.com", "objects.githubusercontent.com"]
packages = {}
with open(src) as f:
    for line in f:
        line = line.strip()
        if not line:
            continue
        lang = json.loads(line)
        if not lang["tags_query"]:
            print(f"  skip {lang['id']}: no tags query", file=sys.stderr)
            continue
        pkg = packages.setdefault(lang["package"], {
            "package": lang["package"],
            "version": lang["version"],
            "languages": [],
            "downloads": {
                plat: dict(a, redirect_hosts=redirect)
                for plat, a in lang["downloads"].items()
            },
        })
        pkg["languages"].append({
            "id": lang["id"],
            "symbol": lang["symbol"],
            "tags_query": dict(lang["tags_query"], redirect_hosts=redirect),
        })

grammars = sorted(packages.values(), key=lambda g: g["package"])
for g in grammars:
    g["languages"].sort(key=lambda l: l["id"])
manifest = {"schema": 1, "release_tag": release_tag, "grammars": grammars}
with open(out, "w") as f:
    json.dump(manifest, f, indent=2)
    f.write("\n")
PY

echo "wrote $manifest_out"
echo "wrote $(find "$out_dir" -maxdepth 1 -name '*.dylib' | wc -l | tr -d ' ') dylib(s) and $(find "$out_dir" -maxdepth 1 -name '*.tags.scm' | wc -l | tr -d ' ') tags query/queries to $out_dir"
