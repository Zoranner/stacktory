# Task 4 实现报告

## 状态

已完成 Cargo Sparse Index 与 crate 只读代理实现，提交逻辑单元由最终 Git 提交
记录。实现范围截至 2026-09-08，只包含 Cargo Sparse Registry 读取、crate 下载、
来源回退、缓存和 checksum 校验。

## TDD 记录

先新增并运行了以下测试：

```text
cargo test --test cargo_path_test --test cargo_index_test --test cargo_service_test
```

首次运行按预期因 `stacktory::cargo` 模块和 API 不存在而编译失败。随后按
`RED -> GREEN` 顺序实现功能，修正一次测试 raw string 终止符和一次下载 URL
缺少 `/crates/` 路径段的问题，目标测试最终通过。

## 实现内容

- 新增 `src/cargo/path.rs`：
  - crate 名称 ASCII 小写规范化。
  - 1、2、3、4+ 字符名称的 Sparse Index 标准路径算法。
  - 绝对路径、空段、`.`、`..`、反斜杠、编码字符和控制字符拒绝。
- 新增 `src/cargo/index.rs`：
  - JSON Lines index 逐行解析。
  - `name`、`vers` 和 64 位十六进制 `cksum` 校验。
  - 生成指向当前 Stacktory 命名入口的 crate 下载 URL。
  - index 内容保留原始 JSON Lines，不改写 Cargo 记录中不存在的下载字段。
- 新增 `src/cargo/service.rs`：
  - `config.json`、`index/**` 和 `crates/**` 请求处理。
  - index 与 crate 分离的文件系统缓存。
  - 缓存路径包含 `cargo` 生态、命名入口、协议角色和对象键。
  - crate 下载按 index 中的 `cksum` 执行 SHA-256 校验。
  - 校验失败不发布缓存，并允许显式 group 继续尝试下一个 proxy。
  - 4xx 等 SourcePool 终止错误不被适配器转换为无条件回退。
- 修改 `src/http.rs`、`src/lib.rs` 和 `src/main.rs`：
  - 注册 Cargo 适配器到统一命名路由。
  - `[cargo]` 存在时同时挂载 Rustup 与 Cargo 路由。
  - 未配置 Cargo 时保持 Rustup-only 启动行为。
- 修改配置测试并新增 Cargo 路径、index 和服务集成测试。
- 更新 README，新增 `docs/architecture/cargo-mvp.md`。

## 路由

```text
/mirror/cargo/{name}/config.json
/mirror/cargo/{name}/index/**
/mirror/cargo/{name}/crates/**
```

生成的 Cargo `dl` 模板只指向：

```text
{root_url}/mirror/cargo/{name}/crates/{crate}/{version}.crate
```

未实现 Cargo Git Index、publish、push、数据库和其他生态。

## 验证

以下命令均在最终代码上执行并通过：

```text
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

测试结果包含新增 Cargo 测试 12 项、配置测试 8 项，以及既有全量测试全部通过。

## 保留项

以下已有工作树改动未暂存、未回退、未提交：

- `docs/plans/2026-08-24-rustup-mirror-mvp.md`
- `docs/architecture/multi-ecosystem-readonly-proxy.md`
- `docs/plans/2026-09-08-multi-ecosystem-proxy.md`
- `docs/reviews/`

## Concerns

- 本地测试使用单一上游 registry 根地址提供 `config.json` 和
  `{sparse-index-path}`，并由 proxy `config.json` 的 `dl` 模板描述 crate 下载地址。
  尚未执行真实 crates.io、Nexus Cargo 仓库或 Cargo 客户端联调，因此实际部署前
  仍需确认目标上游的完整 Cargo 资源布局。
- 本期没有实现 Cargo Git Index，也没有覆盖 publish、push 或写入 API；这些请求
  不会由当前适配器提供。
- Cargo metadata 没有 TTL 或负缓存策略；当前缓存是本地文件系统的只读代理缓存，
  后续若需要刷新策略应作为独立协议需求设计。

## 审查修复

本轮针对 C1、I1、I2、I3、I4、M1 和 M2 追加修复，基于上一轮提交
`c9641d858e22ac0f2202f8542d207e57e786f7c6`。

### TDD 记录

先补充回归测试并运行：

```text
cargo test --test cargo_path_test --test cargo_index_test --test cargo_service_test --test config_test
```

旧实现的 RED 证据包括：

- 直接 registry 根路径请求返回 404。
- 大小写 index `name` 被错误拒绝。
- `abc` 版本被字符过滤错误接受。
- 上游 403 被映射为 502。
- 非法 index 缓存没有重新取源。
- 部署示例仍使用 `https://crates.io`。

