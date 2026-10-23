mod catalog;
mod content;
mod render;

pub(crate) use catalog::{SourceCatalog, SourceEntry};
pub(crate) use content::sections;
pub(crate) use render::{
    render_index_html, render_index_markdown, render_section_html, render_section_markdown,
};

pub(crate) fn section_by_id(id: &str) -> Option<&'static GuideSection> {
    sections().iter().find(|section| section.id == id)
}

#[derive(Debug)]
pub(crate) struct GuideSection {
    pub(crate) id: String,
    pub(crate) group_title: String,
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) ecosystem: String,
    pub(crate) prompt: String,
    pub(crate) manual_steps: String,
    pub(crate) verify: String,
    pub(crate) notes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        SourceCatalog, render_index_html, render_index_markdown, render_section_html,
        render_section_markdown, section_by_id, sections,
    };

    #[test]
    fn index_markdown_groups_every_supported_stack() {
        let catalog = SourceCatalog::sample();
        let markdown = render_index_markdown("https://pkg.example.test", &catalog);
        for title in [
            "Rustup", "Cargo", "npm", "Bun", "NuGet", "UPM", "VS Code", "Docker",
        ] {
            assert!(markdown.contains(&format!("[{title}]")), "missing {title}");
        }
        assert!(markdown.contains("## Rust"));
        assert!(markdown.contains("## Node.js"));
        assert!(!markdown.contains("{base_url}"));
        assert!(!markdown.contains("api.nuget.org"));
        assert_eq!(sections().len(), 8);
    }

    #[test]
    fn section_markdown_lists_all_sources_and_recommends_group() {
        let catalog = SourceCatalog::sample();
        let section = section_by_id("cargo").expect("cargo section");
        let markdown = render_section_markdown(section, "https://pkg.example.test", &catalog);
        assert!(markdown.contains("推荐入口：`https://pkg.example.test/mirror/cargo/group/`"));
        assert!(markdown.contains("| `official` | Proxy |"));
        assert!(markdown.contains("| `group` | Group | official |"));
        assert!(markdown.contains("请把本机 Cargo 的 crates.io 源切换到 Stacktory"));
        assert!(markdown.contains("cargo fetch"));
        assert!(markdown.contains("备份"));
        assert!(markdown.contains("不确定时停止"));
        assert!(!markdown.contains("{base_url}"));
        assert!(!markdown.contains("{endpoint}"));
    }

    #[test]
    fn index_html_has_the_page_shell_and_stack_links() {
        let catalog = SourceCatalog::sample();
        let html = render_index_html("https://pkg.example.test", &catalog);
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("lang=\"zh-CN\""));
        assert!(html.contains("<title>Stacktory | 使用说明</title>"));
        assert!(html.contains("href=\"/docs/cargo\""));
        assert!(html.contains(">Node.js</h2>"));
        assert!(html.contains("href=\"/docs/index.css\""));
        assert!(html.contains("src=\"/docs/index.js\""));
        assert!(!html.contains("{base_url}"));
        assert!(!html.contains("{{"));
    }

    #[test]
    fn section_html_lists_sources_and_has_copy_controls() {
        let catalog = SourceCatalog::sample();
        let section = section_by_id("cargo").expect("cargo section");
        let html = render_section_html(section, "https://pkg.example.test/artifacts", &catalog);
        assert!(html.contains("<title>Stacktory | 使用说明 | Cargo</title>"));
        assert!(html.contains("data-copy-target=\"prompt-cargo\""));
        assert!(html.contains("可用入口"));
        assert!(html.contains("<code>official</code>"));
        assert!(html.contains("<code>group</code>"));
        assert!(html.contains("Proxy"));
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
    fn npm_and_bun_share_the_node_group_and_npm_sources() {
        let catalog = SourceCatalog::sample();
        let npm = section_by_id("npm").expect("npm section");
        let bun = section_by_id("bun").expect("bun section");
        let npm_html = render_section_html(npm, "https://pkg.example.test", &catalog);
        let bun_html = render_section_html(bun, "https://pkg.example.test", &catalog);

        for html in [npm_html, bun_html] {
            assert!(html.contains("Node.js"));
            assert!(html.contains("<code>npmjs</code>"));
            assert!(html.contains("<code>group</code>"));
        }
    }

    #[test]
    fn unity_lists_direct_sources_and_group_members() {
        let catalog = SourceCatalog::sample();
        let section = section_by_id("upm").expect("UPM section");
        let html = render_section_html(section, "https://pkg.example.test", &catalog);

        assert!(html.contains("<code>unity</code>"));
        assert!(html.contains("<code>openupm</code>"));
        assert!(html.contains("unity → openupm"));
        assert!(html.contains("Group"));
    }

    #[test]
    fn html_escapes_dynamic_values() {
        let catalog = SourceCatalog::sample();
        let section = section_by_id("cargo").expect("cargo section");
        let html = render_section_html(
            section,
            "https://pkg.example.test/\"><script>alert(1)</script>",
            &catalog,
        );
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    }

    #[test]
    fn every_section_renders_without_placeholders_or_upstream_urls() {
        let catalog = SourceCatalog::sample();
        for section in sections() {
            let html = render_section_html(section, "https://pkg.example.test", &catalog);
            let markdown = render_section_markdown(section, "https://pkg.example.test", &catalog);
            for rendered in [html, markdown] {
                assert!(!rendered.contains("{base_url}"), "{}", section.id);
                assert!(!rendered.contains("{endpoint}"), "{}", section.id);
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
        let catalog = SourceCatalog::sample();
        let index = render_index_html("https://pkg.example.test", &catalog);
        assert!(!index.contains("class=\"status\""));

        for section in sections() {
            let html = render_section_html(section, "https://pkg.example.test", &catalog);
            let markdown = render_section_markdown(section, "https://pkg.example.test", &catalog);
            assert!(!html.contains("class=\"status\""), "{}", section.id);
            assert!(!markdown.contains("状态："), "{}", section.id);
        }
    }
}
