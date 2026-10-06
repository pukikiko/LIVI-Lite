// LIVI-Lite headless core.
//
// Drop-in replacement for src/main/index.ts with Chromium removed: no window, no
// renderer, no preload. The service layer is reused verbatim through the electron
// shim (see src/node/platform/electron-shim.ts). Settings, projection, telemetry
// and device state reach the native Slint UI over a Unix socket (UiServer).
//
// Boot order is optimized for time-to-phone: config -> UI socket -> services ->
// helperd spawn -> auto-connect, with privileged setup and packaging checks
// pushed to the background.

import './platform/env'
import '@main/logTimestamps'

import { installMainProcessErrorHandlers } from '@main/app/errorHandler'
import { setupLifecycle } from '@main/app/lifecycle'
import { loadConfig } from '@main/config/loadConfig'
import { setDebugLogging } from '@main/constants'
import { registerIpc } from '@main/ipc'
import { configEvents, saveSettings } from '@main/ipc/utils'
import {
  setSystemVolume,
  startSystemVolumeMonitor,
  stopSystemVolumeMonitor
} from '@main/services/audio/SystemVolume'
import { ensureWireplumberBtRoles } from '@main/services/audio/wireplumberBtRoles'
import { CarBridgeService } from '@main/services/carBridge/CarBridgeService'
import { checkAndInstallGvfsGuard, startPhoneSuppression } from '@main/services/gvfsPhoneGuard'
import { followAdapterChoice, reconcileDongleAp } from '@main/services/link/dongleAp'
import { startLinkSpeedMonitor } from '@main/services/link/linkSpeed'
import { checkMissingPackages } from '@main/services/packageCheck'
import { checkAndInstallHelperSudoers } from '@main/services/projection/driver/helper/helperSudoers'
import {
  reconcileWifiAp,
  setWifiApReport,
  settleWifiAp
} from '@main/services/projection/driver/helper/wifiApUnit'
import { ProjectionService } from '@main/services/projection/services/ProjectionService'
import { TelemetrySocket } from '@main/services/Socket'
import { setupTelemetry } from '@main/services/telemetry/setupTelemetry'
import { TelemetryStore } from '@main/services/telemetry/TelemetryStore'
import type { runtimeStateProps } from '@main/types'
import { checkAndInstallUdevRule } from '@main/services/usb/udevRule'
import {
  backdropHex,
  setCompositorBackdrop,
  setStreamGamma
} from '@main/services/video/GstVideo'
import type { Config } from '@shared/types'
import { APP_START_TS } from '@main/services/projection/services/constants'
import { app, BrowserWindow } from 'electron'
import type { WebContents } from 'electron'
import net from 'node:net'
import { mainUiWebContents } from './ui/hub'
import { uiServer, uiSocketPath } from './ui/UiServer'

const bootMs = (): number => Date.now() - APP_START_TS

async function isAnotherCoreRunning(): Promise<boolean> {
  return await new Promise<boolean>((resolve) => {
    const socket = net.connect(uiSocketPath())
    const finish = (value: boolean): void => {
      socket.destroy()
      resolve(value)
    }
    socket.once('connect', () => finish(true))
    socket.once('error', () => finish(false))
    setTimeout(() => finish(false), 300).unref?.()
  })
}

async function bootstrapPrivilegedSetup(config: Config): Promise<void> {
  // First-run repairs (sudoers/udev/AP unit/gvfs/package hints). Never blocks
  // projection; install.sh is expected to have done this already on real units.
  const window = new BrowserWindow()
  try {
    if (process.platform === 'linux') {
      await checkAndInstallHelperSudoers(window)
      if (await checkAndInstallUdevRule(window)) {
        console.log('[core] udev rule installed; a restart is required')
        return
      }
      void reconcileWifiAp(config, window).then(() => settleWifiAp(config))
      await checkAndInstallGvfsGuard(window)
      startPhoneSuppression()
      ensureWireplumberBtRoles()
      const { dismissed } = await checkMissingPackages(window, config.dismissedPackages)
      if (dismissed) saveSettings(runtimeStateRef, { dismissedPackages: dismissed })
    }
  } catch (e) {
    console.warn('[core] privileged setup failed (ignored):', (e as Error).message)
  } finally {
    window.destroy()
  }
}

let runtimeStateRef: runtimeStateProps

