# tinytrac

A tiny, native time tracker for work. No browser engine or required account.

- **Tags**: track time against anything: clients, projects, categories.
- **Several timers**: clicking a timer pauses the running one; ctrl+click runs both.
- **Break bank** (optional): work earns break time (5 min per 25 by default).
  Breaks are logged while you have credit; going over is deducted unless later
  work pays it back.
- **System tray**: start or pause timers, pause all, and keep tracking with the window closed.
- **Totals**: today and past days at a glance; totals per tag for any range.
- **Sync** across your own devices through Google Drive or a shared folder such as Syncthing.

> Status: early development. Tracking, editing, reports, the tray, Google Drive and folder sync work.

## Install

Download a package from [Releases](https://github.com/akuratnik/tinytrac/releases).

**macOS 13 or newer (Apple Silicon and Intel):** open the DMG, drag **tinytrac**
to **Applications**, then open it. Releases use a stable self-signed identity and
are not notarized. On first launch, go to **System Settings → Privacy & Security →
Open Anyway**, confirm, and open it again. This is a one-time step. Add tinytrac
under **System Settings → General → Login Items** to start at login. The app lives
in the menu bar; use its tray menu to reopen the window.

**Linux x86_64 (Ubuntu 24.04 or compatible, glibc 2.39+):** extract the tarball,
then run these commands from the extracted directory:

```sh
install -Dm755 tinytrac "$HOME/.local/bin/tinytrac"
install -Dm644 tinytrac.desktop "$HOME/.local/share/applications/tinytrac.desktop"
install -Dm644 icon.png "$HOME/.local/share/icons/tinytrac.png"
```

Ensure `~/.local/bin` is in your desktop session's `PATH`; log out and back in
if you just created it. Launch from the application menu or `~/.local/bin/tinytrac`.
See the included `INSTALL.txt` for runtime requirements. GNOME needs the
**AppIndicator** extension for the tray. Google sync needs GNOME Keyring or another
Secret Service and `xdg-open` for browser sign-in.

## Build from source

Requires Rust 1.90 or newer. GNOME needs the AppIndicator extension for the tray; without it, closing the window exits.
On GNOME, single-click opens the tray menu; double-click or **Show tinytrac** reopens the window.
On macOS, reopen a closed window from the tray.

```sh
cargo run --release            # data lives in ~/.local/share/tinytrac on Linux
TINYTRAC_DB=/tmp/demo.db cargo run   # try it on a throwaway database
cargo test                     # domain, storage and headless UI tests
```

### macOS app

On a Mac with Rust and Xcode Command Line Tools installed, run from the repo root:

```sh
scripts/bundle-macos.sh --sign "tinytrac" --zip
# Add --universal to build for both Apple Silicon and Intel Macs.
```

For a stable signing identity, run `scripts/make-signing-identity.sh` once (see
[Releasing](#releasing)) and import its `tinytrac.p12` into your login keychain
using Keychain Access and the generated password. Keep using the same certificate
for updates so Keychain can preserve access to saved Google tokens. Omit `--sign`
for ad-hoc signing; those builds may prompt for Keychain access after each update.

The script uses `TINYTRAC_GOOGLE_CLIENT_ID` and `TINYTRAC_GOOGLE_CLIENT_SECRET`
from the environment when both are nonempty; they take precedence over JSON.
A partial pair is an error. Otherwise it reads `~/.config/tinytrac/google-client.json`
(override with `TINYTRAC_GOOGLE_CLIENT`) using macOS's built-in `plutil`.
Without either source it builds without Google sync.
The minimum macOS version defaults to 13.0; override
with `MACOSX_DEPLOYMENT_TARGET`. Universal builds require both Rust targets:
`rustup target add aarch64-apple-darwin x86_64-apple-darwin`.

Add `--dmg` to create `target/bundle/tinytrac-<version>-macos.dmg` with a
drag-to-install Applications link. `--zip` remains available; both can be combined.

Copy `target/bundle/tinytrac.app` to **/Applications** on each Mac, or transfer
`target/bundle/tinytrac-<version>-macos.zip` and unzip it first. Open the app; if
macOS blocks a quarantined copy, use **System Settings → Privacy & Security →
Open Anyway**. Add it under **System Settings → General → Login Items** to start
at login. The bundle runs in the menu bar without a Dock icon.

## Sync with Google Drive

Open **Settings → Sync → Google Drive → Sign in with Google**, then choose your
Google account in the browser. Use the same account on both Macs (Linux works too).
The first sync uploads this Mac's history. Each device must start with its own
fresh database; never copy a database between devices. Sync runs at startup, after
saves and every 30 seconds. Signing out keeps all local data and does not revoke
Google access or delete uploaded history.

Refresh tokens are stored in macOS Keychain or Linux Secret Service (GNOME Keyring),
never in the database or a plaintext file. Unlock your login keychain/keyring if
sign-in reports a keychain error. Expired sign-in pauses sync until you sign in
again; ordinary connection failures retry on the next scheduled sync.

### Build with Google credentials

Google Drive is available on macOS and Linux in builds with client credentials. Enable the Drive API
in your Google Cloud project, set its OAuth consent screen to **In production**, create a **Desktop app** OAuth client, and download its
JSON to `~/.config/tinytrac/google-client.json`. With `jq` installed:

```sh
export TINYTRAC_GOOGLE_CLIENT_ID="$(jq -er '.installed.client_id' ~/.config/tinytrac/google-client.json)" TINYTRAC_GOOGLE_CLIENT_SECRET="$(jq -er '.installed.client_secret' ~/.config/tinytrac/google-client.json)"; cargo build --release -p tinytrac
```

Leaving the consent screen in **Testing** expires refresh tokens after seven days.
The `drive.appdata` scope is non-sensitive and does not require verification.

These values are embedded at build time; the Desktop client secret is not a
confidential server credential. Keep the downloaded JSON outside the repository.
Linux needs a running Secret Service such as GNOME Keyring and `xdg-open`; macOS
uses `open`. No libdbus development package is needed for credential storage.

## Sync with Syncthing

Create a shared Syncthing folder and let it replicate between your devices.
On each device, start tinytrac with its own fresh database, open **Settings → Sync → Folder**, enter
that device's **Sync folder** path, and press Enter or **Save folder**. Keep each
SQLite database outside the shared folder; do not copy a database to another device.
Sync runs in the background at startup, after saves and every 30 seconds. Clear the
field and save to disable. Switching to a different folder republishes all known
history, including deletions and revisions received from other devices, so a fresh
peer can catch up. Re-selecting the same folder does nothing; clearing then re-setting
it safely republishes.

Settings keeps a durable summary of rejected changes and the latest reason, plus
conflicts and the last sync result. New problems also appear in the main error
banner; dismissing it suppresses repeats until something new appears. Files written
in an unsupported newer format stay pending: update that device's tinytrac to read
them. Other files continue syncing.

Concurrent edits keep the newer revision and show a notice; the other revision
remains in the history. Conflict notices can also come from automatic timer repairs
made on devices with different partial histories. Editing that record again resolves
all known conflicting versions and clears the notice after syncing. Keep the shared folder private and
do not delete its batches.

See [ARCHITECTURE.md](ARCHITECTURE.md) for how it fits together.

## Releasing

One-time setup in the repository's **Settings → Secrets and variables → Actions**:

- Run `scripts/make-signing-identity.sh [output-directory]` on Linux or macOS with
  OpenSSL. The default is `~/.config/tinytrac/signing`; the directory is private
  (0700), with files 0600. Paste `tinytrac.p12.base64` into
  `MACOS_SIGNING_P12_BASE64` and `password.txt` into `MACOS_SIGNING_P12_PASSWORD`.
  Back up the directory securely and reuse this identity for every release.
- Set `TINYTRAC_GOOGLE_CLIENT_ID` and `TINYTRAC_GOOGLE_CLIENT_SECRET` from the
  Desktop OAuth client described above. Tag builds require both so releases
  include Google sync. These OAuth values are embedded in the downloaded binary.

The generated certificate lasts about ten years. Its PKCS#12 uses explicit
3DES PBE and a SHA-1 MAC for macOS import compatibility, including when generated
with OpenSSL 3 ([OpenSSL options](https://docs.openssl.org/3.3/man1/openssl-pkcs12/)).
CI imports it into a temporary keychain and deletes that keychain even on failure.
`codesign --verify --deep --strict` checks signature integrity and the designated
requirement; it does not request trusted-anchor or Gatekeeper validation, so
self-signed certificates work without weakening verification
([Apple's guide](https://developer.apple.com/library/archive/documentation/Security/Conceptual/CodeSigningGuide/Procedures/Procedures.html)).
This identity preserves the app's identity for Keychain access across updates;
it does not bypass Gatekeeper or provide notarization.

After updating the workspace version in `Cargo.toml` (and `Cargo.lock`) and
committing the release changes, tag the matching version:

```sh
git tag vX.Y.Z && git push origin vX.Y.Z
```

The Release workflow builds a universal macOS DMG and Linux tarball, then creates
a **draft** named `tinytrac X.Y.Z` with both assets and `SHA256SUMS`. Review the
assets and notes, then publish the draft. Re-running a tag that already has a
release fails rather than overwriting it.

For a dry run, use **Actions → Release → Run workflow** on a trusted ref. This
uploads workflow artifacts only. Missing Google secrets disable Google sync
with a warning; missing signing secrets use ad-hoc macOS signing. Tag builds
require all four secrets. Never run this secrets-bearing workflow on untrusted
code; it deliberately has no pull-request trigger.

## License

MIT. See [LICENSE](LICENSE).
