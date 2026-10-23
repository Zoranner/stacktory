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
        assert!(markdown.starts_with("# Stacktory | 文档"));
        assert!(markdown.contains("## Rust"));
        assert!(markdown.contains("## Node.js"));
        assert!(!markdown.contains("使用说明"));
        assert!(!markdown.contains("{base_url}"));
        assert!(!markdown.contains("api.nuget.org"));
        assert_eq!(sections().len(), 8);
    }

    #[test]
    fn section_markdown_lists_all_sources_and_uses_the_initial_entry() {
        let catalog = SourceCatalog::sample();
        let section = section_by_id("cargo").expect("cargo section");
        let markdown = render_section_markdown(section, "https://pkg.example.test", &catalog);
        assert!(markdown.contains("Markdown 页面使用入口 `group`（Group）"));
        assert!(markdown.contains("### Proxy 入口（直接来源）"));
        assert!(markdown.contains("### Group 入口（按成员顺序回退）"));
        assert!(
            markdown.contains("| `official` | `https://pkg.example.test/mirror/cargo/official/` |")
        );
        assert!(
            markdown.contains(
                "| `group` | official | `https://pkg.example.test/mirror/cargo/group/` |"
            )
        );
        assert!(!markdown.contains("推荐入口"));
        assert!(!markdown.contains("示例入口"));
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
        assert!(html.contains("<title>Stacktory | 文档</title>"));
        assert!(html.contains("class=\"docs-main\""));
        assert!(!html.contains("个客户端"));
        assert!(html.contains("href=\"/docs/cargo\""));
        assert!(html.contains(">技术栈</h2>"));
        assert!(html.contains("class=\"doc-card\""));
        assert!(html.contains("docs-nav-link--home"));
        assert!(html.contains(">文档首页</a>"));
        assert!(!html.contains("breadcrumb"));
        assert!(!html.contains("使用说明"));
        assert!(!html.contains("docs-nav-link\" href=\"/docs/guide\""));
        assert!(html.contains("href=\"/theme.css?v="));
        assert!(html.contains("href=\"/docs/index.css?v="));
        assert!(html.contains("src=\"/docs/index.js?v="));
        assert!(!html.contains("{base_url}"));
        assert!(!html.contains("{{"));
    }

    #[test]
    fn section_html_lists_sources_and_has_copy_controls() {
        let catalog = SourceCatalog::sample();
        let section = section_by_id("cargo").expect("cargo section");
        let html = render_section_html(section, "https://pkg.example.test/artifacts", &catalog);
        assert!(html.contains("<title>Stacktory | 文档 | Cargo</title>"));
        assert!(html.contains("<span class=\"docs-path\">文档</span>"));
        assert!(!html.contains("breadcrumb"));
        assert!(!html.contains("docs-current"));
        assert!(html.contains("data-copy-target=\"prompt-cargo\""));
        assert!(html.contains(">入口</h2>"));
        assert!(html.contains("source-name\">official</code>"));
        assert!(html.contains("source-name\">group</code>"));
        assert!(html.contains("Proxy"));
        assert!(html.contains("https://pkg.example.test/artifacts/mirror/cargo/group/"));
        assert!(html.contains("当前入口"));
        assert!(html.contains("id=\"entry-select\""));
        assert!(html.contains("role=\"combobox\""));
        assert!(html.contains("role=\"listbox\""));
        assert!(html.contains("data-entry-name=\"group\""));
        assert!(!html.contains("<select"));
        assert!(html.contains("data-endpoint-scope"));
        assert!(!html.contains("推荐入口"));
        assert!(!html.contains("示例入口"));
        assert!(html.contains("href=\"/artifacts/theme.css?v="));
        assert!(html.contains("href=\"/artifacts/docs/index.css?v="));
        assert!(html.contains("src=\"/artifacts/docs/index.js?v="));
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
            assert!(html.contains("source-name\">npmjs</code>"));
            assert!(html.contains("source-name\">group</code>"));
        }
    }

    #[test]
    fn unity_lists_direct_sources_and_group_members() {
        let catalog = SourceCatalog::sample();
        let section = section_by_id("upm").expect("UPM section");
        let html = render_section_html(section, "https://pkg.example.test", &catalog);

        assert!(html.contains("source-name\">unity</code>"));
        assert!(html.contains("source-name\">openupm</code>"));
        assert!(html.contains("<code>unity</code> → <code>openupm</code>"));
        assert!(html.contains("Group"));
    }

    #[test]
    fn docker_uses_configured_public_urls_instead_of_listener_ports() {
        let catalog = SourceCatalog::sample();
        let section = section_by_id("docker").expect("Docker section");
        let html = render_section_html(section, "https://pkg.example.test", &catalog);
        let markdown = render_section_markdown(section, "https://pkg.example.test", &catalog);

        for rendered in [&html, &markdown] {
            assert!(rendered.contains("Docker 客户端地址"));
            assert!(rendered.contains("http://docker.example.test:19000"));
            assert!(rendered.contains("public_url"));
            assert!(!rendered.contains("CLI 端口"));
        }
        assert!(html.contains("registry-mirrors"));
        assert!(html.contains("data-entry-url>http://docker.example.test:19000</code>"));
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
                assert!(!rendered.contains("推荐入口"), "{}", section.id);
                assert!(!rendered.contains("示例入口"), "{}", section.id);
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
