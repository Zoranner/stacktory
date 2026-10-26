# Stacktory 后端代码结构设计

## 目标

用一条统一规则重排 `src/`，使每个目录和文件都能回答"职责是什么、依赖谁、为什么
在这一层"。同时拆开职责过宽的大文件，消除反向依赖，让新增一个生态只动一个目录和
少量装配点。

本设计不改变任何客户端可见行为：HTTP 路由、配置 schema、数据目录布局、审计 JSONL、
运行日志格式和错误分类保持现状。

## 非目标

- 不新增生态、配置字段、管理接口或数据库。
- 不改缓存键、保留策略、校验和语义或来源回退顺序。
- 不重排 `tests/`：Cargo 只把 `tests/` 下一层的 `.rs` 当作独立测试目标，分组需要
  额外 `[[test]]` 配置，收益低于维护成本。
- 不治理 `docs/` 下的历史计划与评审；只更新 `docs/architecture/` 中描述当前代码的
  路径引用。
- 不推送、不部署；本地提交按用户授权单独执行。

## 组织规则

第一层只按运行时职责划分，业务概念只允许出现在 `ecosystems/` 第二层。`startup`、
`commands` 是进程生命周期职责，`http` 是入站接口，`upstream` 是出站访问，`storage`
是持久化，`audit`、`logging`、`retention` 是运行期记录与清理。这些都是"运行时谁负责
什么"，不是业务域。

目录深度只由独立子职责决定。一个目录只有在包含两个以上可独立变化、可独立验证的子
职责时才加一层；只有一个职责的文件不套目录。文件按协议对象、生命周期或状态所有权
拆分，不按行数拆分。

目录名必须能读出职责，不使用 `app`、`web`、`sources`、`ops` 这类覆盖整个类别的大词。

依赖只能沿下表向下或同层横向，禁止反向依赖和循环依赖。

| 模块 | 职责 | 允许依赖 |
| --- | --- | --- |
| `startup` | 读配置、装配适配器、绑定监听、优雅退出 | 全部 |
| `commands` | `prune-legacy-layout`、`invalidate-nuget-metadata` | config、storage、audit、retention |
| `http` | 路由表、首页、静态资源、指南页面、镜像与 Rustup 入口 | ecosystems、upstream、storage、config |
| `ecosystems` | 各生态协议解析、缓存键、响应构造 | upstream、storage、audit、logging、config |
| `upstream` | 来源池、抓取、回退、预算、适配器注册表 | config |
| `storage` | 数据目录布局、原子写入、对象锁、访问时间 | 无 |
| `audit` | 审计事件写入 | storage |
| `logging` | 运行日志写入、轮转、UTC 标签 | 无 |
| `retention` | 过期清理、轮转调度、旧布局清理 | storage、audit、logging |
| `config` | 配置模型、解析、校验 | 无 |

禁止的依赖在验收时用文本门禁检查：`ecosystems` 不出现 `crate::retention`，`retention`
不出现 `crate::ecosystems`，`storage` 不出现 `crate::` 以外的模块引用，`upstream`
不出现 `crate::ecosystems` 或 `crate::http`。

## 目标结构

