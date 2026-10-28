# 插件作者打包工具

作者构建完整签名 `.wplug`，分发管理插件只读取、验签和发布。插件运行 manifest 仍为 v3，通信版本不变。

在 Core 开发目录中使用 Node 24 和锁定的 Rust 工具链。各插件原有构建脚本保留编译、UI 和资源准备，统一调用本目录的封包阶段：

```powershell
# 开发包；没有身份时允许未签名，不需要增加本地版本号
pnpm --dir plugins/wla_template_plugin run package:plugin
# 正式成品；缺少原签名身份时失败
pnpm --dir plugins/wla_rich_editor run package:release
node packages/plugin-tools/verify.mjs plugins/wla_rich_editor/target/wla_rich_editor-0.2.3-windows-x86_64.wplug --signed
```

正式上传同 pluginId＋版本必须保持原始包字节。已在线登记的旧版本不能补签覆盖；准备新的在线版本。稳定推广复用原包。

## 原身份首次导入

```powershell
node packages/plugin-tools/cli.mjs import --plugin wla_rich_editor --file plugins/wla_rich_editor/.secrets/PLUGIN_UPDATE_SIGNING_KEY --expected-public-key 31a775e5aae522c4272bcc39547c89cfd78d2df1c768df1dd33d80fa05416fef
node packages/plugin-tools/cli.mjs status --plugin wla_rich_editor
```

文件是原 Ed25519 32 字节 seed 的标准 Base64 文本。工具只输出公钥和状态，原文件不删除。本机身份存于 `%LOCALAPPDATA%/Wonderland/author-keys`，由独立 `wla-plugin-key` 程序调用 Windows DPAPI 保护，无需启动 Core。首次使用会编译这个小工具，亦可预先运行 `cargo build --locked -p wonderland-plugin-tools`。

已有身份不能被不同密钥替换。只有新插件可显式运行 `node packages/plugin-tools/cli.mjs init --plugin new_plugin`。找不到旧私钥时应找回原备份或使用原私有 CI，不能初始化新密钥冒充原发布者。

`WONDERLAND_AUTHOR_HOME` 可选择隔离密钥库；`WONDERLAND_KEY_HELPER` 可指定独立工具路径。开发包自动使用已导入身份；正式打包始终要求签名。

## 口令备份与换机

备份由独立作者工具导出／恢复，使用 scrypt＋AES-256-GCM 格式。已有同格式备份仍可导入；管理插件不提供密钥迁出、备份或恢复入口。密码通过 stdin JSON 传入，不放进命令参数：

```powershell
$backupPassword = Read-Host '备份口令' -MaskInput
@{password=$backupPassword} | ConvertTo-Json -Compress | node packages/plugin-tools/cli.mjs export --plugin wla_rich_editor --file wla_rich_editor-backup.json
@{password=$backupPassword} | ConvertTo-Json -Compress | node packages/plugin-tools/cli.mjs restore --plugin wla_rich_editor --file wla_rich_editor-backup.json
Remove-Variable backupPassword
```

备份导出不覆盖已有文件；错误口令、损坏备份和身份冲突保留原记录。DPAPI 密文不能直接用于换机。

## 私有 CI

各插件的手动 package/release 工作流要求私有仓库、明确的插件与 Core 提交。原 Secret `PLUGIN_UPDATE_SIGNING_KEY` 仅传给构建步骤；公钥变量 `PLUGIN_UPDATE_SIGNING_PUBLIC_KEY` 用于核对原身份。CI 输出完整包和含源码提交、包摘要的收据，不创建 GitHub Release，也不触发 Cloudflare 发布。

使用前将包含这些工具的已验证 Core 提交提供给工作流，并为普通 CI 设置 `WONDERLAND_CORE_COMMIT`。未提交的本机更改不会进入 CI 包。公开原仓库的私有化、Secret 安全配置及正式发行继续单列处理。

## 包签名

根 `signature.json` 使用 schemaVersion 1／ed25519。签名内容为固定域 `wonderland-wplug-signature-v1`、NUL 和原始 `checksums.json` 字节。清单覆盖全部实际载荷，只排除两个根文件 `checksums.json` 和 `signature.json`。嵌套同名文件仍被覆盖。

签名无效时不能当作未签名包。Core 的可信身份来自目录登记及成功在线安装后的固定公钥；包自带公钥只能证明签名成立。完整分发契约由独立文档仓库的 `docs/PLUGIN-DISTRIBUTION-CONTRACT.md` 维护。
