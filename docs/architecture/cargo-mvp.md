# Cargo Sparse Index 与 crate MVP

## 范围

Cargo 适配器只提供只读 restore 所需的 Sparse Registry 配置、Sparse Index 和
crate 下载：

```text
/mirror/cargo/{name}/config.json
/mirror/cargo/{name}/index/**
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
index/{sparse-index-path}
crates/{crate}/{version}.crate
```

Stacktory 对外的 `config.json` 不转发上游配置，而是生成唯一的 `dl` 模板：

```text
{root_url}/mirror/cargo/{name}/crates/{crate}/{version}.crate
```

因此 Cargo 客户端只会继续请求当前 Stacktory 命名入口。

## Sparse Index

crate 名称先规范化为 ASCII 小写，并只允许 Cargo 包名使用的字母、数字、`-` 和
`_`。路径按规范分层：

```text
1/{name}                         # 1 个字符
2/{name}                         # 2 个字符
3/{first}/{name}                # 3 个字符
{first-two}/{next-two}/{name}   # 4 个及以上字符
```

例如 `serde` 的 index 路径是 `se/rd/serde`。请求路径必须与规范化后的结果完全
一致，不能包含绝对路径、空段、`.`、`..`、反斜杠、编码字符或控制字符。

index 内容按 JSON Lines 逐行解析。每条记录至少需要规范化的 `name`、`vers` 和
64 位十六进制 `cksum`；其他字段保持原始 JSON Lines 内容，不在适配器中重建
依赖、feature 或 yanked 语义。Cargo index 记录本身没有下载 URL，下载地址由
Sparse Registry 的 `dl` 配置模板负责重写。

## 缓存与完整性

缓存不使用数据库，且与 Rustup 缓存分开：

```text
data_dir/cargo/metadata/{entry}/index/{object-key}
data_dir/cargo/artifacts/{entry}/crates/{crate}/{version}.crate
```

index 只有在 UTF-8、JSON Lines、crate 名称和 checksum 均通过解析后才会原子写入。
crate 请求会先从 index 找到目标版本的 `cksum`，缓存命中也会重新计算 SHA-256。
上游响应只有在摘要匹配后才会原子写入 artifact 缓存并返回。

group 的每个 proxy 独立尝试 index 和 crate。网络/服务端失败、非法 index 内容和
crate 摘要不匹配都会阻止当前对象发布；可回退的失败会继续尝试下一个成员。4xx
等来源明确拒绝由 `SourcePool` 作为终止错误返回，不被适配器改写为无条件重试。

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
完整性失败返回 502。缓存目录创建、读取、写入失败返回 500。适配器不记录上游
响应正文，也不接受客户端提供的上游 URL。

## 验证边界

Cargo 测试使用本地 HTTP 上游覆盖：

- 1、2、3、4+ 字符 crate 的 Sparse Index 路径。
- 非法路径与名称规范化。
- JSON Lines checksum 解析。
- Stacktory `dl` 地址生成。
- group index/crate 回退。
- checksum 不匹配时不发布 artifact 缓存。

真实 crates.io 联调、Cargo Git Index 和写入操作属于后续独立任务。
