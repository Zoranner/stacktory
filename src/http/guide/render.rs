use super::{GuideSection, SourceCatalog, SourceEntry};
use crate::SourceType;
use std::sync::OnceLock;
use url::Url;

const PAGE_TEMPLATE: &str = include_str!("../../../web/guide/index.html");

pub(crate) fn render_index_markdown(base_url: &str, catalog: &SourceCatalog) -> String {
    let mut output = String::from("# Stacktory | 文档\n\n");
    output.push_str("选择技术栈，查看当前服务提供的入口和配置步骤。\n\n");
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
    let mut output = format!("# Stacktory | 文档 | {}\n\n", section.title);
    output.push_str(&replace_template(&section.summary, base_url, &endpoint));
    output.push_str("\n\n");
    if let Some(source) = catalog.initial(&section.ecosystem) {
        let other_entries = if section.ecosystem == "docker" {
            "其他入口的 Docker 客户端地址见下方入口列表。"
        } else {
            "如需其他入口，请按下方入口列表替换地址。"
        };
        output.push_str(&format!(
            "Markdown 页面使用入口 `{}`（{}）。{other_entries}\n\n",
            source.name,
            source_type_label(source.source_type)
        ));
    }
    output.push_str("## 入口\n\n");
    output.push_str(&render_sources_markdown(section, base_url, catalog));
    if section.ecosystem == "docker"
        && !catalog
            .sources(&section.ecosystem)
            .iter()
            .any(|source| source.registry_public_url.is_some())
    {
        output.push_str(
            "\n## 配置\n\n当前文档未提供可展示的 Docker 客户端地址。请检查入口的 `visible` 设置，或在入口上同时设置 `listen_port` 和 `public_url`。\n",
        );
        return output;
    }
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
        "Stacktory | 文档",
        base_url,
        None,
        render_directory_body(base_url, catalog),
    )
}

pub(crate) fn render_section_html(
    section: &GuideSection,
    base_url: &str,
    catalog: &SourceCatalog,
) -> String {
    render_page(
        &format!("Stacktory | 文档 | {}", section.title),
        base_url,
        Some(section.id.as_str()),
        render_section_body(section, base_url, catalog),
    )
}

fn render_page(title: &str, base_url: &str, current_id: Option<&str>, content: String) -> String {
    let nav = render_navigation(current_id.unwrap_or_default(), base_url);
    let version = docs_asset_version();
    PAGE_TEMPLATE
        .replace("{{TITLE}}", &escape_html(title))
        .replace("{{HOME}}", &escape_html(&site_href(base_url, "/")))
        .replace("{{SERVICE_URL}}", &escape_html(base_url))
        .replace("{{LAYOUT_CLASS}}", "docs-shell")
        .replace("{{NAV}}", &nav)
        .replace("{{CONTENT}}", &content)
        .replace(
            "{{THEME}}",
            &escape_html(&asset_href(base_url, "/theme.css", version)),
        )
        .replace(
            "{{STYLESHEET}}",
            &escape_html(&asset_href(base_url, "/docs/index.css", version)),
        )
        .replace(
            "{{SCRIPT}}",
            &escape_html(&asset_href(base_url, "/docs/index.js", version)),
        )
}

fn docs_asset_version() -> u64 {
    static VERSION: OnceLock<u64> = OnceLock::new();
    *VERSION.get_or_init(|| {
        let mut hash = 0xcbf29ce484222325u64;
        for source in [
            include_str!("../../../web/theme.css"),
            include_str!("../../../web/guide/index.css"),
            include_str!("../../../web/guide/index.js"),
        ] {
            for byte in source.bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x100000001b3);
            }
        }
        hash
    })
}

fn asset_href(base_url: &str, path: &str, version: u64) -> String {
    format!("{}?v={version:016x}", site_href(base_url, path))
}

