# 本地改造与 Git 提交范围

## 仓库位置

Git 根目录为本文件所在的 `source/`，沿用官方 LocalSend 的现有提交历史。
上层 `LocalSend/` 是本机工作目录，不再额外创建嵌套仓库。

```text
LocalSend/
├── source/                 # Git 仓库：源码、测试、通用脚本和文档
│   ├── .git/
│   ├── .gitignore
│   ├── cli/
│   ├── packages/
│   ├── app/
│   ├── support/
│   └── target/             # 已忽略：可重新编译生成
└── development-setup/      # 仓库外：本机安装记录、历史备份和验证产物
```

从上层目录运行 Git 命令时使用 `git -C source ...`。

## 建议提交和上传

| 内容 | 处理方式 |
| --- | --- |
| `cli/src/`、`packages/`、`app/`、`server/` | 提交源码及现有资源文件，包括本地改造文件 |
| `support/nautilus/`、`support/gnome/` | 提交当前菜单、扩展、测试、安装脚本和使用说明；排除缓存 |
| `Cargo.toml`、`Cargo.lock`、`pubspec.yaml`、`pubspec.lock` 等 | 提交构建定义和现有锁文件，便于复现依赖 |
| `support/build/`、`support/scripts/` | 保留上游打包输入和脚本；目录名含 `build` 不代表都是缓存 |
| `.gitignore`、`.gitmodules`、`.github/`、`LICENSE`、文档 | 提交；保留上游许可证和贡献规范 |
| Git 中已有的生成文件、图标、图片、Windows 运行库 | 沿用上游跟踪方式，不因扩展名或目录名批量排除 |

Flutter SDK 是 `.gitmodules` 定义的子模块，继续保留子模块记录，不把整个 SDK 拷贝进源码提交。

## 忽略或留在本机

- Rust 的 `target/`，Flutter/Dart 的构建及依赖缓存，Python 的 `__pycache__/`、虚拟环境和测试缓存。
- 本机的 `development-setup/`、备份、运行日志、安装校验记录、临时截图和旧版本可执行文件。这些资料不影响已安装程序运行，但备份仍可用于本机回退，忽略不代表删除。
- `.env`、证书私钥、签名密钥、`identity.pem`、`paired-v2.json`、控制套接字和 PID 文件。
- 用户自己的 `~/.config/localsend-cli/`、`~/.local/state/localsend-cli/` 和实际接收到的文件；它们都在源码仓库外，不应复制到仓库中。
- 白名单、密钥和个人配置不上传：同时忽略任何版本的 `paired-v*.json` 及临时副本、`identity.pem` 及副本、仓库根目录的个人 `config.toml`。构建用的 `.cargo/config.toml` 不属于个人设备配置，继续保留。
- 本机工具配置目录 `.aws/`、`.codex/`、`.agents/`。

不要全局忽略 `*.png`、`*.json`、`*.toml`、`*.dll` 或所有名为 `build` 的目录，否则会漏掉上游资源、构建配置和当前扩展文件。
`.gitignore` 不会让已跟踪文件自动退出版本控制，也不能清除过去提交里的敏感信息。

## 提交前检查

```bash
git status --short
git diff --check
git add --dry-run .
```

确认列表只有源码、测试、通用脚本、文档和必要资源，再执行 `git add .` 与 `git commit`。
当前初始化不自动提交，也不创建远程仓库或推送。

现有 `origin` 指向官方 `localsend/localsend`，不是个人仓库。
准备上传个人仓库时，建议先将官方远程改名为 `upstream`，再将个人仓库地址设置为 `origin`，确认地址后才推送。
公开发布前还需检查个人设备名、本机路径以及所有暂存内容；本机诊断记录应继续留在仓库外。

## 本地安装说明

当前可复用的应用和菜单源码位于 `cli/`、`support/nautilus/`、`support/gnome/`。
部分历史安装器有固定版本校验，适用于当时的升级步骤，不能直接当作当前版本的通用安装命令：

- `support/nautilus/install_local.py` 校验旧的 `local-menu.3`。
- `support/gnome/install_panel.py` 校验旧的 `local-unified.1`，并约定上层工作目录包含 `source/`。

这些安装脚本可以作为可审查的源码提交，但在向其他电脑发布“一键安装”之前，需要另行更新和验证。
请勿将 `development-setup/` 里的本机升级记录或旧二进制作为安装依赖上传。
