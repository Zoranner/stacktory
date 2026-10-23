use super::{GuideSection, Target};
use url::Url;

const PAGE_TEMPLATE: &str = include_str!("../../../web/docs/index.html");

pub(crate) fn render_index_markdown(base_url: &str) -> String {
    let mut output = String::from("# Stacktory | 使用说明\n\n");
    output.push_str(
        "可以直接按手动步骤配置，也可以把提示词复制给智能体。所有操作先备份，失败时停止。\n\n",
    );
    output.push_str("## 技术栈\n\n");
    for section in super::sections() {
        output.push_str(&format!(
            "- [{}]({})：{}\n",
            section.title,
            site_href(base_url, &format!("/docs/{}/index.md", section.id)),
            section.summary
        ));
    }
    output.push_str("\n页面地址：");
    output.push_str(base_url);
    output.push('\n');
    output
}

pub(crate) fn render_section_markdown(section: &GuideSection, base_url: &str) -> String {
    let mut output = format!("# Stacktory | 使用说明 | {}\n\n", section.title);
    output.push_str(&replace_base_url(section.summary, base_url));
    output.push_str("\n\n");
    output.push_str(&format!(
        "入口：`{}`\n\n",
        target_text(&section.target, base_url)
    ));
    output.push_str("## 给智能体的提示词\n\n```text\n");
    output.push_str(&replace_base_url(section.prompt, base_url));
    output.push_str("\n```\n\n## 手动步骤\n\n");
    output.push_str(&replace_base_url(section.manual_steps, base_url));
    output.push_str("\n\n## 验证\n\n```text\n");
    output.push_str(&replace_base_url(section.verify, base_url));
    output.push_str("\n```\n\n## 注意\n\n");
    for note in section.notes {
        output.push_str(&format!("- {}\n", replace_base_url(note, base_url)));
    }
    output
}

pub(crate) fn render_index_html(base_url: &str) -> String {
    render_page(
        "Stacktory | 使用说明",
        base_url,
        None,
        render_index_body(base_url),
    )
}

pub(crate) fn render_section_html(section: &GuideSection, base_url: &str) -> String {
    render_page(
        &format!("Stacktory | 使用说明 | {}", section.title),
        base_url,
        Some(section),
        render_section_body(section, base_url),
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
                escape_html(section.title)
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
    for section in super::sections() {
        let current_attribute = if current.is_some_and(|current| current.id == section.id) {
            r#" aria-current="page""#
        } else {
            ""
        };
        links.push_str(&format!(
            r#"<a href="{}"{}>{}</a>"#,
            escape_html(&site_href(base_url, &format!("/docs/{}", section.id))),
            current_attribute,
            escape_html(section.title)
        ));
    }
    format!(
        r#"<aside class="sidebar" aria-label="技术栈导航"><div class="side-head">技术栈</div><nav class="stack-nav">{links}</nav></aside>"#
    )
}

fn render_index_body(base_url: &str) -> String {
    let mut cards = String::new();
    for section in super::sections() {
        cards.push_str(&format!(
            r#"<a class="stack-card" href="{href}">
  <div class="stack-card-top"><h2>{title}</h2><span class="stack-arrow" aria-hidden="true">→</span></div>
  <p class="stack-summary">{summary}</p>
  <code class="stack-path">{target}</code>
</a>"#,
            href = escape_html(&site_href(base_url, &format!("/docs/{}", section.id))),
            title = escape_html(section.title),
            summary = escape_html(section.summary),
            target = escape_html(&target_text(&section.target, "")),
        ));
    }
    format!(
        r#"<h1 class="index-title">技术栈</h1><section class="stack-grid" aria-label="技术栈">{cards}</section>"#
    )
}

fn render_section_body(section: &GuideSection, base_url: &str) -> String {
    let prompt = escape_html(&replace_base_url(section.prompt, base_url));
    let manual = escape_html(&replace_base_url(section.manual_steps, base_url));
    let verify = escape_html(&replace_base_url(section.verify, base_url));
    let mut notes = String::new();
    for note in section.notes {
        notes.push_str(&format!(
            "<li>{}</li>",
            escape_html(&replace_base_url(note, base_url))
        ));
    }
    format!(
        r#"<div class="detail-header">
  <a class="back-link" href="{docs_home}"><span aria-hidden="true">←</span> 返回技术栈</a>
  <div class="detail-title-row"><h1>{title}</h1></div>
  <p class="detail-summary">{summary}</p>
</div>
<section class="endpoint-panel" aria-labelledby="endpoint-title">
  <div class="endpoint-label" id="endpoint-title">地址</div>
  <code class="endpoint-code">{target}</code>
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
    <div class="panel-head"><h2>手动配置</h2><code class="file-path">{target}</code></div>
    <pre class="config-snippet"><code>{manual}</code></pre>
    <div class="verify-block"><h2>验证</h2><pre><code>{verify}</code></pre></div>
    <div class="notes"><h2>注意</h2><ul>{notes}</ul></div>
  </div>
</section>"#,
        docs_home = escape_html(&site_href(base_url, "/docs")),
        title = escape_html(section.title),
        summary = escape_html(section.summary),
        target = escape_html(&target_text(&section.target, base_url)),
        id = section.id,
        prompt = prompt,
        manual = manual,
        verify = verify,
        notes = notes,
    )
}

fn target_text(target: &Target, base_url: &str) -> String {
    match target {
        Target::ServicePath(path) => format!("{}{}", base_url.trim_end_matches('/'), path),
        Target::AdminProvided(text) => (*text).to_owned(),
    }
}

fn replace_base_url(value: &str, base_url: &str) -> String {
    value.replace("{base_url}", base_url.trim_end_matches('/'))
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
