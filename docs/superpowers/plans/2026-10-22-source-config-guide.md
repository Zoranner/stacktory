# Stacktory 客户端源配置文档页 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 提供一个只读的 `/docs` 索引页、`/docs/{stack}` 详情页和对应的 Markdown 文档，覆盖各技术栈的手动源配置和智能体提示词；生产页面不依赖远程脚本，旧脚本入口保持 404。

**Architecture:** 用 Rust 结构化内容模型作为唯一事实来源，服务端从同一组数据分别渲染 HTML 和 Markdown。HTML 使用编译期嵌入的 CSS 和脚本，提供提示词复制与“自动配置/手动配置”切换，不引入前端依赖；所有服务地址由当前请求的 Host/协议生成，缺少 Host 时回退到 `server.public_url`。

**Tech Stack:** Rust 1.98、Axum 0.8、现有 `RustupService`、无新增依赖、服务端渲染 HTML/Markdown。

**Spec:** `docs/superpowers/specs/2026-10-22-source-config-guide-design.md`

## Global Constraints

- 不增加 CLI、MCP、`.ps1`、`.sh`、远程执行或配置同步能力。
- 不增加运行时配置字段；页面只读取现有 `server.public_url` 或当前请求 Host。
- 不在页面中写入上游 URL、token、密码、缓存路径或用户隐私信息。
- 不修改 Docker listener 路由；`/docs` 和 `/docs.md` 只挂在主服务路由。
- 所有内容使用中文；文件路径、命令、配置键和代码标识使用反引号。
- HTML 中所有动态文本必须转义；Markdown 和 HTML 必须来自同一组 `GuideSection`。
- 实施前先核对当前工作树；本计划只暂存本功能明确列出的文件，不清理、不回退、不混入其他改动。
- 先写失败测试，再实现，再运行完整门禁；不要用“页面看起来正常”代替测试。
- 视觉和交互以 `docs/superpowers/mockups/2026-10-22-source-config/index.html`、`cargo.html`、`preview.css` 和 `preview.js` 为基线；`landing.html`、`landing.css`、`landing.js` 和 `vendor/three.min.js` 属于首页原型，不在本计划范围。

## File Structure

| 文件 | 职责 |
| --- | --- |
| `src/lib.rs` | 注册内部 `docs` 模块，不扩大公共 API |
| `src/docs/mod.rs` | 暴露内部 `source_config` 模块 |
| `src/docs/source_config/mod.rs` | 定义 `GuideSection`、`Target`、内容入口和渲染入口 |
| `src/docs/source_config/content.rs` | 所有技术栈的静态内容、提示词、验证命令和注意事项 |
| `src/docs/source_config/render.rs` | 索引/详情页的 Markdown 与 HTML 渲染、HTML 转义、复制脚本和 CSS |
| `src/http.rs` | `/docs`、`/docs.md`、`/docs/{stack}`、`/docs/{stack}/index.md` 路由、请求 Host 到公开地址的转换、旧脚本路由保持 404 |
| `tests/http_test.rs` | 索引/详情路由、标题、请求 Host、Markdown、未知 stack 和旧脚本 404 的集成测试 |
| `docs/guides/vscode.md` | 增加源配置页面入口；旧脚本说明已经删除 |
| `docs/guides/README.md` | 增加源配置文档入口 |
| `README.md` | 在开始使用部分增加页面入口 |
| `src/tools/vscode-gallery.ps1` | 已删除；实施时只验证旧入口保持 404 |

## Task: 建立内容模型和 Markdown 文档

**Files:**
- Create: `src/docs/mod.rs`
- Create: `src/docs/source_config/mod.rs`
- Create: `src/docs/source_config/content.rs`
- Create: `src/docs/source_config/render.rs`
- Modify: `src/lib.rs`
- Test: `src/docs/source_config/mod.rs`（单元测试）

**Interfaces:**
- Consumes: 无。
- Produces:

