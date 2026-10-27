# Core 发布

**Cloudflare 正常更新，GitHub Release 暂缓。** Core 当前版本为 0.2.1；2026-10-27 准备该版本的 CF 更新，发布状态以线上稳定索引为准。GitHub 最新正式版仍为 0.1.9。源码准备和推送、CF 更新、本地候选及 GitHub Release 分别管理。

## 渠道与门槛

Windows 使用 Tauri Updater，更新索引为 `https://updates.yuef.me/stable/latest.json`，安装器由独立 Worker `wonderland-core-updates` 和私有 R2 `wonderland-core-releases` 提供。原 Tauri 公钥、安装前验签和 Core 代理设置保持有效。CF 发布开关已开放，没有版本时返回 204，有有效索引时正常提供更新。

[GitHub 发布策略](.github/core-release-policy.json) 保持 `allowRelease=false`。所有维护插件正式接入并登记配套验收证据前，不创建标签或 GitHub Release。运行 `pnpm release:status` 可查看缺少记录的插件；缺少记录表示门槛未关闭，不代表断言该插件从未接入测试服务。

GitHub 暂缓只约束标签工作流。CF 上传不依赖 GitHub 标签、草稿、Release、Actions Secret 或插件接入开关。错误推送标签会在创建任何 Release／草稿前被阻止。

## Cloudflare 正常更新

1. 准备尚未占用的正式 Core 版本，同步 Tauri、根 Cargo／npm 及需要跟随 Core 的配套包；不要批量修改独立插件版本。已在线发布的同版本原包不可覆盖。
2. 按 [CI](.github/workflows/ci.yml) 完成冻结依赖安装、前端检查、Rust fmt／clippy／test、生成绑定、真实插件夹具和供应链检查。
3. 在仅当前进程设置 `TAURI_SIGNING_PRIVATE_KEY` 为原密钥文件路径，按需提供 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`，运行 `pnpm --filter @wonderland/desktop exec tauri build --ci --config src-tauri/tauri.release.conf.json`。结束后清除进程环境变量。不要替换内置公钥或把私钥放进仓库。
4. 从完整 `target/release/bundle` 准备独立交付目录，生成 CF 索引、校验原公钥签名并记录 SHA-256；再用本机 Wrangler OAuth 或专用 R2 Token 上传。命令、资源和恢复步骤见 [Core 更新服务运维](packages/core-updates/README.md)。
5. 上传工具检查 Core 版本、平台、URL、签名与原身份；逐包经 HTTPS 回读并校验摘要后，最后提交稳定索引并回读。此流程不会调用 GitHub 创建 Release。
6. 在隔离旧安装中另行验证下载、签名失败、升级、失败恢复和匹配的 `wla`。升级前排空活跃任务、文件交互和待领取下载，备份用户数据，核对账号、代理、插件密钥、固定公钥、背景和业务数据均保留。

Tauri 2 使用原始签名 NSIS／MSI，不启用旧 updater ZIP 提取。插件 .wplug 使用独立的已修复 zip 9 依赖，仍执行路径、链接、重复项及大小校验。签名构建和回读成功不等于真实旧安装升级及失败恢复已完成。

仍内置 GitHub 地址的旧安装（包括 0.1.x 和早期 0.2.0 本地候选）不会被服务端自动改写。GitHub 暂缓期间，可手动安装本次 CF 的 0.2.0 完成首次迁移；之后检查和下载均走 CF。将来开放 GitHub Release 后，旧客户端也可通过原链路首次迁移。

## 准备 GitHub 源码与未来 Release

先审阅当前分支、与远端的提交差异和已有未提交工作。插件与大部分 docs/ 有独立 Git 边界，只提交应交付的 Core 源码、锁文件和文档；账号、私钥、OAuth、安装包和恢复备份留在忽略目录。已有浏览器会话改动应单独审阅，不被本次渠道变更覆盖。普通分支／PR 推送和 CI 不需要开放 Release；CI 支持手动检查指定分支。

所有维护插件正式接入后，才为发布策略的每个 requiredPlugins 项登记正式版本和验收证据，并明确开放 allowRelease。准备未占用版本并推送源码，等待准确提交的完整 CI 通过，再在同一提交创建匹配版本的 annotated tag 并推送；不得移动历史标签。

标签工作流先检查插件门槛、准确提交的 CI、版本和签名 Secret，再生成安装器、签名及 GitHub latest.json，创建草稿并在构建成功后公开。既有 `TAURI_SIGNING_PRIVATE_KEY` Secret 已配置；有口令时另需 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。CF 更新不要求新增 GitHub Cloudflare Secret。

## 插件发行边界

插件作者在本机或私有 CI 生成完整签名 .wplug，由 distribution_manager 验签并发布到插件 Cloudflare 共享库。Core 渠道部署不会发布插件、改变插件授权或修改仓库可见性。

作者 Ed25519 身份、Core Tauri 签名和 Cloudflare 登录互不替代。作者工具见 [packages/plugin-tools](packages/plugin-tools/README.md)。组合工作区中的 docs/PLUGIN-DISTRIBUTION-CONTRACT.md、docs/PLUGIN-DISTRIBUTION-CLOUDFLARE-SETUP.md 和 docs/CORE-IMPLEMENTATION-WORKPLAN.md 维护插件契约、运维与待验收事项。
