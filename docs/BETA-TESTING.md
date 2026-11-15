# 0.9 Beta 测试指南 / Beta Testing

当前测试版本：`0.9.0-beta.1`。支持 macOS 15+，Intel 和 Apple Silicon
共用一个 Universal 安装包，不需要 Rust。Linux 仅实验性源码构建，暂不支持 Windows。

## 下载和安装

发布后从 [GitHub Release](https://github.com/humbertyu/lazyterm/releases/tag/v0.9.0-beta.1)
下载以下文件：

- `lazyterm-v0.9.0-beta.1-universal-apple-darwin.tar.gz`：程序和文档，内含 Skill。
- `SHA256SUMS`：校验和。
- `lazyterm-v0.9.0-beta.1-skill.zip`：单独的便携 Skill，可选。

先运行下面的命令，把输出和 `SHA256SUMS` 中对应文件的值比较，一致后再解压安装。
Skill ZIP 也有独立校验和。校验和用于检查下载完整性，不替代确认发布来源。

```sh
shasum -a 256 lazyterm-v0.9.0-beta.1-universal-apple-darwin.tar.gz
tar -xzf lazyterm-v0.9.0-beta.1-universal-apple-darwin.tar.gz
mkdir -p "$HOME/.local/bin"
install -m 755 lazyterm-v0.9.0-beta.1-universal-apple-darwin/lazyterm "$HOME/.local/bin/lazyterm"
"$HOME/.local/bin/lazyterm" --version
"$HOME/.local/bin/lazyterm" --diagnostics
```

运行 `~/.local/bin/lazyterm` 打开终端界面。将 `~/.local/bin` 加入 PATH 后，
也可以直接运行 `lazyterm`。程序是 TUI 和 CLI 共用的一个可执行文件。
首次测试建议用临时 Host 和测试目录；使用已有数据前，先完整备份 `~/.ssh`
和 `~/Library/Application Support/lazyterm/`。私钥不在应用导出包中。

安装包未经过 Apple 公证。如果 macOS 阻止打开，在确认来源与校验和后，
按系统“隐私与安全性”的明确授权流程放行；不要全局关闭 Gatekeeper。

## 让 Agent 帮你安装

可以把下面整段交给 WorkBuddy 或其他支持 Skill 的 Agent。Agent 需具备联网下载、
执行本机命令及写入安装目录的权限；沙箱权限不足时仍需在客户端授权。
仅安装 CLI 不会让客户端自动加载 Skill。

```text
请在这台 Mac 上安装 LazyTerm 0.9.0-beta.1 和对应的 lazyterm Skill。
发布地址：https://github.com/humbertyu/lazyterm/releases/tag/v0.9.0-beta.1

1. 检查 macOS 版本，要求 15+。从这个确切版本的 Release 下载
   lazyterm-v0.9.0-beta.1-universal-apple-darwin.tar.gz、
   lazyterm-v0.9.0-beta.1-skill.zip 和 SHA256SUMS。
   请直接下载预编译包，不需要安装 Rust 或编译源码。
2. 对照 SHA256SUMS 校验程序包和 Skill ZIP；校验失败则停止。
   解压前检查条目，不允许路径穿越、绝对路径或指向外部的链接。
3. 如果已有安装，检查版本并报告；升级前确认旧 TUI 已退出，
   提醒我备份 ~/.ssh 与 LazyTerm 数据，不要擅自停止现有项目服务。
   把程序安装到 ~/.local/bin/lazyterm，用绝对路径运行 --version、
   --diagnostics，确认版本是 0.9.0-beta.1。
4. 通过你所在客户端实际支持的 Skill 加载方式安装 ZIP 内的 lazyterm 目录。
   若支持 Skill ZIP 导入就使用该入口；若有已确认的 Skill 目录就放入该目录。
   不要猜目录。已有同名 Skill 时先比较并保留备份。
   无法操作客户端时，告诉我具体需要在哪个入口导入该 ZIP。
5. 确认你能读取 lazyterm/SKILL.md；必要时让我刷新 Skill 或新开会话。
   使用 ~/.local/bin/lazyterm schema --json 验证 CLI。
   若 PATH 不可用，本次会话始终使用这个绝对路径。
6. 最后汇报程序版本、安装路径、Skill 是否已被客户端加载，
   不要把“文件下载完成”当成“Skill 已启用”。

本次只授权安装和只读验证；不要改写我的 Host/Project 配置，
不要读取密码或私钥，不要启动、停止服务或建立远程连接。
不要绕过操作系统或 Agent 客户端的权限限制。
```

不支持 Skill 的客户端仍可以使用 CLI：先执行
`~/.local/bin/lazyterm schema --json` 查看当前能力和输入格式。

## 测试 Skill

加载后可以依次问 Agent：

- “我有哪些主机和项目？”——读取已有配置，无需登录主机。
- “本机哪些服务在跑？3000 端口是什么程序、从哪里启动的？”
- “项目有哪些命令和网页入口？”——只读取定义。
- “把这个项目的启动命令记下来，先不要启动。”——在你明确提供路径和命令后创建定义。
- “启动刚才那个项目命令。”——仅启动你明确指定的命令。
- “停止刚才启动的那次运行。”——先查状态，再按准确的 run ID 停止。

不需要每次强调“在 LazyTerm 里面”。Skill 默认查询本机的 LazyTerm 数据；
远端 Runtime 必须明确指定已有 Host，且需要系统 SSH 认证与主机信任。
Agent 通过自己的 SSH/SFTP 工具登录主机或传文件，LazyTerm 不提供这类 CLI 入口。
CLI 不会绕过客户端沙箱或系统权限；EPERM 等错误应报告并通过正常权限流程解决。

## 测试界面与反馈

优先验证 Hosts 的增改删和分组、Files 的传输与重命名、三类 Tunnel、
Projects 启停和 Runtime 归属，以及配置导出后再次导入的数据完整性。
涉及删文件、停止进程和配置导入，请使用可丢弃测试数据。
所有需要写入的测试都应先确认测试对象。

已知限制：Git 同步是未验收的实验功能；全界面翻译未完成；后台操作
首次连接未知服务器需先通过系统 SSH 确认 Host Key；不保存加密私钥口令；
远端 Project 不提供脱离 SSH 会话的常驻服务。详见
[支持范围](INSTALL.md) 和 [验收记录](v0.9-acceptance.md)。

遇到问题，在 [GitHub Issues](https://github.com/humbertyu/lazyterm/issues)
创建 Issue（如有 Beta feedback 模板可直接选择），附版本、macOS/CPU、终端、复现步骤、预期与实际结果和
`lazyterm --diagnostics` 输出。请先检查截图，勿上传密码、私钥、
配置/凭据导出包或完整原始日志。

## English Quick Start

Download the universal macOS archive and `SHA256SUMS` from the pinned release
above, compare the archive hash, then extract and install with the commands above.
Both CPU architectures use the same archive. Back up SSH and LazyTerm data
before upgrading. The binary is both a TUI and an Agent CLI.

Install the release's Skill ZIP using your Agent client's supported Skill loader
or confirmed Skill directory. Loading instructions vary by client; downloading
the ZIP alone does not activate it. The binary archive includes the same
`skills/lazyterm/SKILL.md`. Verify the binary version and
`lazyterm schema --json`, then ask about Hosts, Projects or local listening ports.
Read-only discovery does not authorize starting/stopping services or remote access.
Submit reviewed diagnostics and reproduction steps through GitHub Issues.
