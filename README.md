# tinytrac

A tiny, native time tracker for work. No browser engine, no account, no server.

- **Tags**: track time against anything: clients, projects, categories.
- **Several timers**: clicking a timer pauses the running one; ctrl+click runs both.
- **Break bank** (optional): work earns break time (5 min per 25 by default).
  Breaks are logged while you have credit; going over is deducted unless later
  work pays it back.
- **Totals**: today and past days at a glance; totals per tag for any range.
- **Sync** across your own devices through a shared folder (e.g. Syncthing).

> Status: early development. Tracking, editing and reports work; the tray icon and sync are next.

## Run

```sh
cargo run --release            # data lives in ~/.local/share/tinytrac on Linux
TINYTRAC_DB=/tmp/demo.db cargo run   # try it on a throwaway database
cargo test                     # domain, storage and headless UI tests
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for how it fits together.