```rust
pub(crate) struct GuideSection { /* 字段见 Spec */ }
pub(crate) enum Target { ServicePath(&'static str), AdminProvided(&'static str) }
pub(crate) fn sections() -> &'static [GuideSection];
pub(crate) fn section_by_id(id: &str) -> Option<&'static GuideSection>;
pub(crate) fn render_index_markdown(base_url: &str) -> String;
pub(crate) fn render_section_markdown(section: &GuideSection, base_url: &str) -> String;
```

- [ ] **Step 1: 写失败测试，固定页面内容契约**

在 `src/docs/source_config/mod.rs` 中加入：

```rust
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
    }
}
```

- [ ] **Step 2: 运行测试，确认模块尚不存在或渲染为空**

Run:

```text
cargo test --all-targets --all-features source_config
```

Expected: 编译失败，提示 `render_index_markdown`、`render_section_markdown` 或
`section_by_id` 不存在。

- [ ] **Step 3: 定义内容模型和静态 section 列表**

在 `src/docs/source_config/mod.rs` 中定义：

```rust
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
```

在 `src/docs/source_config/content.rs` 中放入 8 个 section，顺序固定为
`Rustup`、`Cargo`、`npm`、`Bun`、`NuGet`、`UPM`、`VS Code`、`Docker`。
每个 section 必须填满以下内容，不允许留空或 TODO：

- Rustup：`Target::ServicePath("/mirror/rustup/group")`，提示词要求设置用户级
  `RUSTUP_DIST_SERVER`，验证 `rustup show`，说明需要重新打开终端。
- Cargo：`Target::ServicePath("/mirror/cargo/group/")`，提示词要求合并
  `~/.cargo/config.toml`，只改 crates.io source replacement，验证 `cargo fetch`。
- npm：`Target::ServicePath("/mirror/npm/group/")`，提示词要求修改用户级 `.npmrc`，
  验证 `npm config get registry`，说明项目级 `.npmrc` 可能覆盖。
- Bun：`Target::ServicePath("/mirror/npm/group/")`，提示词要求修改用户级
  `.bunfig.toml`，验证 `bun install` 或 `bun pm cache`，说明项目级 `bunfig.toml`
  优先。
- NuGet：`Target::ServicePath("/mirror/nuget/group/v3/index.json")`，提示词要求修改
  用户级 `NuGet.Config`，备份、保留凭据项、验证 `dotnet restore`，说明项目级配置
  可能覆盖。
- UPM：`Target::ServicePath("/mirror/upm/group")`，提示词要求从当前 Unity 项目
  `Packages/manifest.json` 读取并保留现有 `scopedRegistries` 和 `scopes`，验证
  `Packages/manifest.json` 可解析和 Unity 包解析成功。
- VS Code：`Target::ServicePath("/mirror/vscode/group")`，提示词要求找到当前安装的
  `product.json`、备份、只改 `serviceUrl`、`resourceUrlTemplate`、
  `extensionUrlTemplate`，验证 JSON 可解析并要求重启 VS Code。
- Docker：`Target::AdminProvided("Docker Group 地址由管理员提供")`，提示词要求先
  向用户确认地址、检查管理员权限、备份 daemon 配置、只改 `registry-mirrors`，
  验证 `docker info`；明确不能猜测地址，不能修改无关 daemon 配置。

每个 `prompt` 使用 `{base_url}` 占位符，每个 `manual_steps` 和 `verify` 都写具体命令，
不能写“按实际情况处理”或“参考文档”。

- [ ] **Step 4: 实现 Markdown 渲染**

在 `src/docs/source_config/render.rs` 中实现：

