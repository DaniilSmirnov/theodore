# Releasing

Plan §11.1 (updates) and §11.2 (signing without an Apple Developer ID). The
one-time setup the owner does before the first release, then the branch flow
that makes every build: a push to `staging` publishes a staging build, a
version bump merged to `main` publishes a production release.

## Branch flow: two builds

```
author/feature ──PR──▶ staging ──PR──▶ main
                          │              │
                    staging.yml     release.yml
                          │              │
              <ver>-staging.<run>   <ver>, only if v<ver> is new
                          ▼              ▼
          pre-release channel-staging   release v<ver> (+ tag)
          (assets replaced each run)    else: workflow artifact only
                          ▲              ▲
                staging builds poll   production builds poll
                channel-staging/      releases/latest/
                latest.json           latest.json
```

| Build | Workflow, trigger | `THEODORE_BUILD_ENV` | WorkOS client id (variable) | Updates from |
| --- | --- | --- | --- | --- |
| Staging | `staging.yml`, push to `staging` | `staging` | `THEODORE_WORKOS_CLIENT_ID_STAGING` | `channel-staging` only |
| Production | `release.yml`, push to `main` | `production` | `THEODORE_WORKOS_CLIENT_ID` | `stable` (or `beta`), never `channel-staging` |

Both call `.github/workflows/build.yml` (`workflow_call`), the one build
definition: same toolchain, signing, telemetry variables and assets, so the
two can't drift. `ci.yml` runs on every PR (into `staging`, into `main`, or
stacked) and on pushes to both branches. Both also have a manual **Run
workflow** (`workflow_dispatch`); from any other branch it builds and
publishes nothing.

## One-time: updater signing key (required before any release works)

Done on 2026-10-25: the public key is in `tauri.conf.json`, both secrets are
set, and the private key with its password lives in `~/.tauri/` on the
owner's Mac (`theodore.key`, `theodore.key.password`). Keep a copy of both
in a password manager; losing either means no installed build can update
again. The steps below are how it was done.

Every build, from v0.0.1 onward, carries the updater's public key baked into
`apps/desktop/tauri.conf.json` (`plugins.updater.pubkey`). **This key is
never rotated** — rotating it orphans every installed build, since they can
only verify updates signed by the key they shipped with.

1. Generate the keypair locally (needs `cargo install tauri-cli --version "^2" --locked`):

   ```sh
   cargo tauri signer generate -w ~/.tauri/theodore.key
   ```

   This prints a public key and writes the private key to
   `~/.tauri/theodore.key` (keep it out of git).

2. Paste the **public** key into `apps/desktop/tauri.conf.json`, replacing
   `REPLACE_WITH_UPDATER_PUBLIC_KEY`:

   ```json
   "plugins": { "updater": { "pubkey": "<paste public key here>" } }
   ```

   Commit that change.

3. In the GitHub repo → Settings → Secrets and variables → Actions, add:
   - `TAURI_SIGNING_PRIVATE_KEY` — contents of `~/.tauri/theodore.key`.
   - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — the password you set in step 1
     (or an empty secret if you generated without one).

Without these two secrets, `.github/workflows/build.yml` fails at the
"build app and updater artifacts" step — the updater's pubkey is already
configured, so the bundler insists on a matching private key to sign against.

## Theodore account: the client id

Signing in to a Theodore account is required wherever the account is
configured (plan §6.12). A release build carries the WorkOS client id
compiled in, from `THEODORE_WORKOS_CLIENT_ID` in the build's environment:

