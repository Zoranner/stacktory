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
    let endpoint = initial_endpoint(section, base_url, catalog);
    let mut output = format!("# Stacktory | 使用说明 | {}\n\n", section.title);
    output.push_str(&replace_template(&section.summary, base_url, &endpoint));
    output.push_str("\n\n");
    if let Some(source) = catalog.initial(&section.ecosystem) {
        output.push_str(&format!(
            "Markdown 页面使用入口 `{}`（{}）。如需其他入口，请按下方入口列表替换地址。\n\n",
            source.name,
            source_type_label(source.source_type)
        ));
    }
    output.push_str("## 入口\n\n");
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
            "{{THEME}}",
            &escape_html(&site_href(base_url, "/theme.css")),
        )
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
                r#"<div class="docs-nav-group">{}</div>"#,
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
            r#"<a class="docs-nav-link" href="{}"{}>{}</a>"#,
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
    let mut grouped: Vec<(&str, Vec<&GuideSection>)> = Vec::new();
    for section in super::sections() {
        match grouped.last_mut() {
            Some((title, sections)) if *title == section.group_title => sections.push(section),
            _ => grouped.push((section.group_title.as_str(), vec![section])),
        }
    }
    for (group_title, sections) in grouped {
        let mut rows = String::new();
        for section in &sections {
            let source_count = catalog.sources(&section.ecosystem).len();
            rows.push_str(&format!(
                r#"<a class="directory-row" href="{href}">
  <span class="directory-copy"><span class="directory-name">{title}</span><span class="directory-summary">{summary}</span></span>
  <span class="directory-side"><span class="directory-meta">{source_count} 个入口</span><span class="directory-arrow" aria-hidden="true">→</span></span>
</a>"#,
                href = escape_html(&site_href(base_url, &format!("/docs/{}", section.id))),
                title = escape_html(&section.title),
                summary = escape_html(&section.summary),
                source_count = source_count,
            ));
        }
        groups.push_str(&format!(
            r#"<section class="directory-group"><header class="directory-group-header"><h2>{group_title}</h2><span class="directory-group-meta">{count} 个客户端</span></header><div class="directory-list">{rows}</div></section>"#,
            group_title = escape_html(group_title),
            count = sections.len(),
            rows = rows,
        ));
    }
    format!(
        r#"<header class="docs-intro"><p class="eyebrow">客户端配置</p><h1>使用说明</h1><p class="docs-lede">选择技术栈，查看当前服务提供的入口和配置步骤。配置前请先备份目标文件，失败时停止。</p></header><div class="docs-directory">{groups}</div>"#
    )
}

