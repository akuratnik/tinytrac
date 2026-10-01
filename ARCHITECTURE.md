# Architecture

tinytrac is a Cargo workspace with three crates, each depending only on the one before it:

| Crate | Role | Status |
|---|---|---|
| `tinytrac-core` | Domain rules: tags, timers, break bank, reports. Pure: no I/O, no clock. | done |
| `tinytrac-engine` | SQLite storage, commands, queries, folder sync. | planned |
| `tinytrac` | Desktop app ([iced](https://iced.rs)) and tray icon. | planned |

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
entries are sessions with no segments and only an adjustment.

**Timers**: the open sessions, at most one per tag. Starting a timer pauses the
others (`StartMode::Exclusive`) unless it is started alongside them
(`StartMode::Alongside`). Every mutating call returns the sessions it changed, so
the engine persists exactly those.

**Totals**: logged time per day and tag over a `DateRange`. Recorded time is cut at
local midnight (DST-aware). Covered break is assigned to break segments in order, and
adjustments count towards the session's day.

Time is handled with [jiff](https://docs.rs/jiff): UTC `Timestamp`s for storage and
time-zone-aware calendar days for reporting.

## Storage and sync (planned)

- Each device keeps its own SQLite database (`rusqlite`, bundled). Changes are
  committed together with an outgoing change record.
- Sync writes each change as an immutable file to a shared folder and imports
  the other devices' files. No file is ever written by two devices, so file-sync
  tools such as Syncthing never produce conflicting copies.
- Records are merged whole, with parent revisions to tell successive edits from
  concurrent ones. Concurrent edits get a deterministic winner, and the other
  version is kept and flagged.
- Sync transports sit behind a small trait, so a server backend can be added later.

## Testing

- Unit tests sit next to the code they cover.
- `crates/tinytrac-core/tests/properties.rs` checks invariants over random timer
  usage, for example "logged time never decreases" and "report totals add up to
  logged time".
