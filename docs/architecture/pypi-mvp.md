# PyPI Simple API 适配器 MVP

## 状态与目标

本文档定义 PyPI 的后续只读适配边界。当前 Stacktory 尚未实现或注册 PyPI
路由；本文档中的路径和响应协商是目标契约。

首期只覆盖 Python 包安装所需的 Simple API 项目索引和项目文件下载。上传、
删除、账号、token、搜索和任意 URL 代理不在范围内。

## 公共来源契约

PyPI 每个 proxy 只配置一个上游 registry 根 `base_url`。Simple API 项目索引和
文件下载都由该根地址按协议对象键生成，不配置独立的 `simple_url`、文件 CDN
地址或第二上游地址。

```toml
[pypi]

[[pypi.sources]]
name = "official"
type = "proxy"
base_url = "https://pypi.example"
require_https = true

[[pypi.sources]]
name = "group"
type = "group"
members = ["internal", "official"]
```

没有 group 时，多个 proxy 彼此独立；group 只按 `members` 顺序回退。group
不配置 `base_url`，不合并不同 proxy 的项目版本集合，也不引用其他 group。

## 路由

所有目标路径都包含生态和命名入口：

```text
GET /mirror/pypi/{name}/simple/
GET /mirror/pypi/{name}/simple/{project}/
GET /mirror/pypi/{name}/files/{*path}
```

第一个 `{name}` 是 Stacktory proxy/group 入口。`{project}` 是经过项目名规范化
的 Simple API 项目键；`files` 下的路径只允许由已返回的项目索引链接引用。
适配器不得把客户端给出的绝对 URL 直接当作上游请求地址。

文件链接可以使用当前入口的 `/mirror/pypi/{name}/files/...` 路径，或由适配器
生成同一入口下的受限文件对象键。链接中的 hash fragment 可以保留为客户端
校验提示，但不能变成上游请求目标或缓存键的隐式来源。

## 项目名规范化

Simple API 项目请求必须按 PyPA Simple Repository API 的规范化规则处理：

- ASCII 字母、数字、`.`、`-` 和 `_` 的连续分隔符折叠为一个 `-`。
- 比较时使用小写规范化名称。
- `/simple/` 后的项目路径必须是规范化结果，并以 `/` 结束。
- 原始显示名可以在 HTML/JSON 元数据中保留，但不能改变对象键。

因此 `Friendly_Bard`、`friendly-bard` 和 `friendly.bard` 指向同一个规范化
项目键 `friendly-bard`。实现不能让未规范化别名产生另一个缓存对象，也不能
允许空段、`.`、`..`、反斜杠或编码路径穿越。

## HTML 与 JSON 表示

Simple API 项目索引至少支持 HTML 表示，并为后续 JSON 表示保留同一适配器
边界。客户端通过 `Accept` 协商表示：

- 支持 HTML 时返回 `text/html`，索引链接使用 `<a href="...">`。
- 只有 `Accept` 明确列出 `application/vnd.pypi.simple.v1+json` 且该 media type
  可接受时，才返回 PyPI Simple API JSON，并在协议允许的范围内返回项目文件
  链接及其 hash。
- 无法满足客户端明确要求的表示时返回 `406`，不能静默返回另一种表示。
- 未带 `Accept` 或只声明 `*/*` 时固定返回 HTML；这两个情况不是由上游响应
  临时决定的默认值。

HTML/JSON 不是两个项目事实。两种表示必须由同一个规范化项目对象和同一个
selected proxy 生成，只在表示层编码不同。缓存键必须包含固定的
`representation`（`html` 或 `json`）以及必要的 media type，防止 HTML 和
JSON 响应互相命中。

## 文件链接与完整性

项目索引中的每个文件链接必须：

- 使用当前 `/mirror/pypi/{name}/...` 入口，或通过严格的内部对象引用绑定到
  当前入口。
- 指向当前项目的受限文件对象。
- 保留上游提供的 hash 信息；支持的 hash 算法和格式由适配器明确校验。
- 不包含客户端可控的外部 host、凭据、query 注入或路径穿越。

下载文件先写临时文件，再按索引声明的 hash 校验，最后原子发布 artifact
缓存。缺少 hash 时可以下载并记录“无上游摘要”的状态，但不能把它写成已完成
强完整性校验；若部署策略要求安装制品必须有摘要，应将无摘要视为失败。

Simple API 项目索引成功后，后续文件请求优先使用产生该索引的 selected proxy。
来源不可用或 hash 校验失败时才允许 group 回退；回退得到的文件必须继续满足
当前项目索引声明的 hash，否则不能发布。

## 缓存与错误

缓存键至少包含：

```text
pypi
entry_name
simple-html 或 simple-json
normalized_project
file-object-key
representation = html 或 json
selected_proxy
```

项目索引属于 metadata，wheel、sdist 和其他项目文件属于 artifact。不同命名
入口、HTML/JSON 表示、项目规范化键和 selected proxy 必须隔离。

- 未知入口、非法项目路径、未知项目或未被索引引用的文件：`404`。
- 不支持的 Accept 表示：`406`。
- 上游不可用、响应无法解析或 hash 不匹配：按 group 规则回退；全部失败返回
  `502`。
- 本地缓存和原子写入失败：`500`。

本 MVP 不实现 `/search`、upload、delete、用户认证、API key、依赖解析或
Python 包元数据之外的业务数据库。文件系统缓存是唯一缓存事实源。

## 验证边界

实现阶段使用本地 HTTP 上游覆盖：

- `/simple/` 与规范化项目名路径。
- HTML/JSON 的 `Accept` 协商、响应 media type 和缓存隔离。
- HTML `href` 与 JSON 文件链接的入口化重写。
- wheel/sdist 文件 hash 校验、坏文件不落盘和 group 回退。
- 规范化别名不产生重复对象，路径穿越和外链均被拒绝。
- 未注册 `/mirror/pypi/{name}/...` 之外的假路由不暴露。

真实 pip、外部 PyPI 兼容仓库和生产部署行为必须在适配器实现后单独联调。

## 官方协议依据

- [PyPA Simple Repository API](https://packaging.python.org/en/latest/specifications/simple-repository-api/)：
  项目名规范化、HTML/JSON 表示、Accept 协商、文件链接和 hash 属性。
