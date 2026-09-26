# Core CLI 命令清单

> 状态：W8 已实现的 CLI 契约，2026-09-26。通用命令与参数以当前构建的 `wla --help` 为准；`plugins test-backend-exit` 仅 Debug 构建提供。CLI 尚未提供在线目录安装和插件服务调用命令。

## 设计规则

- CLI 是 Core 的正式入口，命令必须复用 Core 应用服务和权限判断。
- 面向正在运行的桌面窗口的命令通过本地 IPC 执行，并等待窗口确认；不能只改配置文件或 WebView 的 `localStorage` 后就报告成功。
- Core 提供稳定的通用 UI 控制命令；插件内容操作来自插件自己声明的命令及 schema。Core 不注册插件专属业务命令，也不提供执行任意 JavaScript、CSS 或 DOM 操作的接口。
- 查询和成功结果用 JSON 输出到 stdout；错误对象写入 stderr，并带稳定错误码和非零退出码。破坏性操作要求显式 `--yes`，权限授权必须逐项列明。

## 当前已注册命令

| 命令 | 当前行为 | 对打开的桌面 UI 是否立即生效 |
| --- | --- | --- |
| `wla --help` | 打印当前命令帮助 | 否 |
| `wla --version` | 打印 CLI 版本 | 否 |
| `wla [--data-dir <绝对路径>] plugins list` | 返回安装插件及运行状态 | 桌面运行时转发到同一 Core；管理变更后通知 UI 刷新 |
| `... plugins ui-check <插件 ID> [资源路径]` | 启动目标插件并检查 UI 资源、MIME、CSP、`nosniff` | 诊断命令 |
| `... plugins install <本地包路径> --yes [--grant <能力>]... [--overwrite] [--approve-source-change]` | 安装本地插件；Debug 可安装目录，Release 要求 `.wplug` 包；能力需逐项列明 | 管理变更转发到当前 Core |
| `... plugins enable|disable <插件 ID> --yes` | 启用或停用插件 | 管理变更转发到当前 Core |
| `... plugins remove <插件 ID> --yes [--remove-data]` | 移除插件，可显式删除其数据 | 管理变更转发到当前 Core |
| `... plugins permissions list <插件 ID>` | 查看插件声明能力及当前授权 | 读同一 Core 状态 |
| `... plugins permissions set <插件 ID> --grant <能力>... --yes` | 替换授权集合；未列出的授权撤销 | 管理变更转发到当前 Core |
| `... plugins test-backend-exit <插件 ID> --yes` | Debug 构建专用后端故障注入 | Debug 专用，不出现在 Release 帮助中 |
| `wla core status` | Core 版本、数据目录、插件计数与运行计数 | 桌面运行时读同一 Core；独立运行时报告 `desktopRunning: false` |
| `wla logs dir|list|tail [--lines <1-500>]` | 日志目录、文件清单或最近日志行 | 只读 |

`--data-dir` 是全局选项，可放在子命令之前，指定独立的 Core 数据目录。未指定时使用桌面 Core 默认数据目录。默认 profile 有桌面 Core 正在运行时，CLI 通过随机令牌认证的本机 loopback IPC 转发管理操作；两者共用同一个 PluginManager，避免启动重复插件进程或覆盖状态。不同的显式数据目录不会转发给默认 profile。

## 应用与窗口

| 命令 | 行为 |
| --- | --- |
| `wla app status` | 返回 Core 版本、桌面是否运行、当前路由、插件计数、布局和主题；桌面关闭时仍返回 Core 版本及 `desktopRunning: false` |
| `wla app open [--target <页面>]` | 启动 Core 或聚焦现有窗口；可同时导航到指定页面，并等待页面应用确认 |
| `wla ui state` | 查询当前路由、打开的 Activity、活动 Sidebar View、布局和主题 |
| `wla ui navigate <目标>` | 在当前窗口打开 `home`、`workspace`、`plugins`、`plugin-install`、`settings` 或 `plugin:<插件>/<贡献>`，等待 UI 确认 |

`ui navigate` 只接受 Core 路由注册表和插件 contribution 注册表中的目标，不接受任意 URL。

