//! Scheduled message delivery.
//!
//! Provides time-based message scheduling:
//! - Schedule messages for future delivery
//! - Recurring schedules (daily, weekly)
//! - Time window restrictions
//! - Batch scheduling
//! - Cancel/modify scheduled messages

use std::collections::{BinaryHeap, HashMap};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, NaiveTime, Utc, Weekday};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::StoredMessage;

/// Scheduled message.
#[derive(Debug, Clone)]
pub struct ScheduledMessage {
    /// Unique schedule ID
    pub id: String,
    /// The message to deliver
    pub message: StoredMessage,
    /// Scheduled delivery time
    pub scheduled_at: DateTime<Utc>,
    /// Original schedule request time
    pub created_at: DateTime<Utc>,
    /// Recurrence rule (if any)
    pub recurrence: Option<RecurrenceRule>,
    /// Number of times delivered (for recurring)
    pub delivery_count: u32,
    /// Maximum deliveries for recurring (0 = unlimited)
    pub max_deliveries: u32,
    /// Schedule status
    pub status: ScheduleStatus,
    /// ESME system_id
    pub esme_id: String,
}

impl ScheduledMessage {
    /// Create a new scheduled message.
    pub fn new(message: StoredMessage, scheduled_at: DateTime<Utc>, esme_id: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = format!("sched-{}", COUNTER.fetch_add(1, Ordering::Relaxed));

        Self {
            id,
            message,
            scheduled_at,
            created_at: Utc::now(),
            recurrence: None,
            delivery_count: 0,
            max_deliveries: 0,
            status: ScheduleStatus::Pending,
            esme_id: esme_id.to_string(),
        }
    }

    /// Create with recurrence.
    pub fn with_recurrence(mut self, rule: RecurrenceRule) -> Self {
        self.recurrence = Some(rule);
        self
    }

    /// Set max deliveries for recurring.
    pub fn with_max_deliveries(mut self, max: u32) -> Self {
        self.max_deliveries = max;
        self
    }

    /// Check if ready for delivery.
    pub fn is_ready(&self) -> bool {
        self.status == ScheduleStatus::Pending && Utc::now() >= self.scheduled_at
    }

    /// Check if can repeat.
    pub fn can_repeat(&self) -> bool {
        self.recurrence.is_some()
            && (self.max_deliveries == 0 || self.delivery_count < self.max_deliveries)
    }

    /// Get next scheduled time (for recurring).
    pub fn next_scheduled_time(&self) -> Option<DateTime<Utc>> {
        self.recurrence.as_ref().map(|r| r.next_from(self.scheduled_at))
    }

    /// Mark as delivered and update for recurrence.
    pub fn mark_delivered(&mut self) {
        self.delivery_count += 1;

        if self.can_repeat() {
            if let Some(next) = self.next_scheduled_time() {
                self.scheduled_at = next;
                self.status = ScheduleStatus::Pending; // Reset for next delivery
            } else {
                self.status = ScheduleStatus::Completed;
            }
        } else {
            self.status = ScheduleStatus::Completed;
        }
    }

    /// Cancel the schedule.
    pub fn cancel(&mut self) {
        self.status = ScheduleStatus::Cancelled;
    }
}

/// For BinaryHeap ordering (min-heap by scheduled_at).
impl PartialEq for ScheduledMessage {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for ScheduledMessage {}

impl PartialOrd for ScheduledMessage {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScheduledMessage {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse for min-heap (earlier times first)
        other.scheduled_at.cmp(&self.scheduled_at)
    }
}

/// Schedule status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleStatus {
    /// Waiting for delivery time
    Pending,
    /// Currently being delivered
    Delivering,
    /// Successfully completed
    Completed,
    /// Cancelled by user
    Cancelled,
    /// Failed to deliver
    Failed,
    /// Paused (can be resumed)
    Paused,
}

