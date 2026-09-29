export type CloseBehavior = 'ask' | 'tray' | 'exit'

const CLOSE_BEHAVIOR_KEY = 'wonderland.core.close-choice.v1'

export function readCloseBehavior(): CloseBehavior {
  try {
    const value = localStorage.getItem(CLOSE_BEHAVIOR_KEY)
    return value === 'tray' || value === 'exit' ? value : 'ask'
  } catch {
    return 'ask'
  }
}

export function writeCloseBehavior(value: CloseBehavior) {
  if (value === 'ask') localStorage.removeItem(CLOSE_BEHAVIOR_KEY)
  else localStorage.setItem(CLOSE_BEHAVIOR_KEY, value)
}
