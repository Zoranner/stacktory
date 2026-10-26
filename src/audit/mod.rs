use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use thiserror::Error;

/// 同一进程内的所有审计 sink 共用一把写锁。
///
/// 审计文件是全局事实，不能因为不同适配器各自持有 sink 而并发写入。
/// 当前部署是单进程；如果未来多进程共享 `data_dir`，还需要额外的文件锁。
static AUDIT_WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug)]
pub struct AuditSink {
    path: PathBuf,
}

#[derive(Debug, Error)]
pub enum AuditError {
    #[error("audit I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("audit event cannot be serialized: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Serialize)]
struct AuditEvent<'a> {
    timestamp_unix_secs: u64,
    event: &'a str,
    target: &'a str,
    outcome: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ecosystem: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entry_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    object_key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_proxy: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    object_type: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_status: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure_category: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    count: Option<u64>,
}

#[derive(Default)]
pub struct AuditDetails<'a> {
    pub ecosystem: Option<&'a str>,
    pub entry_name: Option<&'a str>,
    pub object_key: Option<&'a str>,
    pub selected_proxy: Option<&'a str>,
    pub object_type: Option<&'a str>,
    pub cache_status: Option<&'a str>,
    pub failure_category: Option<&'a str>,
    pub count: Option<u64>,
}

impl AuditSink {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            path: crate::storage::layout::audit_root(&data_dir.into()).join("events.jsonl"),
        }
    }

    pub fn record(&self, event: &str, target: &str, outcome: &str) -> Result<(), AuditError> {
        self.record_detailed(event, target, outcome, AuditDetails::default())
    }

    pub fn record_detailed(
        &self,
        event: &str,
        target: &str,
        outcome: &str,
        details: AuditDetails<'_>,
    ) -> Result<(), AuditError> {
        let _guard = AUDIT_WRITE_LOCK
            .lock()
            .expect("audit write lock must not be poisoned");
        let parent = self
            .path
            .parent()
            .expect("audit event path must have a parent directory");
        fs::create_dir_all(parent)?;
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)?;
        let mut line = serde_json::to_vec(&AuditEvent {
            timestamp_unix_secs: unix_timestamp()?,
            event,
            target,
            outcome,
            ecosystem: details.ecosystem,
            entry_name: details.entry_name,
            object_key: details.object_key,
            selected_proxy: details.selected_proxy,
            object_type: details.object_type,
            cache_status: details.cache_status,
            failure_category: details.failure_category,
            count: details.count,
        })?;
        line.push(b'\n');
        file.write_all(&line)?;
        file.flush()?;
        Ok(())
    }
}

fn unix_timestamp() -> Result<u64, AuditError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| AuditError::Io(io::Error::other(error)))
        .map(|duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::AuditSink;
    use std::{fs, thread};

    #[test]
    fn concurrent_sinks_write_complete_json_lines() {
        let directory = tempfile::tempdir().expect("audit directory");
        let data_dir = directory.path().to_path_buf();
        let mut writers = Vec::new();

        for writer in 0..4 {
            let sink = AuditSink::new(&data_dir);
            writers.push(thread::spawn(move || {
                for event in 0..25 {
                    sink.record("test_event", &format!("{writer}-{event}"), "success")
                        .expect("audit event");
                }
            }));
        }

        for writer in writers {
            writer.join().expect("audit writer");
        }

        let contents = fs::read_to_string(data_dir.join("audit/events.jsonl")).expect("audit file");
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 100);
        for line in lines {
            let value: serde_json::Value = serde_json::from_str(line).expect("valid JSON line");
            assert_eq!(value["event"], "test_event");
        }
    }
}