```text
src/
  main.rs                 参数分派
  lib.rs                  模块声明与公共导出

  config/
    mod.rs                对外类型与再导出
    model.rs              Config、各生态配置、来源图
    parse.rs              TOML Raw 结构与反序列化
    validate.rs           public_url、来源名、channel 等校验
    catalog.rs            从 Config 生成站点与指南共用的入口目录

  startup/
    mod.rs                配置读取入口
    serve.rs              监听、优雅退出、public_url 标记
    registry.rs           按配置调用各生态的注册函数

  commands/
    mod.rs
    prune_legacy_layout.rs
    invalidate_nuget_metadata.rs

  http/
    mod.rs                路由表、健康检查、响应辅助
    site.rs               首页、静态资源、SiteContext、请求基址推导
    mirror.rs             /mirror 与 /v2 分发、CORS
    rustup_dist.rs        /mirror/rustup 入口与错误映射
    guide/
      mod.rs              指南页面处理器
      handlers.rs         页面请求到渲染器的接线
      content.rs          解析 web/guide/content/*.md
      render.rs           HTML 与 Markdown 渲染

  ecosystems/
    mod.rs
    rustup/
      mod.rs              错误类型与再导出
      manifest.rs         manifest 重写
      dist.rs             dist 路径
      service/
        mod.rs            适配器、就绪检查
        manifest.rs       manifest 刷新与快照发布
        artifact.rs       制品下载、缓存与回退
      cache/
        mod.rs            路径段校验、共享错误类型
        snapshots.rs      快照修订、current 指针、修订清理
        artifacts.rs      制品对象缓存与校验
      maintain.rs         实现保留清理所需的快照维护入口
    cargo/
      mod.rs
      index.rs            索引解析与重写
      path.rs
      service/
        mod.rs            适配器与分发
        index.rs          索引获取与缓存
        artifact.rs       crate 下载与缓存
    npm/
      mod.rs
      metadata.rs         元数据结构与重写
      path.rs
      service/
        mod.rs            适配器、分发、审计
        search.rs
        metadata.rs
        tarball.rs
      cache/
        mod.rs
        metadata.rs       元数据缓存与指针
        artifacts.rs      制品缓存
        integrity.rs      integrity / shasum 校验
    nuget/
      mod.rs
      path.rs
      render.rs
      service_index/
        mod.rs
        cache.rs
        protocol.rs
      registration/
        mod.rs
        cache.rs
        protocol.rs
      package/
        mod.rs
        cache.rs
        protocol.rs
      search.rs
      vulnerability.rs
      icon.rs
    docker/
      mod.rs              适配器、注册、请求分派、错误映射
      path.rs
      manifest.rs         manifest / index 校验与响应
      blob.rs             blob 缓存、range、流式响应
      query.rs            catalog / tags / search 分页
      cache.rs            元数据与制品缓存读写
    upm/
      mod.rs
      path.rs
      service.rs          npm 兼容适配器的 UPM 规则
    vscode/
      mod.rs
      query.rs            Marketplace 查询重写
      assets.rs           资产与 vsix 下载、校验
      cache.rs            descriptor 与资产缓存

  upstream/
    mod.rs
    pool.rs               来源入口解析与请求分发
    group.rs              GroupRun 与成员循环
    budget.rs             制品并发预算与超时
    fallback.rs           失败分类与回退判断
    registry.rs           Adapter trait 与注册表
    fetch/
      mod.rs              HTTP 客户端、代理、gzip
      policy.rs           URL 与重定向安全规则

  storage/
    mod.rs
    layout.rs             数据目录契约（原 layout.rs）
    atomic.rs             原子写入（原 fsutil.rs）
    locks.rs              分片对象锁（原 locks.rs）
    access.rs             缓存命中时更新访问时间（原 retention::note_access）

  audit/
    mod.rs                审计 sink、事件详情、错误

  logging/
    mod.rs                日志文件、全局入口、轮转
    rotation.rs           UTC 日期标签与轮转文件名解析

  retention/
    mod.rs                清扫、轮转调度、旧布局清理、Maintainer 类型
```

## 关键设计决定

`config.rs` 拆成模型、解析、校验三部分。它现在同时承担类型定义、Raw 反序列化、校验
和大量测试，属于典型的"一个文件多种变化原因"。

`storage` 只保留数据目录布局和写入原语。`layout` 从根目录移入，`note_access` 从
`retention` 移入 `storage/access.rs`，使适配器在缓存命中时只依赖 `storage`，不再
反向依赖保留清理。

UTC 时间标签和轮转文件名解析放在 `logging/rotation.rs`。运行日志和审计文件共用这一
份实现，替代现在 `logging` 与 `retention` 各有一份 `rotated_label` 的重复代码。
`retention` 依赖 `logging`，方向单一。

跨生态的快照清理通过 `retention::Maintainer` 注入，不在 `retention` 里直接引用
`ecosystems::rustup`。`startup` 负责把 Rustup 的快照维护入口注册给清扫任务，
`retention` 只认识清扫报告和回调。

`startup/registry.rs` 用一个通用注册函数统一构造 SourcePool、注入抓取超时与制品
预算，再注册对应 Adapter；各生态只提供自己的适配器构造。Docker 因为需要额外监听
端口，保留独立的监听项收集。

首页和指南页面使用 `http::site::SiteContext`（公开地址与入口目录），不再把整个
`RustupService` 作为站点状态。镜像路由使用 `AdapterRegistry`，Rustup 分发入口使用
`RustupService`，三者用各自的子 Router 合并，互不承担对方的依赖。

`http/rustup_dist.rs` 与 `ecosystems/rustup` 同名不复用，前者是 HTTP 入口，后者是
协议实现。Docker Registry 的 token 挑战与交换属于出站访问策略，保留在
`upstream/fetch`，Docker 适配器不另外承担认证状态。

`audit`、`logging`、`retention` 保持独立目录，即使当前各只有一两个文件。它们的变化
原因不同，合并会重新引入 `ops` 这种无法读出职责的名字。

