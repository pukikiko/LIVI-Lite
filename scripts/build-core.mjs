#!/usr/bin/env node
// Bundles the headless core (src/node/index.ts) into out/core/livi-core.cjs.
// Electron is not a dependency: the `electron` module is aliased to the local
// shim, and only the native addon packages stay external.

import { build } from 'esbuild'
import fs from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')

// Vite-style `?raw` imports (base64 car play icons) inline the file as a string.
const rawPlugin = {
  name: 'raw',
  setup(build) {
    build.onResolve({ filter: /\?raw$/ }, (args) => ({
      path: path.resolve(args.resolveDir, args.path.replace(/\?raw$/, '')),
      namespace: 'raw'
    }))
    build.onLoad({ filter: /.*/, namespace: 'raw' }, async (args) => ({
      contents: `export default ${JSON.stringify(await fs.readFile(args.path, 'utf8'))}`,
      loader: 'js'
    }))
  }
}

const alias = {
  electron: path.join(root, 'src/node/platform/electron-shim.ts'),
  '@main': path.join(root, 'src/main'),
  '@shared': path.join(root, 'src/main/shared'),
  '@projection': path.join(root, 'src/main/services/projection')
}

await build({
  absWorkingDir: root,
  entryPoints: ['src/node/index.ts'],
  bundle: true,
  platform: 'node',
  format: 'cjs',
  target: 'node24',
  outfile: 'out/core/livi-core.cjs',
  sourcemap: true,
  metafile: true,
  logLevel: 'info',
  alias,
  plugins: [rawPlugin],
  // Native N-API addons are loaded at runtime, never bundled.
  external: ['livi-crypto', 'livi-gst-video'],
  banner: {
    js: "process.resourcesPath ??= process.env.LIVI_RESOURCES ?? process.env.LIVI_APP_PATH ?? process.cwd();"
  }
})

console.log('[build-core] wrote out/core/livi-core.cjs')
