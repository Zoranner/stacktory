# Stacktory 开发工具源配置文档页设计

## 目标

为开发机用户提供一个只读的在线文档页面，覆盖各技术栈的源配置方法。人可以直接阅读
手动步骤，也可以复制提示词交给智能体；页面同时提供原始 Markdown，方便智能体读取。
不引入 CLI、PowerShell 脚本、Shell 脚本或 MCP 工具。

## 非目标

- 不在服务端执行或修改用户本机配置。
- 不提供 `irm | iex`、`curl | sh` 或其他远程代码执行入口。
- 不实现 CLI、MCP、客户端守护进程或配置同步服务。
- 不增加任何运行时配置字段。
- 不替用户决定 Unity scope、Docker 管理员地址或项目级 NuGet 配置。
- 不暴露上游制品库地址、凭据、内部日志或缓存路径。
- 不实现首页 landing 原型中的三维背景、动效或首页路由；本设计只覆盖 `/docs`。

## 页面

提供目录、使用说明和详情三类只读地址：

```text
GET /docs
GET /docs.md
GET /docs/guide
GET /docs/guide/index.md
GET /docs/{stack}
GET /docs/{stack}/index.md
```

`/docs` 是文档目录；`/docs/guide` 是独立的通用使用说明页；`/docs/{stack}` 是单个
技术栈的详情页。`/docs.md`、`/docs/guide/index.md` 和 `/docs/{stack}/index.md` 是
同一份内容模型渲染出的 Markdown。所有地址共用结构化内容，不允许手工维护多份文案。

浏览器标题规则：

```text
/docs              Stacktory | 文档
/docs/guide        Stacktory | 文档 | 使用说明
/docs/rustup       Stacktory | 文档 | Rustup
/docs/cargo        Stacktory | 文档 | Cargo
```

页面头部固定显示 `Stacktory | 文档`；使用说明页和技术栈详情页在右侧追加当前页面标题。
子标题来自当前页面内容，由服务端渲染，不能由客户端脚本猜测或硬编码。
未知 `stack` 返回 `404`。

页面默认使用当前请求的 Host 和协议生成服务地址；如果没有请求 Host，则使用服务配置
中的 `server.public_url`。通过 `https://pkg.rd.kim/docs` 访问时，页面中的地址和提示词
都使用 `pkg.rd.kim`。

## 信息架构

页面由以下部分组成：

- 页头：产品名、当前服务地址、页面说明和使用边界。
- 目录：第一项是“使用说明”，其后按生态分组展示技术栈目录行。
- 使用说明独立页面：先备份、只改目标配置、完成后验证、失败时停止。
- 技术栈导航按生态分组：Rust（Rustup、Cargo）、Node.js（npm、Bun）、.NET（NuGet）、
  Unity（UPM）、VS Code、Docker；Node.js 后续可继续增加 pnpm、yarn，不改变页面结构。
- 目录页先展示使用说明，再按生态分组展示工具目录行，包含简介、配置入口数量和详情链接。
- 侧栏导航把“使用说明”放在最前，再按生态分组排列技术栈。
- 技术栈详情页包含当前入口选择、当前生态的全部入口、给智能体的提示词、手动步骤、
  验证命令和注意事项。
- 页尾：配置失败时保留备份并报告；页面不执行本地命令。

### 技术栈内容

| 生态 | 工具 | 提示词目标 | 必须说明的边界 |
| --- | --- | --- | --- |
| Rust | Rustup | 设置 `RUSTUP_DIST_SERVER` | 用户级环境变量；重新打开终端 |
| Rust | Cargo | 修改 `~/.cargo/config.toml` 的 source replacement | 保留现有 source；不覆盖无关配置 |
| Node.js | npm | 设置 npm registry | 用户级 `.npmrc`；项目级配置可能覆盖 |
| Node.js | Bun | 修改用户级 `.bunfig.toml` | 与 npm 分开；项目级 `bunfig.toml` 优先 |
| .NET | NuGet | 修改用户级 `NuGet.Config` | 项目级 `NuGet.Config` 可能覆盖；不要添加凭据 |
| Unity | UPM | 修改当前 Unity 项目 `Packages/manifest.json` | 需要项目根目录；scope 由项目决定 |
| VS Code | VS Code | 修改 VS Code `product.json` 的 Gallery 三项 | 必须备份；升级后可能被覆盖 |
| Docker | Docker | 修改 Docker `registry-mirrors` | 需要管理员权限和重启；不猜测地址 |

