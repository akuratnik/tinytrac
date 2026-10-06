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

## Run

Requires Rust 1.90 or newer. GNOME needs the AppIndicator extension for the tray; without it, closing the window exits.
On GNOME, single-click opens the tray menu; double-click or **Show tinytrac** reopens the window.
On macOS, reopen a closed window from the tray; the Dock icon does not reopen it.

```sh
cargo run --release            # data lives in ~/.local/share/tinytrac on Linux
TINYTRAC_DB=/tmp/demo.db cargo run   # try it on a throwaway database
cargo test                     # domain, storage and headless UI tests
```

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
