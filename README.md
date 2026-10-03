# tinytrac

A tiny, native time tracker for work. No browser engine, no account, no server.

- **Tags**: track time against anything: clients, projects, categories.
- **Several timers**: clicking a timer pauses the running one; ctrl+click runs both.
- **Break bank** (optional): work earns break time (5 min per 25 by default).
  Breaks are logged while you have credit; going over is deducted unless later
  work pays it back.
- **System tray**: start or pause timers, pause all, and keep tracking with the window closed.
- **Totals**: today and past days at a glance; totals per tag for any range.
- **Sync** across your own devices through a shared folder such as Syncthing (in progress).

> Status: early development. Tracking, editing, reports and the tray work; sync is next.

## Run

Requires Rust 1.90 or newer. GNOME needs the AppIndicator extension for the tray; without it, closing the window exits.
On GNOME, single-click opens the tray menu; double-click or **Show tinytrac** reopens the window.
On macOS, reopen a closed window from the tray; the Dock icon does not reopen it.

```sh
cargo run --release            # data lives in ~/.local/share/tinytrac on Linux
TINYTRAC_DB=/tmp/demo.db cargo run   # try it on a throwaway database
cargo test                     # domain, storage and headless UI tests
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for how it fits together.
