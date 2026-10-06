// LIVI-Lite: every renderer broadcast goes to the native UI clients through the
// stable proxy. The service layer keeps calling these helpers unchanged.

import { mainUiWebContents } from '../../node/ui/hub'
import type { WebContents } from 'electron'

export function getAllRendererWebContents(): WebContents[] {
  return [mainUiWebContents as unknown as WebContents]
}

export function getSecondaryRendererWebContents(): WebContents[] {
  return []
}

export function broadcastToRenderers(channel: string, ...args: unknown[]): void {
  mainUiWebContents.send(channel, ...args)
}

export function broadcastToSecondaryRenderers(_channel: string, ..._args: unknown[]): void {
  // No secondary native windows (yet).
}