确认失败原因后才修改生产代码，随后同一 focused 命令全部通过。

### Findings 修复

- C1：移除 Cargo 对外 `index/` 路由语义，标准 Sparse Index 请求变为
  `/mirror/cargo/{name}/{sparse-index-path}`；上游 index request key 同样直接使用
  `{sparse-index-path}`。
- I1：index `name` 只按 Cargo 允许的 ASCII 字母、数字、`-`、`_` 字符校验，
  保留原始大小写；路径仍基于 ASCII 小写规范化值，并不额外限制首字符。
- I2：部署示例改为 `https://index.crates.io`；README 和架构文档明确单一 Cargo
  `base_url` 必须实际提供 config、标准 Sparse Index 和 crate 相对资源布局，
  不增加第二 endpoint。
- I3：SourcePool 终止错误保留 HTTP 状态码；Cargo 适配器将上游 404 映射为 404，
  403/401 等终止状态保留原状态，只有可回退来源失败、协议错误或完整性失败返回
  502。
- I4：引入 `semver` 依赖，index `vers` 使用严格 SemVer 解析，非法版本如
  `abc`、`1..0` 被拒绝，合法 prerelease/build metadata 通过。
- M1：metadata 缓存解析失败或不是 UTF-8 时删除坏文件并重新通过 SourcePool
  获取；只有重新校验成功的 index 才会再次写入缓存。
- M2：新增标准根路径、大小写记录、SemVer、404、403 和坏缓存回归测试；文档继续
  明确真实 crates.io、目标 Nexus proxy 和 Cargo 客户端联调尚未执行。

### 本轮验证

以下命令在修复后的最终代码上执行并通过：