fn render_section_body(section: &GuideSection, base_url: &str, catalog: &SourceCatalog) -> String {
    let endpoint = initial_endpoint(section, base_url, catalog);
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
            "<li data-endpoint-scope>{}</li>",
            escape_html(&replace_template(note, base_url, &endpoint))
        ));
    }
    let sources = render_sources_html(section, base_url, catalog);
    let entry_picker = render_entry_picker(section, base_url, catalog);
    format!(
        r#"<nav class="breadcrumb" aria-label="面包屑"><a href="{docs_home}">使用说明</a><span class="breadcrumb-separator" aria-hidden="true">/</span><span>{group_title}</span><span class="breadcrumb-separator" aria-hidden="true">/</span><span>{title}</span></nav>
<header class="article-header">
  <p class="eyebrow">{group_title}</p>
  <h1>{title}</h1>
  <p class="article-summary">{summary}</p>
</header>
<section class="article-section" aria-labelledby="sources-title">
  <div class="section-heading"><h2 id="sources-title">入口</h2></div>
  {sources}
</section>
<section class="article-section" aria-labelledby="config-title">
  <div class="section-heading"><h2 id="config-title">配置</h2></div>
  {entry_picker}
  <div class="mode-switch" role="tablist" aria-label="配置方式">
    <button class="mode-tab" id="tab-auto" type="button" role="tab" aria-selected="true" aria-controls="panel-auto" tabindex="0">自动配置</button>
    <button class="mode-tab" id="tab-manual" type="button" role="tab" aria-selected="false" aria-controls="panel-manual" tabindex="-1">手动配置</button>
  </div>
  <div class="tab-panel prompt-panel" id="panel-auto" role="tabpanel" aria-labelledby="tab-auto" tabindex="0">
    <div class="panel-head"><h2>提示词</h2><button class="copy-button" type="button" data-copy-target="prompt-{id}"><span class="copy-icon" aria-hidden="true"></span><span class="button-label">复制提示词</span></button></div>
    <pre id="prompt-{id}"><code data-endpoint-scope>{prompt}</code></pre>
  </div>
  <div class="tab-panel" id="panel-manual" role="tabpanel" aria-labelledby="tab-manual" tabindex="0" hidden>
    <div class="panel-head"><h2>手动配置</h2></div>
    <pre class="config-snippet"><code data-endpoint-scope>{manual}</code></pre>
    <div class="verify-block"><h2>验证</h2><pre><code data-endpoint-scope>{verify}</code></pre></div>
    <div class="notes"><h2>注意</h2><ul>{notes}</ul></div>
  </div>
</section>"#,
        docs_home = escape_html(&site_href(base_url, "/docs")),
        group_title = escape_html(&section.group_title),
        title = escape_html(&section.title),
        summary = escape_html(&section.summary),
        entry_picker = entry_picker,
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
    let proxies = sources
        .iter()
        .filter(|source| source.source_type == SourceType::Proxy);
    let groups = sources
        .iter()
        .filter(|source| source.source_type == SourceType::Group);
    let docker = section.ecosystem == "docker";
    let mut output = String::new();

    output.push_str("### Proxy 入口（直接来源）\n\n");
    let proxies = proxies.collect::<Vec<_>>();
    if proxies.is_empty() {
        output.push_str("当前未配置 Proxy 入口。\n");
    } else if docker && proxies.iter().any(|source| source.cli_port.is_some()) {
        output.push_str("| 入口 | 地址 | CLI 端口 |\n| --- | --- | --- |\n");
        for source in proxies {
            let endpoint = endpoint_for(&section.ecosystem, &source.name, base_url)
                .unwrap_or_else(|| "—".to_owned());
            output.push_str(&format!(
                "| `{}` | `{}` | {} |\n",
                source.name,
                endpoint,
                source_port(source)
            ));
        }
    } else {
        output.push_str("| 入口 | 地址 |\n| --- | --- |\n");
        for source in proxies {
            let endpoint = endpoint_for(&section.ecosystem, &source.name, base_url)
                .unwrap_or_else(|| "—".to_owned());
            output.push_str(&format!("| `{}` | `{}` |\n", source.name, endpoint));
        }
    }

    output.push_str("\n### Group 入口（按成员顺序回退）\n\n");
    let groups = groups.collect::<Vec<_>>();
    if groups.is_empty() {
        output.push_str("当前未配置 Group 入口。\n");
    } else if docker && groups.iter().any(|source| source.cli_port.is_some()) {
        output.push_str("| 入口 | 成员 | 地址 | CLI 端口 |\n| --- | --- | --- | --- |\n");
        for source in groups {
            let endpoint = endpoint_for(&section.ecosystem, &source.name, base_url)
                .unwrap_or_else(|| "—".to_owned());
            output.push_str(&format!(
                "| `{}` | {} | `{}` | {} |\n",
                source.name,
                source_members(source),
                endpoint,
                source_port(source)
            ));
        }
    } else {
        output.push_str("| 入口 | 成员 | 地址 |\n| --- | --- | --- |\n");
        for source in groups {
            let endpoint = endpoint_for(&section.ecosystem, &source.name, base_url)
                .unwrap_or_else(|| "—".to_owned());
            output.push_str(&format!(
                "| `{}` | {} | `{}` |\n",
                source.name,
                source_members(source),
                endpoint
            ));
        }
    }

    if docker && sources.iter().any(|source| source.cli_port.is_some()) {
        output.push_str(
            "\nCLI 端口是 Docker 客户端直连该入口的独立监听端口；未配置时使用 `/mirror/docker/{name}/v2/`。\n",
        );
    }
    output
}

