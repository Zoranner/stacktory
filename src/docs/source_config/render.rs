use super::{GuideSection, SourceCatalog, SourceEntry};
use crate::SourceType;
use url::Url;

const PAGE_TEMPLATE: &str = include_str!("../../../web/docs/index.html");

pub(crate) fn render_index_markdown(base_url: &str, catalog: &SourceCatalog) -> String {
    let mut output = String::from("# Stacktory | 使用说明\n\n");
    output.push_str(
        "可以直接按手动步骤配置，也可以把提示词复制给智能体。所有操作先备份，失败时停止。\n\n",
    );
    let mut group: Option<&str> = None;
    for section in super::sections() {
        let group_title = section.group_title.as_str();
        if group != Some(group_title) {
            output.push_str(&format!("## {}\n\n", section.group_title));
            group = Some(group_title);
        }
        output.push_str(&format!(
            "- [{}]({})：{}（{} 个入口）\n",
            section.title,
            site_href(base_url, &format!("/docs/{}/index.md", section.id)),
            section.summary,
            catalog.sources(&section.ecosystem).len()
        ));
    }
    output.push_str("\n页面地址：");
    output.push_str(base_url);
    output.push('\n');
    output
}

pub(crate) fn render_section_markdown(
    section: &GuideSection,
    base_url: &str,
    catalog: &SourceCatalog,
) -> String {
    let endpoint = recommended_endpoint(section, base_url, catalog);
    let mut output = format!("# Stacktory | 使用说明 | {}\n\n", section.title);
    output.push_str(&replace_template(&section.summary, base_url, &endpoint));
    output.push_str("\n\n");
    output.push_str(&format!("推荐入口：`{endpoint}`\n\n"));
    output.push_str("## 可用入口\n\n");
    output.push_str(&render_sources_markdown(section, base_url, catalog));
    output.push_str("\n## 给智能体的提示词\n\n```text\n");
    output.push_str(&replace_template(&section.prompt, base_url, &endpoint));
    output.push_str("\n```\n\n## 手动步骤\n\n");
    output.push_str(&replace_template(
        &section.manual_steps,
        base_url,
        &endpoint,
    ));
    output.push_str("\n\n## 验证\n\n```text\n");
    output.push_str(&replace_template(&section.verify, base_url, &endpoint));
    output.push_str("\n```\n\n## 注意\n\n");
    for note in &section.notes {
        output.push_str(&format!(
            "- {}\n",
            replace_template(note, base_url, &endpoint)
        ));
    }
    output
}

pub(crate) fn render_index_html(base_url: &str, catalog: &SourceCatalog) -> String {
    render_page(
        "Stacktory | 使用说明",
        base_url,
        None,
        render_index_body(base_url, catalog),
    )
}

pub(crate) fn render_section_html(
    section: &GuideSection,
    base_url: &str,
    catalog: &SourceCatalog,
) -> String {
    render_page(
        &format!("Stacktory | 使用说明 | {}", section.title),
        base_url,
        Some(section),
        render_section_body(section, base_url, catalog),
    )
}

fn render_page(
    title: &str,
    base_url: &str,
    current: Option<&GuideSection>,
    content: String,
) -> String {
    let nav = current
        .map(|section| render_navigation(Some(section), base_url))
        .unwrap_or_default();
    let header_suffix = current
        .map(|section| {
            format!(
                r#"<span class="docs-separator" aria-hidden="true">|</span><span class="docs-current">{}</span>"#,
                escape_html(&section.title)
            )
        })
        .unwrap_or_default();
    let layout_class = if current.is_some() {
        "docs-shell"
    } else {
        "docs-shell docs-shell--index"
    };
    PAGE_TEMPLATE
        .replace("{{TITLE}}", &escape_html(title))
        .replace("{{HOME}}", &escape_html(&site_href(base_url, "/")))
        .replace("{{SERVICE_URL}}", &escape_html(base_url))
        .replace("{{HEADER_SUFFIX}}", &header_suffix)
        .replace("{{LAYOUT_CLASS}}", layout_class)
        .replace("{{NAV}}", &nav)
        .replace("{{CONTENT}}", &content)
        .replace(
            "{{STYLESHEET}}",
            &escape_html(&site_href(base_url, "/docs/index.css")),
        )
        .replace(
            "{{SCRIPT}}",
            &escape_html(&site_href(base_url, "/docs/index.js")),
        )
}

