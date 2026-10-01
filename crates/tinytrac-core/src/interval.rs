use jiff::{SignedDuration, Timestamp};

/// A half-open span of time, `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub start: Timestamp,
    pub end: Timestamp,
}

impl Interval {
    pub fn duration(&self) -> SignedDuration {
        self.end.duration_since(self.start)
    }

    /// The part of this interval that falls within `other`, if any.
    pub fn intersect(&self, other: &Interval) -> Option<Interval> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        (start < end).then_some(Interval { start, end })
    }
}
