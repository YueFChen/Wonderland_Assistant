# Wonderland Assistant Core

Wonderland Assistant Core 是 Windows 桌面应用，为《原神》相关工作流和独立插件提供统一入口。插件业务代码独立维护；Core 负责工作区、插件生命周期、权限确认、在线目录和应用更新。

## 功能

- **游戏入口**：从 Windows 注册表查找原神安装位置，也可以手动选择官方启动器或游戏本体。
- **工作区**：从活动卡片启动插件功能，调整顺序、固定常用入口，并恢复上次打开的工作页。
- **插件管理**：启用、停用或移除插件，并查看及调整插件申请的宿主能力。
- **插件安装**：从在线目录搜索安装，或从本地选择 `.wplug` 插件包。在线安装前会展示能力请求；安装时校验目录记录的 SHA-256 和插件包内容。
- **应用更新**：在“设置 → 应用更新”中检查 GitHub Releases。更新包在安装前进行签名验证。
- **个性化与诊断**：调整主题、背景、启动页、日志级别和用户数据目录。

## 安装与使用

从 [GitHub Releases](https://github.com/YueFChen/Wonderland_Assistant/releases) 下载 Windows 安装包并运行。安装完成后可从开始菜单或桌面快捷方式启动。

在工作区中打开“插件管理 → 安装插件”可浏览在线目录，或选择本地插件包。在线目录由维护者审核；插件包由各插件作者发布在自己的 GitHub Releases。目录仓库按插件分别维护登记文件，通过 PR 审核后汇总发布。登记与 Core 发布流程见 [RELEASING.md](RELEASING.md)。

## 创建和开发插件

插件仓库位于 `plugins/` 下，每个插件保留独立的 Git、Cargo 和 pnpm workspace，不属于 Core 的 Cargo workspace。使用模板创建插件：

```powershell
pnpm run create:plugin -- --id my_plugin --name "我的插件" --author "作者名"
```

该命令会在 `plugins/my_plugin` 创建插件仓库并安装依赖。开发与调试命令、模板选项和插件目录结构见 [plugins/README.md](plugins/README.md)。

## Core 开发

需要 Node.js 24、pnpm 11 和由 `rust-toolchain.toml` 固定的 Rust 工具链。Windows 构建还需要 Visual Studio C++ Build Tools、WebView2 和 Tauri 所需的 Windows 安装包工具。

```powershell
pnpm install --frozen-lockfile
pnpm dev
```

CI 检查和安装包构建命令：

```powershell
node --check scripts/create-plugin.mjs
pnpm -r run typecheck
pnpm -r run test
pnpm --filter @wonderland/desktop-ui run build
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
pnpm build
```

Windows 安装包位于 `target/release/bundle/`。签名更新文件和 GitHub Release 发布由标签触发的 Release 工作流生成；发布流程见 [RELEASING.md](RELEASING.md)。

## 仓库结构

- `apps/desktop/`：Tauri 桌面应用和 React 前端。
- `crates/`：Core 的 Rust crate，包括配置、网络、用户数据及插件协议。
- `packages/`：前端绑定、协议类型与共享 UI。
- `plugins/`：本地插件仓库目录；其中的独立仓库不会并入 Core。
- `scripts/`：插件模板创建等仓库工具。