```rust
pub(crate) fn render_index_markdown(base_url: &str) -> String {
    let mut out = String::from("# Stacktory | 使用说明\n\n");
    out.push_str("可以直接按手动步骤配置，也可以把提示词复制给智能体。所有操作先备份，失败时停止。\n\n");
    out.push_str("## 技术栈\n\n");
    for section in crate::docs::source_config::sections() {
        out.push_str(&format!(
            "- [{}](/docs/{}/index.md)：{}\n",
            section.title, section.id, section.summary
        ));
    }
    out
}

pub(crate) fn render_section_markdown(section: &GuideSection, base_url: &str) -> String {
    let mut out = format!("# Stacktory | 使用说明 | {}\n\n", section.title);
    out.push_str(&format!("{}\n\n", section.summary));
    out.push_str(&format!("入口：`{}`\n\n", target_text(&section.target, base_url)));
    out.push_str("## 给智能体的提示词\n\n```text\n");
    out.push_str(&section.prompt.replace("{base_url}", base_url));
    out.push_str("\n```\n\n## 手动步骤\n\n");
    out.push_str(section.manual_steps);
    out.push_str("\n\n## 验证\n\n```text\n");
    out.push_str(section.verify);
    out.push_str("\n```\n\n## 注意\n\n");
    for note in section.notes {
        out.push_str(&format!("- {note}\n"));
    }
    out
}
```

`target_text` 对 `ServicePath(path)` 返回
`format!("{}{}", base_url.trim_end_matches('/'), path)`，对 `AdminProvided(text)` 返回 `text`。

- [ ] **Step 5: 注册内部模块并运行单元测试**

在 `src/lib.rs` 中加入：

```rust
mod docs;
```

在 `src/docs/mod.rs` 中加入：

```rust
pub(crate) mod source_config;
```

Run:

```text
cargo test --all-targets --all-features source_config
```

Expected: 2 个单元测试通过。

- [ ] **Step 6: 提交内容模型和 Markdown**

```text
git add -- src/lib.rs src/docs/mod.rs src/docs/source_config/mod.rs src/docs/source_config/content.rs src/docs/source_config/render.rs
git diff --cached --check
git commit -m "增加客户端源配置指南内容模型"
```

## Task: 实现人看的 HTML 页面

**Files:**
- Modify: `src/docs/source_config/render.rs`
- Test: `src/docs/source_config/mod.rs`

**Interfaces:**
- Consumes: `sections()`, `GuideSection`, `Target`。
- Produces:

```rust
pub(crate) fn render_index_html(base_url: &str) -> String;
pub(crate) fn render_section_html(section: &GuideSection, base_url: &str) -> String;
```

- [ ] **Step 1: 写 HTML 渲染的失败测试**

增加：

```rust
#[test]
fn index_html_has_navigation_and_stack_links() {
    let html = render_index_html("https://pkg.example.test");
    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("lang=\"zh-CN\""));
    assert!(html.contains("<title>Stacktory | 使用说明</title>"));
    assert!(html.contains("id=\"nav\""));
    assert!(html.contains("href=\"/docs/cargo\""));
    assert!(!html.contains("{base_url}"));
    assert!(!html.contains("<script src="));
    assert!(!html.contains("<link rel=\"stylesheet\""));
}

#[test]
fn section_html_has_copy_controls_and_dynamic_title() {
    let section = section_by_id("cargo").expect("cargo section");
    let html = render_section_html(section, "https://pkg.example.test");
    assert!(html.contains("<title>Stacktory | 使用说明 | Cargo</title>"));
    assert!(html.contains("data-copy-target=\"prompt-cargo\""));
    assert!(html.contains("https://pkg.example.test/mirror/cargo/group/"));
    assert!(html.contains("请把本机 Cargo"));
    assert!(html.contains("aria-live=\"polite\""));
    assert!(!html.contains("{base_url}"));
}

#[test]
fn html_escapes_dynamic_values() {
    let section = section_by_id("cargo").expect("cargo section");
    let html = render_section_html(section, "https://pkg.example.test?a=<script>");
    assert!(!html.contains("<script>alert"));
}
```

- [ ] **Step 2: 运行测试，确认 HTML 函数尚不存在**

Run:

```text
cargo test --all-targets --all-features index_html_has_navigation_and_stack_links
```

Expected: 编译失败，提示 `render_index_html` 不存在。

- [ ] **Step 3: 实现页面骨架**

`render_index_html` 和 `render_section_html` 输出同一个页面骨架：

```html
<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Stacktory | 使用说明</title>
  <style>/* 内联 CSS */</style>
