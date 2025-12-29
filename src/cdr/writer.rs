//! CDR writer implementations.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;
use tracing::{debug, info};

use super::types::Cdr;

/// CDR writer errors.
#[derive(Debug, Error)]
pub enum CdrError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("writer not available")]
    Unavailable,
}

/// CDR writer configuration.
#[derive(Debug, Clone)]
pub struct CdrConfig {
    /// Output format
    pub format: CdrFormat,
    /// Include message content (privacy concern)
    pub include_content: bool,
    /// Max content length to include
    pub max_content_length: usize,
    /// Rotate files daily
    pub rotate_daily: bool,
    /// Compress rotated files
    pub compress_rotated: bool,
}

impl Default for CdrConfig {
    fn default() -> Self {
        Self {
            format: CdrFormat::Json,
            include_content: false,
            max_content_length: 50,
            rotate_daily: true,
            compress_rotated: false,
        }
    }
}

/// CDR output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CdrFormat {
    /// JSON lines (one JSON object per line)
    Json,
    /// CSV format
    Csv,
    /// Custom delimited format
    Delimited(char),
}

/// CDR writer trait.
#[async_trait]
pub trait CdrWriter: Send + Sync + std::fmt::Debug {
    /// Write a CDR record.
    async fn write(&self, cdr: &Cdr) -> Result<(), CdrError>;

    /// Flush pending writes.
    async fn flush(&self) -> Result<(), CdrError>;

    /// Writer name for logging.
    fn name(&self) -> &str;
}

/// File-based CDR writer.
#[derive(Debug)]
pub struct FileCdrWriter {
    name: String,
    base_path: PathBuf,
    config: CdrConfig,
    current_file: RwLock<Option<CurrentFile>>,
}

#[derive(Debug)]
struct CurrentFile {
    writer: BufWriter<File>,
    date: chrono::NaiveDate,
    records: u64,
}

impl FileCdrWriter {
    /// Create new file CDR writer.
    pub fn new(name: &str, base_path: PathBuf, config: CdrConfig) -> Self {
        Self {
            name: name.to_string(),
            base_path,
            config,
            current_file: RwLock::new(None),
        }
    }

    /// Get current file, rotating if needed.
    fn get_or_create_file(&self) -> Result<(), CdrError> {
        let today = Utc::now().date_naive();

        let mut current = self.current_file.write().unwrap();

        // Check if we need rotation
        let needs_rotation = match &*current {
            Some(cf) if self.config.rotate_daily && cf.date != today => true,
            None => true,
            _ => false,
        };

        if needs_rotation {
            // Close existing file
            if let Some(mut cf) = current.take() {
                cf.writer.flush()?;
                info!(
                    writer = %self.name,
                    records = cf.records,
                    date = %cf.date,
                    "rotated CDR file"
                );
            }

            // Create new file
            let filename = self.filename_for_date(today);
            let path = self.base_path.join(&filename);

            // Ensure parent directory exists
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }

            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;

            let mut writer = BufWriter::new(file);

            // Write header for CSV
            if self.config.format == CdrFormat::Csv {
                writeln!(writer, "{}", Cdr::csv_header())?;
            }

            info!(
                writer = %self.name,
                path = %path.display(),
                "created new CDR file"
            );

            *current = Some(CurrentFile {
                writer,
                date: today,
                records: 0,
            });
        }

        Ok(())
    }

    fn filename_for_date(&self, date: chrono::NaiveDate) -> String {
        let ext = match self.config.format {
            CdrFormat::Json => "jsonl",
            CdrFormat::Csv => "csv",
            CdrFormat::Delimited(_) => "txt",
        };
        format!("cdr_{}.{}", date.format("%Y%m%d"), ext)
    }

    fn format_cdr(&self, cdr: &Cdr) -> Result<String, CdrError> {
        match self.config.format {
            CdrFormat::Json => {
                serde_json::to_string(cdr)
                    .map_err(|e| CdrError::Serialization(e.to_string()))
            }
            CdrFormat::Csv => Ok(cdr.to_csv_line()),
            CdrFormat::Delimited(delim) => {
                // Simple delimited format
                Ok(format!(
                    "{}{}{}{}{}{}{}{}{}",
                    cdr.timestamp,
                    delim,
                    cdr.cdr_type as u8,
                    delim,
                    cdr.message_id,
                    delim,
                    cdr.esme_id,
                    delim,
                    cdr.status_code,
                ))
            }
        }
    }
}

#[async_trait]
impl CdrWriter for FileCdrWriter {
    async fn write(&self, cdr: &Cdr) -> Result<(), CdrError> {
        self.get_or_create_file()?;

        let line = self.format_cdr(cdr)?;

        let mut current = self.current_file.write().unwrap();
        if let Some(cf) = current.as_mut() {
            writeln!(cf.writer, "{}", line)?;
            cf.records += 1;

            debug!(
                writer = %self.name,
                message_id = %cdr.message_id,
                "wrote CDR"
            );
        }

        Ok(())
    }