```text
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

本轮 focused 测试结果为路径 6 项、index 7 项、服务 6 项、配置 8 项全部通过；
完整测试和 release build 也全部通过。

### 本轮剩余 concerns

- 仍未执行真实 crates.io、目标 Nexus Cargo proxy 或真实 Cargo 客户端联调；当前
  本地测试验证的是适配器约定的单一上游根资源布局。
- 仍不实现 Cargo Git Index、publish、push、数据库或其他生态。

## Final Branch 集中修复

本轮针对最终整分支审查 findings 进行集中修复，保持私有 Cargo/Nexus 可用、单一
source `base_url`、只读、无数据库和不修改其他生态的边界。

### TDD 记录

先新增并运行回归测试：

```text
cargo test --test cargo_index_test --test cargo_service_test --test sources_test
```

旧实现的 RED 结果包括：

- 缺少 `render_download_url_for_source`、无尾斜杠 prefix API 和
  `AllSourcesFailed.status`。
- prefix marker 在大小写和分隔符上的行为不符合标准模板。
- Cargo affinity 没有持久化 selected proxy，artifact 不能按绑定来源优先。
- Cargo 审计事件没有详细生态、入口、对象和缓存字段。
- SourcePool 不区分 group 404 回退与其他 terminal 4xx。

确认失败原因后才修改生产代码。随后 focused Cargo/SourcePool 测试全部通过；其中
受控 resolver 只用于测试公共下载 host 到本地 listener 的映射，没有放宽生产私有
目标限制。

### 修复内容

- 私有 dl：
  - `dl` 与 proxy `base_url` 同 scheme、host、port 且路径位于 base path 下时允许
    私有 IP、localhost 和内部域名。
  - 跨 host 仍只允许静态可识别的公共 host，例如 registry 到公共下载 host。
  - 凭据、query、fragment、反斜杠、编码路径、`.`、`..` 和不满足 `require_https`
    的地址仍拒绝。
- group 404：
  - SourcePool 只对 group 的对象缺失 404 继续下一个成员。
  - group 所有成员均 404 时返回带 `status: Some(404)` 的失败。
  - 401、403、408、429 等 terminal 状态不回退；单独 proxy 的 404 仍直接返回。
  - Cargo index、config 和 artifact 都遵循该边界，支持私有源缺失后回退公共源。
- Cargo source affinity：
  - index body 与 `{object}.source` sidecar 一起保存实际 `selected_proxy`。
  - cache hit 必须同时验证 index、当前 crate 和有效 sidecar。
  - artifact 请求先尝试 sidecar 绑定来源，只有来源不可用或 checksum 失败时才按
    group 顺序回退；校验成功后更新 artifact 和 source sidecar。
- prefix：
  - `sparse_prefix` 返回无尾斜杠的 `1`、`2`、`3/s`、`se/rd`。
  - `{prefix}/{crate}` 和 `{lowerprefix}/{sha256-checksum}` 不产生双斜杠；
    `{prefix}` 保留 name 大小写，`{lowerprefix}` 使用小写规范化值。
- Cargo 审计：
  - 扩展现有 `AuditSink` 的结构化事件字段，Cargo 事件包含 `ecosystem`、
    `entry_name`、`object_key`、`selected_proxy`、`object_type`、`cache_status` 和
    `failure_category`。
  - 覆盖 Cargo index/artifact 读取、缓存命中/无效、回退、发布和失败事件。
- 平行实现：
  - `src/upstream.rs` 无生产调用，已删除该旧 Rustup 平行客户端及其
    `tests/upstream_test.rs`，Cargo 继续只通过 SourcePool 访问来源。
- 文档：
  - `docs/architecture/cargo-mvp.md` 和
    `docs/architecture/multi-ecosystem-readonly-proxy.md` 已更新为 Cargo affinity
    和审计已实现状态；README 同步 404、私有 base path 和审计边界。

### 最终验证

以下命令在当前集中修复后的代码上全部通过：

```text
cargo test --test cargo_index_test --test cargo_service_test --test cargo_path_test --test sources_test
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

Focused 结果为 Cargo index 12 项、Cargo service 16 项、Cargo path 6 项、
SourcePool 10 项全部通过；全量测试 75 项通过，Clippy、格式检查和 release build
全部通过。

### 剩余边界

- 尚未执行真实 crates.io、真实 Nexus Cargo proxy 或真实 Cargo 客户端联调。
- SSRF 防线未执行 DNS 解析验证公共域名最终地址；当前只拒绝静态可识别的内部目标，
  真实部署仍需配合网络出口策略。
- Cargo Git Index、publish、push、数据库及其他生态仍未实现。

## Final Branch 集中修复

本轮基于当前分支的最终整分支审查 findings 做集中修复，保留以下不可变边界：

- 私有 Cargo/Nexus registry 可以作为 proxy 使用。
- 每个 proxy 只有一个 `base_url`，不增加多地址配置。
- Cargo 只读，不实现 Git Index、publish、push 或其他生态。
- 不引入数据库，继续使用文件系统缓存和现有 `AuditSink`。

### TDD 记录

先补充并运行：

```text
cargo test --test cargo_index_test --test cargo_service_test --test sources_test
```

旧实现的 RED 结果包括：

- 缺少带 proxy `base_url` 上下文的私有 dl 校验和无尾斜杠 prefix API。
- `AllSourcesFailed` 不携带最终状态，无法表达 group 全部 404。
- Cargo index 没有持久化 selected proxy，artifact 无法基于来源绑定优先。
- Cargo 审计没有生态、入口、对象、来源和缓存字段。

