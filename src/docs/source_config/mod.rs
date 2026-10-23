mod content;
mod render;

pub(crate) use content::sections;
pub(crate) use render::{
    render_index_html, render_index_markdown, render_section_html, render_section_markdown,
};

pub(crate) fn section_by_id(id: &str) -> Option<&'static GuideSection> {
    sections().iter().find(|section| section.id == id)
}

pub(crate) struct GuideSection {
    pub(crate) id: &'static str,
    pub(crate) title: &'static str,
    pub(crate) summary: &'static str,
    pub(crate) target: Target,
    pub(crate) prompt: &'static str,
    pub(crate) manual_steps: &'static str,
    pub(crate) verify: &'static str,
    pub(crate) notes: &'static [&'static str],
}

pub(crate) enum Target {
    ServicePath(&'static str),
    AdminProvided(&'static str),
}

#[cfg(test)]
mod tests {
    use super::{
        render_index_html, render_index_markdown, render_section_html, render_section_markdown,
        section_by_id, sections,
    };

    #[test]
    fn index_markdown_covers_every_supported_stack() {
        let markdown = render_index_markdown("https://pkg.example.test");
        for title in [
            "Rustup", "Cargo", "npm", "Bun", "NuGet", "UPM", "VS Code", "Docker",
        ] {
            assert!(markdown.contains(&format!("[{title}]")), "missing {title}");
        }
        assert!(!markdown.contains("{base_url}"));
        assert!(!markdown.contains("api.nuget.org"));
        assert_eq!(sections().len(), 8);
    }

    #[test]
    fn section_markdown_keeps_prompts_and_verification_separate() {
        let section = section_by_id("cargo").expect("cargo section");
        let markdown = render_section_markdown(section, "https://pkg.example.test");
        assert!(markdown.contains("请把本机 Cargo 的 crates.io 源切换到 Stacktory"));
        assert!(markdown.contains("cargo fetch"));
        assert!(markdown.contains("备份"));
        assert!(markdown.contains("不确定时停止"));
        assert!(markdown.contains("https://pkg.example.test/mirror/cargo/group/"));
        assert!(!markdown.contains("{base_url}"));
    }

    #[test]
    fn index_html_has_the_page_shell_and_stack_links() {
        let html = render_index_html("https://pkg.example.test");
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("lang=\"zh-CN\""));
        assert!(html.contains("<title>Stacktory | 使用说明</title>"));
        assert!(html.contains("href=\"/docs/cargo\""));
        assert!(html.contains("href=\"/docs/index.css\""));
        assert!(html.contains("src=\"/docs/index.js\""));
        assert!(!html.contains("{base_url}"));
        assert!(!html.contains("{{"));
    }

    #[test]
    fn section_html_has_tabs_copy_controls_and_a_dynamic_title() {
        let section = section_by_id("cargo").expect("cargo section");
        let html = render_section_html(section, "https://pkg.example.test/artifacts");
        assert!(html.contains("<title>Stacktory | 使用说明 | Cargo</title>"));
        assert!(html.contains("data-copy-target=\"prompt-cargo\""));
        assert!(html.contains("https://pkg.example.test/artifacts/mirror/cargo/group/"));
        assert!(html.contains("href=\"/artifacts/docs/index.css\""));
        assert!(html.contains("src=\"/artifacts/docs/index.js\""));
        assert!(html.contains("请把本机 Cargo"));
        assert!(html.contains("aria-live=\"polite\""));
        assert!(html.contains("role=\"tab\""));
        assert!(html.contains("aria-controls=\"panel-manual\""));
        assert!(html.contains("<noscript>"));
        assert!(!html.contains("{base_url}"));
    }

    #[test]
    fn html_escapes_dynamic_values() {
        let section = section_by_id("cargo").expect("cargo section");
        let html = render_section_html(
            section,
            "https://pkg.example.test/\"><script>alert(1)</script>",
        );
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    }

    #[test]
    fn every_section_renders_without_placeholders_or_upstream_urls() {
        for section in sections() {
            let html = render_section_html(section, "https://pkg.example.test");
            let markdown = render_section_markdown(section, "https://pkg.example.test");
            for rendered in [html, markdown] {
                assert!(!rendered.contains("{base_url}"), "{}", section.id);
                assert!(!rendered.contains("{{"), "{}", section.id);
                assert!(!rendered.contains("api.nuget.org"), "{}", section.id);
                assert!(!rendered.contains("registry.npmjs.org"), "{}", section.id);
                assert!(!rendered.contains("packages.unity.com"), "{}", section.id);
                assert!(
                    !rendered.contains("marketplace.visualstudio.com"),
                    "{}",
                    section.id
                );
                assert!(!rendered.contains("registry-1.docker.io"), "{}", section.id);
            }
        }
    }

    #[test]
    fn pages_do_not_render_status_badges() {
        let index = render_index_html("https://pkg.example.test");
        assert!(!index.contains("class=\"status\""));

        for section in sections() {
            let html = render_section_html(section, "https://pkg.example.test");
            let markdown = render_section_markdown(section, "https://pkg.example.test");
            assert!(!html.contains("class=\"status\""), "{}", section.id);
            assert!(!markdown.contains("状态："), "{}", section.id);
        }
    }
}