fn render_navigation(current: Option<&GuideSection>, base_url: &str) -> String {
    let mut links = String::new();
    let mut group: Option<&str> = None;
    for section in super::sections() {
        let group_title = section.group_title.as_str();
        if group != Some(group_title) {
            links.push_str(&format!(
                r#"<div class="nav-group-title">{}</div>"#,
                escape_html(&section.group_title)
            ));
            group = Some(group_title);
        }
        let current_attribute = if current.is_some_and(|current| current.id == section.id) {
            r#" aria-current="page""#
        } else {
            ""
        };
        links.push_str(&format!(
            r#"<a href="{}"{}>{}</a>"#,
            escape_html(&site_href(base_url, &format!("/docs/{}", section.id))),
            current_attribute,
            escape_html(&section.title)
        ));
    }
    format!(
        r#"<aside class="sidebar" aria-label="技术栈导航"><div class="side-head">技术栈</div><nav class="stack-nav">{links}</nav></aside>"#
    )
}

fn render_index_body(base_url: &str, catalog: &SourceCatalog) -> String {
    let mut groups = String::new();
    let mut group: Option<&str> = None;
    for section in super::sections() {
        let group_title = section.group_title.as_str();
        if group != Some(group_title) {
            if group.is_some() {
                groups.push_str("</div></section>");
            }
            groups.push_str(&format!(
                r#"<section class="stack-group"><h2 class="group-title">{}</h2><div class="stack-grid">"#,
                escape_html(&section.group_title)
            ));
            group = Some(group_title);
        }
        let source_count = catalog.sources(&section.ecosystem).len();
        groups.push_str(&format!(
            r#"<a class="stack-card" href="{href}">
  <div class="stack-card-top"><h2>{title}</h2><span class="stack-arrow" aria-hidden="true">→</span></div>
  <p class="stack-summary">{summary}</p>
  <span class="source-count">{source_count} 个入口</span>
</a>"#,
            href = escape_html(&site_href(base_url, &format!("/docs/{}", section.id))),
            title = escape_html(&section.title),
            summary = escape_html(&section.summary),
            source_count = source_count,
        ));
    }
    if group.is_some() {
        groups.push_str("</div></section>");
    }
    format!(r#"<h1 class="index-title">技术栈</h1>{groups}"#)
}

fn render_section_body(section: &GuideSection, base_url: &str, catalog: &SourceCatalog) -> String {
    let endpoint = recommended_endpoint(section, base_url, catalog);
    let prompt = escape_html(&replace_template(&section.prompt, base_url, &endpoint));
    let manual = escape_html(&replace_template(
        &section.manual_steps,
        base_url,
        &endpoint,
    ));
    let verify = escape_html(&replace_template(&section.verify, base_url, &endpoint));
    let mut notes = String::new();
    for note in &section.notes {
        notes.push_str(&format!(
            "<li>{}</li>",
            escape_html(&replace_template(note, base_url, &endpoint))
        ));
    }
    let sources = render_sources_html(section, base_url, catalog);
    format!(
        r#"<div class="detail-header">
  <a class="back-link" href="{docs_home}"><span aria-hidden="true">←</span> 返回技术栈</a>
  <div class="detail-title-row"><h1>{title}</h1></div>
  <p class="detail-summary">{summary}</p>
</div>
<section class="endpoint-panel" aria-labelledby="endpoint-title">
  <div class="endpoint-label" id="endpoint-title">推荐入口</div>
  <code class="endpoint-code">{endpoint}</code>
</section>
<section class="source-panel" aria-labelledby="source-title">
  <h2 id="source-title">可用入口</h2>
  {sources}
</section>
<section class="content-panel" aria-label="{title} 配置方式">
  <div class="mode-switch" role="tablist" aria-label="配置方式">
    <button class="mode-tab" id="tab-auto" type="button" role="tab" aria-selected="true" aria-controls="panel-auto" tabindex="0">自动配置</button>
    <button class="mode-tab" id="tab-manual" type="button" role="tab" aria-selected="false" aria-controls="panel-manual" tabindex="-1">手动配置</button>
  </div>
  <div class="tab-panel prompt-panel" id="panel-auto" role="tabpanel" aria-labelledby="tab-auto" tabindex="0">
    <div class="panel-head"><h2>提示词</h2><button class="copy-button" type="button" data-copy-target="prompt-{id}"><span class="copy-icon" aria-hidden="true"></span><span class="button-label">复制提示词</span></button></div>
    <pre id="prompt-{id}"><code>{prompt}</code></pre>
  </div>
  <div class="tab-panel" id="panel-manual" role="tabpanel" aria-labelledby="tab-manual" tabindex="0" hidden>
    <div class="panel-head"><h2>手动配置</h2><code class="file-path">{endpoint}</code></div>
    <pre class="config-snippet"><code>{manual}</code></pre>
    <div class="verify-block"><h2>验证</h2><pre><code>{verify}</code></pre></div>
    <div class="notes"><h2>注意</h2><ul>{notes}</ul></div>
  </div>
</section>"#,
        docs_home = escape_html(&site_href(base_url, "/docs")),
        title = escape_html(&section.title),
        summary = escape_html(&section.summary),
        endpoint = escape_html(&endpoint),
        sources = sources,
        id = section.id,
        prompt = prompt,
        manual = manual,
        verify = verify,
        notes = notes,
    )
}

fn render_sources_markdown(
    section: &GuideSection,
    base_url: &str,
    catalog: &SourceCatalog,
) -> String {
    let sources = catalog.sources(&section.ecosystem);
    if sources.is_empty() {
        return "当前配置未提供该生态入口。\n".to_owned();
    }
    let mut output = String::from("| 入口 | 类型 | 成员 | 地址 |\n| --- | --- | --- | --- |\n");
    for source in sources {
        let endpoint = endpoint_for(&section.ecosystem, &source.name, base_url)
            .unwrap_or_else(|| "—".to_owned());
        output.push_str(&format!(
            "| `{}` | {} | {} | `{}` |\n",
            source.name,
            source_type_label(source.source_type),
            source_members(source),
            endpoint
        ));
    }
    output
}

fn render_sources_html(section: &GuideSection, base_url: &str, catalog: &SourceCatalog) -> String {
    let sources = catalog.sources(&section.ecosystem);
    if sources.is_empty() {
        return r#"<p class="source-empty">当前配置未提供该生态入口。</p>"#.to_owned();
    }
    let mut rows = String::new();
    for source in sources {
        let endpoint = endpoint_for(&section.ecosystem, &source.name, base_url)
            .unwrap_or_else(|| "—".to_owned());
        let port = source
            .cli_port
            .map(|port| format!(":{port}"))
            .unwrap_or_else(|| "—".to_owned());
        rows.push_str(&format!(
            r#"<tr>
  <td><code>{name}</code></td>
  <td><span class="source-type">{source_type}</span></td>
  <td class="source-members">{members}</td>
  <td><code class="source-endpoint">{endpoint}</code></td>
  <td class="source-port">{port}</td>
</tr>"#,
            name = escape_html(&source.name),
            source_type = source_type_label(source.source_type),
            members = escape_html(&source_members(source)),
            endpoint = escape_html(&endpoint),
            port = escape_html(&port),
        ));
    }
    format!(
        r#"<div class="source-table-wrap"><table class="source-table"><thead><tr><th>入口</th><th>类型</th><th>成员</th><th>地址</th><th>CLI 端口</th></tr></thead><tbody>{rows}</tbody></table></div>"#
    )
}

fn source_members(source: &SourceEntry) -> String {
    if source.members.is_empty() {
        "—".to_owned()
    } else {
        source.members.join(" → ")
    }
}

fn source_type_label(source_type: SourceType) -> &'static str {
    match source_type {
        SourceType::Proxy => "Proxy",
        SourceType::Group => "Group",
    }
}

