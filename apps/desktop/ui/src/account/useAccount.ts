import { useCallback, useEffect, useState } from 'react'

import { accountApi, onAccountChanged, toErrorMessage } from './api'
import type { AccountSnapshot } from './types'

interface AccountController {
  /** 尚未取到快照时为 null。 */
  snapshot: AccountSnapshot | null
  /** 最近一次操作的错误文案。 */
  error: string | null
  /** 有操作正在进行。 */
  busy: boolean
  /** 执行一个会改变账号状态的操作（返回值忽略），并刷新快照。 */
  run: (action: () => Promise<unknown>) => Promise<void>
}

/** 账号状态来源：初值 + 宿主事件订阅。 */
export function useAccount(): AccountController {
  const [snapshot, setSnapshot] = useState<AccountSnapshot | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const refresh = useCallback(async () => {
    try {
      setSnapshot(await accountApi.snapshot())
    } catch (cause) {
      setError(toErrorMessage(cause))
    }
  }, [])

  useEffect(() => {
    void refresh()
    const unlisten = onAccountChanged(() => {
      void refresh()
    })
    return () => {
      void unlisten.then((off) => off())
    }
  }, [refresh])

  const run = useCallback(
    async (action: () => Promise<unknown>) => {
      setBusy(true)
      try {
        await action()
        setError(null)
      } catch (cause) {
        setError(toErrorMessage(cause))
      } finally {
        setBusy(false)
        await refresh()
      }
    },
    [refresh],
  )

  return { snapshot, error, busy, run }
}
