use crate::{audit::AuditSink, retention, startup::read_config};

pub fn prune_legacy_layout(
    arguments: impl Iterator<Item = String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut dry_run = false;
    for argument in arguments {
        match argument.as_str() {
            "--dry-run" => dry_run = true,
            other => {
                eprintln!("Stacktory: unknown option `{other}`");
                eprintln!("usage: stacktory [prune-legacy-layout [--dry-run]]");
                std::process::exit(2);
            }
        }
    }
    let config = read_config()?;
    let report = retention::prune_legacy_layout(
        &config.storage.data_dir,
        dry_run,
        &AuditSink::new(config.storage.data_dir.clone()),
    );
    println!(
        "Stacktory: {} {} legacy layout entries ({:.1} MiB)",
        if dry_run { "would remove" } else { "removed" },
        report.removed_entries,
        report.removed_bytes as f64 / (1024.0 * 1024.0)
    );
    if report.failed_entries > 0 {
        eprintln!(
            "Stacktory: {} legacy layout entries could not be removed",
            report.failed_entries
        );
        std::process::exit(1);
    }
    Ok(())
}
