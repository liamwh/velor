//! Cron-based scheduling for automations with timezone support.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use std::str::FromStr;

/// Automation scheduler using cron expressions.
#[derive(Debug)]
pub struct Scheduler {
    schedule: Schedule,
    timezone: Tz,
}

impl Scheduler {
    /// Create a new scheduler from a cron expression and timezone.
    ///
    /// # Errors
    ///
    /// Returns an error if the cron expression is invalid or doesn't have 6 fields.
    #[tracing::instrument(level = "debug", ret, err, fields(cron_expression = %cron_expression, timezone = %timezone))]
    pub fn new(cron_expression: &str, timezone: Tz) -> color_eyre::Result<Self> {
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
    pub fn next_after(&self, after: DateTime<Utc>) -> DateTime<Utc> {
        // Convert to timezone for calculation
        let after_tz = after.with_timezone(&self.timezone);
        self.schedule
            .after(&after_tz)
            .next()
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|| {
                // If no next time (shouldn't happen with valid cron), return far future
                Utc::now() + chrono::Duration::days(365 * 100)
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
        last_run: DateTime<Utc>,
        now: DateTime<Utc>,
        max_count: u32,
    ) -> Vec<DateTime<Utc>> {
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
    use chrono::Timelike;

    #[test]
    fn test_scheduler_requires_6_fields() {
        let tz = chrono_tz::UTC;
        assert!(Scheduler::new("0 * * * *", tz).is_err()); // 5 fields
        assert!(Scheduler::new("0 0 * * * *", tz).is_ok()); // 6 fields
    }

    #[test]
    fn test_scheduler_next_after() {
        let tz = chrono_tz::UTC;
        // Run at the top of every hour
        let scheduler = Scheduler::new("0 0 * * * *", tz).expect("scheduler should be created");

        let base = DateTime::parse_from_rfc3339("2024-01-01T12:30:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let next = scheduler.next_after(base);
        // Should be at 13:00:00
        assert_eq!(next.minute(), 0);
        assert_eq!(next.second(), 0);
        assert_eq!(next.hour(), 13);
    }

    #[test]
    fn test_scheduler_missed_runs() {
        let tz = chrono_tz::UTC;
        // Run every minute
        let scheduler = Scheduler::new("0 * * * * *", tz).expect("scheduler should be created");

        let last_run = DateTime::parse_from_rfc3339("2024-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let now = DateTime::parse_from_rfc3339("2024-01-01T12:05:30Z")
            .unwrap()
            .with_timezone(&Utc);

        let missed = scheduler.missed_runs_since(last_run, now, 10);

        // Should have missed runs at 12:01, 12:02, 12:03, 12:04, 12:05
        assert_eq!(missed.len(), 5);
        assert_eq!(missed[0].minute(), 1);
        assert_eq!(missed[4].minute(), 5);
    }

    #[test]
    fn test_scheduler_missed_runs_respects_max_count() {
        let tz = chrono_tz::UTC;
        // Run every minute
        let scheduler = Scheduler::new("0 * * * * *", tz).expect("scheduler should be created");

        let last_run = DateTime::parse_from_rfc3339("2024-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let now = DateTime::parse_from_rfc3339("2024-01-01T12:10:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let missed = scheduler.missed_runs_since(last_run, now, 3);

        // Should only return 3 despite 10 being available
        assert_eq!(missed.len(), 3);
    }

    #[test]
    fn test_scheduler_timezone_conversion() {
        // Test with a timezone that has an offset
        let tz = chrono_tz::America::New_York;
        let scheduler = Scheduler::new("0 0 12 * * *", tz).expect("scheduler should be created");

        // Noon in New York time
        let base = DateTime::parse_from_rfc3339("2024-01-01T12:00:00-05:00")
            .unwrap()
            .with_timezone(&Utc);

        let next = scheduler.next_after(base);

        // The next scheduled time should still be computed correctly
        // It should be at the next noon in New York time
        assert!(next > base);
    }
}
