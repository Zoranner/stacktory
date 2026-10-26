//! 缓存对象的访问时间更新。
//!
//! 闲置淘汰以文件修改时间为准；读取命中缓存的路径通过这里更新该时间，
//! 不需要为了记录访问而依赖保留清理模块。

use std::{fs, io, path::Path, time::SystemTime};

pub fn note_access(path: &Path) -> io::Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .set_times(fs::FileTimes::new().set_modified(SystemTime::now()))
}
