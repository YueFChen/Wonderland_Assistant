import { createT } from './translate.ts'
import { zh } from './zh-CN.ts'

export { createT }
export type { MessageParams, Messages, Translator } from './translate.ts'

/** 本包资源表的 key 联合；结构表的文案字段用它标注（如 `labelKey: MessageKey`）。 */
export type MessageKey = keyof typeof zh & string

/** 本包组件的解析函数。消费方各自 `createT(自己的表)`，不共用这一份。 */
export const t = createT(zh)
