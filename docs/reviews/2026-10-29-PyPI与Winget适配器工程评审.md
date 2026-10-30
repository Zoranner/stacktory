# Stacktory Rust 工程评审报告

## 评审范围

本次评审对象是 `a4006a8..HEAD` 的 7 个提交，即 PyPI Simple API 与 Winget
REST source 两个只读适配器。评审内容限于工程体系、架构融合、缓存与审计
一致性、文档同步和验证路径，不评价协议字段的业务完备性，也不把未执行的
真实客户端联调写成通过。

## 评审依据

- `review-engineering` 通用工程基线与 `stacks/rust.md` 专项清单。
- 仓库既有架构文档：`docs/architecture/multi-ecosystem-readonly-proxy.md`、
  `pypi-mvp.md`、`winget-mvp.md`。
- `docs/reviews/2026-10-21-系统级工程评审.md` 记录的缓存身份、绑定生命周期、
  回退审计和摘要重算整改方向。
- `git-commit` 规范的提交边界和 message 要求。
- `Cargo.toml` 的 edition 2024、rust-version 1.98，以及
  `.github/workflows/release.yml` 的 fmt、clippy、test、release build 门禁。

## 总体结论

两个适配器与现有体系融合良好。适配器注册、`SourcePool`/`GroupRun` 回退、
`storage::layout`、审计、配置校验、文档页目录和示例配置都已接入，没有复制
协议 handler，也没有引入平行的路由或缓存体系。PyPI 采用“规范项目对象 +
HTML/JSON 表示”的单一事实来源，Winget 把安装包允许列表和逐跳重定向校验
放在共享上游层，边界清楚。原提交新增 43 个集成测试，整改又补充失败语义、
回退和绑定生命周期测试，当前树的全量测试为 419 项通过。

评审发现的问题已经在对应责任层收口：Winget 绑定损坏会自愈为缓存未命中，
`Version` 请求只校验目标版本，PyPI 过期元数据在上游失败时回退到 last-good，
两个新适配器的审计写入失败统一为 fail-closed，group 回退补齐逐成员审计。
允许列表主机的私网解析、无摘要发布审计和文档漂移也已修正。绑定 sidecar
已增加跨生态的独立保留窗口，并在读取时刷新访问时间。

## 检查情况

| 检查 | 命令 | 结果 |
| --- | --- | --- |
| 工作区 | `git status --short --branch` | 整改未提交，`master` 领先 `origin/master` 7 个提交 |
| 合并痕迹 | `git log --merges` | 无输出 |
| 格式 | `cargo fmt --all -- --check` | 通过 |
| 静态检查 | `cargo clippy --all-targets --all-features -- -D warnings` | 通过 |
| 测试 | `cargo test --all-targets --all-features --quiet` | 419 项通过 |
| 发布构建 | `cargo build --release --locked` | 通过 |
| 文档构建 | `cargo doc --all-features --no-deps` | 通过 |

未执行项：真实 `pip` 与 `winget` 客户端联调。仓库没有可用的受信 HTTPS
入口、管理员终端和可安装测试包，`docs/architecture/pypi-mvp.md` 记录的
pip 25.1.1 实测也没有对应的 `docs/verification/` 记录，无法在本次评审中复核。

## 主要问题

### Winget 绑定文件损坏后返回 500，不能自愈

定性：规范缺失导致风险（失败恢复一致性）。
分级：中。
状态：已修复。

绑定读取现在负责校验并清理损坏 sidecar：
`src/ecosystems/winget/cache/mod.rs` 的 `read_binding` 在 JSON 解析失败或
`binding_is_valid` 不通过时删除文件并返回 `Ok(None)`，由安装包入口返回
`404`。校验覆盖 token、package ID/version 路径安全、SHA-256 格式、
`selected_proxy`、来源指纹和 upstream URL；安装包服务层不再保留第二份
`validate_binding`。`corrupt_installer_binding_is_treated_as_a_cache_miss`
验证了损坏 sidecar 返回 `404` 且文件被删除。

### Winget 校验全部版本，非目标版本会阻断当前安装

定性：可选优化建议（协议校验范围）。
分级：中。
状态：已修复。

`src/ecosystems/winget/manifest.rs` 的 `rewrite_manifest` 在请求带
`Version` 时定位目标版本，只校验并重写该版本，然后从响应中移除其他版本。
目标版本不存在时返回 `VersionNotFound`，直接入口和 group 入口都映射为
`404`；请求不带 `Version` 时仍校验全部版本，任一版本不合法都会按来源失败
处理。`version_query_only_validates_the_requested_version` 和
`group_missing_requested_version_returns_not_found` 覆盖了目标版本隔离、
未知版本和 group 状态。

### PyPI 元数据过期后验证失败直接 502

定性：规范缺失导致风险（跨生态缓存策略不一致）。
分级：中。
状态：已修复。

`src/ecosystems/pypi/service/metadata.rs` 的 `project_index_snapshot` 现在
区分三种结果：来源指纹有效且未过期时直接命中；重新验证得到 `404` 时删除
本地 metadata 并返回 `404`；其他上游失败在来源指纹仍有效时返回 last-good，
并记录 `pypi_metadata_revalidate_failed` 与 `pypi_metadata_stale`。来源
配置指纹变化时不使用旧缓存。`stale_metadata_is_served_when_revalidation_fails`
和 `revalidation_not_found_removes_cached_metadata` 覆盖了这两条路径，
`README.md` 与 `docs/architecture/pypi-mvp.md` 已同步。

### 两个新适配器对审计写入失败的处理相反

定性：规范缺失导致风险（平台失败策略未统一）。
分级：中。
状态：已修复。

