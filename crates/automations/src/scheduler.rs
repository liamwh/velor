//! Cron-based scheduling for automations with timezone support.

use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan};
use jiff_cron::Schedule;
use std::str::FromStr;

/// Automation scheduler using cron expressions.
#[derive(Debug)]
pub struct Scheduler {
    schedule: Schedule,
    timezone: TimeZone,
}

impl Scheduler {
    /// Create a new scheduler from a cron expression and timezone.
    ///
    /// # Errors
    ///
    /// Returns an error if the cron expression is invalid or doesn't have 6 fields.
    #[tracing::instrument(level = "debug", ret, err, fields(cron_expression = %cron_expression, timezone = ?timezone))]
    pub fn new(cron_expression: &str, timezone: TimeZone) -> color_eyre::Result<Self> {
        // Validate 6-field format
        let parts: Vec<&str> = cron_expression.split_whitespace().collect();
        if parts.len() != 6 {
            return Err(color_eyre::eyre::eyre!(
                "Invalid cron expression: expected 6 fields (seconds minutes hours day month weekday), got {}",
                parts.len()
            ));
        }

        let schedule = Schedule::from_str(cron_expression)?;
        Ok(Self { schedule, timezone })
    }

    /// Get the next scheduled time after the given timestamp.
    #[must_use]
    #[tracing::instrument(level = "trace", ret, skip(self))]
    pub fn next_after(&self, after: Timestamp) -> Timestamp {
        // Attach the schedule's timezone so DST gaps/folds resolve correctly.
        let after_zoned = after.to_zoned(self.timezone.clone());
        self.schedule
            .after(after_zoned)
            .next()
            .map(|zdt| zdt.timestamp())
            .unwrap_or_else(|| {
                // If no next time (shouldn't happen with valid cron), return far future.
                Timestamp::now() + (365_i64 * 100).days()
            })
    }

    /// Calculate all missed schedules between last run and now.
    ///
    /// # Arguments
    ///
    /// * `last_run` - The last time this automation was run
    /// * `now` - The current time
    /// * `max_count` - Maximum number of missed runs to return
    ///
    /// # Returns
    ///
    /// A vector of scheduled times that were missed.
    #[must_use]
    #[tracing::instrument(level = "trace", ret, skip(self))]
    pub fn missed_runs_since(
        &self,
        last_run: Timestamp,
        now: Timestamp,
        max_count: u32,
    ) -> Vec<Timestamp> {
        let mut missed = Vec::new();
        let mut current = self.next_after(last_run);

        while current <= now && missed.len() < max_count as usize {
            missed.push(current);
            current = self.next_after(current);
        }

        missed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scheduler_requires_6_fields() {
        assert!(Scheduler::new("0 * * * *", TimeZone::UTC).is_err()); // 5 fields
        assert!(Scheduler::new("0 0 * * * *", TimeZone::UTC).is_ok()); // 6 fields
    }

    #[test]
    fn test_scheduler_next_after() {
        // Run at the top of every hour
        let scheduler =
            Scheduler::new("0 0 * * * *", TimeZone::UTC).expect("scheduler should be created");

        let base: Timestamp = "2024-01-01T12:30:00Z".parse().unwrap();

        let next = scheduler.next_after(base);
        let next_zoned = next.to_zoned(TimeZone::UTC);
        // Should be at 13:00:00
        assert_eq!(next_zoned.minute(), 0);
        assert_eq!(next_zoned.second(), 0);
        assert_eq!(next_zoned.hour(), 13);
    }

    #[test]
    fn test_scheduler_missed_runs() {
        // Run every minute
        let scheduler =
            Scheduler::new("0 * * * * *", TimeZone::UTC).expect("scheduler should be created");

        let last_run: Timestamp = "2024-01-01T12:00:00Z".parse().unwrap();
        let now: Timestamp = "2024-01-01T12:05:30Z".parse().unwrap();

        let missed = scheduler.missed_runs_since(last_run, now, 10);

        // Should have missed runs at 12:01, 12:02, 12:03, 12:04, 12:05
        assert_eq!(missed.len(), 5);
        assert_eq!(missed[0].to_zoned(TimeZone::UTC).minute(), 1);
        assert_eq!(missed[4].to_zoned(TimeZone::UTC).minute(), 5);
    }

    #[test]
    fn test_scheduler_missed_runs_respects_max_count() {
        // Run every minute
        let scheduler =
            Scheduler::new("0 * * * * *", TimeZone::UTC).expect("scheduler should be created");

        let last_run: Timestamp = "2024-01-01T12:00:00Z".parse().unwrap();
        let now: Timestamp = "2024-01-01T12:10:00Z".parse().unwrap();

        let missed = scheduler.missed_runs_since(last_run, now, 3);

        // Should only return 3 despite 10 being available
        assert_eq!(missed.len(), 3);
    }

    #[test]
    fn test_scheduler_timezone_conversion() {
        // Test with a timezone that has an offset
        let tz = TimeZone::get("America/New_York").expect("valid IANA timezone");
        let scheduler = Scheduler::new("0 0 12 * * *", tz).expect("scheduler should be created");

        // Noon in New York time
        let base: Timestamp = "2024-01-01T12:00:00-05:00".parse().unwrap();

        let next = scheduler.next_after(base);

        // The next scheduled time should still be computed correctly
        // It should be at the next noon in New York time
        assert!(next > base);
    }

    /// DST spring-forward: 2024-03-10 in America/New_York loses the 2:00-3:00
    /// AM hour. A daily 2:30 AM schedule must resolve to a valid instant
    /// strictly after the given time, with no panic.
    #[test]
    fn test_scheduler_handles_dst_spring_forward() {
        let tz = TimeZone::get("America/New_York").expect("valid IANA timezone");
        let scheduler = Scheduler::new("0 30 2 * * *", tz).expect("scheduler should be created");

        let before_dst: Timestamp = "2024-03-10T02:30:00-05:00".parse().unwrap();
        let next = scheduler.next_after(before_dst);

        assert!(next > before_dst);
    }

    /// DST fall-back: 2024-11-03 in America/New_York repeats the 1:00-2:00 AM
    /// hour. Two successive `next_after` calls starting before the repeated
    /// hour must both resolve to instants strictly after their input, and
    /// must not collapse to the same instant.
    #[test]
    fn test_scheduler_handles_dst_fall_back() {
        let tz = TimeZone::get("America/New_York").expect("valid IANA timezone");
        let scheduler = Scheduler::new("0 30 1 * * *", tz).expect("scheduler should be created");

        let before_fall_back: Timestamp = "2024-11-03T00:30:00-04:00".parse().unwrap();
        let next = scheduler.next_after(before_fall_back);
        assert!(next > before_fall_back);

        let next_next = scheduler.next_after(next);
        assert!(next_next > next);
    }

    /// A weekday-anchored schedule (Monday 14:00) resolves correctly across a
    /// DST boundary in both directions (March spring-forward, November
    /// fall-back).
    #[test]
    fn test_scheduler_weekday_anchor_across_dst() {
        let tz = TimeZone::get("America/New_York").expect("valid IANA timezone");
        let scheduler = Scheduler::new("0 0 14 * * Mon", tz).expect("scheduler should be created");

        let march_monday: Timestamp = "2024-03-11T14:00:00Z".parse().unwrap();
        let next_march = scheduler.next_after(march_monday);
        assert!(next_march > march_monday);

        let november_monday: Timestamp = "2024-11-11T14:00:00Z".parse().unwrap();
        let next_november = scheduler.next_after(november_monday);
        assert!(next_november > november_monday);
    }
}
