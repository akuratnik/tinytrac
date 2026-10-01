# Architecture

tinytrac is a Cargo workspace with three crates, each depending only on the one before it:

| Crate | Role | Status |
|---|---|---|
| `tinytrac-core` | Domain rules: tags, timers, break bank, reports. No I/O; time is passed in. | done |
| `tinytrac-engine` | SQLite storage, commands, queries; folder sync next. | done |
| `tinytrac` | Desktop app ([iced](https://iced.rs)); tray icon next. | done |

The rules live in `core`, where they are tested without a database or a window.
The UI is a thin layer over engine commands.

## Domain model (`tinytrac-core`)

**Tag**: a neutral label to track time against. It has an optional `ExternalRef { source, id }`
so it can later be linked to, say, a client in a PM tool, without changing the model.

**Session**: one timer's run for one tag, from start to stop. It stores what happened:

```
segments:   [Work 09:00–09:50] [Break 09:50–10:02] (paused) [Work 10:30–…]
breaks:     Some(5 min per 25 min)   // captured at start
adjustment: +0                       // manual correction
```

Logged time is always *derived* from the segments, never stored:

```
earned  = work × break_minutes / work_minutes
covered = min(break_taken, earned)
logged  = work + covered + adjustment
```

So a break beyond the earned credit is not logged, but later work can earn the
credit and cover it. Pauses are gaps between segments: not logged, and they keep
the balance. Stopping ends the session and discards any unused credit.

Editing an entry's elapsed time sets `adjustment` to the difference between the
new value and the recorded time. The recorded segments stay as they were. Manual
entries are sessions with no segments and only an adjustment. Sessions are
validated when deserialized, so a corrupt or synced record can't break these
rules.

**Timers**: the open sessions, one per tag. Starting a timer pauses the others
(`StartMode::Exclusive`) unless it is started alongside them
(`StartMode::Alongside`). Starting a timer that is on a break ends the break. Every
mutating call returns the sessions it changed, so the engine persists exactly
those. If two offline devices each opened a session for the same tag, loading
keeps the most recently active one and stops the other, keeping its time.

**Totals**: logged time per day and tag over a `DateRange`. Recorded time is cut at
local midnight (DST-aware). Covered break is assigned to break segments in order. A
negative adjustment trims counted time from the end of the session, so no day goes
negative. A positive adjustment counts towards the session's day.

Time is handled with [jiff](https://docs.rs/jiff): UTC `Timestamp`s for storage and
time-zone-aware calendar days for reporting.

## Engine (`tinytrac-engine`)

`Engine` is the one entry point for the UI. Each command (start a timer, edit an
entry) runs in its own SQLite transaction: load the records, apply the core rules,
save what changed, and return the changed records so the UI can update in place.
The engine keeps no domain state in memory; `Timers` is rebuilt for each command.

Storage is a small document store (`rusqlite`, bundled SQLite):

- `records` holds the current revision of every tag, session and the settings,
  as JSON, plus a few lookup columns derived from sessions (`day`, span, open)
  for range queries. Deleting writes a tombstone.
- `changes` gets one row per write: revision id, parent revision, device, body.
  This is the history sync exchanges.
- `meta` holds local, unsynced values such as this device's id.

The schema is versioned with `PRAGMA user_version` and plain SQL migrations. A
database from a newer version is refused rather than risk damaging it. Write
transactions take the lock up front (`BEGIN IMMEDIATE`) and wait at most two
seconds for another writer.
Settings (break bank on/off and ratio, week start) are a synced record.

## Desktop app (`tinytrac`)

An iced program in the Elm style. `App` owns the `Engine` and routes messages to
four screens (timers, history, reports, settings). Each screen keeps only its edit
state (form buffers) and its own `Message` type, and calls the engine in `update`.

After every command the app reloads `Data`, a small snapshot of tags, open timers,
settings and the last 14 days of sessions. Views compute live values, such as a
running timer's time or today's totals, from that snapshot on each one-second tick,
so they never touch the database. Ctrl+click (Cmd on macOS) starts a timer
alongside the running one.

UI tests (`src/app/tests.rs`) use `iced_test` to drive the real `view` and `update`
headlessly: they click buttons by label and check what is on screen. Setting
`TINYTRAC_SNAPSHOTS=<dir>` also saves a PNG of each screen.

## Sync (planned)

- Sync writes each change as an immutable file to a shared folder and imports
  the other devices' files. No file is ever written by two devices, so file-sync
  tools such as Syncthing never produce conflicting copies.
- Records are merged whole, using parent revisions to tell successive edits from
  concurrent ones. Concurrent edits get a deterministic winner, and the other
  version is kept and flagged.
- Sync transports sit behind a small trait, so a server backend can be added later.
- The database itself never lives in the synced folder; file-sync tools and
  SQLite's write-ahead log don't mix.
- Imports keep the incoming revision ids and parents (unlike local writes, which
  mint new ones), validate each body before writing it, and check that a record
  never changes kind. Duplicate open timers can only arrive through imports,
  where they will be resolved.
- The device id must survive copying a database to another machine, so that two
  devices never write as one.

## Testing

- Unit tests sit next to the code they cover.
- `crates/tinytrac-core/tests/properties.rs` checks invariants over random timer
  usage, for example "logged time never decreases" and "report totals add up to
  logged time".
- `crates/tinytrac-engine/tests/engine.rs` drives the engine through its public
  API with a controllable clock.
