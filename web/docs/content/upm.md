---
id: upm
group: Unity
title: UPM
summary: 在当前 Unity 项目中配置 Stacktory scoped registry。
ecosystem: upm
---
## 提示词
请把当前 Unity 项目的 UPM scoped registry 配置到 Stacktory。

目标地址：{endpoint}
要求：
1. 先确认当前目录是 Unity 项目根目录，并读取 Packages/manifest.json；
2. 修改前备份 manifest.json，并在结果中报告备份路径；
3. 保留已有 scopedRegistries 和 scopes，只添加或更新 name 为 Stacktory 的条目；
4. 保持幂等，重复执行不要重复追加；
5. 不替用户决定 Unity scope，scope 由项目现有配置或项目负责人确认；
6. 修改后用 JSON 解析器确认 manifest.json 可解析，不要启动 Unity Editor；
7. 如果项目根目录、权限或配置格式不确定时停止，不要猜测。

## 手动步骤
配置文件：项目根目录/Packages/manifest.json

{
  "scopedRegistries": [
    {
      "name": "Stacktory",
      "url": "{endpoint}",
      "scopes": ["com.company"]
    }
  ]
}

将示例 scope 替换为当前项目实际使用的 scope。

## 验证
Get-Content -Raw Packages/manifest.json | ConvertFrom-Json

## 注意
- 必须在 Unity 项目根目录修改，不要修改全局 Unity 配置。
- 不要启动 Unity Editor；包解析结果由用户重新打开 Unity 后确认。
- 可用入口由服务配置决定；Group 是回退组，Proxy 是单个上游来源。
