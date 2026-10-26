use std::fs;

use crate::{audit::AuditSink, startup::read_config, storage::layout};

pub fn invalidate_nuget_metadata(
    mut arguments: impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut entry_name = None;
    let mut dry_run = false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--entry" => {
                let value = arguments.next().ok_or("--entry requires a source name")?;
                entry_name = Some(value);
            }
            "--dry-run" => dry_run = true,
            other => return Err(format!("unknown option `{other}`").into()),
        }
    }
    let entry_name = entry_name.ok_or("--entry is required")?;
    let config = read_config()?;
    let Some(nuget) = config.nuget.as_ref() else {
        return Err("the configured file has no [nuget] section".into());
    };
    if nuget.sources.proxy_members(&entry_name).is_none() {
        return Err(format!("NuGet source entry `{entry_name}` was not found").into());
    }
    let metadata_path = layout::metadata_root(&config.storage.data_dir, "nuget").join(&entry_name);
    if dry_run {
        println!("Stacktory: would remove {}", metadata_path.display());
        return Ok(());
    }
    match fs::remove_dir_all(&metadata_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    AuditSink::new(config.storage.data_dir).record(
        "nuget_metadata_invalidated",
        &entry_name,
        "success",
    )?;
    println!("Stacktory: removed NuGet metadata for `{entry_name}`");
    Ok(())
}
