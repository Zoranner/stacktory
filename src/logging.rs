//! 运行日志：写入 `<log_dir>/stacktory.log`，同时保留 stderr 输出。
//!
//! 审计事件是结构化事实，落在 `<data_dir>/audit`；这里的日志是运维诊断，落在
//! 独立的日志目录，按天轮转并由保留策略清理。

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::SystemTime,
};

use crate::timeformat;

pub const LOG_FILE: &str = "stacktory.log";

#[derive(Debug)]
pub struct LogFile {
    path: PathBuf,
    write_lock: Mutex<()>,
}

impl LogFile {
    pub fn open(log_dir: impl Into<PathBuf>) -> io::Result<Self> {
        let log_dir = log_dir.into();
        fs::create_dir_all(&log_dir)?;
        Ok(Self {
            path: log_dir.join(LOG_FILE),
            write_lock: Mutex::new(()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write(&self, level: &str, message: &str) -> io::Result<()> {
        let _guard = self
            .write_lock
            .lock()
            .expect("log write lock must not be poisoned");
        let timestamp =
            timeformat::timestamp_label(SystemTime::now()).unwrap_or_else(|| "unknown".to_owned());
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)?;
        writeln!(file, "{timestamp} {level} {message}")?;
        file.flush()
    }
}

static LOG_FILE_HANDLE: OnceLock<LogFile> = OnceLock::new();

/// 初始化运行日志。服务启动时调用一次；未初始化时只写 stderr。
pub fn init(log_dir: impl Into<PathBuf>) -> io::Result<PathBuf> {
    let log_file = LogFile::open(log_dir)?;
    let path = log_file.path().to_path_buf();
    let _ = LOG_FILE_HANDLE.set(log_file);
    Ok(path)
}

pub fn info(message: impl AsRef<str>) {
    emit("INFO", message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    emit("WARN", message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    emit("ERROR", message.as_ref());
}

fn emit(level: &str, message: &str) {
    eprintln!("Stacktory {level}: {message}");
    let Some(log_file) = LOG_FILE_HANDLE.get() else {
        return;
    };
    if let Err(error) = log_file.write(level, message) {
        eprintln!("Stacktory ERROR: log write failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::LogFile;
    use std::fs;

    #[test]
    fn writes_timestamped_lines_to_the_log_file() {
        let directory = tempfile::tempdir().expect("log directory");
        let log_file = LogFile::open(directory.path()).expect("log file");

        log_file.write("INFO", "started").expect("first line");
        log_file
            .write("ERROR", "upstream failed")
            .expect("second line");

        let contents = fs::read_to_string(log_file.path()).expect("log contents");
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with(" INFO started"), "{contents}");
        assert!(lines[1].ends_with(" ERROR upstream failed"), "{contents}");
        assert!(lines[0].starts_with("20"), "{contents}");
    }
}
