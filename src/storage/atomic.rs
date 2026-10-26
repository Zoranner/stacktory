use std::{
    fs, io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

/// 写入同一个目录下的临时文件，再用 rename 替换目标。
pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("atomic write target has no parent directory"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("atomic write target has no file name"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let temporary = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), nonce));

    fs::write(&temporary, contents)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(temporary);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::atomic_write;
    use std::fs;

    #[test]
    fn replaces_an_existing_file_without_leaving_a_temporary_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("nested/cache.toml");
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(&path, b"old").expect("write old contents");

        atomic_write(&path, b"new").expect("atomic write");

        assert_eq!(fs::read(&path).expect("read contents"), b"new");
        let entries = fs::read_dir(path.parent().expect("parent"))
            .expect("read directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("directory entries");
        assert_eq!(entries.len(), 1);
    }
}
