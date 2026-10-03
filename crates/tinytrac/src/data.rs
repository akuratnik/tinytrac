//! What the views show: a snapshot of the engine's data, reloaded after every
//! command. Views compute live values (a running timer's time) from it.

use jiff::{ToSpan, Zoned, civil::Date};
use tinytrac_core::{DateRange, Session, Status, Tag, TagId, Totals};
use tinytrac_engine::{Engine, Result, Settings};

/// How many days of history the main screen lists.
pub const HISTORY_DAYS: i64 = 14;

#[derive(Debug)]
pub struct Data {
    pub now: Zoned,
    pub settings: Settings,
    pub sync_folder: String,
    /// All tags, archived ones included, sorted by name.
    pub tags: Vec<Tag>,
    /// The open timers.
    pub timers: Vec<Session>,
    /// Sessions logging time in the last [`HISTORY_DAYS`] days.
    pub recent: Vec<Session>,
}

impl Data {
    /// Reads everything the views need. Read-only, so it never waits on writers.
    pub fn load(engine: &Engine) -> Result<Self> {
        let now = engine.now();
        Ok(Self {
            settings: engine.settings()?,
            sync_folder: engine
                .sync_folder()?
                .map_or_else(String::new, |path| path.display().to_string()),
            tags: engine.tags()?,
            timers: engine.timers()?.iter().cloned().collect(),
            recent: engine.sessions(recent_range(now.date()))?,
            now,
        })
    }

    pub fn today(&self) -> Date {
        self.now.date()
    }

    /// The days the history covers, ending today.
    pub fn recent_range(&self) -> DateRange {
        recent_range(self.today())
    }

    /// Totals for `range`, which must lie within [`Data::recent_range`].
    pub fn totals(&self, range: DateRange) -> Totals {
        Totals::compute(
            &self.recent,
            range,
            self.now.time_zone(),
            self.now.timestamp(),
        )
    }

    pub fn active_tags(&self) -> impl Iterator<Item = &Tag> {
        self.tags.iter().filter(|tag| !tag.archived)
    }

    pub fn tag_name(&self, id: TagId) -> &str {
        self.tags
            .iter()
            .find(|tag| tag.id == id)
            .map_or("Unknown tag", Tag::name)
    }

    pub fn timer(&self, tag: TagId) -> Option<&Session> {
        self.timers.iter().find(|session| session.tag == tag)
    }

    pub fn any_running(&self) -> bool {
        self.timers
            .iter()
            .any(|session| matches!(session.status(), Status::Running(_)))
    }
}

fn recent_range(today: Date) -> DateRange {
    DateRange::new(today - (HISTORY_DAYS - 1).days(), today)
}
