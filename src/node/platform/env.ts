// LIVI-Lite: polyfills for Electron-provided globals that the service layer
// reads directly (not through the `electron` import alias).

import path from 'node:path'

const root = process.env.LIVI_APP_PATH ?? process.cwd()
const proc = process as NodeJS.Process & { resourcesPath?: string; defaultApp?: boolean }

if (typeof proc.resourcesPath !== 'string' || proc.resourcesPath.length === 0) {
  proc.resourcesPath = process.env.LIVI_RESOURCES ?? root
}
if (proc.defaultApp === undefined) proc.defaultApp = true

/** Install root the native assets are resolved against. */
export const APP_ROOT = root
export const RESOURCES_ROOT = path.resolve(proc.resourcesPath)
