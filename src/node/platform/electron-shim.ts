// Minimal drop-in replacement for the `electron` module, used by the headless
// core bundle (esbuild aliases `electron` to this file). The service layer was
// written against these APIs; only the parts it actually touches are real.
//
// Implemented: app, ipcMain, BrowserWindow, webContents, screen, dialog, shell,
// session, protocol, net, nativeImage, powerMonitor.
// Everything UI-specific (windows, dialogs) degrades to non-interactive stubs.

import { EventEmitter } from 'node:events'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { spawn } from 'node:child_process'
import { invokeHandlers, eventListeners, removeAllListeners } from '../ui/registry'
import { mainUiWebContents, UiWebContents } from '../ui/hub'

const APP_ROOT = process.env.LIVI_APP_PATH ?? process.cwd()
const USER_DATA = process.env.LIVI_USER_DATA ?? path.join(os.homedir(), '.config', 'LIVI')

function readVersion(): string {
  if (process.env.LIVI_VERSION) return process.env.LIVI_VERSION
  try {
    const pkg = JSON.parse(fs.readFileSync(path.join(APP_ROOT, 'package.json'), 'utf8')) as {
      version?: string
    }
    if (pkg.version) return pkg.version
  } catch {
    // not installed with a package.json next to it
  }
  return '0.0.0'
}

class App extends EventEmitter {
  readonly name = 'LIVI'
  readonly isPackaged = process.env.LIVI_PACKAGED === '1'
  isQuitting = false
  private readonly version = readVersion()

  readonly commandLine = {
    appendSwitch(_name: string, _value?: string): void {},
    appendArgument(_value: string): void {},
    hasSwitch(_name: string): boolean {
      return false
    },
    getSwitchValue(_name: string): string {
      return ''
    }
  }

  getVersion(): string {
    return this.version
  }

  getAppPath(): string {
    return APP_ROOT
  }

  getPath(name: string): string {
    switch (name) {
      case 'userData':
        return USER_DATA
      case 'logs':
        return path.join(USER_DATA, 'log')
      case 'home':
        return os.homedir()
      case 'temp':
        return os.tmpdir()
      case 'appData':
        return path.join(os.homedir(), '.config')
      case 'exe':
        return process.execPath
      default:
        return path.join(USER_DATA, name)
    }
  }

  setAppUserModelId(_id: string): void {}
  setLoginItemSettings(_settings: unknown): void {}
  requestSingleInstanceLock(): boolean {
    return true
  }
  releaseSingleInstanceLock(): void {}
  whenReady(): Promise<void> {
    return Promise.resolve()
  }
  focus(_options?: unknown): void {}
  hide(): void {}
  show(): void {}

  relaunch(_options?: unknown): void {
    try {
      spawn(process.execPath, process.argv.slice(1), {
        detached: true,
        stdio: 'ignore',
        env: process.env
      }).unref()
    } catch {
      // relaunch is best-effort
    }
  }

  quit(): void {
    if (this.isQuitting) return
    this.isQuitting = true
    const event = {
      preventDefault: (): void => {
        ;(event as { defaultPrevented: boolean }).defaultPrevented = true
      },
      defaultPrevented: false
    }
    this.emit('before-quit', event)
    if (event.defaultPrevented) return
    process.exit(0)
  }

  exit(code = 0): void {
    process.exit(code)
  }
}

export const app = new App()

type IpcHandler = (event: unknown, ...args: unknown[]) => unknown

export const ipcMain = {
  handle(channel: string, handler: IpcHandler): void {
    invokeHandlers.set(channel, handler)
  },
  handleOnce(channel: string, handler: IpcHandler): void {
    invokeHandlers.set(channel, handler)
  },
  removeHandler(channel: string): void {
    invokeHandlers.delete(channel)
  },
  on(channel: string, listener: IpcHandler): void {
    let set = eventListeners.get(channel)
    if (!set) {
      set = new Set()
      eventListeners.set(channel, set)
    }
    set.add(listener)
  },
  once(channel: string, listener: IpcHandler): void {
    const wrapped: IpcHandler = (event, ...args) => {
      ipcMain.removeListener(channel, wrapped as unknown as (...a: unknown[]) => void)
      listener(event, ...args)
    }
    ipcMain.on(channel, wrapped)
  },
  removeListener(channel: string, listener: (...a: unknown[]) => void): void {
    eventListeners.get(channel)?.delete(listener as unknown as IpcHandler)
  },
  removeAllListeners(channel?: string): void {
    removeAllListeners(channel)
  },
  listeners(channel: string): Array<(...a: unknown[]) => void> {
    return [...(eventListeners.get(channel) ?? [])] as unknown as Array<(...a: unknown[]) => void>
  }
}

export class BrowserWindow extends EventEmitter {
  static readonly windows = new Set<BrowserWindow>()
  readonly webContents = mainUiWebContents as unknown as UiWebContents
  private destroyed = false

