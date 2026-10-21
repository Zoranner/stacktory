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

pub use service_index::{NugetAdapter, NugetError, NugetService};

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