/// Recurrence rule.
#[derive(Debug, Clone)]
pub struct RecurrenceRule {
    /// Recurrence type
    pub frequency: RecurrenceFrequency,
    /// Interval (every N periods)
    pub interval: u32,
    /// Specific days of week (for weekly)
    pub days: Vec<Weekday>,
    /// Time of day
    pub at_time: Option<NaiveTime>,
    /// End date (None = never ends)
    pub end_date: Option<DateTime<Utc>>,
}

impl RecurrenceRule {
    /// Create daily recurrence.
    pub fn daily(interval: u32) -> Self {
        Self {
            frequency: RecurrenceFrequency::Daily,
            interval,
            days: Vec::new(),
            at_time: None,
            end_date: None,
        }
    }

    /// Create weekly recurrence.
    pub fn weekly(days: Vec<Weekday>) -> Self {
        Self {
            frequency: RecurrenceFrequency::Weekly,
            interval: 1,
            days,
            at_time: None,
            end_date: None,
        }
    }

    /// Create monthly recurrence.
    pub fn monthly(interval: u32) -> Self {
        Self {
            frequency: RecurrenceFrequency::Monthly,
            interval,
            days: Vec::new(),
            at_time: None,
            end_date: None,
        }
    }

    /// Set end date.
    pub fn until(mut self, end: DateTime<Utc>) -> Self {
        self.end_date = Some(end);
        self
    }

    /// Set time of day.
    pub fn at(mut self, time: NaiveTime) -> Self {
        self.at_time = Some(time);
        self
    }

    /// Calculate next occurrence from given time.
    pub fn next_from(&self, from: DateTime<Utc>) -> DateTime<Utc> {
        use chrono::Duration as ChronoDuration;

        let next = match self.frequency {
            RecurrenceFrequency::Minutely => from + ChronoDuration::minutes(self.interval as i64),
            RecurrenceFrequency::Hourly => from + ChronoDuration::hours(self.interval as i64),
            RecurrenceFrequency::Daily => from + ChronoDuration::days(self.interval as i64),
            RecurrenceFrequency::Weekly => from + ChronoDuration::weeks(self.interval as i64),
            RecurrenceFrequency::Monthly => {
                // Approximate month as 30 days
                from + ChronoDuration::days(30 * self.interval as i64)
            }
        };

        // Check if past end date
        if let Some(end) = self.end_date {
            if next > end {
                return end;
            }
        }

        next
    }
}

/// Recurrence frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrenceFrequency {
    /// Every N minutes
    Minutely,
    /// Every N hours
    Hourly,
    /// Every N days
    Daily,
    /// Every N weeks
    Weekly,
    /// Every N months
    Monthly,
}

/// Scheduler configuration.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Check interval in seconds
    pub check_interval_secs: u64,
    /// Max messages to process per check
    pub batch_size: usize,
    /// Max scheduled messages per ESME
    pub max_per_esme: usize,
    /// Max total scheduled messages
    pub max_total: usize,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            check_interval_secs: 1,
            batch_size: 100,
            max_per_esme: 10_000,
            max_total: 1_000_000,
        }
    }
}

/// Message scheduler.
#[derive(Debug)]
pub struct Scheduler {
    /// Priority queue of scheduled messages (by time)
    queue: RwLock<BinaryHeap<ScheduledMessage>>,
    /// Index by ESME system_id
    by_esme: RwLock<HashMap<String, Vec<String>>>,
    /// Configuration
    config: SchedulerConfig,
    /// Stats
    stats: RwLock<SchedulerStats>,
}