async function main(): Promise<void> {
  installMainProcessErrorHandlers()

  // If the native UI disappears (crash, compositor restart), the core follows it.
  const parentPid = Number(process.env.LIVI_PARENT_PID)
  if (Number.isFinite(parentPid) && parentPid > 0) {
    setInterval(() => {
      if (process.ppid !== parentPid) {
        console.log('[core] parent UI is gone, shutting down')
        app.quit()
      }
    }, 2000).unref()
  }

  if (await isAnotherCoreRunning()) {
    console.log('[core] another LIVI core is already running, exiting')
    process.exit(0)
  }

  // The UI can connect immediately, before any hardware work starts.
  await uiServer.start()

  const projectionService = new ProjectionService()
  projectionService.attachRenderer(mainUiWebContents as unknown as WebContents)

  const telemetryStore = new TelemetryStore()
  const telemetrySocket = new TelemetrySocket(telemetryStore, 4000)

  const runtimeState: runtimeStateProps = {
    config: loadConfig(),
    telemetrySocket: null,
    isQuitting: false,
    suppressNextFsSync: false,
    wmExitedKiosk: false
  }
  runtimeStateRef = runtimeState
  setDebugLogging(runtimeState.config.debugLogging === true)
  setWifiApReport((patch) => saveSettings(runtimeState, patch))

  const services = { projectionService, telemetrySocket }
  registerIpc(runtimeState, services)

  const linkKeys: (keyof Config)[] = [
    'wifiInterface',
    'wifiDedicatedInterface',
    'wirelessCpEnabled',
    'wirelessAaEnabled',
    'btAdapter',
    'carName',
    'country',
    'wifiChannel',
    'wifiChannelWidth',
    'wifiPassword'
  ]
  const linkSettings = (c: Config): string => linkKeys.map((k) => String(c[k])).join('|')
  let told = linkSettings(runtimeState.config)
  let before = runtimeState.config
  configEvents.on('changed', (next: Config) => {
    const now = linkSettings(next)
    if (now === told) return
    told = now
    followAdapterChoice(before, next)
    before = next
    void reconcileWifiAp(next)
    void reconcileDongleAp(next)
  })
  void reconcileDongleAp(runtimeState.config)
  startLinkSpeedMonitor()

  // On a fresh UI connection push the current settings snapshot so the UI
  // never has to race its first frame against config loading.
  uiServer.onClientConnected(() => {
    mainUiWebContents.send('settings', runtimeState.config)
    console.log(`[Perf] CoreStart→UIReady: ${bootMs()} ms`)
  })

  const carBridge = new CarBridgeService(runtimeState.config.language)
  carBridge.start()
  projectionService.onProjectionEvent((payload) => carBridge.handleEvent(payload))
  carBridge.onKey = (command) => projectionService.dispatchRemoteInput(command)
  carBridge.onTelemetry = (payload) => telemetryStore.merge(payload)
  carBridge.setBrightness(runtimeState.config.displayBrightness * 100)
  configEvents.on('changed', (next: Config) =>
    carBridge.setBrightness(next.displayBrightness * 100)
  )

  let brightnessAuto = runtimeState.config.displayBrightnessAuto
  configEvents.on('changed', (next: Config) => {
    brightnessAuto = next.displayBrightnessAuto
  })
  telemetryStore.on('change', (patch: { dimmerPct?: unknown }) => {
    if (!brightnessAuto || typeof patch.dimmerPct !== 'number') return
    const next = Math.min(1, Math.max(0, patch.dimmerPct / 100))
    if (Math.abs(next - runtimeState.config.displayBrightness) < 0.005) return
    saveSettings(runtimeState, { displayBrightness: next })
  })

  runtimeState.telemetrySocket = telemetrySocket

  // Backdrop + calibration are a single compositor line each.
  const applyBackdrop = (cfg: Config): void => {
    setCompositorBackdrop(
      backdropHex(cfg.darkMode, cfg.backgroundColorDark, cfg.backgroundColorLight)
    )
  }
  applyBackdrop(runtimeState.config)
  configEvents.on('changed', (next: Config) => applyBackdrop(next))

  const applyGamma = (cfg: Config): void => {
    setStreamGamma(
      cfg.displayGamma,
      cfg.displayContrast,
      cfg.displayColorR,
      cfg.displayColorG,
      cfg.displayColorB
    )
  }
  applyGamma(runtimeState.config)
  configEvents.on('changed', (next: Config) => applyGamma(next))

  let appliedHuVolume: number | null = null
  const applyHuVolume = (cfg: Config): void => {
    if (cfg.huVolumeLinkSystem !== true) {
      appliedHuVolume = null
      stopSystemVolumeMonitor()
      return
    }
    startSystemVolumeMonitor(
      () => runtimeState.config.audioOutputDevice,
      (level) => {
        if (runtimeState.config.huVolumeLinkSystem !== true) return
        if (Math.abs(level - runtimeState.config.huVolume) < 0.005) return
        appliedHuVolume = level
        saveSettings(runtimeState, { huVolume: level })
      },
      () => void setSystemVolume(runtimeState.config.huVolume, runtimeState.config.audioOutputDevice)
    )
    if (appliedHuVolume !== null && Math.abs(cfg.huVolume - appliedHuVolume) < 0.005) return
    appliedHuVolume = cfg.huVolume
    void setSystemVolume(cfg.huVolume, cfg.audioOutputDevice)
  }
  applyHuVolume(runtimeState.config)
  configEvents.on('changed', (next: Config) => applyHuVolume(next))

  setupTelemetry({
    store: telemetryStore,
    projectionService,
    initialConfig: runtimeState.config
  })
  setupLifecycle(runtimeState, services)

  // ── Phone path: start now, everything else later ─────────────────────────
  projectionService.applyConfigPatch(runtimeState.config)
  console.log(`[Perf] CoreStart→HelperRequest: ${bootMs()} ms`)

  const autoStart = projectionService.autoStartIfNeeded().catch((e: unknown) => {
    console.warn('[core] autoStart failed:', e)
  })

  // Repairs and diagnostics run after the phone path is in motion.
  if (process.platform === 'linux' && (app.isPackaged || process.env.LIVI_AUTO_SETUP === '1')) {
    void bootstrapPrivilegedSetup(runtimeState.config)
  } else {
    console.log('[core] skipping privileged setup (dev mode; set LIVI_AUTO_SETUP=1 to enable)')
  }

  await autoStart
  console.log(`[Perf] CoreStart→AutoStart: ${bootMs()} ms`)
}

main().catch((e: unknown) => {
  console.error('[core] fatal:', e)
  process.exit(1)
})
