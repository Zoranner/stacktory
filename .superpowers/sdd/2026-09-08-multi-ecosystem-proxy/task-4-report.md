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
