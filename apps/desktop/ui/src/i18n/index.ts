import { createT } from '@wonderland/ui/i18n'
import { zh } from './zh-CN.ts'

/** 宿主资源表的 key 联合；结构表的文案字段用它标注（如 `labelKey: MessageKey`）。 */
export type MessageKey = keyof typeof zh & string

/** 宿主界面的解析函数。 */
export const t = createT(zh)