</head>
<body>
  <header class="topbar">
    <div class="page-title">
      <span class="brand">Stacktory</span>
      <span class="divider">|</span>
      <span>使用说明</span>
      <!-- 详情页追加：<span class="divider">|</span><span>Cargo</span> -->
    </div>
    <div class="service-url">当前服务：<code>...</code></div>
  </header>
  <div class="layout">
    <nav id="nav" aria-label="技术栈导航">...</nav>
    <main>
      <section class="intro">...</section>
      <!-- 索引页：技术栈卡片链接；详情页：当前 section 的完整内容 -->
    </main>
  </div>
  <div id="copy-status" aria-live="polite"></div>
  <script>/* 只处理复制按钮 */</script>
</body>
</html>
```

索引页的每个技术栈卡片必须包含标题、简介、状态和 `/docs/{id}` 链接。
详情页必须包含：

- 标题和状态标签。
- `target_text` 生成的入口地址。
- 带 `id="prompt-{section.id}"` 的提示词 `<pre>`。
- 复制按钮 `data-copy-target="prompt-{section.id}"`。
- “自动配置”和“手动配置”两个标签页；默认选中自动配置，切换使用 `role="tab"`、
  `aria-selected`、`aria-controls` 和左右方向键。
- 手动配置标签页中的配置文件路径、配置片段、验证命令和注意事项。
- 注意事项列表。

详情页的 `<title>` 必须是 `Stacktory | 使用说明 | {section.title}`，页头必须显示
`Stacktory | 使用说明 | {section.title}`；索引页的 `<title>` 和页头只显示
`Stacktory | 使用说明`。

页面必须以已确认的 mockup 为视觉基线。CSS 可以直接内联输出，但样式和断点不能另起
一套；`preview.css` 中的导航、标签、提示词面板、复制按钮和窄屏规则应作为生产样式的
来源。没有 JavaScript 时，两个标签面板都要可见，提示词仍然可以被选中复制。

- [ ] **Step 4: 加入内联样式和响应式布局**

CSS 至少包含以下变量和规则：

以下片段只是最低结构契约；颜色、间距和响应式规则以 `preview.css` 为准，不重新设计
页面。

```css
:root {
  color-scheme: light;
  --bg: #f5f7f8;
  --surface: #ffffff;
  --text: #182126;
  --muted: #617078;
  --line: #d9e1e4;
  --accent: #087f8c;
  --accent-strong: #056570;
  --warning: #9a5b00;
}

* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--text); font-family: system-ui, sans-serif; }
.layout { display: grid; grid-template-columns: 220px minmax(0, 1fr); max-width: 1280px; margin: 0 auto; gap: 28px; padding: 24px; }
#nav { position: sticky; top: 24px; align-self: start; }
.guide-card { background: var(--surface); border: 1px solid var(--line); border-radius: 12px; padding: 24px; margin-bottom: 20px; }
pre { overflow-x: auto; padding: 16px; background: #f0f4f5; border-radius: 8px; }
code { font-family: ui-monospace, SFMono-Regular, Consolas, monospace; }
button { cursor: pointer; border: 1px solid var(--accent); color: var(--accent-strong); background: transparent; border-radius: 6px; padding: 6px 10px; }
button:focus-visible { outline: 3px solid rgba(8, 127, 140, .35); outline-offset: 2px; }
@media (max-width: 820px) {
  .layout { display: block; padding: 16px; }
  #nav { position: static; overflow-x: auto; white-space: nowrap; margin-bottom: 16px; }
  .guide-card { padding: 18px; }
}
@media (prefers-reduced-motion: reduce) {
  * { scroll-behavior: auto !important; }
}
```

不得使用外部字体、CDN、图片或第三方 CSS。页面在 360px 宽度下不能出现整页水平滚动；
`pre` 可以单独滚动。

- [ ] **Step 5: 加入复制按钮脚本**

脚本只做两件事：切换配置标签页，以及复制提示词。标签页需要支持点击、左右方向键、
`Home` 和 `End`；复制按钮只读取 `data-copy-target` 指向的文本，不使用 `innerHTML`。
核心复制逻辑如下：

```javascript
document.querySelectorAll("[data-copy-target]").forEach((button) => {
  button.addEventListener("click", async () => {
    const target = document.getElementById(button.dataset.copyTarget);
    if (!target) return;
    const text = target.innerText;
    try {
      await navigator.clipboard.writeText(text);
      document.getElementById("copy-status").textContent = "已复制提示词";
    } catch {
      document.getElementById("copy-status").textContent = "复制失败，请手动选择文本";
    }
  });
});
```

不能把提示词拼进 `innerHTML`，不能从 URL 读取脚本或动态执行代码。

- [ ] **Step 6: 运行 HTML 测试**

Run:

```text
cargo test --all-targets --all-features source_config
```

Expected: 所有 `source_config` 单元测试通过。

- [ ] **Step 7: 提交 HTML 页面**

```text
git add -- src/docs/source_config/render.rs
git diff --cached --check
git commit -m "实现客户端源配置页面"
```

## Task: 接入 HTTP 路由并验证旧脚本端点已移除

**Files:**
- Modify: `src/http.rs`
- Modify: `tests/http_test.rs`
- Test: `tests/http_test.rs`

**Interfaces:**
- Consumes: `section_by_id`、`render_index_html`、`render_section_html`、
  `render_index_markdown`、`render_section_markdown`、`RustupService::public_url()`。
- Produces: `GET /docs`、`GET /docs.md`、`GET /docs/{stack}`、
  `GET /docs/{stack}/index.md`；旧 `/tools/vscode-gallery.ps1` 返回 404。

- [ ] **Step 1: 写路由失败测试**

在 `tests/http_test.rs` 增加：

```rust
#[tokio::test]
async fn source_config_pages_use_request_host_and_stack_title() {
    let data_dir = tempdir().expect("data directory");
    let log_dir = tempdir().expect("log directory");
    let service = rustup_service(data_dir.path().to_path_buf(), log_dir.path().to_path_buf());
    let router = router_with_rustup(Arc::new(service));

    let index = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/docs")
                .header("host", "pkg.rd.kim")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .expect("docs request"),
        )
        .await
        .expect("docs index response");
    assert_eq!(index.status(), StatusCode::OK);
    assert_eq!(
        index
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/html; charset=utf-8")
    );
    let body = axum::body::to_bytes(index.into_body(), usize::MAX)
        .await
        .expect("docs index body");
    let html = String::from_utf8(body.to_vec()).expect("docs index UTF-8");
    assert!(html.contains("<title>Stacktory | 使用说明</title>"));
    assert!(html.contains("href=\"/docs/cargo\""));
    assert!(!html.contains("data-copy-target=\"prompt-cargo\""));

    let detail = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/docs/cargo")
                .header("host", "pkg.rd.kim")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .expect("cargo detail request"),
        )
        .await
        .expect("cargo detail response");
    assert_eq!(detail.status(), StatusCode::OK);
    let body = axum::body::to_bytes(detail.into_body(), usize::MAX)
        .await
        .expect("cargo detail body");
    let html = String::from_utf8(body.to_vec()).expect("cargo detail UTF-8");
    assert!(html.contains("<title>Stacktory | 使用说明 | Cargo</title>"));
    assert!(html.contains("https://pkg.rd.kim/mirror/cargo/group/"));
    assert!(html.contains("data-copy-target=\"prompt-cargo\""));
    assert!(!html.contains("{base_url}"));

    let markdown = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/docs/cargo/index.md")
                .header("host", "pkg.rd.kim")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .expect("cargo markdown request"),
        )
        .await
        .expect("cargo markdown response");
    assert_eq!(markdown.status(), StatusCode::OK);
    assert_eq!(
        markdown
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/markdown; charset=utf-8")
    );
    let body = axum::body::to_bytes(markdown.into_body(), usize::MAX)
        .await
        .expect("cargo markdown body");
    let markdown = String::from_utf8(body.to_vec()).expect("markdown UTF-8");
    assert!(markdown.contains("# Stacktory | 使用说明 | Cargo"));
    assert!(markdown.contains("https://pkg.rd.kim/mirror/cargo/group/"));

    let unknown = router
        .oneshot(
            Request::get("/docs/unknown")
                .body(Body::empty())
                .expect("unknown section request"),
        )
        .await
        .expect("unknown section response");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

