use std::sync::OnceLock;

use serde::Deserialize;

pub(crate) const GUIDE_ID: &str = "guide";

const CONTENT: &str = include_str!("../../../web/docs/content/guide.toml");

#[derive(Debug, Deserialize)]
pub(crate) struct OverviewPage {
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) sections: Vec<OverviewSection>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OverviewSection {
    pub(crate) title: String,
    #[serde(default)]
    pub(crate) paragraphs: Vec<String>,
    #[serde(default)]
    pub(crate) items: Vec<String>,
}

static PAGE: OnceLock<OverviewPage> = OnceLock::new();

pub(crate) fn overview() -> &'static OverviewPage {
    PAGE.get_or_init(|| toml::from_str(CONTENT).expect("invalid docs overview content"))
}

#[cfg(test)]
mod tests {
    use super::overview;

    #[test]
    fn every_overview_section_has_content() {
        let page = overview();
        assert!(!page.title.is_empty());
        assert!(!page.summary.is_empty());
        assert!(!page.sections.is_empty());
        for section in &page.sections {
            assert!(!section.title.is_empty());
            assert!(!section.paragraphs.is_empty() || !section.items.is_empty());
        }
    }
}
