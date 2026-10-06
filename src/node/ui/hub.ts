// The stable "webContents" the service layer talks to. It fans every send() out
// to the currently connected UI clients and survives UI restarts, so projection
// is never torn down just because the overlay reconnects.

export interface UiSink {
  sendLine(line: string): void
}

class UiBus {
  private readonly sinks = new Set<UiSink>()

  addSink(sink: UiSink): void {
    this.sinks.add(sink)
  }

  removeSink(sink: UiSink): void {
    this.sinks.delete(sink)
  }

  get clientCount(): number {
    return this.sinks.size
  }

  broadcast(channel: string, ...args: unknown[]): void {
    if (this.sinks.size === 0) return
    const line = `${JSON.stringify({ event: channel, args })}\n`
    for (const sink of this.sinks) {
      try {
        sink.sendLine(line)
      } catch {
        // The connection closes asynchronously; drop the write.
      }
    }
  }
}

export const uiBus = new UiBus()

/** WebContents-shaped proxy for the (single) main UI surface. */
export class UiWebContents {
  readonly id = 1
  readonly type = 'window'

  isDestroyed(): boolean {
    // The core outlives UI reconnects; frames keep flowing while the overlay restarts.
    return false
  }

  send(channel: string, ...args: unknown[]): void {
    uiBus.broadcast(channel, ...args)
  }

  setZoomFactor(_factor: number): void {}
  setBackgroundThrottling(_allowed: boolean): void {}
  once(_event: string, _listener: (...args: unknown[]) => void): this {
    return this
  }
  on(_event: string, _listener: (...args: unknown[]) => void): this {
    return this
  }
  removeListener(_event: string, _listener: (...args: unknown[]) => void): this {
    return this
  }
}

export const mainUiWebContents = new UiWebContents()