- In the GitHub repo → Settings → Secrets and variables → Actions →
  **Variables**, add `THEODORE_WORKOS_CLIENT_ID` = `client_…` (the
  production environment's client id). It's a public value, so a variable,
  not a secret; it is never committed. `release.yml` passes it to the build;
  `staging.yml` passes `THEODORE_WORKOS_CLIENT_ID_STAGING` instead.
- Without it the release builds and runs with no account at all: no
  sign-in screen, no account card.
- At run time the same env var or the `account.workos.client_id` setting
  still wins over the compiled-in id (staging, a test environment).

## Error reports: New Relic

Error reports go over OTLP/HTTP to New Relic, set at build time and sent
only when the user opts in (`docs/telemetry.md`):

| Where | Name | Value |
| --- | --- | --- |
| Variables | `THEODORE_OTLP_ENDPOINT` | `https://otlp.nr-data.net` (EU account: `https://otlp.eu01.nr-data.net`) |
| Secrets | `THEODORE_OTLP_HEADERS` | `api-key=<New Relic ingest license key>` |

Both are set in the repo. The key ships inside the app, so anyone can pull
it out and send data to the account: it can only ingest, never read. Rotate
it in New Relic if it is abused. A local nightly bakes them in only when
they are exported in the shell that runs `scripts/nightly-build.sh`.

## Optional: Developer ID signing + notarization

Without this, every release is **ad-hoc signed** (`tauri.conf.json`'s
`bundle.macOS.signingIdentity: "-"`, equivalent to `codesign --force --deep -s -`).
That's enough to run on Apple Silicon, but Gatekeeper still blocks the first
launch (see below) and updates install into an unnotarized app.

To turn on real signing, add these secrets — `build.yml` picks them up
automatically (`tauri-bundler` reads them directly) and the ad-hoc path is
skipped:

| Secret | What |
| --- | --- |
| `APPLE_CERTIFICATE` | Developer ID Application cert, exported as `.p12`, base64-encoded (`base64 -i cert.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | password used when exporting the `.p12` |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | Apple ID used for `notarytool` |
| `APPLE_PASSWORD` | an [app-specific password](https://support.apple.com/en-us/102654) for that Apple ID (not the account password) |
| `APPLE_TEAM_ID` | your Apple Developer team id |

No other change: the workflow and `tauri.conf.json` are unchanged whether or
not these are set.

## Cutting a production release

Bump the version and let it travel the branch flow; `main` makes the tag:

```sh
git switch -c author/release-0.3.0 staging
scripts/bump-version.sh 0.3.0   # Cargo.toml + tauri.conf.json
git commit -am "chore: bump version to 0.3.0"
git push -u origin HEAD          # PR into staging, merge
                                 # then PR staging → main, merge
```

Every push to `main` runs `.github/workflows/release.yml`: `build.yml`
checks both version files agree, builds `aarch64-apple-darwin`, signs,
packages the DMG and the updater artifacts and writes `latest.json`. Then:

- **No `v0.3.0` tag yet** → it creates the tag at that commit and a GitHub
  release with the DMG, the `.app.tar.gz` + `.sig`, and `latest.json`.
  Installed production builds pick it up from `releases/latest`.
- **`v0.3.0` already exists** (a merge without a bump) → build only: the
  assets stay as a workflow artifact on the run, nothing is published.

There is no tag-push release any more: don't push `v*` tags by hand (a
hand-pushed tag only makes the next merge skip that version).

Merges: a feature PR goes into `staging` as a squash (one commit per
change); `staging → main` is a merge commit, so `main` keeps each squashed
change. The repository rules enforce both.

Release notes: `release.yml` publishes with GitHub's generated notes (a
list of PR titles). Replace them with user-facing notes once the release is
up: `gh release edit v0.3.0 --notes-file notes.md` — what got faster, what
got fixed, what to know.

- **Never re-tag a published release.** Ship a new patch instead.

### A hotfix

Same path, just quick: a fix plus a patch bump, from `main`.

```
git switch -c author/fix-x origin/main   # staging must already contain main
fix, scripts/bump-version.sh 0.1.1, commit ──▶ PR into staging ──▶ staging build
PR staging → main ──▶ release v0.1.1 ──▶ installed apps update on next launch / check
```

When `staging` equals `main` (nothing else in flight), both merges are
fast-forwards; otherwise the PR into `main` carries whatever else `staging`
holds, so ship that too or wait.

## Staging channel

Every push to `staging` runs `.github/workflows/staging.yml`, which builds
with `THEODORE_BUILD_ENV=staging` and the staging client id, as version
`<version files>-staging.<run number>` (e.g. `0.3.0-staging.57`; the run
number only grows, so each build is newer to the updater). It publishes to
one rolling **pre-release tagged `channel-staging`**, made on the first run:

1. uploads the versioned DMG, `.app.tar.gz` and `.sig`,
2. then `latest.json`, pointing at that tarball,
3. then deletes the previous build's assets and names this build's commit
   in the release notes.

An updater polling mid-run sees either the old manifest with its tarball or
the new one with its. A re-run of an older run publishes nothing. Being a pre-release, `channel-staging` is never
`releases/latest`, and no `v*` tag or release is touched.

A staging build polls only
`.../releases/download/channel-staging/latest.json`, whatever
`updates.channel` says, and Settings → Updates shows **Staging** instead of
the channel picker. Any other build resolves `updates.channel` to `stable`
or `beta` and never to staging (`effective_channel` in
`apps/desktop/src/updater.rs`); a nightly polls nothing (below).

- **Beta channel**: `updates.channel` = `"beta"` resolves to
  `.../releases/download/channel-beta/latest.json` instead of
  `.../releases/latest/download/latest.json` — publish a pre-release tagged
  exactly `channel-beta` and overwrite its `latest.json` on each beta build.
- **Never re-tag a published release.** Ship a new patch instead.

## Local nightly builds

`scripts/nightly-build.sh` builds a DMG from the current commit, numbered by
it: `<version>-nightly.<commits on HEAD>` (e.g. `0.0.1-nightly.340`), file
`target/release/bundle/dmg/Theodore_Nightly_<version>_<sha>_aarch64.dmg`,
holding `Theodore Nightly.app` (its own app and data, see "One identity
per build" below). The number only grows, one per commit. Nothing is
tagged or published, and the version files keep the release version; a
dirty tree is refused, so every nightly maps to exactly one commit.

A nightly doesn't update itself: no feed publishes one, and the stable feed
would replace it with a production app (`self_updates` in
`apps/desktop/src/updater.rs`). So it checks nothing, Settings → Updates says
so, and it doesn't offer the move to Applications. Build a new one to move ahead.

It bakes in the account's client id like a release, so it needs
`THEODORE_WORKOS_CLIENT_ID` set and stops with a message when it isn't:

```sh
THEODORE_WORKOS_CLIENT_ID=client_… scripts/nightly-build.sh
scripts/nightly-build.sh --no-account   # no account, no sign-in
```

## Build stamp: version, commit, environment

Every build bakes in its version, commit and build environment
(`apps/desktop/build.rs`); Settings → About shows them as
`0.0.1 · a1b2c3d · staging` (full sha and build date on hover, **Copy**
for bug reports), and the problem report and telemetry carry them too.

| Build | `THEODORE_BUILD_ENV` | Set by |
| --- | --- | --- |
| Production (`main`) | `production` | `release.yml` → `build.yml`, with `THEODORE_GIT_SHA=${{ github.sha }}` |
| Staging (`staging`) | `staging` | `staging.yml` → `build.yml`; or by hand, below |
| Nightly | `nightly` | `scripts/nightly-build.sh` (default) |
| Local `cargo` / `cargo tauri dev` | `dev` | nothing set |

The commit is `THEODORE_GIT_SHA` when set, else `git rev-parse HEAD`, else
`unknown`; a `dev` build from uncommitted changes says `-dirty`. Any other
`THEODORE_BUILD_ENV` value fails the build. `THEODORE_VERSION` overrides
the version (the nightly script sets `-nightly.N`, `build.yml` the version
with its suffix).

The account client id, the GitHub OAuth client id and the telemetry keys
are compiled in only by `build.yml` (from repository variables and
secrets). So `build.rs` refuses a `production` or `staging` build without
`THEODORE_WORKOS_CLIENT_ID`, `THEODORE_OTLP_ENDPOINT`,
`THEODORE_OTLP_HEADERS` and `THEODORE_POSTHOG_KEY` (a nightly needs the
client id only), and says what to do instead. `THEODORE_ALLOW_INCOMPLETE_BUILD=1`
builds it anyway with an `-incomplete` commit, which About and problem
reports show. Such an app has no account sign-in, no "Sign in with GitHub"
and no diagnostics: never distribute it or use it in place of a release;
install the release DMG.

### A local staging build

The CI staging build is the usual one (above). For a local one: the
nightly script with the staging environment and the staging WorkOS
client id (the repository variable `THEODORE_WORKOS_CLIENT_ID_STAGING`):

```sh
THEODORE_BUILD_ENV=staging \
THEODORE_WORKOS_CLIENT_ID=$(gh variable get THEODORE_WORKOS_CLIENT_ID_STAGING) \
scripts/nightly-build.sh
```

Same DMG as a nightly (`<version>-nightly.<n>`), labelled `staging` in
About and in telemetry (`deployment.environment.name`). Like any staging
build it updates from `channel-staging` only, so the next CI staging build
of the same version (`-staging.N` sorts after `-nightly.N`) replaces it.
It is `Theodore Staging.app`, the same app as the CI staging build.

## One identity per build

Every build environment is its own app: it installs and runs beside
production and never reads or writes production's data. A dev build
applying a new migration to production's database is what this prevents
(production then fails with "downgrade is not supported").

| | production | staging | nightly | dev |
| --- | --- | --- | --- | --- |
| App (`productName`) | `Theodore` | `Theodore Staging` | `Theodore Nightly` | not bundled (window: `Theodore Dev`) |
| Bundle id | `app.theodore.desktop` | `app.theodore.desktop.staging` | `app.theodore.desktop.nightly` | (`app.theodore.desktop.dev`, unused) |
| Data dir | `~/.theodore` | `~/.theodore-staging` | `~/.theodore-nightly` | `~/.theodore-dev` |
| Keychain item (service; account `vault`) | `app.theodore.secrets` | `app.theodore.secrets.staging` | `app.theodore.secrets.nightly` | `app.theodore.secrets.dev` |
| URL scheme | `theodore` | `theodore-staging` | `theodore-nightly` | none |
| Sign-in redirect (bundle) | `theodore://callback` | `theodore-staging://callback` | `theodore-nightly://callback` | `http://127.0.0.1:47823/callback` |

The data dir holds everything else per build: `theodore.db`, `repos/`,
`wt/`, `spaces/`, `config.yaml`, `logs/` and the telemetry install id.

```
THEODORE_BUILD_ENV ──▶ build.rs bakes it ──▶ identity.rs (crates/runtime)
                                               data dir, keychain item,
                                               URL scheme, sign-in redirect
                   ──▶ build.yml / nightly-build.sh ──▶ cargo tauri build
                         --config tauri.conf.json ⊕ tauri.<env>.conf.json
                         (productName, identifier, deep-link scheme)
```

- Production builds with `tauri.conf.json` as is. Staging and nightly
  merge `apps/desktop/tauri.staging.conf.json` /
  `tauri.nightly.conf.json` over it in the one `--config` the build
  passes. A desktop test checks those files against `identity.rs`.
- `THEODORE_DATA_DIR` still wins over the build's data dir, in every
  build. The sandbox keeps `~/.theodore-sandbox` and refuses every build's
  data dir. Fixture runs keep everything in memory.
- The CLI (`theodore`) has no build env: it keeps `~/.theodore` and
  production's keychain item.
- **No data is copied or migrated between builds.** The first launch of a
  staging, nightly or dev build starts empty: onboarding, sign-in, GitHub
  token, projects, all from scratch.
- A dev build run bare (`cargo tauri dev`, `cargo run`) uses the loopback
  redirect. A dev build bundled by hand (`cargo tauri build` without
  `THEODORE_BUILD_ENV`) still carries production's bundle id and scheme
  from `tauri.conf.json`, but keeps `~/.theodore-dev`, the `.dev` keychain
  item and the loopback; build an installable copy with
  `scripts/nightly-build.sh` instead.
- A staging build installed before this change updates in place: it keeps
  its old location (e.g. `/Applications/Theodore.app`) but starts empty
  in `~/.theodore-staging`. Install `Theodore Staging.app` from the
  staging DMG once and delete the old copy.

### WorkOS redirects to register

Each environment's WorkOS application needs the redirects of the builds
that sign in to it (plan §6.12), else sign-in fails on the AuthKit page
with an invalid redirect error:

| WorkOS environment | Redirects |
| --- | --- |
| Production (`THEODORE_WORKOS_CLIENT_ID`) | `theodore://callback`, `http://127.0.0.1:47823/callback` |
| Staging (`THEODORE_WORKOS_CLIENT_ID_STAGING`) | `theodore-staging://callback`, `theodore-nightly://callback`, `http://127.0.0.1:47823/callback` |

A nightly bakes in whichever client id the shell exported; register
`theodore-nightly://callback` on the environment it uses.

## Build colours

Each build's logo has its own tile colour, so builds are told apart at a
glance, in the app and on the Dock:

| Build env | Tile | Blobs | Lens shadow |
| --- | --- | --- | --- |
| `production` | blue `#3F86D6` | `#0C5BC0` | `#03306B` |
| `staging` | orange `#DE8435` | `#C05A0C` | `#6D2D03` |
| `nightly` | purple `#8C56D2` | `#6120B6` | `#320868` |
| `dev` | grey-green `#669980` | `#406D57` | `#183929` |

```
in the app   AppInfo.build_env ──▶ logoColors() ──▶ <Logo>, <WelcomeLogo>
                                   (production until app_info answers)

on the Dock  icons/src/icon.svg ──▶ scripts/icon-variant.sh <env> <dir>
                                     recolour + cargo tauri icon
                                       │
             build.yml (staging),      ▼
             nightly-build.sh ──▶ cargo tauri build --config
                                  '{"bundle":{"icon":[<dir>/32x32.png, …]}}'
```

- The colours live in two places that must agree:
  `ui/src/ui/logoColors.ts` and `scripts/icon-variant.sh`.
- Production builds use the committed `apps/desktop/icons`; variants are
  generated at build time into `target/icons/<env>`, never committed.
- `cargo tauri dev` keeps the blue Dock icon (only the in-app logo is
  grey-green).

## DMG window

The DMG opens as an installer: a 660 x 400 window, the app on the left,
Applications on the right, 128 px icons, an arrow between them. Staging
and nightly DMGs get the same window.

```
icons/src/dmg-background.svg ──▶ scripts/dmg-background.sh
                                   ──▶ icons/dmg-background.png    660 x 400
                                   ──▶ icons/dmg-background@2x.png 1320 x 800

.app ──▶ scripts/build-dmg.sh <app> <out.dmg> <volume name>
           throwaway venv, dmgbuild (pinned)
           scripts/dmg-settings.py ◀── tauri.conf.json bundle.macOS.dmg
           ──▶ .dmg: app + Applications link, .DS_Store written directly,
               both PNGs joined into one HiDPI .background.tiff
           ──▶ hdiutil verify
```

| Who | Builds the `.app` | Then |
| --- | --- | --- |
| `build.yml` (staging, production) | `cargo tauri build --bundles app` (+ updater artifacts) | `build-dmg.sh`, then Developer ID `codesign` of the DMG when that secret is set |
| `scripts/nightly-build.sh` | `cargo tauri build --bundles app` | `build-dmg.sh` |

- dmgbuild writes the window's `.DS_Store` itself: no Finder, no
  AppleScript, no automation permission. tauri-bundler's own DMG is not
  used: under `CI=true` it skips its Finder layout, and on macOS 26 that
  layout drops the background and icon size.
- `tauri.conf.json` `bundle.macOS.dmg` stays the one place the window size,
  icon positions and background are set; `dmg-settings.py` reads it.
- The background is transparent with no text; Finder paints white under it,
  in light and dark mode alike. The title bar takes ~32 pt of the 400, so
  the visible area is 660 x 368.
- `build-dmg.sh` needs python >= 3.10 (`PYTHON` picks another);
  `build.yml` pins 3.13 with `actions/setup-python`.
- Asset names are unchanged: `Theodore_<version>_aarch64.dmg`, volume name
  = product name (`Theodore`, `Theodore Staging`).

## First launch on a clean Mac (ad-hoc signed builds)

Without a Developer ID, Gatekeeper blocks the first launch of a downloaded
`.app` (it does not block the updater's own installs — see plan §11.2). Either:

- System Settings → Privacy & Security → scroll down → "Open Anyway" next to
  the blocked-app notice, then open Theodore.app again and confirm, **or**
- `xattr -dr com.apple.quarantine /Applications/Theodore.app` (or
  `Theodore Staging.app`, `Theodore Nightly.app`)

After that first launch, the app updates itself normally — downloads happen
outside the browser, so no quarantine flag is ever attached, and the updater
verifies its own signature rather than relying on Gatekeeper.

The updater replaces the bundle in place, so it can't update an app run from
the disk image or from a copy macOS translocated (a quarantined app opened
where it was downloaded): both are read-only. The first launch of a production
or staging build outside an Applications folder asks once
(`apps/desktop/src/relocate.rs`):

```
launch, not in /Applications or ~/Applications, never asked
  └─▶ "Move Theodore to Applications?"
        Not Now ──▶ never asked again (marker in the data dir)
        Move    ──▶ ditto → /Applications (~/Applications if not writable)
                    an existing copy there → Trash; quarantine flag stripped
                    original → Trash (a plain folder only)
                    quit ──▶ reopen from Applications ──▶ eject the image
```

Declined, the updater skips the download and says why ("move it to
Applications to get updates") instead of failing on every check.
