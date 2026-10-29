---
id: pypi
group: Python
title: PyPI
summary: 通过 PyPI Simple API 安装 Python 包。
ecosystem: pypi
---
## 提示词
请把本机 pip 的默认包索引切换到 Stacktory。

目标地址：{endpoint}
要求：
1. 先执行 python -m pip config list -v，确认全局、用户和项目级配置；
2. 修改前备份原配置文件，并在结果中报告备份路径；
3. 只修改 index-url，不删除 trusted-host、extra-index-url 或其他已有配置；
4. 保持幂等，重复执行不要重复追加；
5. 执行 python -m pip download --no-deps --dest <临时目录> <package> 验证；
6. 不写入账号、密码或 token；
7. 如果路径、权限或配置格式不确定时停止，不要猜测。

## 手动步骤
配置文件位置由 pip 决定，先执行：

```text
python -m pip config list -v
```

设置用户级默认索引：

```text
python -m pip config set global.index-url {endpoint}
```

也可以只在单次命令中指定：

```text
python -m pip install --index-url {endpoint} <package>
```

## 验证
```text
python -m pip download --no-deps --dest <临时目录> <package>
```

## 注意
- 如果入口使用 HTTP，pip 还需要为该主机配置 trusted-host，或仅在受信内网中临时使用 --trusted-host。
- Stacktory 根据 Accept 返回 Simple API 的 HTML 或 JSON 表示；pip 会自动协商。
- Group 按成员顺序回退；Proxy 只访问自身，不会自动切换到兄弟来源。
- 文件下载会按项目索引声明的摘要校验，摘要不匹配的内容不会写入缓存。
- Stacktory 只提供只读安装能力，不提供 PyPI 上传、删除、账号或 token 管理。