## 工作区与外观

| 命令 | 行为与对应 UI 状态 |
| --- | --- |
| `wla ui activity list` | 列出可用 Activity、状态及已打开标签 |
| `wla ui activity open <插件>/<贡献>` | 打开该 Activity；缺少标签时加入工作区并激活 |
| `wla ui activity activate <插件>/<贡献>` | 激活已有 Activity 标签 |
| `wla ui activity close <插件>/<贡献>` | 关闭标签并按 UI 规则选取后续活动 |
| `wla ui sidebar state` | 返回折叠状态和当前 Sidebar View |
| `wla ui sidebar <action>` | `action` 为 `collapse` 或 `expand`；立即更新工作区侧栏 |
| `wla ui sidebar open <插件>/<贡献>` | 打开已注册的 Sidebar View |
| `wla ui sidebar close` | 关闭当前 Sidebar View |
| `wla ui layout <action>` | `action` 为 `get` 或 `reset`；查询或恢复 Core 工作区布局 |
| `wla ui layout <action> <插件>/<贡献>` | `action` 为 `pin`、`unpin`、`hide` 或 `show`；管理固定区与隐藏项 |
| `wla ui layout move <插件>/<贡献> --before <目标>` | 调整固定入口或 Activity 卡片顺序 |
| `wla ui theme get` | 返回主题模式、透明度和背景选择 |
| `wla ui theme set --mode {system,light,dark,custom} [--opacity <0-100>] [--background <ID>]` | 修改 Core 主题并让当前窗口立即重绘 |

工作区布局由活动 UI 的 store 执行修改并确认结果；CLI 不直接读写 WebView 存储。

## 插件 UI 命令

| 命令 | 行为 |
| --- | --- |
| `wla ui command list <插件>/<贡献>` | 列出插件为该 UI contribution 声明的可调用命令 |
| `wla ui command run <插件>/<贡献> <命令 ID> --input-json <JSON> [--yes]` | 将输入按声明 schema 校验，经版本化 UI Bridge 发送给对应 contribution，并等待执行结果；必要时先打开目标 Activity |

`ui command run` 用于影响插件界面，例如在富文本编辑器中设置格式、把内容载入某个工作区视图。插件须在 contribution 中声明命令 ID、标题、输入 schema 和影响类型；会修改用户内容的命令要求 `--yes`。Core 验证插件声明、输入 schema、确认参数和 UI 响应超时；插件仍负责执行具体业务和更新自己的页面内容。Core 不接受任意 JavaScript、URL、CSS 或 DOM 命令。

`services call` 表达业务服务调用；它与 UI 命令分开。前者请求插件服务处理数据，后者请求某个页面执行用户可见的 UI 操作。

## 已完成范围与后续项

1. **CLI 契约：**稳定 JSON envelope、错误码、退出码、帮助与版本语义。
2. **桌面 IPC：**随机令牌、本机 loopback、单实例数据目录互斥和 UI 应用确认。
3. **Core 管理：**状态、日志、插件列表、生命周期、资源诊断和能力授权。
4. **UI 控制：**窗口、路由、Activity、Sidebar、布局与主题命令。
5. **插件 UI 命令：**版本化 manifest 声明、JSON Schema 校验、mutating 确认和插件 Bridge 响应。
6. **发行集成：**`wla.exe` 作为 Tauri sidecar 进入安装包与更新产物；Windows CI 验证版本、JSON 和退出码。

在线目录 CLI 搜索／安装尚未纳入本轮：需将现有目录流程整理为可复用的 Core 应用服务。W4 Service Broker 已支持插件后端之间的短调用，但尚未定义 CLI 的服务发现、参数传入、确认及长任务结果契约，因此暂不注册 `services list/call`。桌面在线插件页和插件后端服务调用可正常使用。

实现遵守 [Core 改造工作计划 W8](CORE-IMPLEMENTATION-WORKPLAN.md) 的分层和权限边界。桌面未运行时，窗口 UI 命令返回 `DESKTOP_NOT_RUNNING`；`app open` 会启动或聚焦桌面 Core。
