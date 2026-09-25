/**
 * Translation resource lookup and parameter interpolation.
 */

/** 资源表：扁平 key → 文案。键形如 `settings.title`。 */
export type Messages = Record<string, string>

/** 宽泛的插值参数；只有表不是 `as const` 时才会用到。 */
export type MessageParams = Record<string, string | number>

/** 从模板里取出 `{name}` 的占位符名。 */
type Placeholder<T extends string> = T extends `${string}{${infer Name}}${infer Rest}`
  ? Name | Placeholder<Rest>
  : never

/**
 * Parameter type inferred from a translation template.
 */
type ParamsFor<T extends string> = string extends T
  ? MessageParams
  : Record<Placeholder<T>, string | number>

/**
 * Translation function bound to a resource table.
 */
export type Translator<M extends Messages> = <K extends keyof M & string>(
  key: K,
  params?: ParamsFor<M[K]>,
) => string

/** Track emitted missing-key and interpolation warnings. */
const warned = new Set<string>()

const warnOnce = (reason: string, detail: string) => {
  const signature = `${reason}:${detail}`
  if (warned.has(signature)) return
  warned.add(signature)
  console.warn(`[i18n] ${reason}：${detail}`)
}

/**
 * Create a translation function for a resource table.
 */
export function createT<M extends Messages>(messages: M): Translator<M> {
  return <K extends keyof M & string>(key: K, params?: ParamsFor<M[K]>): string => {
    const template: string | undefined = messages[key]
    if (template === undefined) {
      warnOnce('资源表里没有 key', key)
      return key
    }
    if (!params) return template
    const values = params as MessageParams
    const rendered = template.replace(/\{(\w+)\}/g, (placeholder, name: string) =>
      name in values ? String(values[name]) : placeholder,
    )
    if (/\{\w+\}/.test(rendered)) warnOnce(`key ${key} 有占位符没被替换`, rendered)
    return rendered
  }
}