入口不是写死的 `group`。每个详情页从当前服务配置读取对应生态的全部 source，展示
入口名、`Group`/`Proxy` 类型、Group 成员、解析后的地址和 Docker 客户端地址。Proxy 和
Group 分开展示；页面初始选择第一个 Group，没有 Group 时选择第一个 Proxy。该初始值
只用于生成首屏配置步骤，用户可以在详情页切换当前入口，不表示推荐。
Docker 的独立 Registry 入口用 `listen_port` 和 `public_url` 成对配置，
页面只展示客户端可访问的完整地址，不单独展示监听端口。

没有实现的后端生态（PyPI、Winget）不出现在可执行目录中，只保留在页面说明里。

## 给智能体的提示词

每个提示词必须包含以下约束：

1. 先读取目标配置的当前内容，确认文件位置和格式；
2. 修改前备份，并在结果中报告备份路径；
3. 只改本技术栈需要改的键或条目，不删除其他来源；
4. 保持幂等，重复执行不重复追加；
5. 不写入凭据、token、密码或用户隐私信息；
6. 修改后执行可观察的验证命令；无法验证时明确报告未验证；
7. 路径、权限、格式或客户端版本不确定时停止，不猜测；
8. 不修改无关项目、全局代理或其他技术栈配置。

提示词示例：

```text
请把本机 Cargo 的 crates.io 源切换到 Stacktory。

目标地址：{base_url}/mirror/cargo/group/
要求：
1. 先读取 ~/.cargo/config.toml，保留无关配置；
2. 修改前备份原文件；
3. 只修改 crates.io 的 source replacement，不删除其他 source；
4. 修改后执行 cargo fetch 验证；
5. 输出实际修改的文件、备份路径和验证结果；
6. 如果路径、权限或配置格式不确定，停止并报告，不要猜测。
```

页面渲染时把 `{base_url}` 和 `{endpoint}` 替换为当前服务地址和当前入口；不得把未替换
的占位符发给用户。

## 人看的页面

页面不依赖外部 CSS、字体、脚本或前端框架，使用服务端渲染的单个 HTML 文档。

布局：

- 桌面端左侧固定导航，右侧内容区；移动端导航变为顶部横向可滚动列表。
- 页头显示当前服务地址和“可以手动配置，也可以复制提示词给智能体”的简短说明。
- 每个技术栈是一行目录项，显示简介和当前配置的入口数量，不使用状态标签。
- 提示词放在 `<pre><code>` 中，旁边有复制按钮；复制成功通过 `aria-live` 提示。
- 详情页在配置区提供当前入口选择器，切换后提示词、手动步骤和验证命令中的地址同步更新；
  只有一个入口时直接显示该入口，不显示选择器。
- 详情页提供“自动配置”和“手动配置”两个标签，默认显示自动配置；标签支持点击、
  左右方向键、`Home` 和 `End` 切换。
- 自动配置区显示提示词和复制按钮；手动配置区显示配置文件路径、配置片段、验证命令
  和注意事项。
- 没有 JavaScript 时两个标签面板都展开，提示词仍然可以被选中复制。
- 注意事项使用短列表，不使用大段说明文字。

视觉：

- 使用系统字体和深色中性背景，暖金色作为强调色，与首页共用颜色、字体和间距令牌。
- 代码块使用等宽字体、浅色底和可横向滚动。
- 入口表和 Group 列表使用细分隔线，不使用没有信息量的状态标签或卡片网格。
- 焦点样式清晰，复制按钮可键盘操作，颜色对比满足可读性。
- 不加载网络资源，避免离线或内网环境出现布局缺失。

## 技术设计

新增 `src/docs/source_config/`，其中 `content.rs` 解析技术栈内容文件，`overview.rs`
解析通用使用说明，`catalog.rs` 把运行时配置转换为文档入口，`render.rs` 只负责
HTML/Markdown 输出：

```rust
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
```

每个工具的正文放在 `web/docs/content/<id>.md`，使用简单的 front matter 和固定章节：

