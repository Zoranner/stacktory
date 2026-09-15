//! 运行日志：写入 `<log_dir>/stacktory.log`，同时保留 stderr 输出。
//!
//! 审计事件是结构化事实，落在 `<data_dir>/audit`；这里的日志是运维诊断，落在
//! 独立的日志目录，按天轮转并由保留策略清理。

use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime},
};

use crate::timeformat;

pub const LOG_FILE: &str = "stacktory.log";
const ROTATED_PREFIX: &str = "stacktory.";
const ROTATED_SUFFIX: &str = ".log";
const SECS_PER_DAY: u64 = 86_400;

#[derive(Debug)]
pub struct LogFile {
    path: PathBuf,
    write_lock: Mutex<()>,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct RotationOutcome {
    pub rotated: Option<String>,
    pub pruned: u64,
    pub failed: bool,
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
        self.append(level, message)
    }

    /// 跨天时把当前日志轮转为 `stacktory.<UTC 日期>.log`。
    ///
    /// 轮转与写入使用同一把锁，因此不会把刚写入的行归档到旧日期的文件里。
    pub fn rotate_stale(&self, now: SystemTime) -> RotationOutcome {
        let _guard = self
            .write_lock
            .lock()
            .expect("log write lock must not be poisoned");
        let modified = match fs::metadata(&self.path) {
            Ok(metadata) => metadata.modified(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return RotationOutcome::default();
            }
            Err(error) => {
                self.report_failure(format!("log rotation failed: {error}"));
                return RotationOutcome {
                    failed: true,
                    ..RotationOutcome::default()
                };
            }
        };
        let Ok(modified) = modified else {
            return RotationOutcome::default();
        };
        let Some(label) = timeformat::date_label(modified) else {
            return RotationOutcome::default();
        };
        if timeformat::date_label(now).as_deref() == Some(label.as_str()) {
            return RotationOutcome::default();
        }
        let rotated = format!("{ROTATED_PREFIX}{label}{ROTATED_SUFFIX}");
        match fs::rename(&self.path, self.directory().join(&rotated)) {
            Ok(()) => RotationOutcome {
                rotated: Some(rotated),
                ..RotationOutcome::default()
            },
            Err(error) => {
                self.report_failure(format!("log rotation failed: {error}"));
                RotationOutcome {
                    failed: true,
                    ..RotationOutcome::default()
                }
            }
        }
    }

    /// 删除超过保留期的轮转日志。
    pub fn prune_rotated(&self, retention_days: u64, now: SystemTime) -> RotationOutcome {
        let _guard = self
            .write_lock
            .lock()
            .expect("log write lock must not be poisoned");
        let Ok(entries) = fs::read_dir(self.directory()) else {
            return RotationOutcome::default();
        };
        let Some(cutoff) = now
            .checked_sub(Duration::from_secs(
                retention_days.saturating_mul(SECS_PER_DAY),
            ))
            .and_then(timeformat::date_label)
        else {
            return RotationOutcome::default();
        };
        let mut outcome = RotationOutcome::default();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(label) = name
                .to_str()
                .and_then(|name| rotated_label(name, ROTATED_PREFIX, ROTATED_SUFFIX))
            else {
                continue;
            };
            if label >= cutoff {
                continue;
            }
            match fs::remove_file(entry.path()) {
                Ok(()) => outcome.pruned += 1,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    self.report_failure(format!("log cleanup failed: {error}"));
                    outcome.failed = true;
                }
            }
        }
        outcome
    }

    /// 轮转加清理，供保留策略按周期调用。
    pub fn maintain(&self, now: SystemTime, retention_days: u64) -> RotationOutcome {
        let mut outcome = self.rotate_stale(now);
        let pruned = self.prune_rotated(retention_days, now);
        outcome.pruned += pruned.pruned;
        outcome.failed |= pruned.failed;
        outcome
    }

    fn directory(&self) -> PathBuf {
        self.path
            .parent()
            .expect("log file path must have a parent directory")
            .to_path_buf()
    }

    fn append(&self, level: &str, message: &str) -> io::Result<()> {
        let timestamp =
            timeformat::timestamp_label(SystemTime::now()).unwrap_or_else(|| "unknown".to_owned());
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.path)?;
        writeln!(file, "{timestamp} {level} {message}")?;
        file.flush()
    }

    /// 锁已被当前持有，直接写 stderr，不能再走日志入口。
    fn report_failure(&self, message: String) {
        eprintln!("Stacktory ERROR: {message}");
    }
}

static LOG_FILE_HANDLE: OnceLock<Arc<LogFile>> = OnceLock::new();

/// 初始化运行日志：打开日志文件、轮转跨天的旧文件，并登记日志入口。
///
/// 返回日志句柄（供保留策略轮转和清理）以及本次轮转出的文件名。服务启动时调用
/// 一次；未初始化时日志只写 stderr。
pub fn init(
    log_dir: impl Into<PathBuf>,
    now: SystemTime,
) -> io::Result<(Arc<LogFile>, Option<String>)> {
    let log_file = Arc::new(LogFile::open(log_dir)?);
    let rotated = log_file.rotate_stale(now).rotated;
    let handle = log_file.clone();
    let _ = LOG_FILE_HANDLE.set(log_file);
    Ok((handle, rotated))
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

fn rotated_label(name: &str, rotated_prefix: &str, rotated_suffix: &str) -> Option<String> {
    let label = name
        .strip_prefix(rotated_prefix)?
        .strip_suffix(rotated_suffix)?;
    (label.len() == 8 && label.bytes().all(|byte| byte.is_ascii_digit())).then(|| label.to_owned())
}

#[cfg(test)]
mod tests {
    use super::LogFile;
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

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

    #[test]
    fn rotates_a_stale_log_file_and_keeps_the_current_one() {
        let directory = tempfile::tempdir().expect("log directory");
        let log_file = LogFile::open(directory.path()).expect("log file");
        log_file.write("INFO", "yesterday").expect("stale line");
        let stale = SystemTime::now() - Duration::from_secs(86_400);
        fs::File::options()
            .write(true)
            .open(log_file.path())
            .expect("open log file")
            .set_times(fs::FileTimes::new().set_modified(stale))
            .expect("backdate log file");

        let outcome = log_file.rotate_stale(SystemTime::now());

        let rotated = outcome.rotated.expect("rotated log file");
        let archived = fs::read_to_string(directory.path().join(&rotated)).expect("rotated log");
        assert!(archived.starts_with("20"), "{archived}");
        assert!(archived.ends_with(" INFO yesterday\n"), "{archived}");
        assert!(!log_file.path().exists());
        log_file.write("INFO", "today").expect("current line");
        assert!(
            fs::read_to_string(log_file.path())
                .expect("current contents")
                .ends_with(" INFO today\n")
        );
    }

    #[test]
    fn prunes_rotated_logs_past_the_retention_window() {
        let directory = tempfile::tempdir().expect("log directory");
        let log_file = LogFile::open(directory.path()).expect("log file");
        let retired = directory.path().join("stacktory.19700101.log");
        let recent = directory.path().join("stacktory.29991231.log");
        fs::write(&retired, "retired\n").expect("retired log");
        fs::write(&recent, "recent\n").expect("recent log");

        let outcome = log_file.prune_rotated(30, SystemTime::now());

        assert_eq!(outcome.pruned, 1);
        assert!(!retired.exists());
        assert!(recent.exists());
    }
}