fn render_navigation(current_id: &str, base_url: &str) -> String {
    let home_current = if current_id.is_empty() {
        r#" aria-current="page""#
    } else {
        ""
    };
    let mut links = format!(
        r#"<a class="docs-nav-link docs-nav-link--home" href="{}"{}>文档首页</a>"#,
        escape_html(&site_href(base_url, "/docs")),
        home_current,
    );
    links.push_str(r#"<div class="docs-nav-divider" aria-hidden="true"></div>"#);
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
        let current_attribute = if current_id == section.id {
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
        r#"<aside class="sidebar" aria-label="文档导航"><nav class="stack-nav">{links}</nav></aside>"#
    )
}

fn render_directory_body(base_url: &str, catalog: &SourceCatalog) -> String {
    let mut cards = String::new();
    for section in super::sections() {
        let source_count = catalog.sources(&section.ecosystem).len();
        cards.push_str(&format!(
            r#"<a class="doc-card" href="{href}">
  <span class="doc-card-top">
    <span class="doc-card-group">{group_title}</span>
    <span class="doc-card-arrow" aria-hidden="true">→</span>
  </span>
  <span class="doc-card-title">{title}</span>
  <span class="doc-card-summary">{summary}</span>
  <span class="doc-card-meta">{source_count} 个入口</span>
</a>"#,
            href = escape_html(&site_href(base_url, &format!("/docs/{}", section.id))),
            group_title = escape_html(&section.group_title),
            title = escape_html(&section.title),
            summary = escape_html(&section.summary),
            source_count = source_count,
        ));
    }
    format!(
        r#"<header class="article-header"><h1>文档</h1><p class="article-summary">选择技术栈，查看当前服务提供的入口和配置步骤。</p></header>
<section class="article-section" aria-labelledby="stacks-title">
  <div class="section-heading"><h2 id="stacks-title">技术栈</h2></div>
  <div class="docs-card-grid">{cards}</div>
</section>"#
    )
}

fn render_section_body(section: &GuideSection, base_url: &str, catalog: &SourceCatalog) -> String {
    let sources = render_sources_html(section, base_url, catalog);
    let config = render_config_html(section, base_url, catalog);
    format!(
        r#"<header class="article-header">
  <h1>{title}</h1>
  <p class="article-summary">{summary}</p>
</header>
<section class="article-section" aria-labelledby="sources-title">
  <div class="section-heading"><h2 id="sources-title">入口</h2></div>
  {sources}
</section>
  {config}"#,
        title = escape_html(&section.title),
        summary = escape_html(&section.summary),
        sources = sources,
        config = config,
    )
}

fn render_config_html(section: &GuideSection, base_url: &str, catalog: &SourceCatalog) -> String {
    let has_endpoint = section.ecosystem != "docker"
        || catalog
            .sources(&section.ecosystem)
            .iter()
            .any(|source| source.registry_public_url.is_some());
    if !has_endpoint {
        return r#"<section class="article-section" aria-labelledby="config-title">
  <div class="section-heading"><h2 id="config-title">配置</h2></div>
  <p class="config-empty">当前文档未提供可展示的 Docker 客户端地址。请检查入口的 <code>visible</code> 设置，或在入口上同时设置 <code>listen_port</code> 和 <code>public_url</code>。</p>
</section>"#
            .to_owned();
    }

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
    let entry_picker = render_entry_picker(section, base_url, catalog);
    format!(
        r#"<section class="article-section" aria-labelledby="config-title">
  <div class="section-heading"><h2 id="config-title">配置</h2></div>
  {entry_picker}
  <div class="mode-switch" role="tablist" aria-label="配置方式">
    <button class="mode-tab" id="tab-auto" type="button" role="tab" aria-selected="true" aria-controls="panel-auto" tabindex="0">自动配置</button>
    <button class="mode-tab" id="tab-manual" type="button" role="tab" aria-selected="false" aria-controls="panel-manual" tabindex="-1">手动配置</button>
  </div>
  <div class="tab-panel prompt-panel" id="panel-auto" role="tabpanel" aria-labelledby="tab-auto" tabindex="0">
    <div class="panel-head">
      <h2>提示词</h2>
      <div class="panel-actions">
        <span class="copy-status" id="copy-status" role="status" aria-live="polite"></span>
        <button class="copy-button" type="button" data-copy-target="prompt-{id}"><span class="copy-icon" aria-hidden="true"></span><span class="button-label">复制提示词</span></button>
      </div>
    </div>
    <pre id="prompt-{id}"><code data-endpoint-scope>{prompt}</code></pre>
  </div>
  <div class="tab-panel" id="panel-manual" role="tabpanel" aria-labelledby="tab-manual" tabindex="0" hidden>
    <div class="panel-head"><h2>手动配置</h2></div>
    <pre class="config-snippet"><code data-endpoint-scope>{manual}</code></pre>
    <div class="verify-block"><h2>验证</h2><pre><code data-endpoint-scope>{verify}</code></pre></div>
    <div class="notes"><h2>注意</h2><ul>{notes}</ul></div>
  </div>
</section>"#,
        id = section.id,
        entry_picker = entry_picker,
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
        return "当前配置未提供可在文档中展示的入口。\n".to_owned();
    }
    let proxies = sources
        .iter()
        .filter(|source| source.source_type == SourceType::Proxy);
    let groups = sources
        .iter()
        .filter(|source| source.source_type == SourceType::Group);
    let docker = section.ecosystem == "docker";
    let show_registry = docker
        && sources
            .iter()
            .any(|source| source.registry_public_url.is_some());
    let mut output = String::new();

    output.push_str("### Proxy 入口（直接来源）\n\n");
    let proxies = proxies.collect::<Vec<_>>();
    if proxies.is_empty() {
        output.push_str("当前未配置 Proxy 入口。\n");
    } else if show_registry
        && proxies
            .iter()
            .any(|source| source.registry_public_url.is_some())
    {
        output.push_str("| 入口 | 服务地址 | Docker 客户端地址 |\n| --- | --- | --- |\n");
        for source in proxies {
            let endpoint = endpoint_for(&section.ecosystem, &source.name, base_url)
                .unwrap_or_else(|| "—".to_owned());
            output.push_str(&format!(
                "| `{}` | `{}` | {} |\n",
                source.name,
                endpoint,
                source_registry_url(source)
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
    } else if show_registry
        && groups
            .iter()
            .any(|source| source.registry_public_url.is_some())
    {
        output.push_str(
            "| 入口 | 成员 | 服务地址 | Docker 客户端地址 |\n| --- | --- | --- | --- |\n",
        );
        for source in groups {
            let endpoint = endpoint_for(&section.ecosystem, &source.name, base_url)
                .unwrap_or_else(|| "—".to_owned());
            output.push_str(&format!(
                "| `{}` | {} | `{}` | {} |\n",
                source.name,
                source_members(source),
                endpoint,
                source_registry_url(source)
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

    if show_registry {
        output.push_str(
            "\nDocker 客户端地址来自每个入口的 `public_url`，可直接用于 Docker `registry-mirrors`。\n",
        );
    } else if docker {
        output.push_str(
            "\n当前未配置 Docker 客户端地址。请在入口上同时设置 `listen_port` 和 `public_url`。\n",
        );
    }
    output
}

fn render_sources_html(section: &GuideSection, base_url: &str, catalog: &SourceCatalog) -> String {
    let sources = catalog.sources(&section.ecosystem);
    if sources.is_empty() {
        return r#"<p class="source-empty">当前配置未提供可在文档中展示的入口。</p>"#.to_owned();
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
    let show_registry = docker
        && sources
            .iter()
            .any(|source| source.registry_public_url.is_some());
    let mut output = String::new();

    output.push_str(r#"<h3 class="subsection-title">Proxy 入口（直接来源）</h3>"#);
    if proxies.is_empty() {
        output.push_str(r#"<p class="source-empty">当前未配置 Proxy 入口。</p>"#);
    } else {
        let show_registry_proxy = show_registry
            && proxies
                .iter()
                .any(|source| source.registry_public_url.is_some());
        let additional_header = if show_registry_proxy {
            "<th>服务地址</th><th>Docker 客户端地址</th>"
        } else {
            "<th>地址</th>"
        };
        let mut rows = String::new();
        for source in proxies {
            let endpoint = endpoint_for(&section.ecosystem, &source.name, base_url)
                .unwrap_or_else(|| "—".to_owned());
            let registry_cell = if show_registry_proxy {
                format!(
                    r#"<td><code class="source-url registry-url">{}</code></td>"#,
                    escape_html(&source_registry_url(source))
                )
            } else {
                String::new()
            };
            rows.push_str(&format!(
                r#"<tr><td><code class="source-name">{}</code></td><td><code class="source-url">{}</code></td>{}</tr>"#,
                escape_html(&source.name),
                escape_html(&endpoint),
                registry_cell,
            ));
        }
        output.push_str(&format!(
            r#"<div class="source-table-wrap"><table class="source-table"><thead><tr><th>入口</th>{additional_header}</tr></thead><tbody>{rows}</tbody></table></div>"#
        ));
    }

    output.push_str(r#"<h3 class="subsection-title">Group 入口（按成员顺序回退）</h3>"#);
    if groups.is_empty() {
        output.push_str(r#"<p class="source-empty">当前未配置 Group 入口。</p>"#);
    } else {
        let show_registry_group = show_registry
            && groups
                .iter()
                .any(|source| source.registry_public_url.is_some());
        output.push_str(r#"<div class="group-list">"#);
        for source in groups {
            let endpoint = endpoint_for(&section.ecosystem, &source.name, base_url)
                .unwrap_or_else(|| "—".to_owned());
            let registry = if show_registry_group {
                format!(
                    r#"<span class="registry-address">Docker 客户端地址 <code>{}</code></span>"#,
                    escape_html(&source_registry_url(source))
                )
            } else {
                String::new()
            };
            let members = group_members_html(source);
            output.push_str(&format!(
                r#"<div class="group-row"><div class="group-head"><code class="source-name">{name}</code><span class="group-members">成员：{members}</span></div><code class="source-url">{endpoint}</code>{registry}</div>"#,
                name = escape_html(&source.name),
                members = members,
                endpoint = escape_html(&endpoint),
            ));
        }
        output.push_str("</div>");
    }

    if show_registry {
        output.push_str(
            r#"<p class="field-note">Docker 客户端地址来自每个入口的 <code>public_url</code>，可直接用于 Docker <code>registry-mirrors</code>。</p>"#,
        );
    } else if docker {
        output.push_str(
            r#"<p class="field-note">当前未配置 Docker 客户端地址；请在入口上同时设置 <code>listen_port</code> 和 <code>public_url</code>。</p>"#,
        );
    }
    output
}

fn render_entry_picker(section: &GuideSection, base_url: &str, catalog: &SourceCatalog) -> String {
    let initial = catalog.initial(&section.ecosystem);
    let Some(initial) = initial else {
        return String::new();
    };
    let Some(initial_endpoint) = configurable_endpoint_for(&section.ecosystem, initial, base_url)
    else {
        return String::new();
    };

    let mut groups = Vec::new();
    let mut proxies = Vec::new();
    for source in catalog.sources(&section.ecosystem) {
        let Some(endpoint) = configurable_endpoint_for(&section.ecosystem, source, base_url) else {
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

    let mut option_groups = String::new();
    for (label, type_label, entries) in [
        ("Group 入口", "Group", groups),
        ("Proxy 入口", "Proxy", proxies),
    ] {
        if entries.is_empty() {
            continue;
        }
        let mut options = String::new();
        for (source, endpoint) in entries {
            let selected = if source.name == initial.name {
                r#" aria-selected="true""#
            } else {
                r#" aria-selected="false""#
            };
            options.push_str(&format!(
                r#"<div class="entry-select-option" role="option" tabindex="-1" data-endpoint="{}" data-entry-name="{}"{}><span class="entry-select-option-name">{}</span><span class="entry-select-option-type">{type_label}</span></div>"#,
                escape_html(&endpoint),
                escape_html(&source.name),
                selected,
                escape_html(&source.name),
            ));
        }
        option_groups.push_str(&format!(
            r#"<div class="entry-select-group" role="group" aria-label="{label}"><div class="entry-select-group-label" aria-hidden="true">{label}</div>{options}</div>"#
        ));
    }

    format!(
        r#"<div class="entry-picker"><span class="entry-picker-label" id="entry-picker-label">当前入口</span><div class="entry-select" data-entry-select data-current-endpoint="{}"><button class="entry-select-trigger" id="entry-select" type="button" data-entry-trigger role="combobox" aria-haspopup="listbox" aria-expanded="false" aria-controls="entry-listbox" aria-labelledby="entry-picker-label entry-select-value"><span class="entry-select-value" id="entry-select-value" data-entry-label>{}</span><span class="entry-select-chevron" aria-hidden="true"></span></button><div class="entry-select-popup" id="entry-listbox" data-entry-listbox role="listbox" aria-labelledby="entry-picker-label" hidden>{}</div></div><code class="entry-url" data-entry-url>{}</code></div>"#,
        escape_html(&initial_endpoint),
        escape_html(&initial.name),
        option_groups,
        escape_html(&initial_endpoint),
    )
}

fn source_members(source: &SourceEntry) -> String {
    if source.members.is_empty() {
        if source.members_hidden {
            "已隐藏".to_owned()
        } else {
            "直连".to_owned()
        }
    } else {
        source.members.join(" → ")
    }
}

fn group_members_html(source: &SourceEntry) -> String {
    if source.members.is_empty() {
        if source.members_hidden {
            "已隐藏".to_owned()
        } else {
            "—".to_owned()
        }
    } else {
        source
            .members
            .iter()
            .map(|member| format!("<code>{}</code>", escape_html(member)))
            .collect::<Vec<_>>()
            .join(" → ")
    }
}

fn source_registry_url(source: &SourceEntry) -> String {
    source
        .registry_public_url
        .clone()
        .unwrap_or_else(|| "—".to_owned())
}

fn initial_endpoint(section: &GuideSection, base_url: &str, catalog: &SourceCatalog) -> String {
    catalog
        .initial(&section.ecosystem)
        .and_then(|source| configurable_endpoint_for(&section.ecosystem, source, base_url))
        .unwrap_or_else(|| "当前配置未提供入口".to_owned())
}

fn configurable_endpoint_for(
    ecosystem: &str,
    source: &SourceEntry,
    base_url: &str,
) -> Option<String> {
    if ecosystem == "docker" {
        return source.registry_public_url.clone();
    }
    endpoint_for(ecosystem, &source.name, base_url)
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
        "pypi" => format!("/mirror/pypi/{entry}/simple/"),
        "upm" => format!("/mirror/upm/{entry}"),
        "vscode" => format!("/mirror/vscode/{entry}"),
        "docker" => format!("/mirror/docker/{entry}/v2/"),
        "winget" => format!("/mirror/winget/{entry}/"),
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