```text
---
id: cargo
group: Rust
title: Cargo
summary: ...
ecosystem: cargo
---
## 提示词
...
## 手动步骤
...
## 验证
...
## 注意
- ...
```

新增工具只需增加内容文件并在 `content.rs` 的文件列表中登记；新增生态分组不需要改
渲染器。页面模板和样式位于 `web/docs/index.html`、`web/docs/index.css`、`web/docs/index.js`。
通用使用说明位于 `web/docs/content/guide.toml`，包含标题、摘要和按章节组织的条目。

渲染函数：

```rust
pub(crate) fn render_index_markdown(base_url: &str, catalog: &SourceCatalog) -> String;
pub(crate) fn render_guide_markdown(base_url: &str) -> String;
pub(crate) fn render_section_markdown(
    section: &GuideSection,
    base_url: &str,
    catalog: &SourceCatalog,
) -> String;
pub(crate) fn render_index_html(base_url: &str, catalog: &SourceCatalog) -> String;
pub(crate) fn render_guide_html(base_url: &str) -> String;
pub(crate) fn render_section_html(
    section: &GuideSection,
    base_url: &str,
    catalog: &SourceCatalog,
) -> String;
```

HTML 和 Markdown 渲染器只读取同一份内容模型，并且：

- 对 HTML 文本、属性、提示词和 URL 使用 HTML 转义。
- 对 `{base_url}` 做一次明确替换，渲染后检查页面中不存在该占位符。
- Markdown 和 HTML 的标题、顺序、提示词和验证命令保持一致。
- 目录页、使用说明页和技术栈详情页共享页面骨架、侧边导航和 CSS；非目录页的浏览器
  标题追加当前页面标题。
- `web/docs/index.js` 只切换标签和复制 `data-copy-target` 指向的文本，不使用 `innerHTML`。

HTTP 路由放在主服务路由中：

```text
GET /docs
GET /docs.md
GET /docs/guide
GET /docs/guide/index.md
GET /docs/{stack}
GET /docs/{stack}/index.md
```

Docker listener 不提供这些文档页面。页面不增加运行时配置；服务地址来自请求或 `public_url`。

## 验收

- `/docs` 返回 `200` 和 `text/html; charset=utf-8`，标题为 `Stacktory | 文档`。
- `/docs` 的第一项是“使用说明”，其后是按生态分组的技术栈目录。
- `/docs/guide` 返回 `200`，标题为 `Stacktory | 文档 | 使用说明`。
- `/docs/rustup` 返回 `200`，标题为 `Stacktory | 文档 | Rustup`。
- `/docs/cargo` 返回 `200`，标题为 `Stacktory | 文档 | Cargo`。
- `/docs/unknown` 返回 `404`。
- `/docs.md` 返回 `200` 和 `text/markdown; charset=utf-8`。
- `/docs/guide/index.md` 返回 `200` 和 `text/markdown; charset=utf-8`。
- `/docs/rustup/index.md` 返回 `200` 和 `text/markdown; charset=utf-8`。
- 页面包含全部可用技术栈，且每个技术栈详情页有可复制提示词。
- npm 和 Bun 位于同一个 Node.js 分组，共用当前 npm 生态的入口列表。
- 详情页列出当前生态配置的全部 Group/Proxy 入口、成员、地址和 Docker 客户端地址，
  不再只显示一个写死的 `group`。
- Unity 页面的入口列表来自 UPM 配置，而不是单一的 `group` 字符串。
- 用户修改 `web/docs/content/*.md` 或 `web/docs/content/guide.toml` 后，页面文案和
  提示词随构建更新。
- 通过 `pkg.rd.kim` 访问时，未替换的 `{base_url}` 不出现，地址指向 `pkg.rd.kim`。
- 渲染结果中不出现未替换的 `{endpoint}`。
- 页面不包含上游 URL、token、密码或本地缓存路径。
- 复制按钮可用；没有 JavaScript 时，提示词仍然可以被选中复制。
- 页面在窄屏下不出现水平溢出；代码块可独立横向滚动。
- 现有旧脚本端点和 `src/tools/vscode-gallery.ps1` 被移除。
- `cargo fmt --all -- --check`、`cargo clippy --all-targets --all-features -- -D warnings`、
  `cargo test --all-targets --all-features`、`cargo build --release --locked` 通过。
