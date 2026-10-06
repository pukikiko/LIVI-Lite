// LIVI-Lite: headless replacement for the Electron main-window module.
//
// The native Slint UI owns its own Wayland surface; the core has no BrowserWindow.
// `getMainWindow()` stays null so all window-geometry code paths short-circuit,
// while `mainUiWebContents` carries the renderer-style event stream to the UI.

import { mainUiWebContents } from '../../node/ui/hub'
import type { runtimeStateProps, ServicesProps } from '@main/types'
import type { BrowserWindow } from 'electron'

export function createMainWindow(_runtimeState: runtimeStateProps, _services: ServicesProps): void {
  // The UI is a separate native process (livi-ui). Nothing to create here.
}

export function getMainWindow(): BrowserWindow | null {
  return null
}

/** The stable event sink for the native UI (projects the old main window's webContents). */
export function getMainWebContents(): typeof mainUiWebContents {
  return mainUiWebContents
}
