// 生产只读 API 快照：登录后用页面上下文 fetch 所有 GET 端点（不执行任何写操作）。
import { chromium } from 'playwright'
import { writeFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'
const BASE = process.env.KHAEN_BASE_URL || 'http://192.168.63.222'
const PASSWORD = process.env.KHAEN_PASSWORD || ''
const OUT = process.env.KHAEN_EVIDENCE_DIR || './artifacts'
mkdirSync(OUT, { recursive: true })
const GETS = [
  '/api/auth/status', '/api/auth/session', '/api/runtime/status', '/api/runtime/config',
  '/api/runtime/volume', '/api/runtime/mute', '/api/runtime/mode', '/api/runtime/target',
  '/api/runtime/coeff', '/api/mode', '/api/mode/script-status', '/api/audio/format',
  '/api/channelmap/input-mode', '/api/calibration/jobs', '/healthz'
]
const browser = await chromium.launch({ headless: true })
const context = await browser.newContext({ viewport: { width: 1280, height: 800 }, locale: 'zh-CN' })
const page = await context.newPage()
await page.goto(BASE + '/login', { waitUntil: 'domcontentloaded' })
await page.waitForSelector('h1.login-title')
await page.locator('input[type=password]').first().fill(PASSWORD)
await Promise.all([page.waitForURL((url) => !url.pathname.startsWith('/login')), page.getByRole('button', { name: '登录' }).click()])
await page.waitForTimeout(1200)
const result = {}
for (const path of GETS) {
  result[path] = await page.evaluate(async (url) => {
    const res = await fetch(url, { credentials: 'same-origin' })
    const text = await res.text()
    let json = null
    try { json = JSON.parse(text) } catch { json = text }
    return { status: res.status, json }
  }, path)
}
await browser.close()
const outPath = join(OUT, 'prod-api-snapshot.json')
writeFileSync(outPath, JSON.stringify(result, null, 2))
console.log('saved ' + outPath)
console.log('status codes: ' + JSON.stringify(Object.fromEntries(Object.entries(result).map(([k, v]) => [k, v.status]))))
const jobs = result['/api/calibration/jobs'].json
console.log('cal jobs: ' + JSON.stringify(jobs && { engine: jobs.engine, synthetic: jobs.synthetic, real_audio: jobs.real_audio, cal_engine: jobs.cal_engine, count: jobs.count }))
const status = result['/api/runtime/status'].json
console.log('runtime: ' + JSON.stringify({ engine: status.engine, synthetic: status.synthetic, actual: status.actual, mode: status.mode, mode_services: status.mode_services, sample_rate: status.sample_rate, chunksize: status.chunksize, volume_db: status.volume_db, mute: status.mute }))
