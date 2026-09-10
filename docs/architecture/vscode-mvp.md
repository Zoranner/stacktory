# VS Code 扩展只读适配器

## 边界

VS Code 适配器只提供扩展 metadata、精确版本 metadata 和 `.vsix` 只读下载。
它不提供发布、删除、账号、token、搜索写入或管理后台能力。

本实现不把 Microsoft Marketplace API 和 Open VSX API 视为同一个通用协议。
当前只接受配置来源能够提供的 Open VSX-compatible 安全子集；无法满足该契约的
来源应返回 `502`，不能通过猜测字段、跟随任意 URL 或兼容分支伪造支持。

适配器不修改公共 `Config` schema。主程序注册由主会话负责，注册时使用已有的
`AdapterRegistry` 和 `SourcePool`：

```rust
let pool = Arc::new(SourcePool::new("vscode", vscode_sources)?);
let adapter = Arc::new(VscodeAdapter::new(
    config.root_url.clone(),
    config.data_dir.clone(),
));
registry.register("vscode", pool, adapter)?;
```

## 对外路径

所有路径位于：

```text
/mirror/vscode/{name}/...
```

适配器当前支持：

```text
GET /mirror/vscode/{name}/api/{publisher}/{extension}
GET /mirror/vscode/{name}/api/{publisher}/{extension}/{version}
GET /mirror/vscode/{name}/api/{publisher}/{extension}/{version}/file/{publisher}.{extension}-{version}.vsix
```

`publisher`、`extension` 和 `version` 只允许受限 ASCII 路径段；版本必须是
可解析的 SemVer；制品文件名必须与 publisher、extension 和 version 完全匹配。
路径不接受编码斜杠、反斜杠、路径穿越、查询参数或 fragment。

metadata 请求从配置 proxy 的 `base_url` 生成以下上游路径：

```text
api/{publisher}/{extension}
api/{publisher}/{extension}/{version}
```

制品请求只生成固定的：

```text
api/{publisher}/{extension}/{version}/file/{publisher}.{extension}-{version}.vsix
```

上游 metadata 中的下载地址只用于验证其是否位于对应的 Open VSX-compatible
`file/{filename}` 路径，之后重写为当前 Stacktory 命名入口。适配器不会请求
metadata 中的任意外部下载地址。

## Metadata 契约

上游响应必须是 JSON 对象，并至少包含：

```json
{
  "namespace": "redhat",
  "name": "vscode-yaml",
  "version": "1.17.0",
  "files": {
    "download": "https://source.example/api/redhat/vscode-yaml/1.17.0/file/redhat.vscode-yaml-1.17.0.vsix",
    "sha256": "64 hexadecimal characters"
  }
}
```

`namespace`、`name` 和请求路径必须一致；精确版本请求还要求 metadata 的
`version` 一致。`files.download` 必须是无凭据、无查询参数、无 fragment 的
HTTP(S) URL，且路径以固定 VSIX 文件名结尾。`files.sha256` 必须是 64 位十六进制
SHA-256；缺失或格式错误时 metadata 不发布。

响应中的 `files.download` 被重写为：

```text
{root_url}/mirror/vscode/{name}/api/{publisher}/{extension}/{version}/file/{filename}
```

## 来源、回退与缓存

metadata 首次成功响应按命名入口和请求对象写入：

```text
{data_dir}/vscode/metadata/{name}/api/{publisher}/{extension}/{latest|version}
{data_dir}/vscode/metadata/{name}/api/{publisher}/{extension}/{latest|version}.source
```

`.source` 保存实际返回 metadata 的 proxy。VSIX 请求先读取精确版本 metadata，
再读取其来源绑定，并将该 proxy 放到 group 成员顺序的最前面。

只有以下情况才尝试 group 的下一个 proxy：

- 当前 proxy 不可用；
- 当前 proxy 返回 `404`，且入口是 group；
- 下载内容的 SHA-256 与 metadata 声明不一致。

`401`、`403`、`429` 和其他明确的客户端拒绝不会被适配器转换为无条件回退。
来源回退成功后，只有在 SHA-256 校验通过时才更新 `.source`。

VSIX 缓存按入口、publisher、extension 和 version 隔离：

```text
{data_dir}/vscode/artifacts/{name}/{publisher}/{extension}/{version}/{filename}
```

缓存命中必须重新按 metadata SHA-256 校验；不匹配的缓存会被删除。新制品先写
临时文件，再原子替换目标文件；完整性校验失败的响应不会进入缓存。

## 审计

适配器复用现有 `AuditSink`，不记录响应正文、凭据或完整制品。事件使用既有字段
记录：

- `ecosystem = "vscode"`；
- `entry_name`、`object_key` 和 `object_type`；
- 实际 `selected_proxy`；
- `cache_status`，包括 `hit`、`miss`、`fallback`、`published` 和 `failed`；
- 完整性失败、来源不可用、未找到和缓存损坏等 `failure_category`。

回退链按每个候选 proxy 产生独立事件，结合相同的 `object_key` 和时间顺序可以
复现来源尝试过程。

## 错误边界

- 非法路径、未知生态、未知入口或不匹配的 VSIX 文件名：`404`。
- 来源返回明确的 HTTP 客户端状态：保留该状态。
- metadata 无法解析、metadata 契约不满足、来源失败或所有制品候选都校验失败：
  `502`。
- 本地缓存、sidecar、原子写入或审计存储失败：`500`。

## 验证

`tests/vscode_protocol_test.rs` 使用本地 HTTP 上游覆盖：

- metadata 路径和 SemVer 路径校验；
- metadata 字段校验和 Stacktory URL 重写；
- metadata 缓存命中；
- VSIX SHA-256 校验；
- group 来源回退和来源绑定；
- 完整性失败不发布缓存；
- VSIX 缓存命中；
- JSONL 审计字段。

真实 Microsoft Marketplace 联调、真实 Open VSX 站点联调、主程序配置接线和生产
部署不属于本适配器提交的验证范围。