确认上述失败后才修改生产代码。实现期间还使用了编译器和 focused 测试输出定位
接口返回类型、旧 404 断言和 marker 分隔符问题，未用警告屏蔽或测试绕过替代修复。

### 修复内容

- 私有 dl 与公共跨 host：
  - `dl` 与 proxy `base_url` 同 scheme、host、port 且路径位于 base path 下时，
    允许私有 IP、localhost 和 `.internal` 等内部 host。
  - 不同 host 仍只接受公共目标，保留 registry host 到公共下载 host 的能力。
  - 继续拒绝凭据、query、fragment、反斜杠、编码路径、`.`、`..`、非 HTTP(S) 和
    不满足 `require_https` 的地址。
- group 404：
  - `SourcePool` 只对 group 对象缺失 404 回退。
  - group 所有成员均 404 时以 `status: Some(404)` 返回；单独 proxy 404 仍直接
    返回 404。
  - 401、403、408、429 继续作为 terminal 状态，不回退。
  - Cargo index、config、artifact 均按该来源状态边界处理，支持私有源缺失后回退
    公共源。
- Cargo source affinity：
  - index 内容旁写入 `{object}.source` sidecar，保存成功响应的
    `selected_proxy`。
  - index cache hit 必须同时满足内容可解析、包含当前 crate 且 sidecar 属于当前
    entry。
  - artifact 请求先按 sidecar 选择来源；来源不可用或 checksum 失败时才按 group
    顺序回退。
  - 回退 artifact 通过 checksum 后才写入缓存，并原子更新 selected proxy sidecar。
- prefix：
  - `sparse_prefix` 返回无尾斜杠的 `1`、`2`、`3/s`、`se/rd`。
  - `{prefix}/{crate}` 和 `{lowerprefix}/{sha256-checksum}` 不产生双斜杠；
    prefix 保留记录 name 大小写，lowerprefix 使用规范化小写名称。
- Cargo 审计：
  - 扩展现有 `AuditSink`，Cargo 事件包含 `ecosystem`、`entry_name`、`object_key`、
    `selected_proxy`、`object_type`、`cache_status` 和 `failure_category`。
  - 已接入 index/artifact 读取、缓存命中/无效、来源回退、发布和失败事件。
- 平行实现清理：
  - `rg` 确认 `src/upstream.rs` 无生产调用。
  - 删除旧 `src/upstream.rs` 和只服务于它的 `tests/upstream_test.rs`，避免 Rustup
    存在绕过 `SourcePool` 的平行来源客户端。
- 文档：
  - `docs/architecture/cargo-mvp.md` 和
    `docs/architecture/multi-ecosystem-readonly-proxy.md` 已同步 Cargo affinity、
    审计和私有 dl 边界。

### 最终验证

Focused：

```text
cargo test --test cargo_index_test --test cargo_service_test --test cargo_path_test --test sources_test
```

结果为 index 12 项、service 16 项、path 6 项、SourcePool 10 项全部通过。

完整门禁：