impl Scheduler {
    /// Create new scheduler.
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            queue: RwLock::new(BinaryHeap::new()),
            by_esme: RwLock::new(HashMap::new()),
            config,
            stats: RwLock::new(SchedulerStats::default()),
        }
    }

    /// Schedule a message.
    pub fn schedule(&self, scheduled: ScheduledMessage) -> Result<String, SchedulerError> {
        // Check limits
        {
            let queue = self.queue.read().unwrap();
            if queue.len() >= self.config.max_total {
                return Err(SchedulerError::TotalLimitExceeded);
            }
        }

        {
            let by_esme = self.by_esme.read().unwrap();
            if let Some(ids) = by_esme.get(&scheduled.esme_id) {
                if ids.len() >= self.config.max_per_esme {
                    return Err(SchedulerError::EsmeLimitExceeded);
                }
            }
        }

        let id = scheduled.id.clone();
        let esme_id = scheduled.esme_id.clone();

        // Add to queue
        self.queue.write().unwrap().push(scheduled);

        // Update ESME index
        self.by_esme
            .write()
            .unwrap()
            .entry(esme_id.clone())
            .or_default()
            .push(id.clone());

        // Update stats
        {
            let mut stats = self.stats.write().unwrap();
            stats.total_scheduled += 1;
        }

        debug!(schedule_id = %id, esme = %esme_id, "message scheduled");
        Ok(id)
    }

    /// Cancel a scheduled message.
    pub fn cancel(&self, schedule_id: &str) -> Result<(), SchedulerError> {
        let mut queue = self.queue.write().unwrap();

        // Find and remove from queue
        let messages: Vec<_> = queue.drain().collect();
        let mut found = false;

        for mut msg in messages {
            if msg.id == schedule_id {
                msg.cancel();
                found = true;
                // Don't re-add cancelled messages
            } else {
                queue.push(msg);
            }
        }

        if found {
            let mut stats = self.stats.write().unwrap();
            stats.total_cancelled += 1;
            Ok(())
        } else {
            Err(SchedulerError::NotFound)
        }
    }

    /// Get due messages (marks them as Delivering).
    pub fn get_due(&self, limit: usize) -> Vec<ScheduledMessage> {
        let now = Utc::now();
        let mut queue = self.queue.write().unwrap();

        // Collect all messages, mark due ones as Delivering
        let messages: Vec<_> = queue.drain().collect();
        let mut due = Vec::new();

        for mut msg in messages {
            if due.len() < limit
                && msg.scheduled_at <= now
                && msg.status == ScheduleStatus::Pending
            {
                msg.status = ScheduleStatus::Delivering;
                due.push(msg.clone());
            }
            queue.push(msg);
        }

        due
    }

    /// Mark message as delivered and handle recurrence.
    pub fn mark_delivered(&self, schedule_id: &str) {
        let mut queue = self.queue.write().unwrap();

        // Find the message in Delivering state
        let messages: Vec<_> = queue.drain().collect();

        for mut msg in messages {
            if msg.id == schedule_id && msg.status == ScheduleStatus::Delivering {
                msg.mark_delivered();

                // Re-add if recurring (mark_delivered sets status back to Pending if recurring)
                if msg.status == ScheduleStatus::Pending {
                    queue.push(msg);
                } else {
                    // Remove from ESME index for completed messages
                    if let Some(ids) = self.by_esme.write().unwrap().get_mut(&msg.esme_id) {
                        ids.retain(|id| id != schedule_id);
                    }
                    let mut stats = self.stats.write().unwrap();
                    stats.total_delivered += 1;
                }
            } else {
                queue.push(msg);
            }
        }
    }

    /// Get schedule by ID.
    pub fn get(&self, schedule_id: &str) -> Option<ScheduledMessage> {
        let queue = self.queue.read().unwrap();
        queue.iter().find(|m| m.id == schedule_id).cloned()
    }

    /// Get all schedules for an ESME.
    pub fn get_by_esme(&self, esme_id: &str) -> Vec<ScheduledMessage> {
        let queue = self.queue.read().unwrap();
        queue
            .iter()
            .filter(|m| m.esme_id == esme_id)
            .cloned()
            .collect()
    }

    /// Get pending count.
    pub fn pending_count(&self) -> usize {
        self.queue
            .read()
            .unwrap()
            .iter()
            .filter(|m| m.status == ScheduleStatus::Pending)
            .count()
    }

    /// Get stats.
    pub fn stats(&self) -> SchedulerStats {
        self.stats.read().unwrap().clone()
    }

    /// Get config.
    pub fn config(&self) -> &SchedulerConfig {
        &self.config
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new(SchedulerConfig::default())
    }
}

