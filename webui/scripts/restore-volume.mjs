// 一次性恢复：把生产音量还原为实测初始值 -12.0 dB（本次测试写入的 -18 不是用户原值）。
import { chromium } from 'playwright'
const BASE = process.env.KHAEN_BASE_URL || 'http://192.168.63.222'
const PASSWORD = process.env.KHAEN_PASSWORD || ''
const TARGET_DB = Number(process.env.KHAEN_RESTORE_DB || '-12')
const browser = await chromium.launch({ headless: true })
const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
const page = await context.newPage()
await page.goto(BASE + '/login', { waitUntil: 'domcontentloaded' })
await page.waitForSelector('h1.login-title')
await page.locator('input[type=password]').first().fill(PASSWORD)
await Promise.all([page.waitForURL((url) => !url.pathname.startsWith('/login')), page.getByRole('button', { name: '登录' }).click()])
await page.waitForTimeout(1000)
const result = await page.evaluate(async (db) => {
  const csrf = (await (await fetch('/api/auth/session', { credentials: 'same-origin' })).json()).csrf_token
  const response = await fetch('/api/runtime/volume', {
    method: 'PUT',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json', 'X-KHAEN-CSRF': csrf },
    body: JSON.stringify({ db })
  })
  const body = await response.json()
  const status = await (await fetch('/api/runtime/status', { credentials: 'same-origin' })).json()
  return { status: response.status, body: { volume_db: body.volume_db, mute: body.mute, synthetic: body.synthetic, actual: body.actual, engine: body.engine }, after: { volume_db: status.volume_db, mute: status.mute } }
}, TARGET_DB)
await browser.close()
console.log(JSON.stringify(result, null, 2))