# Wonderland Assistant Core

Wonderland Assistant Core 是 Windows 桌面应用，为《原神 千星奇域》相关工作流和独立插件、应用工具提供统一入口。插件业务代码独立维护；Core 负责工作区、插件生命周期、统一宿主服务、在线目录和应用更新。

当前 Core 版本为 0.2.0，更新通过 Cloudflare 独立分发；所有维护插件正式接入前暂缓 GitHub Release。渠道与发布门槛见 [Core 发布说明](RELEASING.md)，完整开发契约与未完成验收由独立文档仓库维护，组合工作区入口为 `docs/README.md`。

## 功能

- **工作区**：从活动卡片启动插件功能，调整顺序、固定常用入口，并恢复上次打开的工作页。
- **插件管理**：启用、停用或移除插件，并查看运行状态与依赖诊断。普通宿主服务默认可用，插件后端以当前 Windows 用户权限运行；卸载默认保留插件数据与密钥，也可明确选择一起删除。
- **插件安装**：从在线目录或本地 `.wplug` 安装，多选和目录拖入统一预览、一次确认，逐包独立提交。在线包携带作者资料和签名，Core 验证内容及登记身份，后续更新核对已固定公钥；本地开发包可未签名。
- **用户数据与网络**：可在设置中迁移 Core 数据目录；插件私有数据按插件 ID 管理。Core 发出的请求可使用设置中的代理，插件后端自行联网不受 Core 代理强制约束。
- **应用更新**：在“设置 → 应用更新”中检查 Cloudflare 更新渠道，更新包在安装前验证原 Tauri 签名。
- **个性化与诊断**：调整主题、背景、启动页、日志级别和用户数据目录；查看 Core 状态与日志。
- **命令行入口**：安装包附带版本匹配的 `wla` CLI，可管理插件，也可控制正在运行的桌面窗口和工作区。
- **网页与手机访问**：在设置中开启服务并选择共享插件，访问密钥可选；支持局域网和 HTTPS 反向代理／内网穿透。文件交互归属发起设备，大文件通过原生下载交付。见[网页访问说明](docs/WEB-ACCESS.md)。
- **运行中更新提醒**：启动及运行期间每小时检查 Core 更新，返回窗口或恢复网络时补查（至少间隔 15 分钟）。新版本显示在首页通知、设置入口红点及“应用更新”中；关闭首页通知后，本次运行不再重复提示同一版本，忽略版本会同时隐藏红点。下载与安装仍由用户主动触发。

## 安装与使用

