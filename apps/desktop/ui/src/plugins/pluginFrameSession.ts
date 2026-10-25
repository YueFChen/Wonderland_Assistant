/** An opaque frame cannot reveal its current URL. Only a Core-created document gets a bridge. */
export class PluginFrameSession {
  private window: object | null = null
  private available = false

  beginLoad(frameWindow: object | null): boolean {
    if (frameWindow === null) return false
    if (this.window === frameWindow) {
      this.available = false
      return false
    }
    this.window = frameWindow
    this.available = true
    return true
  }

  accepts(frameWindow: object | null): boolean {
    return this.available && frameWindow !== null && this.window === frameWindow
  }
}
