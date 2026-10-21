mod package;
mod path;
mod registration;
mod render;
mod search;
mod service_index;
mod vulnerability;

use crate::{
    SourcePool,
    sources::{SourcePoolError, source_error_is_group_fallback},
};
use std::time::{SystemTime, UNIX_EPOCH};

pub use service_index::{NugetAdapter, NugetError, NugetService};

pub(crate) const METADATA_REVALIDATE_AFTER_SECS: u64 = 15 * 60;

pub(crate) fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

pub(crate) fn metadata_is_stale(validated_at_unix_secs: u64) -> bool {
    validated_at_unix_secs == 0
        || now_unix_secs().saturating_sub(validated_at_unix_secs) >= METADATA_REVALIDATE_AFTER_SECS
}

pub(crate) fn group_fallback_source(
    error: &SourcePoolError,
    pool: &SourcePool,
    entry_name: &str,
) -> bool {
    pool.is_group(entry_name) && source_error_is_group_fallback(error)
}

pub(crate) fn group_fallback_error(
    error: &NugetError,
    pool: &SourcePool,
    entry_name: &str,
) -> bool {
    match error {
        NugetError::Source(source) => group_fallback_source(source, pool, entry_name),
        _ => false,
    }
}
