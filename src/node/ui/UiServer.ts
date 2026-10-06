// Unix-socket server for the native UI client. One JSON value per line.
//
//   UI -> core:  {"id":7,"method":"getSettings","params":[]}      (request)
//                {"method":"projection-touch","params":[x,y,a]}   (fire & forget)
//   core -> UI:  {"id":7,"ok":true,"result":{...}}
//                {"id":7,"ok":false,"error":"..."}
//                {"event":"settings","args":[ {...} ]}
//
// Requests map 1:1 onto the channel names the Electron renderer used through
// preload, so the service layer's registerIpcHandle/registerIpcOn code is reused
// unchanged.

import fs from 'node:fs'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import { mainUiWebContents, uiBus, type UiSink } from './hub'
import { eventListeners, invokeHandlers } from './registry'

export const UI_PROTOCOL_VERSION = 1

export function uiSocketPath(): string {
  if (process.env.LIVI_UI_SOCK) return process.env.LIVI_UI_SOCK
  const dir = process.env.XDG_RUNTIME_DIR || os.tmpdir()
  return path.join(dir, 'livi-ui.sock')
}

type Request = { id?: number; method?: string; params?: unknown[] }

class UiServer {
  private server: net.Server | null = null
  private readonly sockets = new Set<net.Socket>()
  private connectListeners = new Set<() => void>()
  private started = false
  readonly socketPath = uiSocketPath()

  onClientConnected(listener: () => void): () => void {
    this.connectListeners.add(listener)
    return () => this.connectListeners.delete(listener)
  }

  get clientCount(): number {
    return this.sockets.size
  }

  async start(): Promise<void> {
    if (this.started) return
    this.started = true

    try {
      fs.unlinkSync(this.socketPath)
    } catch {
      // no stale socket
    }

    const server = net.createServer((socket) => this.onConnection(socket))
    this.server = server
    await new Promise<void>((resolve, reject) => {
      server.once('error', reject)
      server.listen(this.socketPath, () => {
        server.off('error', reject)
        resolve()
      })
    })
    try {
      fs.chmodSync(this.socketPath, 0o600)
    } catch {
      // best-effort
    }
    console.log(`[core] UI socket listening at ${this.socketPath}`)
  }

  async stop(): Promise<void> {
    for (const socket of this.sockets) socket.destroy()
    this.sockets.clear()
    const server = this.server
    this.server = null
    this.started = false
    if (!server) return
    await new Promise<void>((resolve) => server.close(() => resolve()))
    try {
      fs.unlinkSync(this.socketPath)
    } catch {
      // already gone
    }
  }

  private onConnection(socket: net.Socket): void {
    socket.setNoDelay(true)
    this.sockets.add(socket)

    const sink: UiSink = {
      sendLine: (line) => {
        if (!socket.destroyed) socket.write(line)
      }
    }
    uiBus.addSink(sink)

    sink.sendLine(
      `${JSON.stringify({
        event: 'hello',
        args: [{ protocol: UI_PROTOCOL_VERSION, pid: process.pid }]
      })}\n`
    )

    let inbox = ''
    socket.on('data', (chunk) => {
      inbox += chunk.toString('utf8')
      let nl = inbox.indexOf('\n')
      while (nl >= 0) {
        const line = inbox.slice(0, nl).trim()
        inbox = inbox.slice(nl + 1)
        if (line) this.dispatch(line, sink)
        nl = inbox.indexOf('\n')
      }
      // Guard against a client that never sends a newline.
      if (inbox.length > 8 * 1024 * 1024) inbox = ''
    })

    const cleanup = (): void => {
      uiBus.removeSink(sink)
      this.sockets.delete(socket)
    }
    socket.on('close', cleanup)
    socket.on('error', cleanup)

    for (const listener of this.connectListeners) {
      try {
        listener()
      } catch (e) {
        console.warn('[core] UI connect listener threw (ignored)', e)
      }
    }
  }

  private dispatch(line: string, sink: UiSink): void {
    let msg: Request
    try {
      msg = JSON.parse(line) as Request
    } catch {
      return
    }
    const method = msg.method
    if (typeof method !== 'string' || !method) return
    const params = Array.isArray(msg.params) ? msg.params : []
    const event = {
      sender: mainUiWebContents,
      senderFrame: null,
      preventDefault: (): void => {}
    }

    if (typeof msg.id === 'number') {
      const handler = invokeHandlers.get(method)
      if (!handler) {
        sink.sendLine(`${JSON.stringify({ id: msg.id, ok: false, error: `unknown method ${method}` })}\n`)
        return
      }
      Promise.resolve()
        .then(() => handler(event, ...params))
        .then((result) => {
          sink.sendLine(`${JSON.stringify({ id: msg.id, ok: true, result: result ?? null })}\n`)
        })
        .catch((e: unknown) => {
          const message = e instanceof Error ? e.message : String(e)
          sink.sendLine(`${JSON.stringify({ id: msg.id, ok: false, error: message })}\n`)
        })
      return
    }

    const listeners = eventListeners.get(method)
    if (!listeners) return
    for (const listener of listeners) {
      try {
        listener(event, ...params)
      } catch (e) {
        console.warn(`[core] UI event '${method}' threw (ignored)`, e)
      }
    }
  }
}

export const uiServer = new UiServer()
