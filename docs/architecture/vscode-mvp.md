# VS Code Marketplace 只读适配器

## 边界

VS Code 适配器代理公开 Microsoft Marketplace Gallery 的扩展查询与查询结果引用的资源。
目标是让 VS Code 扩展侧栏搜索、查看详情并安装扩展，不支持发布、删除、账号、私有
Marketplace、评分写入或 Open VSX `/api/...` 协议。

## 对外路由

```text
POST /mirror/vscode/{name}/extensionquery
GET  /mirror/vscode/{name}/asset/{asset-id}
```

`extensionquery` 接收最大 1 MiB 的 JSON body。`asset-id` 是上游资源 URL 的 SHA-256，
只能解析已由成功 query 发布的资源描述符；未知、格式非法或未绑定的 asset 返回 `404`。
错误方法返回 `405`。

## 上游请求

Microsoft Marketplace proxy 的 `base_url` 固定为：

```text
https://marketplace.visualstudio.com/_apis/public/gallery
```

Stacktory 将 query body POST 到 `{base_url}/extensionquery`，并固定发送：

```text
Accept: application/json;api-version=3.0-preview.1
Content-Type: application/json
X-Market-Client-Id: VSCode
```

请求 body 不包含上游 URL。group 只按已配置成员选择 query 的 Gallery 上游，保留公共来源
池的网络失败、404 回退和终止客户端错误分类。

## 查询与资源绑定

Gallery query 必须返回 `results[].extensions[].versions[].files[]`。每个 file 必须提供
`assetType` 和安全的 HTTP(S) `source` URL。Stacktory 将 source URL 改写为：

```text
{root_url}/mirror/vscode/{name}/asset/{asset-id}
```

并保存：

```text
data_dir/stores/vscode/metadata/{name}/marketplace/{query-id}.json
data_dir/stores/vscode/metadata/{name}/marketplace/assets/{asset-id}.toml
```

`query-id` 是原始 query body 的 SHA-256。资源描述符保存 selected proxy、原始资源 URL
和 Gallery asset type。资源读取只允许通过描述符的 selected proxy 访问，不能把客户端
输入作为任意 URL 请求。

## 缓存与审计

资源对象按实际 proxy 和上游 URL identity 缓存：

```text
data_dir/stores/vscode/artifacts/{proxy}/objects/marketplace/{asset-id}
data_dir/stores/vscode/artifacts/{proxy}/objects/marketplace/{asset-id}.toml
```

sidecar 保存上游 `Content-Type`，命中缓存时原样返回，并与主对象一起更新访问时间，
因此闲置淘汰时两者同进同退。group 和直接 proxy 只要绑定到同一 actual proxy 与
资源 URL，即可复用同一对象。

审计记录 `ecosystem = "vscode"`、entry name、asset/query object key、selected proxy、
metadata/artifact 类型、cache 状态和失败类别，不记录 query body、响应正文、资源内容或
凭据。

## 客户端接入

VS Code 使用产品级 `extensionsGallery` 配置：

```json
{
  "extensionsGallery": {
    "serviceUrl": "https://stacktory.example.internal/mirror/vscode/group",
    "resourceUrlTemplate": "https://stacktory.example.internal/mirror/vscode/group/resource/{publisher}/{name}/{version}/{path}",
    "extensionUrlTemplate": "https://stacktory.example.internal/mirror/vscode/group/extension/{publisher}/{name}/latest"
  }
}
```

用户 settings 的 `extensions.gallery.serviceUrl` 会进入 VS Code 私有 Marketplace 登录
流程，不能作为无登录内网镜像入口。真实验证必须使用隔离的 VS Code runtime 副本、独立
`--user-data-dir` 与 `--extensions-dir`，不能修改日常安装或 profile。产品级地址必须
使用受信任 HTTPS；通过扩展侧栏搜索扩展、打开详情、安装扩展，并核对 Stacktory query
和 asset 审计事件。
