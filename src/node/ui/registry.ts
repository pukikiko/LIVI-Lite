// IPC registry shared by the electron shim (registration side) and the UI server
// (dispatch side). Replaces Electron's ipcMain without pulling in Chromium.

export type InvokeHandler = (event: unknown, ...args: unknown[]) => unknown
export type EventListener = (event: unknown, ...args: unknown[]) => void

export const invokeHandlers = new Map<string, InvokeHandler>()
export const eventListeners = new Map<string, Set<EventListener>>()

export function removeAllListeners(channel?: string): void {
  if (channel === undefined) eventListeners.clear()
  else eventListeners.delete(channel)
}
