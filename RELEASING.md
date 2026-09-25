# Core 发布

Core 的用户说明、开发环境和安装流程见 [README.md](README.md)。Windows 更新使用 Tauri Updater：应用检查公开仓库 `YueFChen/Wonderland_Assistant` 的最新 GitHub Release，并在安装前验证更新包签名。`pnpm build` 可在本地构建 Windows 安装包；推送版本标签后，发布工作流另外生成签名更新产物和 `latest.json` 并上传到 GitHub Release。

## 发布

1. 同步修改 `apps/desktop/src-tauri/tauri.conf.json` 与根 `Cargo.toml` 的版本；如有需要，再更新对应的前端包版本。
2. 提交、推送代码，等待 CI 的前端、Rust 和供应链检查通过。
3. 创建与 Tauri 版本一致的标签，例如 `v0.2.0`，再推送标签。
4. `Release Core` 工作流将构建 Windows 安装包、签名更新产物，并把 `latest.json` 上传到同一个 GitHub Release。
5. 在旧版本应用的“设置 → 应用更新”中检查新版本，确认下载、验证签名和安装流程。

标签与版本不一致或私钥 Secret 缺失时，工作流会在打包前失败。首次发布前应在测试安装中验证 NSIS 更新路径。

## 在线插件目录

在线目录作为独立的 `YueFChen/Wonderland_Plugin_Catalog` 公共仓库维护，Core 从 GitHub Pages 读取目录 JSON，插件作者各自在自己的 GitHub Releases 发布 `.wplug`。每个插件单独提交登记文件；目录工作流检索并校验这些文件，汇总生成供 Core 读取的目录数据。目录仓库的首次设置、Pages 发布与分支规则见其 `REPOSITORY_SETUP.md`。

目录 PR 校验固定 Release 地址、包大小、SHA-256、manifest、compatibility、platform 和包内校验清单。Core 只展示兼容版本，安装前列出能力请求并要求确认；下载后复核索引哈希和 manifest，再走现有 `.wplug` 校验、暂存和原子安装流程。旧版本不会被目录更新自动替换，也不允许从目录安装低于已装版本的版本。
