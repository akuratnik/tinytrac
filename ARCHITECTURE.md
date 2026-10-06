# Architecture

tinytrac is a Cargo workspace with three crates, each depending only on the one before it:

| Crate | Role | Status |
|---|---|---|
| `tinytrac-core` | Domain rules: tags, timers, break bank, reports. No I/O; time is passed in. | done |
| `tinytrac-engine` | SQLite storage, commands, queries and transport-independent sync. | done |
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
ranks sessions by (is running, last state change, session id). A running session
beats a paused one. A paused loser keeps all its time; only its stopped flag changes.
When both are running, the later start wins and the earlier session stops there,
like an exclusive start. A segmentless winner falls back to `Timestamp::MIN`.
The repair uses no local clock, so identical inputs produce identical bodies.

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
database from a newer version is refused rather than risk damaging it. Migration
003 adds durable rejection diagnostics, a sync epoch and lookup indexes, clears
imported filenames once to reinspect files older clients swallowed, and runs the
Rust duplicate repair in the migration transaction. Opening an up-to-date database
does not write repairs. Migration 004 converts `meta.sync_folder` to a serialized
`SyncTarget` without touching exported flags, imported keys or the epoch. Write
transactions take the lock up front (`BEGIN IMMEDIATE`) and wait at most two
seconds for another writer.
Settings (break bank on/off and ratio, week start) are a synced record.

## Desktop app (`tinytrac`)