  constructor(_options?: unknown) {
    super()
    BrowserWindow.windows.add(this)
  }

  static getAllWindows(): BrowserWindow[] {
    return [...BrowserWindow.windows]
  }

  static fromWebContents(_wc: unknown): BrowserWindow | null {
    return null
  }

  static getFocusedWindow(): BrowserWindow | null {
    return null
  }

  isDestroyed(): boolean {
    return this.destroyed
  }
  isMinimized(): boolean {
    return false
  }
  isFullScreen(): boolean {
    return false
  }
  isKiosk(): boolean {
    return false
  }
  show(): void {}
  hide(): void {}
  focus(): void {}
  restore(): void {}
  destroy(): void {
    this.destroyed = true
    BrowserWindow.windows.delete(this)
  }
  close(): void {
    this.destroy()
  }
  setFullScreen(_value: boolean): void {}
  setKiosk(_value: boolean): void {}
  setContentSize(_w: number, _h: number): void {}
  setBounds(_b: unknown): void {}
  getBounds(): { x: number; y: number; width: number; height: number } {
    return { x: 0, y: 0, width: 1024, height: 600 }
  }
  getContentSize(): [number, number] {
    return [1024, 600]
  }
  getPosition(): [number, number] {
    return [0, 0]
  }
  setPosition(_x: number, _y: number): void {}
}

export const webContents = {
  fromId(id: number): unknown {
    return id === mainUiWebContents.id ? mainUiWebContents : undefined
  },
  getAllWebContents(): unknown[] {
    return [mainUiWebContents]
  }
}

interface DisplayInfo {
  id: number
  bounds: { x: number; y: number; width: number; height: number }
  workAreaSize: { width: number; height: number }
  scaleFactor: number
}

export const screen = {
  getPrimaryDisplay(): DisplayInfo {
    return {
      id: 1,
      bounds: { x: 0, y: 0, width: 1024, height: 600 },
      workAreaSize: { width: 1024, height: 600 },
      scaleFactor: 1
    }
  },
  getDisplayMatching(_rect: unknown): DisplayInfo {
    return screen.getPrimaryDisplay()
  },
  getAllDisplays(): DisplayInfo[] {
    return [screen.getPrimaryDisplay()]
  },
  on(_event: string, _listener: (...args: unknown[]) => void): void {}
}

export const dialog = {
  async showMessageBox(_win: unknown, _options?: unknown): Promise<{ response: number }> {
    return { response: 0 }
  },
  showMessageBoxSync(_win: unknown, _options?: unknown): number {
    return 0
  },
  async showOpenDialog(_win?: unknown, _options?: unknown): Promise<{
    canceled: boolean
    filePaths: string[]
  }> {
    return { canceled: true, filePaths: [] }
  },
  async showSaveDialog(_win?: unknown, _options?: unknown): Promise<{
    canceled: boolean
    filePath: string
  }> {
    return { canceled: true, filePath: '' }
  },
  showErrorBox(_title: string, _content: string): void {}
}

export const shell = {
  async openExternal(url: string): Promise<void> {
    try {
      spawn('xdg-open', [url], { detached: true, stdio: 'ignore' }).unref()
    } catch {
      // no desktop session available
    }
  },
  async openPath(p: string): Promise<string> {
    try {
      spawn('xdg-open', [p], { detached: true, stdio: 'ignore' }).unref()
      return ''
    } catch (e) {
      return String(e)
    }
  },
  showItemInFolder(_p: string): void {},
  beep(): void {}
}

const defaultSession = { webRequest: { onBeforeRequest: (..._a: unknown[]): void => {} } }

export const session = {
  defaultSession,
  fromPartition(_partition: string): typeof defaultSession {
    return defaultSession
  }
}

export const protocol = {
  registerSchemesAsPrivileged(_schemes: unknown[]): void {},
  handle(_scheme: string, _handler: unknown): void {},
  unhandle(_scheme: string): void {},
  isProtocolHandled(_scheme: string): boolean {
    return false
  }
}

export const net = {
  fetch: (...args: Parameters<typeof fetch>): Promise<Response> => fetch(...args)
}

interface FakeNativeImage {
  isEmpty(): boolean
  getSize(): { width: number; height: number }
  toPNG(): Buffer
  resize(_options: unknown): FakeNativeImage
}

function emptyImage(): FakeNativeImage {
  const image: FakeNativeImage = {
    isEmpty: () => true,
    getSize: () => ({ width: 0, height: 0 }),
    toPNG: () => Buffer.alloc(0),
    resize: () => image
  }
  return image
}

export const nativeImage = {
  createFromPath(_p: string): FakeNativeImage {
    return emptyImage()
  },
  createFromBuffer(_b: Buffer, _scale?: number): FakeNativeImage {
    return emptyImage()
  },
  createEmpty(): FakeNativeImage {
    return emptyImage()
  }
}

class PowerMonitor extends EventEmitter {}

export const powerMonitor = new PowerMonitor()
