#[cfg(test)]
use std::sync::Arc;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> OffsetDateTime;
}

/// Formats a timestamp as RFC 3339 for the `serverTimestamp` wire field,
/// falling back to a Unix seconds string if formatting somehow fails.
pub(crate) fn format_time(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .unwrap_or_else(|_| value.unix_timestamp().to_string())
}

/// A compact "how long ago" label for presence and contact rows: under a
/// minute is "now", then whole minutes, hours, and days ("5m", "2h", "6d").
pub fn ago_compact(seconds: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    if seconds < MINUTE {
        "now".to_owned()
    } else if seconds < HOUR {
        format!("{}m", seconds / MINUTE)
    } else if seconds < DAY {
        format!("{}h", seconds / HOUR)
    } else {
        format!("{}d", seconds / DAY)
    }
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<std::sync::RwLock<OffsetDateTime>>,
}

#[cfg(test)]
impl ManualClock {
    pub fn new(now: OffsetDateTime) -> Self {
        Self {
            now: Arc::new(std::sync::RwLock::new(now)),
        }
    }

    pub fn advance(&self, duration: time::Duration) {
        if let Ok(mut now) = self.now.write() {
            *now += duration;
        }
    }
}

#[cfg(test)]
impl Clock for ManualClock {
    fn now(&self) -> OffsetDateTime {
        self.now
            .read()
            .map_or(OffsetDateTime::UNIX_EPOCH, |now| *now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_compact_picks_the_largest_whole_unit() {
        assert_eq!(ago_compact(0), "now");
        assert_eq!(ago_compact(59), "now");
        assert_eq!(ago_compact(60), "1m");
        assert_eq!(ago_compact(2 * 60 * 60 + 40 * 60), "2h");
        assert_eq!(ago_compact(6 * 24 * 60 * 60 + 3600), "6d");
    }
}