    async fn flush(&self) -> Result<(), CdrError> {
        let mut current = self.current_file.write().unwrap();
        if let Some(cf) = current.as_mut() {
            cf.writer.flush()?;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// In-memory CDR writer (for testing/debugging).
#[derive(Debug)]
pub struct MemoryCdrWriter {
    name: String,
    records: RwLock<VecDeque<Cdr>>,
    max_records: usize,
}

impl MemoryCdrWriter {
    /// Create new memory CDR writer.
    pub fn new(name: &str, max_records: usize) -> Self {
        Self {
            name: name.to_string(),
            records: RwLock::new(VecDeque::with_capacity(max_records)),
            max_records,
        }
    }

    /// Get recent CDRs.
    pub fn recent(&self, count: usize) -> Vec<Cdr> {
        let records = self.records.read().unwrap();
        records.iter().rev().take(count).cloned().collect()
    }

    /// Get CDRs by ESME.
    pub fn by_esme(&self, esme_id: &str, count: usize) -> Vec<Cdr> {
        let records = self.records.read().unwrap();
        records
            .iter()
            .rev()
            .filter(|c| c.esme_id == esme_id)
            .take(count)
            .cloned()
            .collect()
    }

    /// Get CDRs in time range.
    pub fn in_range(&self, start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<Cdr> {
        let records = self.records.read().unwrap();
        records
            .iter()
            .filter(|c| {
                c.timestamp_datetime()
                    .map(|ts| ts >= start && ts <= end)
                    .unwrap_or(false)
            })
            .cloned()
            .collect()
    }

    /// Get total count.
    pub fn count(&self) -> usize {
        self.records.read().unwrap().len()
    }

    /// Calculate totals.
    pub fn totals(&self) -> CdrTotals {
        let records = self.records.read().unwrap();
        let mut totals = CdrTotals::default();

        for cdr in records.iter() {
            totals.count += 1;
            totals.segments += cdr.segments as u64;
            if let Some(cost) = cdr.cost {
                totals.cost += cost;
            }
            if let Some(revenue) = cdr.revenue {
                totals.revenue += revenue;
            }
        }

        totals.margin = totals.revenue - totals.cost;
        totals
    }

    /// Clear all records.
    pub fn clear(&self) {
        self.records.write().unwrap().clear();
    }
}

#[async_trait]
impl CdrWriter for MemoryCdrWriter {
    async fn write(&self, cdr: &Cdr) -> Result<(), CdrError> {
        let mut records = self.records.write().unwrap();

        if records.len() >= self.max_records {
            records.pop_front();
        }

        records.push_back(cdr.clone());

        debug!(
            writer = %self.name,
            message_id = %cdr.message_id,
            total = records.len(),
            "wrote CDR to memory"
        );

        Ok(())
    }

    async fn flush(&self) -> Result<(), CdrError> {
        Ok(()) // No-op for memory writer
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// CDR totals.
#[derive(Debug, Clone, Default)]
pub struct CdrTotals {
    pub count: u64,
    pub segments: u64,
    pub cost: i64,
    pub revenue: i64,
    pub margin: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdr::types::CdrType;

    #[tokio::test]
    async fn test_memory_writer() {
        let writer = MemoryCdrWriter::new("test", 100);

        let cdr = Cdr::submit("msg1", "esme1", "SENDER", "+1234567890");
        writer.write(&cdr).await.unwrap();

        assert_eq!(writer.count(), 1);

        let recent = writer.recent(10);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].message_id, "msg1");
    }

    #[tokio::test]
    async fn test_memory_writer_max_records() {
        let writer = MemoryCdrWriter::new("test", 2);

        writer.write(&Cdr::submit("msg1", "c", "S", "D")).await.unwrap();
        writer.write(&Cdr::submit("msg2", "c", "S", "D")).await.unwrap();
        writer.write(&Cdr::submit("msg3", "c", "S", "D")).await.unwrap();

        assert_eq!(writer.count(), 2);

        let recent = writer.recent(10);
        assert_eq!(recent[0].message_id, "msg3");
        assert_eq!(recent[1].message_id, "msg2");
    }

    #[tokio::test]
    async fn test_totals() {
        let writer = MemoryCdrWriter::new("test", 100);

        let cdr1 = Cdr::submit("msg1", "c", "S", "D")
            .with_pricing(100, 150, "MZN");
        let cdr2 = Cdr::submit("msg2", "c", "S", "D")
            .with_pricing(100, 200, "MZN");

        writer.write(&cdr1).await.unwrap();
        writer.write(&cdr2).await.unwrap();

        let totals = writer.totals();
        assert_eq!(totals.count, 2);
        assert_eq!(totals.cost, 200);
        assert_eq!(totals.revenue, 350);
        assert_eq!(totals.margin, 150);
    }

    #[tokio::test]
    async fn test_by_esme() {
        let writer = MemoryCdrWriter::new("test", 100);

        writer.write(&Cdr::submit("msg1", "esme1", "S", "D")).await.unwrap();
        writer.write(&Cdr::submit("msg2", "esme2", "S", "D")).await.unwrap();
        writer.write(&Cdr::submit("msg3", "esme1", "S", "D")).await.unwrap();

        let esme1 = writer.by_esme("esme1", 10);
        assert_eq!(esme1.len(), 2);

        let esme2 = writer.by_esme("esme2", 10);
        assert_eq!(esme2.len(), 1);
    }
}