```text
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

上述命令全部通过；删除旧 upstream 专属测试后，全量测试共 87 项通过，release
build 成功。

### 剩余边界

- 尚未执行真实 crates.io、真实 Nexus Cargo proxy 或真实 Cargo 客户端联调。
- 公共跨 host 目标的 SSRF 防护目前基于静态 host/IP 分类，没有执行 DNS 解析和
  运行时地址重绑定防护；真实部署仍需配合网络出口策略和 DNS 安全控制。
- Cargo Git Index、publish、push、数据库及其他生态仍未实现。

## 上游 dl 模板驱动下载修复

本轮继续修复 Cargo Task 4 的上游下载契约。Cargo registry 的单一 `base_url` 仍是
唯一 source 配置；适配器不增加第二个 endpoint，也不把客户端提供的地址转发到
上游。crate 下载地址改为由每个 proxy 自身的 `config.json` 中的 `dl` 模板决定。

### TDD 记录

先新增分阶段回归测试并运行：

```text
cargo test --test cargo_index_test --test cargo_service_test
```

新增测试覆盖：

- proxy `config.json` 提供 `dl` 模板。
- `{crate}`、`{version}`、`{prefix}`、`{lowerprefix}` 和
  `{sha256-checksum}` marker 展开。
- group 第一 proxy 成功提供 config/index 后，按其 dl 下载 crate。
- 缺失 config、非法 dl 不发布 artifact 缓存。
- 401、403、404、坏缓存和存储错误边界。

旧实现首先因 `parse_registry_config`/`render_download_url` API 不存在而编译失败；
此前补充的根路径、大小写 name、SemVer、4xx 和坏缓存测试也继续作为 RED 基线。
确认失败后才实现生产代码。实现过程中又通过失败输出修正了测试夹具的下载路由
checksum 和 config 装配，最终 focused 测试全部通过。

### 实现

- 新增 `CargoRegistryConfig` 和 `parse_registry_config`，要求 `dl` 非空并只允许
  已知 marker。
- 新增安全的 `render_download_url`：
  - 展开 `{crate}`、`{version}`、`{prefix}`、`{lowerprefix}`、
    `{sha256-checksum}`。
  - 保留合法大小写 crate name 作为 `{crate}`，用小写名称计算 lowerprefix。
  - 严格校验 SemVer 和 checksum。
  - 拒绝未知 marker、凭据、query、fragment、非 HTTP(S)、编码路径、反斜杠和
    `.`/`..` 路径段。
- 扩展 `SourcePool`，只允许 Cargo 服务把已从对应 proxy `config.json` 读取并经过
  校验的 URL 交给对应 proxy 请求；来源的 `require_https` 仍然生效。
- Cargo crate 请求对每个 proxy 读取自身 `config.json`，使用该 proxy 的 dl 生成
  URL；config 缺失、非法或 URL 无法安全构造只使当前来源失败并继续 group。
- artifact 只有在 index checksum 校验通过后才原子发布。
- Cargo 对外 Sparse Index 继续保持 registry 根直接访问：
  `/mirror/cargo/{name}/{sparse-path}`。
- index name 保留原始大小写，路径使用规范化名称；`vers` 继续使用严格 SemVer。
- terminal 401/403/404 继续映射对应 HTTP 状态；协议、完整性、来源全部失败和
  本地存储错误保持各自边界，其中存储错误返回 500。

### 文档与配置

- README 和 `docs/architecture/cargo-mvp.md` 已说明 proxy `base_url` 只指向一个
  Cargo registry 根，config/index 由该根提供，crate 下载由该根返回的 dl 模板
  描述。
- `deploy/app/config.example.toml` 保持使用真实 Sparse Registry 根
  `https://index.crates.io`。
- 本报告的 I2 concern 已修正为“尚未真实 Cargo/Nexus/Cargo 客户端联调”，不再把
  默认根地址表述为本轮主要问题。

### 本轮验证

本轮 focused 测试结果为：

```text
cargo test --test cargo_index_test --test cargo_service_test
```

index 9 项、service 11 项全部通过。最终的格式、Clippy、全量测试和 release build
在提交前执行。

### 本轮剩余 concern

- 尚未执行真实 crates.io、目标 Nexus Cargo proxy 或真实 Cargo 客户端联调；本地
  测试验证的是单一 registry `base_url` 加 proxy `config.json` dl 模板的协议链路。
- 仍不实现 Cargo Git Index、publish、push、数据库或其他生态。

## 上游 dl 配置修复

本轮继续修复 Cargo 下载链路，重点是把 crate 下载地址的权威来源从固定路径改为
每个 proxy 自身的 `config.json`。本轮没有增加第二个 source endpoint 或多地址配置。

### 分阶段 TDD