fn recommended_endpoint(section: &GuideSection, base_url: &str, catalog: &SourceCatalog) -> String {
    catalog
        .recommended(&section.ecosystem)
        .and_then(|source| endpoint_for(&section.ecosystem, &source.name, base_url))
        .unwrap_or_else(|| "当前配置未提供入口".to_owned())
}

fn endpoint_for(ecosystem: &str, entry: &str, base_url: &str) -> Option<String> {
    let base = base_url.trim_end_matches('/');
    let path = match ecosystem {
        "rustup" => format!("/mirror/rustup/{entry}"),
        "cargo" => format!("/mirror/cargo/{entry}/"),
        "npm" => format!("/mirror/npm/{entry}/"),
        "nuget" => format!("/mirror/nuget/{entry}/v3/index.json"),
        "upm" => format!("/mirror/upm/{entry}"),
        "vscode" => format!("/mirror/vscode/{entry}"),
        "docker" => format!("/mirror/docker/{entry}/v2/"),
        _ => return None,
    };
    Some(format!("{base}{path}"))
}

fn replace_template(value: &str, base_url: &str, endpoint: &str) -> String {
    value
        .replace("{base_url}", base_url.trim_end_matches('/'))
        .replace("{endpoint}", endpoint)
}

fn site_href(base_url: &str, suffix: &str) -> String {
    let prefix = Url::parse(base_url)
        .map(|url| url.path().trim_end_matches('/').to_owned())
        .unwrap_or_default();
    format!("{prefix}{suffix}")
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            character => escaped.push(character),
        }
    }
    escaped
}