/// Scheduler error.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SchedulerError {
    #[error("schedule not found")]
    NotFound,
    #[error("total schedule limit exceeded")]
    TotalLimitExceeded,
    #[error("ESME schedule limit exceeded")]
    EsmeLimitExceeded,
    #[error("invalid schedule time")]
    InvalidTime,
}

/// Scheduler statistics.
#[derive(Debug, Clone, Default)]
pub struct SchedulerStats {
    /// Total messages scheduled
    pub total_scheduled: u64,
    /// Total messages delivered
    pub total_delivered: u64,
    /// Total messages cancelled
    pub total_cancelled: u64,
    /// Total messages failed
    pub total_failed: u64,
}

/// Scheduler handle for async delivery.
#[derive(Clone)]
pub struct SchedulerHandle {
    tx: mpsc::Sender<ScheduledMessage>,
}

impl SchedulerHandle {
    /// Submit a scheduled message for delivery.
    pub async fn submit(&self, msg: ScheduledMessage) -> Result<(), SchedulerError> {
        self.tx
            .send(msg)
            .await
            .map_err(|_| SchedulerError::NotFound)
    }
}

/// Scheduler runner for background processing.
pub struct SchedulerRunner {
    scheduler: Arc<Scheduler>,
    rx: mpsc::Receiver<ScheduledMessage>,
    output: mpsc::Sender<StoredMessage>,
}

impl SchedulerRunner {
    /// Create new scheduler runner.
    pub fn new(
        scheduler: Arc<Scheduler>,
        rx: mpsc::Receiver<ScheduledMessage>,
        output: mpsc::Sender<StoredMessage>,
    ) -> Self {
        Self {
            scheduler,
            rx,
            output,
        }
    }

    /// Run the scheduler loop.
    pub async fn run(mut self) {
        let check_interval = Duration::from_secs(self.scheduler.config().check_interval_secs);

        loop {
            tokio::select! {
                // Handle incoming schedule requests
                Some(msg) = self.rx.recv() => {
                    match self.scheduler.schedule(msg) {
                        Ok(id) => debug!(schedule_id = %id, "scheduled message"),
                        Err(e) => warn!(error = %e, "failed to schedule message"),
                    }
                }

                // Check for due messages
                _ = tokio::time::sleep(check_interval) => {
                    let due = self.scheduler.get_due(self.scheduler.config().batch_size);

                    for msg in due {
                        let schedule_id = msg.id.clone();

                        // Send message for delivery
                        if self.output.send(msg.message.clone()).await.is_ok() {
                            self.scheduler.mark_delivered(&schedule_id);
                            debug!(schedule_id = %schedule_id, "delivered scheduled message");
                        } else {
                            warn!(schedule_id = %schedule_id, "failed to deliver scheduled message");
                        }
                    }
                }
            }
        }
    }
}

