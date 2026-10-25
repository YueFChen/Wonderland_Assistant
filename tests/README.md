# Core 测试维护

本目录只保存跨模块测试所需的 fixture 和运行说明。Core 的 Cargo／pnpm workspace 是常规检查范围；`plugins/` 和文档仓库有独立历史与验证入口。

## 检查入口

```powershell
pnpm run check
pnpm run check:docs
cargo build --manifest-path tests/fixtures/direct-plugin/Cargo.toml --locked
cargo test -p wonderland-desktop plugin_manager:: --lib --locked -- --ignored --test-threads=1
cargo test -p wonderland-desktop web_access::tests::cold_remote --lib --locked -- --ignored --test-threads=1
```

`pnpm run check` 覆盖前端类型、Node 测试、资源构建、Rust 格式、Clippy、常规 Rust 测试和生成绑定。前端资源构建在此流程中使用 `build:assets`，避免重复类型检查；单独运行 `pnpm build:ui` 仍检查类型。

真实进程测试需要先构建 [direct-plugin fixture](fixtures/direct-plugin/README.md)，随后显式运行，CI 使用同一组命令。不要直接运行工作区全部 `--ignored` 测试：其中包含仅供崩溃恢复测试派生的子进程入口。

## 测试归属

| 范围 | 维护位置 | 主要行为 |
| --- | --- | --- |
| 桌面与手机前端 | `apps/desktop/ui/tests/` | 工作区、状态同步、主题、设备身份和传输 |
| 插件 UI SDK | `packages/plugin-ui-sdk/tests/` | Bridge 身份、调用结算、主题同步、取消和 LAN 随机 ID |
| 作者打包工具 | `packages/plugin-tools/tests/` | 跨语言签名、打包复现、Windows 密钥库恢复 |
| Core 更新服务 | `packages/core-updates/tests/` | 渠道、清单、安装包读取和签名验证 |
| Rust 服务 | 各模块内的 `tests` | 协议、网络、文件、配置、数据迁移和恢复 |
| 管理器常规测试 | `plugin_manager_tests.rs` | 控制记录、偏好、截止时间和数据保留 |
| 管理器真实进程 | `plugin_process_tests.rs`、`plugin_lifecycle_tests.rs`、`plugin_local_install.rs` | 进程链、按需启动、并发、取消、安装和回滚 |
| 原生 WebView 弹窗 | `scripts/check-browser-session-popups.mjs` | 隔离桌面中的 opener、嵌套窗口和共享 Cookie；需手工启动隔离 Debug Core |

管理器的 CLI 解析和输出位于 `plugin_cli.rs`，经 `plugin_manager` 原入口导出；状态、锁和安装事务仍由管理器维护。测试文件拆分保持原模块名，CI 的过滤条件继续生效。

## 精简准则

每项测试应验证一个独立失败场景。共享签名向量需要在 Rust 和 Node 两端分别验证；主机、SDK、HTTP 等边界各自的取消和身份测试也应保留。不能仅凭相似名称或调用同一函数判定重复。

可重复构建、包历史和无密钥行为由跨平台作者测试统一验证。Windows 密钥库测试只保留加密存储、导入身份、备份恢复、恢复后签名一致性及损坏处理。

已移除的 `core-cli-smoke.ps1`、`core-cli-benchmark.ps1` 和 `plugin-distribution-cli-smoke.mjs` 依赖旧 `w18` 目录、Core 0.1.9／旧插件数量或旧包签名格式，属于历史阶段验收。当前分发、签名、数据保留和回滚由 Rust／Node 测试及 CI 的安装后 CLI 检查承担。旧脚本可从 Git 历史查询，不再作为当前验证入口。

生命周期性能采样没有延迟门槛，进程数量断言与现有按需启动测试重叠，已从回归测试移除。后续基准应明确测量方法、基线和阈值，再作为单独入口维护。

## 临时产物

常规测试使用隔离临时目录，退出前停止自己创建的进程并清理目录。缓存与编译输出位于 Git 忽略的 `target/`、`node_modules/` 等位置。手工验收收据放在 `artifacts/reports/`；当前交付、私有备份和历史恢复资料按 `artifacts/README.md` 保留，不能当作临时文件批量删除。

已授权清理的内容若被审批策略阻止删除，按 [项目维护约定](../AGENTS.md) 移入 [垃圾桶](../trash/README.md)，由用户手动清理。
