use super::{GuideSection, Target};

pub(crate) const SECTIONS: &[GuideSection] = &[
    GuideSection {
        id: "rustup",
        title: "Rustup",
        summary: "通过 Stacktory 下载 Rust 工具链和组件。",
        target: Target::ServicePath("/mirror/rustup/group"),
        prompt: r#"请把本机 Rustup 的下载地址切换到 Stacktory。

目标地址：{base_url}/mirror/rustup/group
要求：
1. 先确认 rustup 命令可用，并读取当前用户级 RUSTUP_DIST_SERVER；
2. 修改前记录原值，并在结果中报告恢复方式；
3. 只设置用户级 RUSTUP_DIST_SERVER，不修改其他 Rustup 配置；
4. 保持幂等，重复执行不要重复写入；
5. 重新打开终端后执行 rustup update stable，报告实际结果；
6. 如果路径、权限或当前配置不确定，停止并报告，不要猜测。"#,
        manual_steps: r#"配置文件：用户级环境变量 RUSTUP_DIST_SERVER

PowerShell（Windows）：
[Environment]::SetEnvironmentVariable("RUSTUP_DIST_SERVER", "{base_url}/mirror/rustup/group", "User")

设置后关闭并重新打开终端。"#,
        verify: "rustup update stable",
        notes: &[
            "设置的是用户级环境变量，修改后需要重新打开终端。",
            "Stacktory 负责工具链和组件下载，不替代 Rustup 首次安装程序。",
        ],
    },
    GuideSection {
        id: "cargo",
        title: "Cargo",
        summary: "通过 Sparse Registry 下载 crates.io 依赖。",
        target: Target::ServicePath("/mirror/cargo/group/"),
        prompt: r#"请把本机 Cargo 的 crates.io 源切换到 Stacktory。

目标地址：{base_url}/mirror/cargo/group/
要求：
1. 先读取 ~/.cargo/config.toml，确认文件位置、当前格式和已有 source；
2. 修改前备份原文件，并在结果中报告备份路径；
3. 只修改 crates.io 的 source replacement，不删除其他 source；
4. 保持幂等，重复执行不要重复追加配置；
5. 修改后在包含依赖的项目中执行 cargo fetch 验证；
6. 输出实际修改的文件、备份路径和验证结果；
7. 如果路径、权限或配置格式不确定时停止，不要猜测。"#,
        manual_steps: r#"配置文件：~/.cargo/config.toml
Windows 通常为 %USERPROFILE%\.cargo\config.toml

[source.crates-io]
replace-with = "stacktory"

[source.stacktory]
registry = "sparse+{base_url}/mirror/cargo/group/""#,
        verify: "cargo fetch",
        notes: &[
            "保留现有 source，不要覆盖无关配置；项目级 .cargo/config.toml 可能覆盖用户配置。",
            "Stacktory 只提供只读 Sparse Registry，不包含 crate 发布。",
        ],
    },
    GuideSection {
        id: "npm",
        title: "npm",
        summary: "通过 npm-compatible Registry 安装依赖。",
        target: Target::ServicePath("/mirror/npm/group/"),
        prompt: r#"请把本机 npm 的默认 registry 切换到 Stacktory。

目标地址：{base_url}/mirror/npm/group/
要求：
1. 先读取用户级 .npmrc，确认当前 registry 和已有配置；
2. 修改前备份原文件，并在结果中报告备份路径；
3. 只修改 registry，不删除其他配置项；
4. 保持幂等，重复执行不要重复追加；
5. 执行 npm ping --registry {base_url}/mirror/npm/group/ 验证；
6. 不写入 token、密码或认证信息；
7. 如果路径、权限或配置格式不确定时停止，不要猜测。"#,
        manual_steps: r#"配置文件：用户级 .npmrc
Windows 通常为 %USERPROFILE%\.npmrc

npm config set registry {base_url}/mirror/npm/group/

也可以直接写入 .npmrc：
registry={base_url}/mirror/npm/group/"
"#,
        verify: "npm ping --registry {base_url}/mirror/npm/group/",
        notes: &[
            "项目级 .npmrc 可能覆盖用户级配置。",
            "npm 和 Bun 共用 npm-compatible Registry，但客户端配置方式不同。",
        ],
    },
    GuideSection {
        id: "bun",
        title: "Bun",
        summary: "通过 npm-compatible Registry 安装 Bun 依赖。",
        target: Target::ServicePath("/mirror/npm/group/"),
        prompt: r#"请把本机 Bun 的默认 registry 切换到 Stacktory。

目标地址：{base_url}/mirror/npm/group/
要求：
1. 先读取用户级 ~/.bunfig.toml，确认当前 registry 和已有配置；
2. 修改前备份原文件，并在结果中报告备份路径；
3. 只修改 install.registry，不删除其他配置项；
4. 保持幂等，重复执行不要重复追加；
5. 在已有项目中执行 bun install 验证；
6. 不写入 token、密码或认证信息；
7. 如果路径、权限或配置格式不确定时停止，不要猜测。"#,
        manual_steps: r#"配置文件：用户级 ~/.bunfig.toml
Windows 通常为 %USERPROFILE%\.bunfig.toml

[install]
registry = "{base_url}/mirror/npm/group/"
"#,
        verify: "bun install",
        notes: &[
            "项目级 bunfig.toml 优先于用户级配置。",
            "Bun 与 npm 分开配置，不要只修改 npm 就认为 Bun 已生效。",
        ],
    },
    GuideSection {
        id: "nuget",
        title: "NuGet",
        summary: "通过 NuGet V3 只读入口还原包和执行只读查询。",
        target: Target::ServicePath("/mirror/nuget/group/v3/index.json"),
        prompt: r#"请把本机用户级 NuGet 配置的 Stacktory source 设置为只读源。

目标地址：{base_url}/mirror/nuget/group/v3/index.json
要求：
1. 先读取用户级 NuGet.Config，确认当前 source 和文件位置；
2. 修改前备份原文件，并在结果中报告备份路径；
3. 只添加或更新名为 stacktory 的 source，不删除其他 source；
4. 保持幂等，重复执行不要重复追加；
5. 不写入账号、token、密码或凭据；
6. 在已有项目中执行 dotnet nuget list source 和 dotnet restore --source stacktory 验证；
7. 如果路径、权限或配置格式不确定时停止，不要猜测。"#,
        manual_steps: r#"配置文件：用户级 NuGet.Config
Windows 通常为 %APPDATA%\NuGet\NuGet.Config

dotnet nuget add source "{base_url}/mirror/nuget/group/v3/index.json" --name stacktory
dotnet nuget list source"#,
        verify: "dotnet restore --source stacktory",
        notes: &[
            "项目级 NuGet.Config 可能覆盖用户级配置。",
            "Stacktory 只支持 restore、搜索和漏洞数据等只读接口，不要配置上游地址或凭据。",
        ],
    },
    GuideSection {
        id: "upm",
        title: "UPM",
        summary: "在当前 Unity 项目中配置 Stacktory scoped registry。",
        target: Target::ServicePath("/mirror/upm/group"),
        prompt: r#"请把当前 Unity 项目的 UPM scoped registry 配置到 Stacktory。

目标地址：{base_url}/mirror/upm/group
要求：
1. 先确认当前目录是 Unity 项目根目录，并读取 Packages/manifest.json；
2. 修改前备份 manifest.json，并在结果中报告备份路径；
3. 保留已有 scopedRegistries 和 scopes，只添加或更新 name 为 Stacktory 的条目；
4. 保持幂等，重复执行不要重复追加；
5. 不替用户决定 Unity scope，scope 由项目现有配置或项目负责人确认；
6. 修改后用 JSON 解析器确认 manifest.json 可解析，不要启动 Unity Editor；
7. 如果项目根目录、权限或配置格式不确定时停止，不要猜测。"#,
        manual_steps: r#"配置文件：项目根目录/Packages/manifest.json

{
  "scopedRegistries": [
    {
      "name": "Stacktory",
      "url": "{base_url}/mirror/upm/group",
      "scopes": ["com.company"]
    }
  ]
}

将示例 scope 替换为当前项目实际使用的 scope。"#,
        verify: "Get-Content -Raw Packages/manifest.json | ConvertFrom-Json",
        notes: &[
            "必须在 Unity 项目根目录修改，不要修改全局 Unity 配置。",
            "不要启动 Unity Editor；包解析结果由用户重新打开 Unity 后确认。",
        ],
    },
    GuideSection {
        id: "vscode",
        title: "VS Code",
        summary: "将 VS Code 扩展市场切换到 Stacktory。",
        target: Target::ServicePath("/mirror/vscode/group"),
        prompt: r#"请把本机 VS Code 的扩展市场切换到 Stacktory。

目标地址：{base_url}/mirror/vscode/group
要求：
1. 先通过 code --version 和安装目录定位当前 product.json，不要修改 settings.json；
2. 修改前备份原文件，并在结果中报告备份路径；
3. 只修改 extensionsGallery.serviceUrl、resourceUrlTemplate 和 extensionUrlTemplate；
4. 保留其他配置，保持幂等；
5. 修改后确认 product.json 可解析；
6. 要求完全退出并重新打开 VS Code；
7. 如果版本、commit、路径或权限不确定时停止，不要猜测。"#,
        manual_steps: r#"配置文件：VS Code 安装目录/resources/app/product.json
部分版本位于版本目录/resources/app/product.json

"extensionsGallery": {
  "serviceUrl": "{base_url}/mirror/vscode/group",
  "resourceUrlTemplate": "{base_url}/mirror/vscode/group/resource/{publisher}/{name}/{version}/{path}",
  "extensionUrlTemplate": "{base_url}/mirror/vscode/group/extension/{publisher}/{name}/latest"
}"#,
        verify: "Get-Content -Raw product.json | ConvertFrom-Json",
        notes: &[
            "修改前必须备份 product.json，VS Code 升级后可能覆盖该配置。",
            "普通 VS Code 不使用 settings.json 切换扩展市场，修改后需要完全重启。",
        ],
    },
    GuideSection {
        id: "docker",
        title: "Docker",
        summary: "将 Docker 镜像源指向管理员提供的 Stacktory Group 地址。",
        target: Target::AdminProvided("Docker Group 地址由管理员提供"),
        prompt: r#"请把本机 Docker 配置为使用管理员提供的 Stacktory Docker Group 镜像地址。

要求：
1. 先向用户确认管理员提供的 Docker Group 地址，不要猜测；
2. 确认当前用户具备修改 Docker daemon 配置的管理员权限；
3. 读取当前 daemon 配置并备份，Linux 通常是 /etc/docker/daemon.json；
4. 只修改 registry-mirrors，保留其他 daemon 配置；
5. 保持幂等，不重复追加；
6. 修改后重启 Docker，执行 docker info 检查 Registry Mirrors；
7. 如果地址、权限或配置路径不确定时停止，不要猜测。"#,
        manual_steps: r#"配置文件：Docker Desktop 的 daemon.json，或 Linux 的 /etc/docker/daemon.json
地址：由管理员提供，不要猜测。

{
  "registry-mirrors": ["管理员提供的地址"]
}"#,
        verify: "docker info",
        notes: &[
            "需要管理员权限，修改后必须重启 Docker。",
            "不要修改无关 daemon 配置；不要写入账号、密码或 token。",
        ],
    },
];

pub(crate) fn sections() -> &'static [GuideSection] {
    SECTIONS
}