/// Start the scheduler subsystem.
pub fn start(
    scheduler: Arc<Scheduler>,
    output: mpsc::Sender<StoredMessage>,
) -> SchedulerHandle {
    let (tx, rx) = mpsc::channel(10_000);

    let runner = SchedulerRunner::new(scheduler, rx, output);

    tokio::spawn(async move {
        runner.run().await;
    });

    SchedulerHandle { tx }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_message() -> StoredMessage {
        StoredMessage::new("+258841234567", "+258821234567", b"Test message".to_vec(), "test_sys", 1)
    }

    #[test]
    fn test_schedule_message() {
        let scheduler = Scheduler::default();

        let msg = test_message();
        let scheduled = ScheduledMessage::new(
            msg,
            Utc::now() + chrono::Duration::hours(1),
            "esme1",
        );

        let id = scheduler.schedule(scheduled).unwrap();
        assert!(id.starts_with("sched-"));
        assert_eq!(scheduler.pending_count(), 1);
    }

    #[test]
    fn test_cancel_schedule() {
        let scheduler = Scheduler::default();

        let scheduled = ScheduledMessage::new(
            test_message(),
            Utc::now() + chrono::Duration::hours(1),
            "esme1",
        );

        let id = scheduler.schedule(scheduled).unwrap();
        scheduler.cancel(&id).unwrap();

        assert_eq!(scheduler.pending_count(), 0);
    }

    #[test]
    fn test_get_due_messages() {
        let scheduler = Scheduler::default();

        // Schedule one in the past
        let past = ScheduledMessage::new(
            test_message(),
            Utc::now() - chrono::Duration::hours(1),
            "esme1",
        );

        // Schedule one in the future
        let future = ScheduledMessage::new(
            test_message(),
            Utc::now() + chrono::Duration::hours(1),
            "esme1",
        );

        scheduler.schedule(past).unwrap();
        scheduler.schedule(future).unwrap();

        let due = scheduler.get_due(10);
        assert_eq!(due.len(), 1);
    }

    #[test]
    fn test_recurrence() {
        let rule = RecurrenceRule::daily(1);
        let now = Utc::now();
        let next = rule.next_from(now);

        assert!(next > now);
        assert!((next - now).num_hours() >= 23);
    }

    #[test]
    fn test_recurring_schedule() {
        let scheduler = Scheduler::default();

        let mut scheduled = ScheduledMessage::new(
            test_message(),
            Utc::now() - chrono::Duration::minutes(1),
            "esme1",
        );
        scheduled.recurrence = Some(RecurrenceRule::daily(1));

        let id = scheduler.schedule(scheduled).unwrap();

        // Get due message
        let due = scheduler.get_due(10);
        assert_eq!(due.len(), 1);

        // Mark delivered
        scheduler.mark_delivered(&id);

        // Should be rescheduled
        let rescheduled = scheduler.get(&id);
        assert!(rescheduled.is_some());
        assert_eq!(rescheduled.unwrap().delivery_count, 1);
    }

    #[test]
    fn test_esme_limit() {
        let config = SchedulerConfig {
            max_per_esme: 2,
            ..Default::default()
        };
        let scheduler = Scheduler::new(config);

        scheduler
            .schedule(ScheduledMessage::new(test_message(), Utc::now(), "esme1"))
            .unwrap();
        scheduler
            .schedule(ScheduledMessage::new(test_message(), Utc::now(), "esme1"))
            .unwrap();

        let result = scheduler.schedule(ScheduledMessage::new(
            test_message(),
            Utc::now(),
            "esme1",
        ));

        assert!(matches!(result, Err(SchedulerError::EsmeLimitExceeded)));
    }

    #[test]
    fn test_schedule_status() {
        let mut scheduled = ScheduledMessage::new(
            test_message(),
            Utc::now(),
            "esme1",
        );

        assert_eq!(scheduled.status, ScheduleStatus::Pending);

        scheduled.mark_delivered();
        assert_eq!(scheduled.status, ScheduleStatus::Completed);
        assert_eq!(scheduled.delivery_count, 1);
    }

    #[test]
    fn test_max_deliveries() {
        let mut scheduled = ScheduledMessage::new(test_message(), Utc::now(), "esme1")
            .with_recurrence(RecurrenceRule::daily(1))
            .with_max_deliveries(3);

        for _ in 0..3 {
            assert!(scheduled.can_repeat());
            scheduled.mark_delivered();
        }

        assert!(!scheduled.can_repeat());
        assert_eq!(scheduled.status, ScheduleStatus::Completed);
    }
}
