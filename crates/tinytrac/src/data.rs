//! What the views show: a snapshot of the engine's data, reloaded after every
//! command. Views compute live values (a running timer's time) from it.

use std::collections::{BTreeMap, BTreeSet};

use jiff::{ToSpan, Zoned, civil::Date};
use tinytrac_core::{DateRange, Session, Status, Tag, TagId, Totals};
use tinytrac_engine::{Engine, Result, Settings};

/// How many days of history the main screen lists.
pub const HISTORY_DAYS: i64 = 14;

#[derive(Debug)]
pub struct Data {
    pub now: Zoned,
    pub settings: Settings,
    pub sync_target: Option<tinytrac_engine::SyncTarget>,
    pub sync_diagnostics: tinytrac_engine::SyncDiagnostics,
    /// All tags, archived ones included, sorted by name.
    pub tags: Vec<Tag>,
    labels: BTreeMap<TagId, String>,
    /// The open timers.
    pub timers: Vec<Session>,
    /// Sessions logging time in the last [`HISTORY_DAYS`] days.
    pub recent: Vec<Session>,
}

impl Data {
    /// Reads everything the views need. Read-only, so it never waits on writers.
    pub fn load(engine: &Engine) -> Result<Self> {
        engine.snapshot(|engine| {
            let now = engine.now();
            let tags = engine.tags()?;
            Ok(Self {
                labels: tag_labels(&tags),
                sync_diagnostics: engine.sync_diagnostics()?,
                settings: engine.settings()?,
                sync_target: engine.sync_target()?,
                tags,
                timers: engine.timers()?.iter().cloned().collect(),
                recent: engine.sessions(recent_range(now.date()))?,
                now,
            })
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
        self.labels.get(&id).map_or("Unknown tag", String::as_str)
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

/// Reserve original names first, including archived tags and existing suffixes.
fn tag_labels(tags: &[Tag]) -> BTreeMap<TagId, String> {
    let mut reserved: BTreeSet<_> = tags.iter().map(|tag| tag.name().to_lowercase()).collect();
    let mut seen = BTreeSet::new();
    let mut ordered: Vec<_> = tags.iter().collect();
    ordered.sort_by_key(|tag| tag.id);
    ordered
        .into_iter()
        .map(|tag| {
            let mut label = tag.name().to_owned();
            if !seen.insert(label.to_lowercase()) {
                for suffix in 2.. {
                    label = format!("{} ({suffix})", tag.name());
                    if reserved.insert(label.to_lowercase()) {
                        break;
                    }
                }
            }
            (tag.id, label)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colliding_labels_reserve_names_and_follow_ids() {
        let a = Tag::new("Name").unwrap();
        let mut b = Tag::new("name").unwrap();
        b.archived = true;
        let c = Tag::new("Name (2)").unwrap();
        let tags = vec![a.clone(), b.clone(), c.clone()];
        let labels = tag_labels(&tags);
        assert_eq!(labels[&a.id], "Name");
        assert_eq!(labels[&b.id], "name (3)");
        assert_eq!(labels[&c.id], "Name (2)");
        assert_eq!(labels, tag_labels(&[c.clone(), b.clone(), a.clone()]));
        b.rename("Renamed").unwrap();
        assert_eq!(tag_labels(&[a, b.clone(), c])[&b.id], "Renamed");
    }
}
