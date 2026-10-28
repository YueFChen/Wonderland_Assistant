# 插件仓库

Core 的 `.gitignore` 忽略 `plugins/` 下的独立插件仓库。每个插件保留自己的 Git、Cargo 和 pnpm workspace；Core 不将插件业务源码并入自身仓库或 workspace。

## 使用模板创建插件

Core 根目录的创建命令默认使用公开模板仓库 `git@github.com:YueFChen/wla_template_plugin.git`。若本地已检出 `plugins/wla_template_plugin`，会优先使用该目录；否则会临时克隆默认仓库。直接运行：

```powershell
pnpm run create:plugin -- --id wla_my_plugin --name "我的插件" --author "作者名"
```

命令会在 `plugins/wla_my_plugin` 创建独立仓库，填入插件元数据、安装依赖并检查 manifest/contract。它不会修改模板，也不会提交初始 Git commit。可用 `--template <本地路径或 Git URL>` 指定其他模板来源，也可通过 `WONDERLAND_PLUGIN_TEMPLATE_REPOSITORY` 环境变量覆盖默认远程地址。SSH 克隆需要本机 GitHub SSH 访问已配置。

## 开发与调试

从 Core 根目录运行插件仓库自己的脚本：

```powershell
pnpm --dir .\plugins\wla_my_plugin run debug
```

## 本地维护范围

当前维护奇域监测（`wla_wonderland_monitor`，合并原 `genshin_ugc` 与 `comment_collector`）、分发管理、知识库、我的奇域、富文本编辑器（`wla_rich_editor`）、插件模板、翻译工作台和网站收藏。各仓库的 README 说明自身业务、数据位置和检查命令；构建与检查从对应插件目录执行。

合并后的奇域监测使用新 ID 和独立空白数据分区，两侧旧数据均不迁移，旧评论仓库保留作历史参考。

日常修改沿用清单中的开发版本，覆盖安装保留 pluginId 对应的数据与密钥。正式在线版本的内容不可覆盖；作者构建完整签名包，分发管理端只验签和发布。Core 根仓库的提交不包含这些独立仓库的修改。

## UI 组件规范

插件页面跟随 Core 的主题与容器宽度，复用语义色、控件尺寸、边框、圆角及动效，不重复绘制宿主页头、品牌或背景。业务预览中的自选颜色和图片由插件保留。

- 普通 CSS 入口导入 `@wonderland/ui/plugin.css`；Tailwind 页面使用 `@wonderland/ui/theme.css`。
- 设置、编辑与辅助资料使用 `@wonderland/ui` 的 `Drawer` 非模态侧栏：主页面可继续操作，不放全屏遮罩，统一标题、正文滚动、底部提交和关闭后的焦点。内容留在插件 iframe 内，不穿透宿主 DOM；已注册为 Core View 的独立页面继续使用宿主侧栏。
- 删除、替换、稳定推广和身份变化等确认使用 `Dialog` 与 `DialogButton`。提交期间禁止重复操作与退出；不把关键后果藏进 hint、折叠说明或 hover。色板等锚定小浮层继续就地呈现。
- Vite 的 `resolve.dedupe` 包含 `react` 和 `react-dom`，确保独立插件与链接的共享 UI 包使用同一运行时。
- 默认文案保留操作、状态、错误及实际后果；使用说明放入文档，避免常驻重复的副标题和脚注。

## 插件图标

`manifest.json` 与 UI contribution 的 `icon` 字段继续接受旧版内置 Lucide 名称（如 `languages`）。也可以引用插件包内图标，例如：

```json
"icon": "asset:ui/icons/brand.svg"
```

自定义图标文件放在 `ui/icons/` 下，支持 SVG、PNG 和 WebP，单个文件上限为 512 KiB。该路径只指向已安装插件包内资源；Core 不加载远程图标 URL。资源缺失、读取失败或图标值无效时会回退到通用拼图图标，不影响插件加载。
