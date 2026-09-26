import { UserRound } from 'lucide-react'
import { useEffect, useState } from 'react'

/** 远端头像的加载尝试次数与重试间隔。 */
const MAX_IMAGE_ATTEMPTS = 3
const IMAGE_RETRY_DELAY_MS = 1200

interface UserAvatarProps {
  /** 名字，用于生成回落用的首字。 */
  name?: string | null
  /** 远端头像 URL；为空或加载失败时回落到首字。 */
  src?: string | null
  /** 直径（像素），字号按比例推导。 */
  size?: number
}

/**
 * 用户头像。
 *
 * Shows the remote avatar or a name-based fallback.
 */
export function UserAvatar({ name, src, size = 40 }: UserAvatarProps) {
  const [attempt, setAttempt] = useState(0)
  const [failed, setFailed] = useState(false)

  useEffect(() => {
    setAttempt(0)
    setFailed(false)
  }, [src])

  useEffect(() => {
    if (!failed || !src || attempt >= MAX_IMAGE_ATTEMPTS) {
      return
    }
    const timer = setTimeout(() => {
      setAttempt((current) => current + 1)
      setFailed(false)
    }, IMAGE_RETRY_DELAY_MS)
    return () => clearTimeout(timer)
  }, [failed, attempt, src])

  const initial = name ? (Array.from(name)[0] ?? '') : ''
  const showImage = Boolean(src) && !failed

  return (
    <div
      className="user-avatar-fallback relative flex shrink-0 items-center justify-center overflow-hidden rounded-full"
      style={{ width: size, height: size }}
    >
      {initial ? (
        <span className="font-bold text-white" style={{ fontSize: size * 0.42 }} aria-hidden>
          {initial}
        </span>
      ) : (
        <UserRound
          className="text-white/70"
          style={{ width: size * 0.5, height: size * 0.5 }}
          aria-hidden
        />
      )}

      {showImage ? (
        <img
          key={attempt}
          src={src ?? ''}
          alt=""
          className="absolute inset-0 h-full w-full object-cover"
          onError={() => setFailed(true)}
        />
      ) : null}
    </div>
  )
}
