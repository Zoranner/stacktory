# Cargo Sparse Index 与 crate MVP

## 当前实现状态

本文档覆盖 Cargo 协议细节。当前 Cargo MVP 已实现 Sparse Index、crate 下载、
来源回退、缓存、checksum 校验和 metadata current/sidecar affinity：

- index 成功响应会持久化 `selected_proxy` sidecar。
- artifact 请求先使用该 selected proxy；来源不可用或 checksum 失败时才按 group
  顺序回退。
- 回退制品通过 checksum 校验后，才原子更新 artifact 和 selected proxy sidecar。
- Cargo 读取、缓存命中/未命中、回退、发布和失败会写入现有 `AuditSink` 的结构化
  JSONL 事件。

## 范围

Cargo 适配器只提供只读 restore 所需的 Sparse Registry 配置、Sparse Index 和
crate 下载：

```text
/mirror/cargo/{name}/config.json
/mirror/cargo/{name}/{sparse-index-path}
/mirror/cargo/{name}/crates/**
```

Cargo Git Index、publish、push、账号管理和任意 URL 代理不在范围内。

## 来源边界

Cargo 使用配置中的 `sources`。`proxy` 是一个固定上游根地址，`group` 只按
`members` 顺序组合 proxy，不直接访问网络。适配器收到请求后只把协议对象键交给
`SourcePool`，客户端路径不能改变来源选择。

上游 proxy 需要提供与 Cargo 资源键对应的只读路径：

```text
config.json
{sparse-index-path}
```

每个 Cargo proxy 只有一个 `base_url`。该根地址必须实际提供上述 registry 配置和
Sparse Index 资源；
适配器不引入第二个配置 endpoint，也不把 `base_url` 当作可转发任意 URL 的入口。
crate 下载地址由该 proxy 的 `config.json` 中 `dl` 模板描述，因此 `dl` 可以指向
同一 registry 根之外的固定 HTTP/HTTPS 地址，但必须经过对应 proxy 的配置读取和
Stacktory 安全校验。同 scheme/host/port 且路径位于该 proxy `base_url` 下的
地址允许使用私有 IP、localhost 或内部域名；跨 host 只允许公共目标。所有情况
都拒绝凭据、query、fragment、编码路径、反斜杠和路径穿越。真实私有或 Nexus
proxy 的管理地址不一定就是 Cargo registry 根地址，部署前必须确认其配置、
Sparse Index 和 `dl` 资源布局。

Stacktory 对外的 `config.json` 不转发上游配置，而是生成唯一的 `dl` 模板：

```text
{root_url}/mirror/cargo/{name}/crates/{crate}/{version}.crate
```

因此 Cargo 客户端只会继续请求当前 Stacktory 命名入口。

## Sparse Index

crate 名称先规范化为 ASCII 小写。index 的 `name` 字段保留上游提供的合法 Cargo
package name，不要求它已经是小写；路径使用其规范化结果。当前适配器允许 ASCII
字母、数字、`-` 和 `_`，不额外限制首字符。路径按规范分层：

```text
1/{name}                         # 1 个字符
2/{name}                         # 2 个字符
3/{first}/{name}                # 3 个字符
{first-two}/{next-two}/{name}   # 4 个及以上字符
```

例如 `serde` 的 index 路径是 `se/rd/serde`。请求路径必须与规范化后的结果完全
一致，不能包含绝对路径、空段、`.`、`..`、反斜杠、编码字符或控制字符。

index 内容按 JSON Lines 逐行解析。每条记录至少需要合法的 `name`、`vers` 和
64 位十六进制 `cksum`；其他字段保持原始 JSON Lines 内容，不在适配器中重建
依赖、feature 或 yanked 语义。Cargo index 记录本身没有下载 URL，下载地址由
Sparse Registry 的 `dl` 配置模板负责重写。

## 缓存与完整性

缓存不使用数据库，且与 Rustup 缓存分开：

```text
data_dir/cargo/metadata/{entry}/index/{object-key}
data_dir/cargo/artifacts/{proxy}/objects/crates/{crate}/{version}.crate
```

index 只有在 UTF-8、JSON Lines、合法 crate 名称、严格 SemVer 版本和 checksum 均
通过解析后才会原子写入。
crate 请求会先从 index 找到目标版本的 `cksum`，缓存命中也会重新计算 SHA-256。
crate 请求会先读取当前 proxy 的 `config.json`，按 `dl` 中的 `{crate}`、
`{version}`、`{prefix}`、`{lowerprefix}` 和 `{sha256-checksum}` marker 生成下载
URL。上游响应只有在摘要匹配后才会原子写入 artifact 缓存并返回。

group 的每个 proxy 独立尝试 index 和 crate。网络/5xx、非法 index 内容和 crate
摘要不匹配都会阻止当前对象发布，并可继续尝试下一个成员；group 对象缺失 404
也可回退，所有成员缺失时返回 404。401、403、408、429 等 terminal 状态不回退，
直接保留对应 HTTP 状态。

## 请求流程

```text
客户端
  -> Cargo 命名入口
  -> CargoPath / Sparse Index 路径校验
  -> Cargo metadata 或 artifact 缓存
  -> SourcePool 按 proxy/group 顺序访问
  -> JSON Lines 解析或 crate SHA-256 校验
  -> 原子发布缓存
  -> 返回内部化响应
```

未知命名入口、非法路径和 index 中不存在的 crate 版本返回 404。无可用来源或
完整性失败返回 502。缓存目录创建、读取、写入失败返回 500。Cargo 的读取、
缓存、回退、发布和失败会写入结构化审计事件；适配器不记录上游响应正文，也不
接受客户端提供的上游 URL。

## 验证边界

Cargo 测试使用本地 HTTP 上游覆盖：

- 1、2、3、4+ 字符 crate 的 Sparse Index 路径。
- 非法路径与名称规范化。
- JSON Lines checksum 解析。
- Stacktory `dl` 地址生成。
- group index/crate 回退。
- checksum 不匹配时不发布 artifact 缓存。

真实 crates.io、目标 Nexus proxy 和 Cargo 客户端联调尚未执行；Cargo Git Index
和写入操作属于后续独立任务。
