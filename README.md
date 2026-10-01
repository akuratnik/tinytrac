# tinytrac

A tiny, native time tracker for work. No browser engine, no account, no server.

- **Tags**: track time against anything: clients, projects, categories.
- **Several timers**: clicking a timer pauses the running one; ctrl+click runs both.
- **Break bank** (optional): work earns break time (5 min per 25 by default).
  Breaks are logged while you have credit; going over is deducted unless later
  work pays it back.
- **Totals**: today and past days at a glance; totals per tag for any range.
- **Sync** across your own devices through a shared folder (e.g. Syncthing).

> Status: early development. The domain core is done; storage, desktop UI and sync are next.

## Build

```sh
cargo test
```

See [ARCHITECTURE.md](ARCHITECTURE.md) for how it fits together.