两个适配器统一采用 fail-closed：PyPI 新增 `PyPiError::Audit`，`record` 和
`record_metadata_source_failure` 返回 `Result`，所有调用点传播审计错误；
Winget 的 `record_object` 和 `record_source_failure` 同样传播错误。审计文件
不可写时请求返回 `500`，不会在丢审计的情况下继续服务。Winget group 回退的
成员失败审计也受同一约束，`source_failure_audit_write_failure_stops_fallback`
验证了回退路径不会吞掉审计故障；PyPI 的
`index_returns_500_when_audit_storage_fails` 覆盖直接请求路径。
`docs/architecture/multi-ecosystem-readonly-proxy.md`、`pypi-mvp.md` 和
`winget-mvp.md` 的错误边界均已写成审计失败返回 `500`。

### Winget 的 group 回退缺少逐成员审计

定性：可选优化建议（与既有整改方向不一致）。
分级：中低。
状态：已修复。

`src/ecosystems/winget/service/source.rs` 的 `information`、`manifestSearch`
和 `packageManifests`，以及 `service/artifact.rs` 的安装包重绑定、策略拒绝
和下载失败分支，都会在 `run.record_failure` 之外记录带 `selected_proxy`、
`cache_status`、`failure_category` 的成员级事件。共享的
`source_failure_category` 已提升到 `src/upstream/fallback.rs`，PyPI、npm 和
Rustup 不再各自维护同名实现。`manifest_falls_back_without_merging_and_records_the_selected_proxy`
断言了 `winget_manifest_source_failed`，同时保留原有 `selected_proxy` 绑定
检查。

### 内网安装包主机判定依赖域名后缀

定性：违反项目已确认规范（配置语义与文档不一致）。
分级：中低。
状态：已修复。

`src/upstream/fetch/mod.rs` 的 `allowlisted_redirect_response` 现在对所有
安装包请求使用允许私网解析的客户端。允许列表本身是运维声明的信任边界，
每一跳重定向仍重新校验主机、scheme 和凭据，列表外主机不会发出请求。
`docs/architecture/winget-mvp.md` 已改为同一表述。该改动不扩大
`SourcePool` 普通来源请求的私网策略。

### PyPI 没有记录“无上游摘要”状态

定性：违反项目已确认规范（文档与实现漂移）。
分级：中低。
状态：已修复。

`src/ecosystems/pypi/service/artifact.rs` 在发布成功且
`expected_hashes.is_empty()` 时记录
`pypi_artifact_published_without_digest`，明确区分“按摘要校验”和“上游未
声明摘要”。摘要存在时仍先校验后发布，失败按 `integrity_mismatch` 回退。
`artifact_without_upstream_digest_records_audit_state` 覆盖了无摘要发布。

### Winget 绑定没有淘汰路径

定性：可选优化建议（平台级遗留问题被放大）。
分级：中低。
状态：已修复。

`src/storage/layout.rs` 的 `is_binding_dir` 把 `installers` 目录登记为绑定
目录。retention 现在保留 metadata 的独立过期规则，同时新增
`binding_idle_secs`，单独扫描 npm/UPM `descriptors`、NuGet `bindings`、
PyPI `files`、Docker `blobs`、VS Code 四类 descriptor 目录和 Winget
`installers`。绑定被读取时会刷新修改时间，避免热绑定先于热制品过期；
`binding_idle_secs = 0` 可关闭绑定淘汰。清理轮次记录
`binding_cleanup`，绑定过期后客户端重新读取 metadata 或 manifest 即可重建。
`evicts_idle_bindings_and_keeps_fresh_ones` 覆盖全部绑定目录，
`keeps_an_idle_binding_that_was_accessed` 和 `binding_cleanup_can_be_disabled`
覆盖访问时间与禁用路径。

### 文档与提交记录的小问题

定性：违反项目已确认规范（文档同步）。
分级：低。
状态：文档漂移已修复，pip 实测留档和中间提交可编译仍保留。

- 配置分层表已补 `[winget]`；Winget 架构文档已同步目标版本校验、请求版本
  不存在返回 `404` 和允许列表私网语义；PyPI 架构文档和 README 已同步 stale
  回退；`web/guide/content/winget.md` 的命令已使用 `text` 代码块。
- `docs/architecture/pypi-mvp.md` 的 pip 25.1.1 实测仍没有
  `docs/verification/` 记录。本轮只把这一事实写清楚，没有伪造命令输出；
  部署前需要按仓库惯例补验并留档。
- 7 条提交本身线性、无 merge、message 与 diff 对应；但中间提交不保证独立
  可编译，`git bisect` 会落在无法构建的节点上。这是已确认的取舍，记录为
  维护性风险，不要求本次整改。

## 整改优先级建议

### 已收口

- 失败语义：绑定损坏自愈、PyPI stale 回退与 404 清理、审计失败统一
  fail-closed，均已有定向测试。
- Winget 边界：带 `Version` 只校验目标版本、group 请求版本缺失返回 `404`、
  group 成员失败逐条审计、允许列表主机允许私网解析。
- PyPI 完整性：无上游摘要发布写入独立审计事件。
- 绑定生命周期：全部绑定目录有独立 `binding_idle_secs`，读取刷新访问时间，
  清理记录 `binding_cleanup`，并支持 `0` 禁用。
- 文档：配置分层、缓存失效语义、Winget 版本与私网规则、指南代码块已同步。

### 后续顺序

1. 在受信 HTTPS 入口、管理员终端和可安装测试包条件下执行真实 `pip` 与
   `winget source add/search/install` 联调，并把命令和输出按
   `docs/verification/` 惯例留档。
2. 中间提交可编译性属于已确认取舍；如果后续要求 `git bisect` 可直接使用，
   再单独规划提交边界，不与本次失败语义整改混合。