`router_with_rustup_and_cargo` 没有调用方，随本次结构调整删除。

## 旧到新映射

| 旧结构 | 新结构 | 处理 |
| --- | --- | --- |
| `src/main.rs` | `src/main.rs` + `startup/` + `commands/` | 拆分，入口只留参数分派 |
| `src/lib.rs` | `src/lib.rs` | 模块声明与公共导出更新 |
| `src/config.rs` | `src/config/` | 拆模型、解析、校验 |
| `src/http.rs` | `src/http/` | 拆路由、站点、指南、镜像、Rustup 入口 |
| `src/docs/source_config/*` | `src/http/guide/` + `src/config/catalog.rs` | 页面逻辑归 `http`，配置派生目录归 `config` |
| `src/service.rs` | `src/ecosystems/rustup/service/` | 移入生态目录并拆分 |
| `src/storage.rs` | `src/ecosystems/rustup/cache/` | 拆快照与制品缓存 |
| `src/layout.rs` | `src/storage/layout.rs` | 移动 |
| `src/fsutil.rs` | `src/storage/atomic.rs` | 改名移动 |
| `src/locks.rs` | `src/storage/locks.rs` | 移动 |
| `src/audit.rs` | `src/audit/mod.rs` | 移动 |
| `src/logging.rs` | `src/logging/` | 移动并抽出轮转标签 |
| `src/retention.rs` | `src/retention/` | 移动，访问时间抽出 |
| `src/timeformat.rs` | `src/logging/rotation.rs` | 合并 |
| `src/sources/*` | `src/upstream/` | 改名并拆池、组、预算、抓取 |
| `src/cargo/*` | `src/ecosystems/cargo/` | 移入并拆 service |
| `src/docker/*` | `src/ecosystems/docker/` | 移入并拆 manifest、blob、查询、缓存 |
| `src/npm/*` | `src/ecosystems/npm/` | 移入，service 与 storage 各自拆分 |
| `src/nuget/*` | `src/ecosystems/nuget/` | 移入，三个大模块按缓存与协议拆分 |
| `src/rustup/*` | `src/ecosystems/rustup/` | 移入并拆 service 与 cache |
| `src/upm/*` | `src/ecosystems/upm/` | 移动 |
| `src/vscode/*` | `src/ecosystems/vscode/` | 移入并拆查询、资产、缓存 |

## 迁移批次

每批完成实现移动、引用切换、测试更新、旧路径删除和定向验证；结构变化不夹带行为
变化。

1. 契约与底座：`config/`、`storage/`、`audit/`、`logging/`、`retention/`。验证配置、
   保留清理、缓存和审计定向测试。
2. 上游访问：`upstream/` 拆分。验证来源回退、超时、代理与 URL 安全测试。
3. 协议适配：按 Rustup、Docker、npm、NuGet、Cargo、VS Code、UPM 顺序逐个子批，每批
   跑对应测试文件。
4. 入口装配：`http/`、`startup/`、`commands/`、`main.rs`、`lib.rs`。验证路由、
   首页与指南页面测试。
5. 收口：删除旧路径和空转发模块，更新 `docs/architecture/multi-ecosystem-readonly-proxy.md`
   中的路径引用，执行完整质量门禁。

迁移开始时的中间结构包含 `app/`、`web` 名称的模块和 `guide/` 等目录；实施时在它
之上继续迁移到本设计的目标结构，不保留任何旧模块路径或兼容双轨。用户已有的未提交
修复内容随文件迁移保留，不重排、不回退。

## 验证

| 类型 | 检查 |
| --- | --- |
| 行为 | `cargo test --all-targets --all-features`，覆盖现有全部集成测试 |
| 格式与静态 | `cargo fmt --all -- --check`；`cargo clippy --all-targets --all-features -- -D warnings` |
| 结构 | 旧路径文件不存在；`src/app`、`src/web`、`src/sources`、`src/ops` 不出现；依赖方向门禁的 `rg` 检查为空 |
| 运行 | 本次不启动服务。路由、页面和协议行为由现有测试覆盖；真实上游与客户端联调仍是未验证边界 |

## 剩余边界

`retention` 仍然是唯一跨生态的清扫调度者，只是通过注入的维护入口工作。它不拥有
各生态的缓存格式，这一点在拆分后保持不变。

`tests/` 保持平铺，测试文件名继续用 `<生态>_<对象>_test.rs` 表达归属，不跟随源码目录
分层。

`docs/` 下的历史计划、评审和验证记录保留原路径引用，不回填新结构；历史文档记录的是
当时的事实。
