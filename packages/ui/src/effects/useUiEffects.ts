import { useEffect } from 'react'

/** Decorative motion follows device preferences and active component visibility. */
export function useUiEffects(enabled: boolean) {
  useEffect(() => {
    const root = document.documentElement
    root.dataset.effects = enabled ? 'standard' : 'off'
    root.dataset.characterTheme = 'keqing-opulent'
    const visibility = () => { root.dataset.motionPaused = String(document.hidden) }
    visibility()
    document.addEventListener('visibilitychange', visibility)
    const tracked = new Set<HTMLElement>()
    const observer = new IntersectionObserver(entries => {
      for (const entry of entries) (entry.target as HTMLElement).dataset.motionPaused = String(!entry.isIntersecting)
    })
    const sync = (element: HTMLElement) => {
      if (element.getAttribute('aria-busy') === 'true') {
        if (!tracked.has(element)) { tracked.add(element); observer.observe(element) }
      } else if (tracked.delete(element)) {
        observer.unobserve(element)
        delete element.dataset.motionPaused
      }
    }
    const discover = (element: Element) => {
      if (element instanceof HTMLElement) sync(element)
      element.querySelectorAll<HTMLElement>('[aria-busy="true"]').forEach(sync)
    }
    discover(root)
    const mutations = new MutationObserver(records => {
      for (const record of records) {
        if (record.type === 'attributes' && record.target instanceof HTMLElement) sync(record.target)
        else for (const node of record.addedNodes) if (node instanceof Element) discover(node)
      }
      for (const element of tracked) if (!element.isConnected) {
        tracked.delete(element)
        observer.unobserve(element)
      }
    })
    mutations.observe(root, { subtree: true, childList: true, attributes: true, attributeFilter: ['aria-busy'] })
    return () => {
      document.removeEventListener('visibilitychange', visibility)
      mutations.disconnect()
      observer.disconnect()
      for (const element of tracked) delete element.dataset.motionPaused
      delete root.dataset.motionPaused
    }
  }, [enabled])
}
