# Core 发布

Core 当前正式版为 0.2.1，已通过 Cloudflare 稳定渠道发布。GitHub Release 同步同版本的原始 NSIS／MSI 和 Tauri 签名，不重新构建已上线版本。维护插件独立发行，Core 不再以插件接入登记作为发布前置条件。

## 渠道与版本

Windows 使用 Tauri Updater，更新索引为 `https://updates.yuef.me/stable/latest.json`。安装器由独立 Worker `wonderland-core-updates` 和私有 R2 `wonderland-core-releases` 提供；原 Tauri 公钥、安装前验签和 Core 代理设置保持有效。

新版本同步根 Cargo workspace、Tauri、根 npm、桌面包和桌面 UI 版本。SDK、Worker、打包工具和插件按各自兼容需求维护版本。`pnpm release:status` 检查 Core 版本一致性，`node scripts/check-core-release.mjs --tag vX.Y.Z` 另行校验发布标签。

CF 新稳定版必须高于线上版本；GitHub 同步使用 CF 当前稳定版号，不为同步渠道单独递增。已上线的同 ID＋版本原包不得重新构建覆盖，历史标签不得移动。

## 签名构建与 Cloudflare 发布

1. 审阅当前分支、远端差异和未提交工作。插件与大部分 docs/ 有独立 Git 历史；只提交本次 Core 源码、锁文件和文档。账号、私钥、OAuth、安装包和恢复备份保留在忽略目录。
2. 按 [CI](.github/workflows/ci.yml) 完成冻结依赖安装、前端检查、Rust fmt／clippy／test、生成绑定、真实插件夹具、NSIS／MSI 安装后 CLI 检查及供应链检查。推送准确源码并等待对应提交的完整 CI。
3. 仅在构建进程设置 `TAURI_SIGNING_PRIVATE_KEY` 为原密钥文件路径，按需提供 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`，运行 `pnpm --filter @wonderland/desktop exec tauri build --ci --config src-tauri/tauri.release.conf.json`。结束后清除环境变量；不替换内置公钥或把私钥放进仓库。
4. 从完整 `target/release/bundle` 准备独立交付目录，生成 CF 索引、验证原签名并记录 SHA-256；使用本机 Wrangler OAuth 或专用 R2 Token 上传。命令、资源与恢复步骤见 [Core 更新服务运维](packages/core-updates/README.md)。
5. 发布器逐包通过 HTTPS 回读校验摘要，最后提交稳定索引并回读。记录原始构建提交、准确 CI、安装器与签名摘要，保留原始成品供 GitHub 同步。
6. 在隔离旧安装中验证下载、签名失败、升级、失败恢复和匹配的 `wla`。升级前排空活跃任务、文件交互和待领取下载，备份用户数据；实际核对账号、代理、插件密钥、固定公钥、背景和业务数据保留情况。

Tauri 2 使用原始签名 NSIS／MSI，不启用旧 updater ZIP 提取。签名构建和回读成功不代替真实旧安装升级与失败恢复验收。

## 同步 GitHub Release

1. 确认 CF 稳定版、仓库 Core 版本和拟发布标签一致，GitHub 目标版本尚不存在。更新 [RELEASE_NOTES.md](RELEASE_NOTES.md)，说明本版改动、升级注意事项和安装器原始构建提交。
2. 比对待打标签提交与原始构建提交的 Core 应用源码、Rust crate、前端、运行时共享包和锁文件。仅发布工具及文档整理可以晚于原始构建；尚未进入 CF 成品的功能改动另留后续版本。
3. 先推送准备提交，等待该准确提交的完整 CI，再在同一提交创建匹配版本的 annotated tag 并推送。不要同时手动创建相同 Release。
4. [标签工作流](.github/workflows/release.yml) 核对版本和准确提交的 CI，再从 CF 下载原始 NSIS／MSI 及 `.sig`。复用既有签名验证，拒绝版本不符、非 CF 地址、缺少安装器或签名不匹配的成品。
5. 工具保留安装器、签名、平台映射和 CF 发布时间，仅将 GitHub `latest.json` 的下载地址及更新说明切换到本次 Release，并生成 SHA256SUMS.txt。原 CF 索引保持不变。
6. 工作流上传全部成品到草稿，成功后公开并设为 Latest。完成后回读 GitHub Release、`latest.json` 和附件摘要，与 CF 原包逐项比较。

本地可在 Core 根目录预先准备相同成品，输出目录必须尚不存在：

```powershell
pnpm --filter @wonderland/core-updates run prepare:github '<新交付目录的绝对路径>' YueFChen/Wonderland_Assistant v0.2.1
```

同步既有成品不使用签名私钥或 Cloudflare 写入凭据。仍使用旧 GitHub 更新入口的 0.1.x 和早期 0.2.0 安装可通过 GitHub Release 首次迁移；安装后继续从 CF 检查与下载更新。

## 插件发行边界

插件作者在本机或私有 CI 构建完整签名 `.wplug`，分发管理端验签并发布到插件 Cloudflare 共享库。Core Release 不重新发布插件、不修改插件授权或仓库可见性。

作者 Ed25519 身份、Core Tauri 签名和 Cloudflare 登录互不替代。插件作者工具见 [packages/plugin-tools](packages/plugin-tools/README.md)。
