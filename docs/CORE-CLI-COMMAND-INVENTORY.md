# Core CLI 命令清单

> 状态：2026-10-18，已按当前本地构建的 `wla --help` 核对。独立 CLI 与桌面共用执行服务；Debug 故障注入命令不属于 Release 接口。剩余验收由独立文档仓库的 `docs/CORE-IMPLEMENTATION-WORKPLAN.md` 维护。

## 设计规则

- CLI 是 Core 的正式入口，管理和插件调用复用同一 Core 应用服务、真实调用身份、参数契约及资源正确性检查；普通宿主服务不维护 capability grants。
- 面向正在运行的桌面窗口的命令通过本地 IPC 执行，并等待窗口确认；不能只改配置文件或 WebView 的 `localStorage` 后就报告成功。
- Core 提供稳定的通用 UI 控制命令；插件内容操作来自插件自己声明的命令及 schema。Core 不注册插件专属业务命令，也不提供执行任意 JavaScript、CSS 或 DOM 操作的接口。
- 查询和成功结果用 JSON 输出到 stdout；错误对象写入 stderr，并带稳定错误码和非零退出码。管理变更和后端插件方法执行要求显式 `--yes`。Core 不猜测插件方法的业务效果，UI 命令继续按声明的 read_only / mutating 语义确认。

## 当前已注册命令

| 命令 | 当前行为 | 对打开的桌面 UI 是否立即生效 |
| --- | --- | --- |
| `wla --help` | 打印当前命令帮助 | 否 |
| `wla --version` | 打印 CLI 版本 | 否 |
| `wla [--data-dir <绝对路径>] plugins list` | 返回安装插件及运行状态 | 桌面运行时转发到同一 Core；管理变更后通知 UI 刷新 |
| `... plugins ui-check <插件 ID> [资源路径]` | 启动目标插件并检查 UI 资源、MIME、CSP、`nosniff` | 诊断命令 |
| `... plugins call <插件 ID> <method> --input-json <JSON> --yes [--timeout-ms <ms>]` | 按实际 contract 校验参数、结果和期限 | 桌面运行时走同一 manager，否则独立非交互执行 |
| `... plugins install <本地包路径> --yes [--overwrite] [--approve-source-change]` | 安装 manifest v3 本地插件；Debug 可安装目录，Release 要求 `.wplug` 包；来源变更确认保留 | 桌面运行时转发，否则独立执行 |
| `... plugins enable／disable <插件 ID> --yes` | 允许使用或停用；普通插件保持待用 | 管理变更转发到当前 Core |
| `... plugins retry <插件 ID> --yes` | 显式重试运行失败，保留启用偏好 | 与 UI 共用激活服务，不修复损坏的安装包 |
| `... plugins uninstall <插件 ID> --yes [--remove-data]` | 卸载默认保留数据与密钥；`--remove-data` 明确选择同时删除两者 | 桌面运行时转发，否则独立执行；`remove` 别名保留 |
| `... plugins test-backend-exit <插件 ID> --yes` | Debug 构建专用后端故障注入 | Debug 专用，不出现在 Release 帮助中 |
| `wla core status` | Core 版本、数据目录、插件计数与运行计数 | 桌面运行时读同一 Core；独立运行时报告 `desktopRunning: false` |
| `wla web status` | 返回网页服务状态与当前临时访问密钥；输出包含凭据，勿提交到日志或仓库 | 必须有桌面 Core 正在运行 |
| `wla web start --config-json <JSON> --yes` | 根据监听范围、端口、公网入口及插件白名单开启 Web 服务 | 立即生效，设置页同步状态 |
| `wla web stop --yes` | 关闭监听，撤销密钥及插件资源授权 | 立即生效 |
| `wla logs dir／list／tail [--lines <1-500>]` | 日志目录、文件清单或最近日志行 | 只读 |

## 在线分发与下载

| 命令 | 行为 |
| --- | --- |
| `wla plugins catalog` | 当前来源目录和同步／授权状态 |
| `wla plugins distribution status` | 安全配置，不返回访问码 |
| `wla plugins distribution source <https-origin> --yes` | 设置单一来源；`clear-source --yes` 显式清源 |
| `wla plugins distribution set-code --stdin --yes` | 仅从 stdin 读码；`clear-code --yes` 清码，不允许在参数中传原码 |
| `wla plugins install-online <plugin-id> --yes [--approve-source-change] [--repair 或 --rollback]` | 安装／更新指定插件；可显式修复同版本或跟随当前渠道回退，重新在线授权并核对原发布公钥 |
| `wla plugins update <plugin-id> --yes [--approve-source-change]` | 仅更新已安装插件 |
| `wla web downloads list` | 查看所有设备待领／活跃下载，要求当前桌面 |
| `wla web downloads discard <download-id> --yes` | 移除指定交付并停止 reader，不删除用户原文件 |

