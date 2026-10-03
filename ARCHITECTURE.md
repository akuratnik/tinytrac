# Architecture

tinytrac is a Cargo workspace with three crates, each depending only on the one before it:

| Crate | Role | Status |
|---|---|---|
| `tinytrac-core` | Domain rules: tags, timers, break bank, reports. No I/O; time is passed in. | done |
| `tinytrac-engine` | SQLite storage, commands, queries and folder sync. | done |
| `tinytrac` | Desktop app ([iced](https://iced.rs)) with system tray. | done |

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
four screens (timers, history, reports, settings). Each screen keeps its own edit
state (form buffers; reports also caches its range's sessions) and its own
`Message` type, and calls the engine in `update`. That returns an `Outcome`:
`Edited` when only the screen's state changed, such as typing, and `Saved` when
the engine wrote something. Only saves reload data and clear the error banner, so
typing never touches the database.

`Data` is a read-only snapshot of tags, open timers, settings and the last 14 days
of sessions. It is reloaded after every save, on a new day, and every 30 seconds to
pick up changes made elsewhere. Views compute live values, such as a running timer's
time or today's totals, from the snapshot on each one-second tick (one-minute when
nothing runs), so they never touch the database. Forms remember the values they
opened with and save only what the user changed. Start buttons carry their
`StartMode`, decided from the modifier keys when the view is built: Ctrl+click
(Cmd on macOS) starts a timer alongside the running one.

The app runs as an `iced::daemon`: closing the window destroys it, keeping `App`
and its engine alive when a tray is available. Show opens a new window or focuses
an existing one; Quit exits without stopping the persisted timers. Without a tray,
closing the window exits. `app/lifecycle.rs` handles window and tray lifecycle.
Native tray resources are created in `update`, after the event loop has started
(required on macOS), and stay on that thread.

`tray.rs` adapts a pure menu model (`tray/model.rs`) to `tray-icon`. Menus and the
today tooltip refresh with `Data`, not with clock ticks; the generated RGBA clock
changes when running state changes. Native callbacks feed a channel subscription,
delivering one iced message per event through the screens' command and reload
paths. On Linux, the ksni backend uses D-Bus without a GTK loop. Since it assumes
a tray host exists, a
separate asynchronous subscription uses one D-Bus connection to check ownership
of `org.kde.StatusNotifierWatcher`, then follows `NameOwnerChanged` for that name.
It does not poll or use the host registration property, which hosts may hardcode.
GNOME needs the AppIndicator extension. If the host disappears while the window
is closed, the app reopens it so the user can still reach the app.

UI tests (`src/app/tests.rs`) use `iced_test` to drive the real `view` and `update`
headlessly: they click buttons by label and check what is on screen. Setting
`TINYTRAC_SNAPSHOTS=<dir>` also saves a PNG of each screen.

## Sync

Each device has its own database and a persistent device id. Configure a shared
folder in Settings; Syncthing replicates that folder between devices. Its path is
local metadata, never part of the synced settings record. The engine resolves
paths and refuses a folder containing its database, including through symlinks.
Create a fresh database on each device: copying a database also copies its device
id and must not be used to provision another writer.

`transport.rs` defines three operations: publish, list, and read batches. Its only
implementation is the folder. Merge and validation live in `sync.rs`, independent
of the transport. Files are `<device>/<last-seq:020>-<uuidv7>.json`, containing format
version 1 and an array of changes. Publication writes and fsyncs a temporary file
in the same directory, renames it, and on Unix fsyncs the directories. Unique names
allow sequence reuse after backup restore. The export transaction holds the writer lock
until publication succeeds, then marks only its selected local changes exported.
A crash before commit can republish revisions; revision ids make this harmless.
Files are immutable and only a device's own changes are exported. Export errors
are reported without preventing imports.

Imports remember individual filenames in `imported_files`, allowing late arrivals.
Non-JSON filenames are ignored. A bad file or change is reported without blocking
other imports. Each file uses one transaction with a savepoint per change, so a
validation failure rolls back only that change. Applied, known and permanently
rejected changes all count toward marking the immutable file imported; rejection
notices appear once. Invalid JSON is permanent except for unexpected EOF, which
may mean a partial copy. Read failures, truncated files and database failures are
retried. Counts and notices are published only after the transaction commits.
Imports preserve each revision, parent, merge references and device.
Bodies deserialize through core invariants, ids must match, and record kinds
cannot change, even after deletion. Revisions must be canonical UUIDv7 strings.

A record's heads are revisions that no known revision of that record names as its
parent or in its `merges` array. A local edit names the winner as parent and all
other current heads as merges, superseding them together. The greatest head by UUID string wins; ancestry takes precedence over
wall-clock time. Recompute after imports and local writes, updating the body and
lookup columns when the winner changes. This converges for the same revision
history regardless of delivery order. Tombstones participate in the same rule.
`conflicts` contains each current losing head, rebuilt for the affected record,
so late ancestors do not leave false conflict warnings. The original losing
bodies remain in `changes`.

This is whole-record merging: concurrent edits to different fields can overwrite
each other. UUID time reflects device clocks, so clock skew can favor an unexpected
winner. Local revision ids advance beyond the greatest revision stored for that
device even if its clock steps backward. Editing a conflicted record resolves all
known branches; an unseen concurrent branch can still produce another notice.
There is no dedicated conflict picker, history pruning, encryption or untrusted
peer authentication. Keep the folder private and retain its batches for new
installations. Duplicate open timers are settled by `Timers::load` and persisted
by the next timer command, rather than during import.

The app syncs after saved commands and on its 30-second refresh, then reloads data
and report caches. Settings shows the last result/error and current conflict count.
Folder I/O is synchronous; a slow or unavailable filesystem can delay the UI.
Batches and revision histories have no size limit and are loaded into memory;
merge work grows with the affected record's history. Device identity and revision
monotonicity use the local database: restoring it cannot recover unseen edits or
the revision high-water mark beyond the backup. Never run two writers with a
copied device identity.

## Testing

- Unit tests sit next to the code they cover.
- `crates/tinytrac-core/tests/properties.rs` checks invariants over random timer
  usage, for example "logged time never decreases" and "report totals add up to
  logged time".
- `crates/tinytrac-engine/tests/engine.rs` drives the engine through its public
  API with a controllable clock.
