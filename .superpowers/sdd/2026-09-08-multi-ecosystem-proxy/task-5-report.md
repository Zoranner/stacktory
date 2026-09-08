# Task 5 文档报告

## 状态

已完成后续生态适配器契约文档。本任务只修改文档，没有修改生产代码、测试、
依赖、配置运行逻辑，也没有实现 npm、UPM、PyPI 或 NuGet。

## 变更文件

- `docs/architecture/multi-ecosystem-readonly-proxy.md`
  - 统一所有目标路由为 `/mirror/{ecosystem}/{name}/...`。
  - 固定 `proxy` 只有一个 `base_url`，由适配器按协议对象键生成 metadata、
    index、registration 和 artifact 请求。
  - 固定 `group` 不配置 `base_url`，只按 `members` 顺序引用同一生态的 proxy；
    没有 group 时多个 proxy 彼此独立、不自动组合或回退。
  - 增加 metadata 到 artifact 的 `selected_proxy` 来源绑定，禁止不同来源的
    metadata、版本集合和制品隐式混合。
  - 固定无数据库、文件系统缓存/快照/审计、缓存键组成、原子发布、完整性和
    错误边界。
  - 明确未注册生态不暴露假路由，Rustup 旧路由仅作为已移除边界说明。

- `docs/architecture/npm-upm-mvp.md`
  - 定义 npm metadata、普通/scoped package 和 tarball 的只读目标边界。
  - 定义 `/mirror/npm/{name}/...` 与独立 `/mirror/upm/{name}/...` 入口。
  - 明确 UPM 复用 npm-compatible metadata/tarball 核心，但独立处理 scope、
    Unity package name 和客户端入口，不复制 npm handler。
  - 固定 scoped npm metadata 的 `%2F` 原始路径特例、tarball 受限对象键、
    来源绑定、完整性校验和缓存隔离。
  - 排除 publish、unpublish、dist-tag 修改、账号、token、包写入、数据库和
    任意 URL。

- `docs/architecture/pypi-mvp.md`
  - 定义 `/mirror/pypi/{name}/simple/...` 和文件下载目标路径。
  - 固定 Simple API 项目名规范化、HTML/JSON 表示、`Accept` 协商和 `406` 边界。
  - 明确文件链接必须入口化重写并保留上游 hash；无摘要时不伪装成强完整性证明。
  - 固定 metadata/artifact 缓存、selected proxy 来源绑定、group 回退和只读
    排除项。

- `docs/architecture/nuget-mvp.md`
  - 定义 `/mirror/nuget/{name}/v3/index.json`、registration 和 package content
    目标路径。
  - 明确 service index 资源发现、`@id` 重写、registration 读取和 nupkg 下载
    的适配职责。
  - 固定单一 `base_url`、selected proxy 绑定、资源范围校验、完整性和缓存隔离。
  - 排除搜索、push、delete、API key、账号、符号服务和未知 service resource
    的伪支持。

## 官方协议依据

文档中已链接并按官方协议边界整理：

- npm Registry API。
- Unity Manual 的 scoped registries。
- PyPA Simple Repository API。
- Microsoft NuGet API overview 与 V3 service index。

官方协议事实只用于确定请求表示、资源关系、路径和 hash/URL 字段；不表示
Stacktory 已经实现相应生态。

## 验证

### 文档静态检查

当前环境未安装 `markdownlint`、`markdownlint-cli2`、`vale`、`rumdl` 或
`cmark`，因此执行了等价的仓库内 PowerShell 静态门禁：

```text
markdown-static-check: PASS (4 architecture files)
route-contract-check: PASS
boundary-check: PASS
untracked-doc-diff-check: PASS
```

检查内容包括：

- 每个文档只有一个 H1，代码围栏成对。
- 目标生态路由包含 `{name}`，并统一使用
  `/mirror/{ecosystem}/{name}/...`。
- 不重新引入 `default_source` 或 `upstreams`。
- 每个文档明确 `base_url`、group、只读、缓存、完整性和官方协议依据。
- 明确无数据库、任意 URL 禁止、未实现状态、来源绑定和响应表示隔离。

### Git 检查

对四个新/改架构文档执行了未跟踪文件检查：

```text
git diff --no-index --check -- NUL <document>
```

结果：全部通过。提交前还会对 staged diff 执行 `git diff --cached --check`。

本任务仅为文档变更，不执行 Rust 格式化、Clippy、测试或 release build；Task 4
已有 Rust 验证结果不作为本任务的新验证证据。

## 提交

提交标题：

```text
补充后续生态只读适配契约
```

提交内容包含四个架构文档和本报告。最终 commit hash 以提交后 Git 输出为准。

## Concerns

- npm、UPM、PyPI 和 NuGet 当前仍未实现或注册路由；本文档中的路径是目标契约，
  不是运行能力。
- 官方文档链接和协议细节仍需在各生态实现阶段结合真实客户端版本复核；本任务
  未执行 npm CLI、Unity Package Manager、pip 或 `dotnet restore` 联调。
- UPM scope 冲突、PyPI 默认表示选择和 NuGet service resource 支持集合已经
  被要求显式化，但具体运行配置 schema 仍属于对应适配器实现任务。
- 本任务未修改工作树中既有的 `docs/plans/`、`docs/reviews/` 和其他未相关文件。
