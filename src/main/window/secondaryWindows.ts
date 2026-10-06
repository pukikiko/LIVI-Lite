// LIVI-Lite: secondary screens (dash/aux) are not part of the micro UI yet.
// Kept as a stub so the projection core's imports stay valid; `getSecondaryWindow`
// always returns null and VideoPlaneManager simply builds no planes for them.

import type { Config } from '@shared/types'
import type { runtimeStateProps } from '@main/types'
import type { BrowserWindow } from 'electron'
import { EventEmitter } from 'node:events'

export const secondaryWindowEvents = new EventEmitter()

export type SecondaryWindowRole = 'dash' | 'aux'

export function syncSecondaryWindows(_runtimeState: runtimeStateProps, _prev?: Config): void {}

export function setupSecondaryWindows(_runtimeState: runtimeStateProps): void {}

export function closeAllSecondaryWindows(): void {}

export function getSecondaryWindow(_role: SecondaryWindowRole): BrowserWindow | null {
  return null
}