分发不回退 GitHub；设置码通过敏感 IPC 载荷发送，旧进程不支持时明确拒绝，不降级为参数。离线快照只展示，获取需联网。完整规则由独立文档仓库的 `docs/PLUGIN-DISTRIBUTION-CONTRACT.md` 维护。

`--data-dir` 是全局选项，可放在子命令之前，指定独立的 Core 数据目录。未指定时使用桌面 Core 默认数据目录。所选 profile 有桌面 Core 正在运行时，CLI 通过随机令牌认证的本机 loopback IPC 转发，两者共用同一个 PluginManager。桌面未运行时，普通管理、日志和非交互插件方法使用独立宿主执行，不构建 Tauri App 或 WebView，不自动打开桌面。不同的显式数据目录不会转发给默认 profile。启动期先只读解析默认、活动及迁移目标目录并取得互斥，再恢复定位文件或迁移数据，防止不同 profile 重复写入同一 activeRoot。

网页服务的 JSON 配置、代理和手机使用方法见 [WEB-ACCESS.md](WEB-ACCESS.md)。`web` 命令只经本地 IPC 管理，不提供对应远程管理 API。

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
| `wla ui command run <插件>/<贡献> <命令 ID> --input-json <JSON> [--yes] [--timeout-ms <ms>]` | 按声明 schema 校验，经 Bridge 等待执行结果；必要时先打开目标 Activity |

`ui command run` 用于影响插件界面，例如在富文本编辑器中设置格式、把内容载入某个工作区视图。插件须在 contribution 中声明命令 ID、标题、输入 schema 和影响类型；会修改用户内容的命令要求 `--yes`。Core 验证插件声明、输入 schema、确认参数和 UI 响应超时；插件仍负责执行具体业务和更新自己的页面内容。Core 不接受任意 JavaScript、URL、CSS 或 DOM 命令。

`plugins call` 执行插件后端方法，`ui command run` 请求指定页面执行 UI 操作。两者使用各自的实际 contract/schema，不保留旧服务发现或动态 provider 命令。

## 独立插件调用与交互边界

`plugins call <plugin-id> <method> --input-json <json> --yes` 使用明确 pluginId 与方法名。输入按该插件实际 contract 校验，结果按实际结果 schema 校验，等待时间采用方法契约超时；桌面 IPC 不再用固定短超时截断长方法。CLI 不推断方法是否只读，所有后端方法执行均显式要求 --yes。

独立 CLI 列表、检查、启用和冷态安装不启动无关后端；本次方法调用按需激活真实目标和实际调用的依赖，命令退出关闭本次进程。独立 CLI 不恢复桌面后台任务，也不成为常驻服务；需要持续监控时运行桌面 Core。

独立 CLI 可执行网络、业务存储和其他非交互方法。需要文件选择器、浏览器或工作区等交互时，返回明确不可交互错误，不隐式启动桌面。用户要桌面交互时显式使用 `app open` 后执行相应命令。`ui` 与 `web` 管理要求当前桌面；桌面未运行时 UI 命令返回 DESKTOP_NOT_RUNNING。现有 Activity、Sidebar、布局、主题和插件 UI 命令均保留。

跨插件后端使用 SDK `core.plugins.call`，依赖只作诊断，CLI 不增加服务 ID、服务发现、动态 provider 或持久任务系统。升级和退出前排空当前调用与下载，跨重启不保证继续。

## 期限、输出与验收

`plugins call` 与 `ui command run` 可指定正整数 `--timeout-ms`；省略不增加额外总期限，contract／祖先期限仍有效。不使用旧固定 45 秒 UI 等待或 24 小时封顶。在线分发有独立网络预算，不能解释为普通插件调用上限。

成功 JSON 写 stdout，错误 JSON 写 stderr。成功退出码 0，用法／确认错误 2，Core 正忙 3，其他运行错误 1。UI 命令等待实际应用确认；安装已开始后的等待超时不得宣称事务已回滚。

可执行文件、源码与用户实际安装可能不同，脚本先核对版本和帮助。Windows 安装路径、桌面交互、签名更新及数据恢复的未完成项集中在独立文档仓库的 `docs/CORE-IMPLEMENTATION-WORKPLAN.md`。