从 [Cloudflare 更新索引](https://updates.yuef.me/stable/latest.json) 的 Windows 下载地址获取当前安装包，或从 [GitHub Releases](https://github.com/YueFChen/Wonderland_Assistant/releases) 获取既有正式版。GitHub 新 Release 暂缓；当前 Core 通过 CF 正常更新。安装完成后可从开始菜单或桌面快捷方式启动。

在工作区中打开“插件管理 → 安装插件”可浏览在线目录或选择本地插件包。插件分发由私有 Cloudflare R2／D1 和 Worker 提供，测试／稳定渠道共用不可变版本库及授权政策。作者构建完整签名包，管理插件验签后发布；受限插件按访问码获取。作者构建见[作者工具](packages/plugin-tools/README.md)；完整分发契约由独立文档仓库的 `docs/PLUGIN-DISTRIBUTION-CONTRACT.md` 维护。Core 使用独立的 [Cloudflare 更新服务](packages/core-updates/README.md)，与 GitHub Release 的开放状态分别管理。

安装包会把匹配当前 Core 版本的 `wla.exe` 放在应用安装目录。可在 PowerShell 中通过完整路径调用：

```powershell
& '<Wonderland Assistant 安装目录>\wla.exe' core status
& '<Wonderland Assistant 安装目录>\wla.exe' plugins list
& '<Wonderland Assistant 安装目录>\wla.exe' app open --target settings
```

默认情况下，成功结果以 JSON 写到 stdout，错误 JSON 写到 stderr；命令退出码为成功 `0`、用法或确认错误 `2`、Core 正忙 `3`、其他运行错误 `1`。面向已打开窗口的命令通过本地 IPC 转发，并等待 UI 确认。命令清单、插件 UI 命令声明与脚本示例见 [`docs/CORE-CLI-COMMAND-INVENTORY.md`](docs/CORE-CLI-COMMAND-INVENTORY.md)。

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

常用命令均在仓库根目录执行：

| 命令 | 用途 |
| --- | --- |
| `pnpm dev` | 启动 Tauri 桌面应用与前端热更新 |
| `pnpm dev:ui` | 仅启动 Vite，地址为 `http://localhost:1420` |
| `pnpm run dev:cli core status` | 通过 Cargo 运行开发版 CLI，后接 CLI 参数 |
| `pnpm build` | 构建本地 NSIS 安装包，自动准备 CLI 和前端 |
| `pnpm build:ui` / `pnpm build:cli` | 单独构建前端或 release CLI |
| `pnpm build:ci` | 构建 CI 使用的 NSIS／MSI 安装包，不生成签名更新产物 |
| `pnpm typecheck` / `pnpm test` | 检查工作区 TypeScript／运行前端与打包工具测试 |
| `pnpm check:frontend` | 类型检查、测试和前端构建，与前端 CI 共用 |
| `pnpm check:rust` | Rust 格式、Clippy 和工作区测试 |
| `pnpm check:bindings` | 检查 Core 与插件协议的生成绑定是否同步 |
| `pnpm check` | 顺序执行前端、Rust 和绑定检查 |
| `pnpm check:docs` | 检查 Core 随库文档的本地链接；可传入独立文档路径另行检查 |
| `pnpm release:status` | 查看 GitHub Release 暂缓状态及缺少正式接入证据的插件 |

VS Code 的 F5 配置包括桌面运行、Rust 宿主调试和独立 CLI 调试；Rust 调试需安装推荐的 CodeLLDB 扩展。宿主调试通过后台任务等待 Vite 就绪，再由 Cargo 构建并选择可执行文件，遵循 [Tauri 调试流程](https://v2.tauri.app/develop/debug/vscode/)。`Ctrl+Shift+B` 构建本地安装包，也可从“运行任务”选择前端、CLI 或检查任务。

编辑器沿用系统的 `CARGO_HOME`、`RUSTUP_HOME` 和 npm 缓存配置；工具链由 `rust-toolchain.toml` 固定，Cargo 输出目录由 Cargo 配置或 `CARGO_TARGET_DIR` 决定。

Windows 安装包位于 `target/release/bundle/`。签名 Core 产物可直接发布到 CF，无需 GitHub Release 或版本标签。GitHub 的标签工作流另有插件接入门槛；发布流程见 [RELEASING.md](RELEASING.md)。

`target/` 仅保存 Cargo 构建输出。本地交付、验收记录和恢复备份统一放在 Git 忽略的 `artifacts/`，当前交付清单为 `artifacts/current/manifest.json`，目录说明为 `artifacts/README.md`。

## 仓库结构

- `apps/desktop/`：Tauri 桌面应用和 React 前端。
- `crates/`：Core 的 Rust crate，包括配置、网络、用户数据及插件协议。
- `packages/`：前端绑定、协议类型、共享 UI 与独立 Core 更新服务。
- `plugins/`：本地插件仓库目录；其中的独立仓库不会并入 Core。
- `scripts/`：插件模板创建等仓库工具。
- `artifacts/`：本地当前交付、验收报告、恢复备份及历史归档。

## 素材致谢

应用图标素材来自 Pixiv 作者 [Saika2017](https://www.pixiv.net/users/9896587) 发布的作品[《阿晴的霓裾翩跹》](https://www.pixiv.net/artworks/95101974)。
