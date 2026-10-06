// Regenerates ui/icons.slint from the @mui/icons-material package that the
// Electron renderer already depends on, so the native UI ships the exact same
// Material outlined glyphs.
//
// Usage: node native/livi-ui/tools/gen-icons.mjs > native/livi-ui/ui/icons.slint

import fs from 'node:fs'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..')

function findIconsDir() {
  const pnpm = path.join(root, 'node_modules', '.pnpm')
  for (const entry of fs.readdirSync(pnpm)) {
    if (entry.startsWith('@mui+icons-material@')) {
      const dir = path.join(pnpm, entry, 'node_modules', '@mui', 'icons-material')
      if (fs.existsSync(dir)) return dir
    }
  }
  throw new Error('@mui/icons-material not found under node_modules/.pnpm')
}

const MUI = findIconsDir()

// Slint component name -> MUI icon file.
const ICONS = {
  IconProjection: 'CropPortraitOutlined',
  IconDevices: 'PhoneIphoneOutlined',
  IconMedia: 'PlayCircleOutlined',
  IconSettings: 'SettingsOutlined',
  IconCar: 'DirectionsCarOutlined',
  IconAutoConnect: 'AutorenewOutlined',
  IconAndroid: 'AndroidOutlined',
  IconKey: 'KeyOutlined',
  IconAntenna: 'SettingsInputAntennaOutlined',
  IconPublic: 'PublicOutlined',
  IconRouter: 'RouterOutlined',
  IconBluetooth: 'BluetoothOutlined',
  IconDarkMode: 'DarkModeOutlined',
  IconBrightness: 'Brightness6Outlined',
  IconVolume: 'VolumeUpOutlined',
  IconMusicNote: 'MusicNoteOutlined',
  IconNavigation: 'NavigationOutlined',
  IconVoice: 'RecordVoiceOverOutlined',
  IconCall: 'CallOutlined',
  IconSpeaker: 'SpeakerOutlined',
  IconMic: 'MicNoneOutlined',
  IconAspectRatio: 'AspectRatioOutlined',
  IconTranslate: 'TranslateOutlined',
  IconBug: 'BugReportOutlined',
  IconInfo: 'InfoOutlined',
  IconRestart: 'RestartAltOutlined',
  IconPower: 'PowerSettingsNewOutlined',
  IconUpdate: 'SystemUpdateAltOutlined',
  IconSpeed: 'SpeedOutlined',
  IconBack: 'ArrowBackIosOutlined',
  IconChevron: 'ArrowForwardIosOutlined',
  IconClose: 'Close',
  IconRefresh: 'RefreshOutlined',
  IconCable: 'CableOutlined',
  IconWifi: 'WifiOutlined',
  IconWifiOff: 'WifiOffOutlined',
  IconBolt: 'BoltOutlined',
  IconPlay: 'PlayArrow',
  IconPause: 'Pause',
  IconNext: 'SkipNext',
  IconPrev: 'SkipPrevious',
  IconLink: 'LinkOutlined',
  IconSmartphone: 'SmartphoneOutlined',
  IconArrowDown: 'KeyboardArrowDown',
  IconCheck: 'Check',
  IconCluster: 'SpeedOutlined',
  IconWidgets: 'WidgetsOutlined',
  IconTune: 'TuneOutlined',
  IconMonitor: 'MonitorOutlined',
  IconMemory: 'MemoryOutlined',
  IconContrast: 'ContrastOutlined'
}

function extract(file) {
  const src = fs.readFileSync(file, 'utf8')
  const paths = []
  const pathRe = /jsx\)?\("path",\s*\{([^}]*)\}/g
  let match
  while ((match = pathRe.exec(src))) {
    const body = match[1]
    const d = body.match(/d:\s*"([^"]+)"/)
    if (!d) continue
    paths.push({ d: d[1], evenodd: /fillRule:\s*"evenodd"/.test(body) })
  }
  const circleRe = /jsx\)?\("circle",\s*\{([^}]*)\}/g
  while ((match = circleRe.exec(src))) {
    const body = match[1]
    const num = (key) => Number(body.match(new RegExp(`${key}:\\s*"([-\\d.]+)"`))[1])
    const cx = num('cx')
    const cy = num('cy')
    const r = num('r')
    paths.push({
      d: `M ${cx - r} ${cy} a ${r} ${r} 0 1 0 ${2 * r} 0 a ${r} ${r} 0 1 0 ${-2 * r} 0`,
      evenodd: false
    })
  }
  return paths
}

let out = `// Generated from @mui/icons-material by tools/gen-icons.mjs.
// Do not edit by hand; re-run the generator instead.
import { Theme } from "theme.slint";

`
for (const [name, file] of Object.entries(ICONS)) {
  const paths = extract(path.join(MUI, `${file}.js`))
  if (!paths.length) throw new Error(`no path data for ${file}`)
  const evenodd = paths.some((p) => p.evenodd)
  const commands = paths.map((p) => p.d).join(' ')
  out += `export component ${name} inherits Path {
    in property <color> color: Theme.text-primary;
    width: 24px;
    height: 24px;
    viewbox-width: 24;
    viewbox-height: 24;
    fill: color;
${evenodd ? '    fill-rule: evenodd;\n' : ''}    commands: "${commands}";
}
`
}

process.stdout.write(out)
