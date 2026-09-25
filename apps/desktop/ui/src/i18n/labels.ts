/**
 * 「代号 → 显示名」映射（纯数据）。
 *
 * Maps backend enum values to their display labels.
 */
import type { AccountStatus, LoginCancelReason } from '@wonderland/core-bindings'

export const ACCOUNT_STATUS_LABEL: Record<AccountStatus, string> = {
  logged_out: '未登录',
  logging_in: '登录中',
  logged_in: '已登录',
}

export const LOGIN_CANCEL_REASON_LABEL: Record<LoginCancelReason, string> = {
  window_closed: '登录窗口已关闭',
  timeout: '等待登录超时',
  cancelled: '已取消登录',
  failed: '登录流程失败',
}