先新增测试并运行 focused Cargo 测试。测试要求第一 proxy 同时提供
`config.json` 和 Sparse Index，`dl` 使用 marker 指向独立下载路径；旧实现会继续
请求固定的 `/crates/{crate}/{version}.crate`，因此无法取得配置描述的 crate。
同时补充了所有 marker、非法 dl、缺失 config、非法 dl 和 group 第一源成功路径。

确认 RED 后实现：

- 读取每个 proxy 的 `config.json`，要求存在非空 `dl`。
- 支持 `{crate}`、`{version}`、`{prefix}`、`{lowerprefix}` 和
  `{sha256-checksum}`；无 marker 时保留 Cargo 默认的
  `/{crate}/{version}/download` 展开规则。
- 只接受经过解析的 `http`/`https` URL，拒绝凭据、query、fragment、编码路径、
  反斜杠、路径穿越和不符合来源 `require_https` 的下载地址。
- 通过 SourcePool 的对应 proxy 发起已验证下载 URL 请求；配置缺失、配置非法或
  URL 无法安全构造按该来源失败处理。
- 下载继续以 index 中的 checksum 为唯一完整性依据，只有校验通过才发布 artifact
  缓存。

### Findings 口径修正

- 1：Cargo 对外 index 请求统一为 `/mirror/cargo/{name}/{sparse-path}`，registry
  根后直接请求 `se/rd/serde`。
- 2：I2 不再表述为默认 `crates.io` 根地址问题；当前真实边界是 proxy 自身
  `config.json` 的 `dl` 模板，以及尚未执行真实 Cargo/Nexus 联调。
- 3：合法大小写 name 继续保留原始记录字段，路径使用规范化名称；版本继续使用
  严格 SemVer。
- 4：SourcePool terminal 4xx 继续映射对应 HTTP 状态，Cargo 测试覆盖 401、403、
  404。
- 5：坏 index 缓存删除后重新获取，且新增缺失/非法 dl 不发布缓存测试。

### 本轮验证

本轮 focused 结果为 index 9 项、service 11 项，全部通过。最终门禁已在提交前重新
执行：

```text
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

上述命令全部通过。

### 本轮剩余 concern

- 尚未执行真实 crates.io、目标 Nexus Cargo proxy 或真实 Cargo 客户端联调；本地
  测试只验证单一 registry `base_url` 加 proxy `config.json` `dl` 模板的协议链路。
- 仍不实现 Cargo Git Index、publish、push、数据库或其他生态。

## Scoped Re-review 修复

本轮针对 Cargo `config.json` terminal 4xx、`dl` host SSRF 和 index 缓存一致性
继续修复。保持 Sparse registry 根路径、单一 source `base_url`、proxy `dl`
marker、只读和无数据库边界，不增加多地址配置。

### TDD 记录

先补充并运行：

```text
cargo test --test cargo_index_test --test cargo_service_test
```

旧实现的 RED 结果包括：

- proxy `config.json` 返回 404/403 时被错误收集为 `AllSourcesFailed` 并返回 502。
- `dl` 指向 `127.0.0.1` 时被错误接受。
- 合法但属于其他 crate 的 index 缓存被错误命中并直接返回。

随后按失败原因分别修改实现。为使集成测试同时覆盖“公共 dl host”与本地 HTTP
服务，测试构造了受控 DNS resolver，将公共测试域名解析到本地 listener；生产
安全校验没有放宽 loopback 或私有地址限制。

### 修复内容

- config 4xx：读取每个 proxy 的 `config.json` 时，网络错误和 5xx 继续按来源失败
  回退；401、403、404 等 terminal 状态立即返回对应 HTTP 状态。group 不绕过
  terminal config 错误访问后续成员。
- dl host SSRF：复用 SourcePool 的 URL 安全 helper，允许配置声明的公共跨 host
  地址，拒绝 loopback、私有、link-local、unspecified、IPv4-mapped loopback、
  `localhost` 和明显内部域名，并继续拒绝凭据、query、fragment、编码路径、反斜杠
  和路径穿越。
- index 缓存一致性：缓存命中时增加当前请求 crate 的规范化 name 检查；错误 crate
  缓存删除后重新从 SourcePool 获取。
- 新增并覆盖 config 404/403、group terminal 不回退、公共 host marker 下载、
  内部目标拒绝、错误 crate 缓存恢复和存储错误 500 边界。

### 本轮验证

Focused 结果：

```text
cargo test --test cargo_index_test --test cargo_service_test
```

index 9 项、service 14 项全部通过。最终门禁已执行：

```text
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

