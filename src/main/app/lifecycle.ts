// LIVI-Lite: headless lifecycle. Same ordered teardown as the Electron build,
// minus windows and dialogs; signals and app.quit() both funnel through here.

import { stopSystemVolumeMonitor } from '@main/services/audio/SystemVolume'
import { stopPhoneSuppression } from '@main/services/gvfsPhoneGuard'
import { runPendingPowerAction } from '@main/services/power/hostPower'
import { releaseWifiApForQuit } from '@main/services/projection/driver/helper/wifiApUnit'
import { runtimeStateProps, ServicesProps } from '@main/types'
import { closeAllSecondaryWindows } from '@main/window/secondaryWindows'
import { app } from 'electron'

export function setupLifecycle(runtimeState: runtimeStateProps, services: ServicesProps): void {
  const { projectionService, telemetrySocket } = services

  let shuttingDown = false

  const shutdown = async (): Promise<void> => {
    if (shuttingDown) return
    shuttingDown = true
    runtimeState.isQuitting = true

    const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms))

    const withTimeout = async <T>(label: string, p: Promise<T>, ms: number): Promise<T | undefined> => {
      let t: NodeJS.Timeout | undefined
      try {
        return (await Promise.race([
          p,
          new Promise<T | undefined>((resolve) => {
            t = setTimeout(() => {
              console.warn(`[core] shutdown timeout: ${label} after ${ms}ms`)
              resolve(undefined)
            }, ms)
          })
        ])) as T | undefined
      } finally {
        clearTimeout(t)
      }
    }

    const measureStep = async (label: string, fn: () => Promise<unknown>): Promise<void> => {
      const t0 = Date.now()
      console.log(`[core] shutdown step:start ${label}`)
      try {
        await fn()
      } finally {
        console.log(`[core] shutdown step:done ${label} (${Date.now() - t0}ms)`)
      }
    }

    try {
      closeAllSecondaryWindows()
      projectionService.beginShutdown()
      stopPhoneSuppression()
      stopSystemVolumeMonitor()

      await measureStep('projection.shutdownWirelessSessions()', () =>
        withTimeout(
          'projection.shutdownWirelessSessions()',
          projectionService.shutdownWirelessSessions(),
          8000
        )
      )

      await measureStep('projection.disconnectPhone()', async () => {
        await withTimeout('projection.disconnectPhone()', projectionService.disconnectPhone(), 800)
        await sleep(75)
      })

      await measureStep('projection.disconnectHostBtPhones()', () =>
        withTimeout(
          'projection.disconnectHostBtPhones()',
          projectionService.disconnectHostBtPhones(),
          1500
        )
      )

      await measureStep('telemetrySocket.disconnect()', () =>
        withTimeout(
          'telemetrySocket.disconnect()',
          telemetrySocket?.disconnect?.() ?? Promise.resolve(),
          300
        )
      )

      await measureStep('projection.stopHelper()', () =>
        withTimeout('projection.stopHelper()', projectionService.stopHelper(), 2500)
      )

      await measureStep('projection.stop()', () =>
        withTimeout('projection.stop()', projectionService.stop(), 6000)
      )

      await measureStep('wifiAp.release()', () =>
        withTimeout('wifiAp.release()', releaseWifiApForQuit(runtimeState.config), 2000)
      )
    } catch (err) {
      console.warn('[core] Error while quitting:', err)
    } finally {
      runPendingPowerAction()
      setImmediate(() => process.exit(0))
    }
  }

  app.on('before-quit', (event: { preventDefault: () => void }) => {
    if (runtimeState.isQuitting && shuttingDown) return
    event.preventDefault()
    void shutdown()
  })

  const onSignal = (signal: string) => (): void => {
    console.log(`[core] received ${signal}, shutting down`)
    app.quit()
  }
  process.on('SIGTERM', onSignal('SIGTERM'))
  process.on('SIGINT', onSignal('SIGINT'))
}