```

旧脚本端点的 404 回归测试已经存在，测试名为
`removed_vscode_script_endpoint_returns_not_found`；不要重复添加同名覆盖。

- [ ] **Step 2: 运行测试，确认新路由不存在**

Run:

```text
cargo test --test http_test
```

Expected: 新路由返回 404，相关测试失败；旧脚本端点应返回 404。

- [ ] **Step 3: 确认旧脚本入口已完全移除**

`src/tools/vscode-gallery.ps1` 和对应路由已经移除。实现前确认 `src/http.rs` 中不存在：

- `VSCODE_GALLERY_TOOL`
- `GALLERY_URL_PLACEHOLDER`
- `vscode_gallery_tool`
- `gallery_url_for_request`
- `forwarded_header`
- `is_valid_forwarded_host`
- `is_local_host`
- `powershell_single_quoted`
- `/tools/vscode-gallery.ps1` 路由

保留 `RustupService::public_url()`，供页面回退使用。不要重新引入脚本或远程执行入口。

- [ ] **Step 4: 添加公开地址解析和页面 handler**

在 `src/http.rs` 中增加：

```rust
async fn source_config_index_page(
    State(service): State<Arc<RustupService>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let base_url = public_base_url_for_request(&headers, service.public_url());
    html_response(crate::docs::source_config::render_index_html(
        base_url.as_str(),
    ))
}

async fn source_config_section_page(
    State(service): State<Arc<RustupService>>,
    Path(stack): Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    let Some(section) = crate::docs::source_config::section_by_id(&stack) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let base_url = public_base_url_for_request(&headers, service.public_url());
    html_response(crate::docs::source_config::render_section_html(
        section,
        base_url.as_str(),
    ))
}

async fn source_config_index_markdown(
    State(service): State<Arc<RustupService>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let base_url = public_base_url_for_request(&headers, service.public_url());
    text_response(
        "text/markdown; charset=utf-8",
        crate::docs::source_config::render_index_markdown(base_url.as_str()),
    )
}

async fn source_config_section_markdown(
    State(service): State<Arc<RustupService>>,
    Path(stack): Path<String>,
    headers: HeaderMap,
) -> axum::response::Response {
    let Some(section) = crate::docs::source_config::section_by_id(&stack) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let base_url = public_base_url_for_request(&headers, service.public_url());
    text_response(
        "text/markdown; charset=utf-8",
        crate::docs::source_config::render_section_markdown(
            section,
            base_url.as_str(),
        ),
    )
}

fn html_response(body: String) -> axum::response::Response {
    text_response("text/html; charset=utf-8", body)
}

