use serde_json::Value;

/// Rebase client-visible URLs in a cached JSON response from one public base
/// to another. Only exact origin/path prefixes followed by `/` are replaced.
pub(crate) fn rebase_public_urls(
    body: &[u8],
    old_base: &str,
    new_base: &str,
) -> Result<Option<Vec<u8>>, serde_json::Error> {
    let old_base = old_base.trim_end_matches('/');
    let new_base = new_base.trim_end_matches('/');
    if old_base.is_empty() || old_base == new_base {
        return Ok(None);
    }
    let mut value: Value = serde_json::from_slice(body)?;
    if !rebase_value(&mut value, old_base, new_base) {
        return Ok(None);
    }
    serde_json::to_vec(&value).map(Some)
}

fn rebase_value(value: &mut Value, old_base: &str, new_base: &str) -> bool {
    match value {
        Value::String(value) => {
            let Some(rest) = value.strip_prefix(old_base) else {
                return false;
            };
            if !rest.is_empty() && !rest.starts_with('/') {
                return false;
            }
            *value = format!("{new_base}{rest}");
            true
        }
        Value::Array(values) => {
            let mut changed = false;
            for value in values {
                changed |= rebase_value(value, old_base, new_base);
            }
            changed
        }
        Value::Object(values) => {
            let mut changed = false;
            for value in values.values_mut() {
                changed |= rebase_value(value, old_base, new_base);
            }
            changed
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::rebase_public_urls;

    #[test]
    fn rebases_only_url_prefixes() {
        let body = br#"{
            "url": "https://old.example/mirror/nuget/group/v3/index.json",
            "nested": ["https://old.example/mirror/nuget/group/x", "https://old.example.evil/x"]
        }"#;
        let rebased =
            rebase_public_urls(body, "https://old.example", "https://new.example/stacktory")
                .expect("rebase")
                .expect("changed");
        let value: serde_json::Value = serde_json::from_slice(&rebased).expect("JSON");
        assert_eq!(
            value["url"],
            "https://new.example/stacktory/mirror/nuget/group/v3/index.json"
        );
        assert_eq!(
            value["nested"][0],
            "https://new.example/stacktory/mirror/nuget/group/x"
        );
        assert_eq!(value["nested"][1], "https://old.example.evil/x");
    }

    #[test]
    fn leaves_matching_base_unchanged() {
        let body = br#"{"url":"https://same.example/mirror/nuget/group/x"}"#;
        assert!(
            rebase_public_urls(body, "https://same.example", "https://same.example")
                .expect("rebase")
                .is_none()
        );
    }
}
