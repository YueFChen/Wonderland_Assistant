# 插件仓库

Core 的 `.gitignore` 忽略 `plugins/` 下的独立插件仓库。每个插件保留自己的 Git、Cargo 和 pnpm workspace；Core 不将插件业务源码并入自身仓库或 workspace。

## 使用模板创建插件

Core 根目录的创建命令默认使用公开模板仓库 `git@github.com:YueFChen/template_plugin.git`。若本地已检出 `plugins/template_plugin`，会优先使用该目录；否则会临时克隆默认仓库。直接运行：

```powershell
pnpm run create:plugin -- --id my_plugin --name "我的插件" --author "作者名"
```

命令会在 `plugins/my_plugin` 创建独立仓库，填入插件元数据、安装依赖并检查 manifest/contract。它不会修改模板，也不会提交初始 Git commit。可用 `--template <本地路径或 Git URL>` 指定其他模板来源，也可通过 `WONDERLAND_PLUGIN_TEMPLATE_REPOSITORY` 环境变量覆盖默认远程地址。SSH 克隆需要本机 GitHub SSH 访问已配置。

## 开发与调试

从 Core 根目录运行插件仓库自己的脚本：

```powershell
pnpm --dir .\plugins\my_plugin run debug
```