An iced program in the Elm style. `App` owns the `Engine` and routes messages to
four screens (timers, history, reports, settings). Each screen keeps its own edit
state (form buffers; reports also caches its range's sessions) and its own
`Message` type, and calls the engine in `update`. That returns an `Outcome`:
`Edited` when only the screen's state changed, such as typing, and `Saved` when
the engine wrote something. Saves reload data, clear the command error banner, and request sync. Failed commands
reload without syncing; commands are transactional. Typing never touches the database.

`Data` is a read-only snapshot of tags, open timers, settings and the last 14 days
of sessions. `Engine::snapshot` wraps its queries in one deferred read transaction.
It is reloaded after saves, failed commands, sync completion and on a new day.
Tag labels are disambiguated here in tag-id order, reserving existing names and
including archived tags; suffixes are display-only. Views compute live values, such as a running timer's
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
today tooltip refresh with `Data`, not with clock ticks. The native menu is replaced
only when visible ids, labels, enabled states or separators change, and the tooltip
only when its text changes. Routing always uses the latest model. The generated
RGBA clock changes when running state changes. Native callbacks feed a channel subscription,
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
folder in Settings; Syncthing replicates that folder between devices. `SyncTarget`
is either a folder path or a remote target with a caller-chosen stable, opaque id
and a display-only label. Targets are local metadata, never part of the synced
settings record. The engine resolves paths and refuses a folder containing its
database, including through symlinks.
Create a fresh database on each device: copying a database also copies its device
id and must not be used to provision another writer.

`Transport` is a public, mutable interface: `publish(name, bytes)` stores an
immutable batch and publishing the same batch twice is harmless; `list()` returns
stable opaque read keys for batches from other devices; `read(key)` retrieves the
immutable bytes. Keys need not match publish names (remote file ids are valid).
Implementations must never reuse a key for different bytes. Mutable methods allow
credential refresh and caches without a `Sync` bound or internal mutex. The engine
does no networking. `Folder::for_context` constructs the std-only folder transport
and returns `None` for remote targets; a later crate will supply Google Drive.
`sync.rs` coordinates import/export, `store.rs` provides typed storage and timer
repair, and `revisions.rs` owns validation, head computation and materialized
winners. These layers share no reverse dependencies.

`Engine::sync_context()` captures target, epoch and device together in a read
transaction. Its immutable `SyncContext` binds caller-created transports to that
selection. `Engine::sync(&context, &mut transport)` rejects stale contexts before
transport I/O, including A -> B -> A switches. Callers must build the transport for
the supplied context. Folder runs re-resolve the path and check database containment
before I/O; a changed canonical path requires selecting the target again.

Files are `<device>/<last-seq:020>-<uuidv7>.json`, containing format
version 1 and an array of changes. **Any batch-format change bumps the version.**
Publication writes and fsyncs a temporary file in the same directory, renames it,
and on Unix fsyncs the directories. Unique names allow sequence reuse after backup
restore. Export selects at most 1000 pending revisions and the target's `sync_epoch`
in a read transaction, releases it for publication, then marks exactly those
revisions in a short IMMEDIATE transaction only if the epoch is unchanged. It loops
until no pending revisions remain. A crash after publication can republish revisions;
revision ids make this harmless. Ordinary export errors do not prevent imports.

Selecting a different target identity (canonical folder path or remote id)
atomically stores the target, resets all revisions (including other devices'
revisions) to unexported, clears imported keys, and increments `sync_epoch`.
Disabling invalidates the epoch but retains diagnostics until a target is selected
again, matching the existing folder behavior. Re-selecting the same identity
preserves progress; a remote label-only change updates its display without
invalidating existing contexts. Clearing and re-setting
republishes history. A sync already publishing to the old target cannot mark
revisions or import markers for the new epoch. Imported revisions normally
arrive with `exported = 1`, so ordinary syncs export only locally written changes.

Imports remember opaque read keys in `imported_files`, allowing late arrivals.
Only the folder transport filters non-JSON filenames; remote keys have no
extension requirement. The epoch is checked before reading and parsing each file;
importing stops when it becomes stale. The envelope version is checked before
interpreting its payload; unsupported versions remain retryable with an update-device notice.
Each change is decoded separately from borrowed `RawValue`, without a whole-batch
JSON value tree. Each file uses one transaction and a savepoint per change. Invalid
ids, bodies or kinds roll back only that change. Kind validation reads history,
independent of materialized records. After validation, each affected record is
merged once per batch. If any affected record has no head, the entire file's changes
are rolled back and the file is permanently recorded as corrupt. Valid files still
import. No separate graph tables are maintained. A single query over `changes`
computes each record's heads using the (record, parent) index and `json_each` for
nonempty merges arrays; references from other records do not hide heads. Merge work is
roughly linear in revisions and ancestry edges, plus the size of snapshot bodies.

Applied, known and permanently rejected changes mark the immutable file processed.
The marker atomically stores a rejected count and latest reason, including malformed
files. `SyncDiagnostics` exposes the durable sum and latest reason independently of
`SyncRun`, which carries only exported/imported counts and transient issues. Invalid
JSON is permanent except for unexpected EOF, which may mean a partial copy. Read
failures, truncated files, unsupported versions and database failures remain retryable
in the current report's `skipped` list. Counts are exposed only after commit.
`Error::AuthRequired` signals missing, expired or revoked credentials requiring
user intervention. From publish, list or read it aborts the run as a distinct error,
not a skipped issue. A failed read never marks its key imported or rejected, and a
failed publish never acknowledges its revisions. Earlier committed batches remain
committed. Other export failures still allow import; other per-key read failures
remain retryable skipped issues.
Imports preserve each revision, parent, merge reference and device. Bodies deserialize
through core invariants, ids must match, and kinds cannot change, even after deletion.
Revisions must be canonical UUIDv7 strings.

A record's heads are revisions that no known revision of that record names as its
parent or in its `merges` array. A local edit names the winner as parent and all
other current heads as merges, superseding them together. The greatest head by UUID string wins; ancestry takes precedence over
wall-clock time. Materialize after imports and local writes, updating the body and
lookup columns when the winner changes. This converges for the same revision
history regardless of delivery order. Tombstones participate in the same rule.
`conflicts` contains each current losing head, rebuilt for the affected record,
so late ancestors do not leave false conflict warnings. The original losing
bodies remain in `changes`. Reported conflict counts exclude losing heads whose
body equals the winner's body, including identical repairs made on two devices.
Those heads remain in `conflicts` so the next write can merge all ancestry.

This is whole-record merging: concurrent edits to different fields can overwrite
each other. UUID time reflects device clocks, so clock skew can favor an unexpected
winner. Local revision ids advance beyond the greatest revision stored for that
device even if its clock steps backward. Editing a conflicted record resolves all
known branches; an unseen concurrent branch can still produce another notice.
There is no dedicated conflict picker, history pruning, encryption or untrusted
peer authentication. Keep the folder private and retain its batches for new
installations. Duplicate open timers are settled
inside each import transaction before commit, and once during migration 003.
`Store::settle` writes only stopped losers; timer commands
also persist losers returned by loading. Readers never observe an unsettled import.
Repairs from different partial histories may differ: with three or more devices
running the same tag, repairs made with different knowledge can surface as a
conflict. The next edit clears its known branches. Normal revision merging and
further settlement converge once the same history has arrived. A further exchange
of a settled history creates no new revisions.

The app requests sync at startup, after saved commands and every 30 seconds. At boot it opens
one second `Engine` for file databases and retains it with an optional `(SyncContext, Folder)` transport cache in one worker
for the app's lifetime. An `Arc<Mutex<Worker>>` moves serialized work between
threads; transport state survives runs and is dropped when the context changes.
Worker threads use that engine; ordinary commands and native
tray operations stay on the UI thread. One sync runs at a time; requests during a run
queue exactly one rerun. Completion returns a cloneable `Message::Synced` with cloneable errors that preserve the authentication-required variant, updates status, and reloads data and report caches. In-memory
engines never sync. Iced 0.14 does not export `task::blocking`, so a standard thread
and iced's futures channel, wrapped in a small blocking helper, provide the same
task delivery without another dependency.

Settings shows durable rejections and current conflicts only when nonzero, alongside
the last run's timestamped status: Synced, Sync incomplete with skipped issues,
or Sync failed with the error (or Sync disabled). When the latest run is not clean,
the last successful sync time is shown separately if one is known. Remote targets
show their display label; this step adds no remote setup UI. The main error banner
appears when the rejected total grows or a new transient problem appears; whole-sync errors count as transient.
The rejected baseline is seeded from the initial snapshot, so old rejections do
not raise a banner on launch. Banners describe only new problems. Problem identity
excludes timestamps, so dismissing a repeated issue sticks.

Full-snapshot revisions are an accepted trade-off: segment histories repeat in each
session revision. Export batches are bounded by revision count, not bytes; imported
files are read into memory, and large bodies still cost time to decode and store.
There is no history pruning. Device identity and revision monotonicity use the local
database: restoring it cannot recover unseen edits or the revision high-water mark
beyond the backup. Never run two writers with a copied device identity.

## Testing

- Unit tests sit next to the code they cover.
- `crates/tinytrac-core/tests/properties.rs` checks invariants over random timer
  usage, for example "logged time never decreases" and "report totals add up to
  logged time".
- `crates/tinytrac-engine/tests/engine.rs` drives the engine through its public
  API with a controllable clock.
