# Core Cloudflare 更新服务

Core 的更新服务由 Core 仓库维护，独立于插件分发服务。Cloudflare 正常提供 Core 更新；GitHub Release 暂缓不会关闭 CF。

## 资源与接口

| 项目 | 配置 |
| --- | --- |
| Cloudflare Account | `58c3a089c5810ac7d56430a6f757ef69` |
| Worker | `wonderland-core-updates` |
| Worker Custom Domain | `updates.yuef.me` |
| R2 | `wonderland-core-releases`，Standard，私有，关闭 r2.dev，无桶直连域名 |
| 稳定索引对象 | `channels/stable/latest.json` |
| 安装包与签名对象 | `releases/X.Y.Z/<filename>` |
| 发布开关 | `RELEASES_ENABLED=true` |

Worker 的 workers.dev／预览 URL 关闭，只允许配置的 HTTPS origin 和 GET／HEAD，不提供公网写入口。响应全部 no-store，不要添加覆盖它的缓存规则。

| 路径 | 行为 |
| --- | --- |
| `/v1/service` | 服务标识、协议版本与开关 |
| `/stable/latest.json` | Tauri 签名索引；未发布时 204，损坏索引 503 |
| `/releases/X.Y.Z/<filename>` | R2 流式下载，支持 HEAD 与单段 Range；缺失时 404 |

索引只允许 Windows x86_64 的 NSIS／MSI、原签名及同域同版本下载地址。服务不代理 GitHub，不持有签名私钥，不读取插件访问政策。外部契约依据为 [Tauri Updater](https://v2.tauri.app/plugin/updater/) 和 [R2 Worker API](https://developers.cloudflare.com/r2/api/workers/workers-api-reference/)。

## 检查与部署

在 Core 根目录运行：

```powershell
pnpm install --frozen-lockfile
pnpm --filter @wonderland/core-updates test
pnpm --filter @wonderland/core-updates exec wrangler deploy --dry-run
pnpm --filter @wonderland/core-updates exec wrangler whoami
pnpm --filter @wonderland/core-updates run deploy
```

部署复用本机 Wrangler OAuth；需要代理时设置当前进程的 HTTPS_PROXY，不从浏览器复制 Cookie 或将 OAuth 上传到 GitHub。依赖由 Core pnpm 锁文件固定，测试使用真实本地 Miniflare／R2。首次建桶使用 `wrangler r2 bucket create wonderland-core-releases`；已有桶不重复创建。部署前核对账号、资源和域名，避免接管其他项目。

只读检查：

```powershell
pnpm --filter @wonderland/core-updates exec wrangler r2 bucket dev-url get wonderland-core-releases
pnpm --filter @wonderland/core-updates exec wrangler r2 bucket domain list wonderland-core-releases
pnpm --filter @wonderland/core-updates exec wrangler deployments list --json
```

2026-10-25 部署已启用服务，Worker 版本为 `2d7a4be4-fc8c-4f4c-9fdf-e209accdedea`。GitHub Release 的插件接入门槛另见 [RELEASING.md](../../RELEASING.md)，与此服务开关无关。

## 直接发布签名 Core 更新

按 Core 发布说明完成原密钥签名构建及源码验证后，执行：

```powershell
pnpm --filter @wonderland/core-updates run prepare:update '<完整 bundle 目录>' '<尚不存在的交付目录>' '<更新说明文本文件>'
pnpm --filter @wonderland/core-updates run publish '<上述交付目录>'
```

prepare 收集一份 NSIS 与一份 MSI 及各自 .sig，只将文件名中的空格改成点号，复制原字节，生成 CF latest.json 和 SHA256SUMS.txt。工具按内置公钥进行 Minisign Ed25519／BLAKE2b-512 验签，也验证受信任注释；修改内容、签名或身份均拒绝。

publish 独立核对 Core 版本和已开放的 CF 服务。稳定版本必须递增，同路径对象有冲突立即停止。每份安装包／签名上传后经 HTTPS SHA-256 回读，最后提交稳定索引再回读；客户端仍使用 Tauri 原验签流程。流程不创建 GitHub 标签、草稿或 Release。

日常发布只允许一个发布者。使用本机 OAuth 或仅供 Core 桶对象读写的 Token；Core 应用本身不持有它们。不要覆盖已在线发布的同版本原包。

## 失败和恢复

上传或指针写入结果不明时，先检查 R2 和 HTTPS 当前状态，再决定继续；不要假设失败意味着没有写入。历史原对象保留，同版本重新构建的不同字节必须使用新版本。

应急停更时可将版本化配置的 RELEASES_ENABLED 改为 false 后部署：索引返回 204，安装包路径隐藏，R2 对象保留。恢复前核对原签名和全部摘要。索引回退先保存当前指针，并按实际授权执行恢复；普通发布工具拒绝自动降级，Tauri 默认版本比较也不会让已升级客户端降级。

本地运行时测试和 HTTPS 回读不能代替真实旧安装升级、用户数据保留及失败恢复验收。插件正式接入和 GitHub Release 是单独的后续工作。