fn render_sources_html(section: &GuideSection, base_url: &str, catalog: &SourceCatalog) -> String {
    let sources = catalog.sources(&section.ecosystem);
    if sources.is_empty() {
        return r#"<p class="source-empty">当前配置未提供该生态入口。</p>"#.to_owned();
    }
    let proxies = sources
        .iter()
        .filter(|source| source.source_type == SourceType::Proxy)
        .collect::<Vec<_>>();
    let groups = sources
        .iter()
        .filter(|source| source.source_type == SourceType::Group)
        .collect::<Vec<_>>();
    let docker = section.ecosystem == "docker";
    let mut output = String::new();

    output.push_str(r#"<h3 class="subsection-title">Proxy 入口（直接来源）</h3>"#);
    if proxies.is_empty() {
        output.push_str(r#"<p class="source-empty">当前未配置 Proxy 入口。</p>"#);
    } else {
        let show_cli_port = docker && proxies.iter().any(|source| source.cli_port.is_some());
        let port_header = if show_cli_port {
            "<th>CLI 端口</th>"
        } else {
            ""
        };
        let mut rows = String::new();
        for source in proxies {
            let endpoint = endpoint_for(&section.ecosystem, &source.name, base_url)
                .unwrap_or_else(|| "—".to_owned());
            let port_cell = if show_cli_port {
                format!(
                    r#"<td class="source-port">{}</td>"#,
                    escape_html(&source_port(source))
                )
            } else {
                String::new()
            };
            rows.push_str(&format!(
                r#"<tr><td><code class="source-name">{}</code></td><td><code class="source-url">{}</code></td>{}</tr>"#,
                escape_html(&source.name),
                escape_html(&endpoint),
                port_cell,
            ));
        }
        output.push_str(&format!(
            r#"<div class="source-table-wrap"><table class="source-table"><thead><tr><th>入口</th><th>地址</th>{port_header}</tr></thead><tbody>{rows}</tbody></table></div>"#
        ));
    }

    output.push_str(r#"<h3 class="subsection-title">Group 入口（按成员顺序回退）</h3>"#);
    if groups.is_empty() {
        output.push_str(r#"<p class="source-empty">当前未配置 Group 入口。</p>"#);
    } else {
        let show_cli_port = docker && groups.iter().any(|source| source.cli_port.is_some());
        output.push_str(r#"<div class="group-list">"#);
        for source in groups {
            let endpoint = endpoint_for(&section.ecosystem, &source.name, base_url)
                .unwrap_or_else(|| "—".to_owned());
            let port = if show_cli_port {
                format!(
                    r#"<span class="source-port">CLI 端口 {}</span>"#,
                    escape_html(&source_port(source))
                )
            } else {
                String::new()
            };
            let members = group_members_html(source);
            output.push_str(&format!(
                r#"<div class="group-row"><div class="group-head"><code class="source-name">{name}</code><span class="group-members">成员：{members}</span></div><code class="source-url">{endpoint}</code>{port}</div>"#,
                name = escape_html(&source.name),
                members = members,
                endpoint = escape_html(&endpoint),
            ));
        }
        output.push_str("</div>");
    }

    if docker && sources.iter().any(|source| source.cli_port.is_some()) {
        output.push_str(
            r#"<p class="field-note">CLI 端口是 Docker 客户端直连该入口的独立监听端口；未配置时使用 <code>/mirror/docker/{name}/v2/</code>。</p>"#,
        );
    }
    output
}

fn render_entry_picker(section: &GuideSection, base_url: &str, catalog: &SourceCatalog) -> String {
    let initial = catalog.initial(&section.ecosystem);
    let Some(initial) = initial else {
        return String::new();
    };
    let Some(initial_endpoint) = endpoint_for(&section.ecosystem, &initial.name, base_url) else {
        return String::new();
    };

    let mut groups = Vec::new();
    let mut proxies = Vec::new();
    for source in catalog.sources(&section.ecosystem) {
        let Some(endpoint) = endpoint_for(&section.ecosystem, &source.name, base_url) else {
            continue;
        };
        match source.source_type {
            SourceType::Group => groups.push((source, endpoint)),
            SourceType::Proxy => proxies.push((source, endpoint)),
        }
    }

    if groups.len() + proxies.len() == 1 {
        return format!(
            r#"<div class="entry-picker"><span class="entry-picker-label">当前入口</span><code class="entry-current">{} · {}</code><code class="entry-url" data-entry-url>{}</code></div>"#,
            source_type_label(initial.source_type),
            escape_html(&initial.name),
            escape_html(&initial_endpoint),
        );
    }

    let mut options = String::new();
    for (label, entries) in [("Group 入口", groups), ("Proxy 入口", proxies)] {
        if entries.is_empty() {
            continue;
        }
        options.push_str(&format!(r#"<optgroup label="{label}">"#));
        for (source, endpoint) in entries {
            let selected = if source.name == initial.name {
                " selected"
            } else {
                ""
            };
            options.push_str(&format!(
                r#"<option value="{}"{}>{}</option>"#,
                escape_html(&endpoint),
                selected,
                escape_html(&source.name),
            ));
        }
        options.push_str("</optgroup>");
    }

    format!(
        r#"<div class="entry-picker"><label class="entry-picker-label" for="entry-select">当前入口</label><select id="entry-select" class="entry-select" data-entry-select data-current-endpoint="{}">{}</select><code class="entry-url" data-entry-url>{}</code></div>"#,
        escape_html(&initial_endpoint),
        options,
        escape_html(&initial_endpoint),
    )
}

fn source_members(source: &SourceEntry) -> String {
    if source.members.is_empty() {
        "直连".to_owned()
    } else {
        source.members.join(" → ")
    }
}

fn group_members_html(source: &SourceEntry) -> String {
    if source.members.is_empty() {
        "—".to_owned()
    } else {
        source
            .members
            .iter()
            .map(|member| format!("<code>{}</code>", escape_html(member)))
            .collect::<Vec<_>>()
            .join(" → ")
    }
}

fn source_port(source: &SourceEntry) -> String {
    source
        .cli_port
        .map(|port| format!(":{port}"))
        .unwrap_or_else(|| "—".to_owned())
}

fn initial_endpoint(section: &GuideSection, base_url: &str, catalog: &SourceCatalog) -> String {
    catalog
        .initial(&section.ecosystem)
        .and_then(|source| endpoint_for(&section.ecosystem, &source.name, base_url))
        .unwrap_or_else(|| "当前配置未提供入口".to_owned())
}

fn source_type_label(source_type: SourceType) -> &'static str {
    match source_type {
        SourceType::Proxy => "Proxy",
        SourceType::Group => "Group",
    }
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