上述命令全部通过。

### 本轮剩余 concern

- 尚未执行真实 crates.io、目标 Nexus Cargo proxy 或真实 Cargo 客户端联调；当前
  证据覆盖的是本地 HTTP 上游、单一 registry `base_url`、proxy `config.json`
  `dl` 模板和静态可识别的公共/内部 host 边界。
- 没有执行 DNS 解析来判断任意公共域名最终是否解析到私有地址，真实部署仍需配合
  网络出口策略。
- 仍不实现 Cargo Git Index、publish、push、数据库或其他生态。

## Scoped Re-review 修复

本轮针对 Cargo Task 4 的 scoped re-review findings 继续修复，仍基于单一 source
`base_url`、proxy `config.json` `dl` 模板和只读代理边界，没有增加多地址配置。

### TDD 记录

先补充回归测试并运行：

```text
cargo test --test cargo_index_test --test cargo_service_test
```

旧实现的 RED 结果包括：

- `config.json` 返回 404/403 时被收集为 `AllSourcesFailed`，最终响应为 502。
- `dl` 指向 `127.0.0.1` 时被错误接受。
- 合法但属于其他 crate 的 index 缓存被错误命中并直接返回。

随后实现对应修复，并在调试过程中将本地测试 listener 与公共 dl host 分离，避免
为了测试下载成功而放宽生产 SSRF 防线。最终 focused 测试全部通过。

### 修复内容

- config 4xx：Cargo crate 下载读取 proxy `config.json` 时，只有网络错误和 5xx
  作为可回退来源失败；401、403、404 等 terminal 状态立即返回对应 HTTP 状态。
  group 不会绕过 terminal config 4xx 继续访问后续成员。
- dl host SSRF：复用 SourcePool 的安全 URL helper，允许经过 proxy config 声明的
  公共跨 host HTTP/HTTPS 地址，同时拒绝 loopback、私有、link-local、unspecified
  IP、IPv4-mapped loopback、`localhost` 及明显内部域名，并继续拒绝凭据、query、
  fragment、编码路径、反斜杠和路径穿越。
- index 缓存一致性：缓存命中时除了解析 JSON Lines、SemVer 和 checksum，还确认
  至少有一条记录的规范化 name 等于当前请求 crate；错误 crate 缓存会删除并重新
  通过 SourcePool 获取。
- 测试补充 config 404/403、group terminal 不回退、公共 dl host marker 下载、
  内部目标拒绝和错误 crate 缓存恢复。

### 验证

本轮 focused 结果：

```text
cargo test --test cargo_index_test --test cargo_service_test
```

index 9 项、service 14 项全部通过。最终门禁执行：

```text
cargo fmt --all
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --release --locked
```

上述命令全部通过。

### Scoped re-review concerns

- 尚未执行真实 crates.io、目标 Nexus Cargo proxy 或真实 Cargo 客户端联调；当前
  证据覆盖的是本地 HTTP 上游、单一 registry `base_url`、proxy `config.json`
  `dl` 模板和受限公共跨 host 请求。
- 当前拒绝的是可静态识别的内部目标；没有执行 DNS 解析来判断任意公共域名最终是否
  解析到私有地址，真实部署仍需配合网络出口策略。
- 仍不实现 Cargo Git Index、publish、push、数据库或其他生态。
