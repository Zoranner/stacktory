use std::{collections::BTreeMap, sync::OnceLock};

use super::GuideSection;

const CONTENT_FILES: &[&str] = &[
    include_str!("../../../web/guide/content/rustup.md"),
    include_str!("../../../web/guide/content/cargo.md"),
    include_str!("../../../web/guide/content/npm.md"),
    include_str!("../../../web/guide/content/bun.md"),
    include_str!("../../../web/guide/content/nuget.md"),
    include_str!("../../../web/guide/content/pypi.md"),
    include_str!("../../../web/guide/content/upm.md"),
    include_str!("../../../web/guide/content/vscode.md"),
    include_str!("../../../web/guide/content/docker.md"),
];

static SECTIONS: OnceLock<Vec<GuideSection>> = OnceLock::new();

pub(crate) fn sections() -> &'static [GuideSection] {
    SECTIONS.get_or_init(load_sections)
}

fn load_sections() -> Vec<GuideSection> {
    CONTENT_FILES
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            parse_guide(raw)
                .unwrap_or_else(|error| panic!("invalid guide content file #{index}: {error}"))
        })
        .collect()
}

fn parse_guide(raw: &str) -> Result<GuideSection, String> {
    let raw = raw
        .strip_prefix("---\n")
        .ok_or_else(|| "missing front matter".to_owned())?;
    let (front_matter, body) = raw
        .split_once("\n---\n")
        .ok_or_else(|| "unterminated front matter".to_owned())?;

    let mut metadata = BTreeMap::new();
    for line in front_matter.lines() {
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| format!("invalid front matter line `{line}`"))?;
        metadata.insert(key.trim().to_owned(), unquote(value.trim()));
    }

    let mut parts = BTreeMap::<String, String>::new();
    let mut current = None;
    for line in body.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            current = Some(name.trim().to_owned());
            continue;
        }
        if let Some(name) = current.as_ref() {
            let part = parts.entry(name.clone()).or_default();
            part.push_str(line);
            part.push('\n');
        }
    }

    let required = |name: &str| {
        metadata
            .get(name)
            .map(|value| value.to_owned())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("missing front matter key `{name}`"))
    };
    let required_section = |name: &str| {
        parts
            .get(name)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("missing content section `## {name}`"))
    };
    let notes = parts
        .get("注意")
        .map(|value| {
            value
                .lines()
                .filter_map(|line| line.strip_prefix("- "))
                .map(|line| line.trim().to_owned())
                .filter(|line| !line.is_empty())
                .collect()
        })
        .unwrap_or_default();

    Ok(GuideSection {
        id: required("id")?,
        group_title: required("group")?,
        title: required("title")?,
        summary: required("summary")?,
        ecosystem: required("ecosystem")?,
        prompt: required_section("提示词")?,
        manual_steps: required_section("手动步骤")?,
        verify: required_section("验证")?,
        notes,
    })
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_owned()
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_guide, sections};

    #[test]
    fn every_content_file_parses_and_keeps_its_identity() {
        let sections = sections();
        assert_eq!(sections.len(), 9);
        for section in sections {
            assert!(!section.id.is_empty());
            assert!(!section.title.is_empty());
            assert!(!section.group_title.is_empty());
            assert!(!section.ecosystem.is_empty());
            assert!(!section.prompt.is_empty());
            assert!(!section.manual_steps.is_empty());
            assert!(!section.verify.is_empty());
        }
    }

    #[test]
    fn rejects_content_without_front_matter() {
        let error = parse_guide("## 提示词\ntext\n").expect_err("front matter should be required");
        assert!(error.contains("front matter"));
    }
}
