# 0.9 Beta 测试指南 / Beta Testing

当前测试版本：`0.9.0-beta.3`。支持 macOS 15+，Intel 和 Apple Silicon
共用一个 Universal 安装包，不需要 Rust。Linux 仅实验性源码构建，暂不支持 Windows。

## 下载和安装

已安装 Homebrew 的用户推荐：

```sh
brew install humbertyu/brew/lazyterm
lazyterm
```

完整安装流程以及如何理解 Homebrew trust 提示，见
[Agent 安装说明](AGENT-INSTALL.md)。完整公式名只选择 LazyTerm 这个公式，
不需要信任整个 tap。
更新使用 `brew update` 和 `brew upgrade lazyterm`，卸载使用
`brew uninstall lazyterm`；卸载保留用户配置与凭据。

也可以从 [GitHub Release](https://github.com/humbertyu/lazyterm/releases/tag/v0.9.0-beta.3)
下载以下文件：

- `lazyterm-v0.9.0-beta.3-universal-apple-darwin.tar.gz`：程序和文档，内含 Skill。
- `SHA256SUMS`：校验和。
- `lazyterm-v0.9.0-beta.3-skill.zip`：单独的便携 Skill，可选。

先运行下面的命令，把输出和 `SHA256SUMS` 中对应文件的值比较，一致后再解压安装。
Skill ZIP 也有独立校验和。校验和用于检查下载完整性，不替代确认发布来源。

```sh
shasum -a 256 lazyterm-v0.9.0-beta.3-universal-apple-darwin.tar.gz
tar -xzf lazyterm-v0.9.0-beta.3-universal-apple-darwin.tar.gz
mkdir -p "$HOME/.local/bin"
install -m 755 lazyterm-v0.9.0-beta.3-universal-apple-darwin/lazyterm "$HOME/.local/bin/lazyterm"
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

将 [Agent 安装说明](AGENT-INSTALL.md) 分享给 WorkBuddy 或其他 Agent。
其中包含可直接交给 Agent 的安装提示、Homebrew trust 说明，以及 Skill
安装和校验步骤。Agent 需具备联网、执行命令和安装 Skill 的客户端权限；
仅安装 CLI 不会自动启用 Skill。

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
