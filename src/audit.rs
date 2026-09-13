use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use thiserror::Error;

#[derive(Debug)]
pub struct AuditSink {
    path: PathBuf,
    write_lock: Mutex<()>,
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
            path: data_dir.into().join("audit/events.jsonl"),
            write_lock: Mutex::new(()),
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
        let _guard = self
            .write_lock
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
        serde_json::to_writer(
            &mut file,
            &AuditEvent {
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
            },
        )?;
        file.write_all(b"\n")?;
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