fn text_response(content_type: &'static str, body: String) -> axum::response::Response {
    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static(content_type),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
```

`public_base_url_for_request` 的行为：

- 优先读取 `x-forwarded-host`，其次 `Host`。
- 只接受 ASCII 字母、数字、`.`、`-`、`:`、`[`、`]`，长度不超过 255。
- 优先读取 `x-forwarded-proto` 的 `http`/`https`；没有时对 localhost/127.0.0.1/[::1]
  使用 `http`，其他 Host 使用 `https`。
- 没有合法 Host 时使用 `service.public_url()`。
- 只保留 fallback 的路径前缀，不带 query/fragment。

响应头：

- `/docs`：`Content-Type: text/html; charset=utf-8`
- `/docs.md`：`Content-Type: text/markdown; charset=utf-8`
- 两者都设置 `Cache-Control: no-store`

- [ ] **Step 5: 注册路由并运行集成测试**

在 `router_with_rustup` 增加：

```rust
.route("/docs", get(source_config_index_page))
.route("/docs.md", get(source_config_index_markdown))
.route("/docs/{stack}", get(source_config_section_page))
.route("/docs/{stack}/index.md", get(source_config_section_markdown))
```

Run:

```text
cargo test --test http_test
```

Expected: `http_test` 全部通过，包括旧脚本端点 404。

- [ ] **Step 6: 提交路由和清理**

```text
git add -- src/http.rs tests/http_test.rs
git diff --cached --check
git commit -m "接入客户端源配置页面"
```

## Task: 更新文档入口并完成门禁

**Files:**
- Modify: `docs/guides/vscode.md`
- Modify: `docs/guides/README.md`
- Modify: `README.md`

**Interfaces:**
- Consumes: `/docs` 页面。
- Produces: 用户可从 README 和 VS Code 指南进入客户端源配置文档。

- [ ] **Step 1: 增加 VS Code 的源配置页面入口**

在 `docs/guides/vscode.md` 的“使用条件”之后增加：

```markdown
## 源配置文档

打开 [Stacktory | 使用说明](https://pkg.rd.kim/docs)，进入 VS Code 详情页，复制提示词
发给智能体。提示词要求先备份当前 `product.json`，只修改扩展市场地址，并在修改后要求
完全重启 VS Code。

页面提供的提示词不包含凭据，也不会自动执行远程脚本。
```

- [ ] **Step 2: 增加文档导航入口**

在 `docs/guides/README.md` 的制品库文档列表前增加：

```markdown
- [Stacktory | 使用说明](https://pkg.rd.kim/docs)
```

在 `README.md` 的“开始使用”段落增加：

```markdown
需要配置本机开发工具源时，打开 [Stacktory | 使用说明](https://pkg.rd.kim/docs)。
页面按技术栈提供手动步骤和可复制提示词，不执行远程脚本。
```

- [ ] **Step 3: 检查文档和页面内容不漂移**

Run:

```text
rg -n -e "vscode-gallery\.ps1" -e "irm .*iex" -e "curl .*[|] sh" README.md docs src
```

Expected: 只允许测试代码或历史计划中出现旧命令；当前用户指南中不再出现旧的一行脚本。

- [ ] **Step 4: 运行完整门禁**

Run:

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
git diff --check
```

Expected: 全部通过。

- [ ] **Step 5: 提交文档**

```text
git add -- README.md docs/guides/README.md docs/guides/vscode.md
git diff --cached --check
git commit -m "补充客户端源配置文档入口"
```

## Coverage Review

| Spec 要求 | 对应任务 |
| --- | --- |
| `/docs`、`/docs.md`、`/docs/{stack}`、`/docs/{stack}/index.md` | 接入 HTTP 路由 |
| 同一份内容模型 | 建立内容模型和 Markdown |
| 人看的页面、复制按钮、响应式布局 | 实现 HTML 页面 |
| 当前请求 Host 或 `public_url` | 接入 HTTP 路由 |
| 8 个技术栈及边界 | 建立内容模型和 Markdown |
| 不执行远程脚本 | 旧脚本入口保持 404、页面只读 |
| 不暴露上游和凭据 | 内容模型验收测试 |
| 文档入口 | 更新文档入口 |
| 旧脚本入口保持 404 | 接入 HTTP 路由 |
