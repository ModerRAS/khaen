// 记录 22:33 外部重启后的生产 daemon 状态（未登录页 / setup_required 警示），作为报告证据。
import { chromium } from 'playwright'
import { writeFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'
const BASE = process.env.KHAEN_BASE_URL || 'http://192.168.63.222'
const OUT = process.env.KHAEN_EVIDENCE_DIR || './artifacts'
mkdirSync(OUT, { recursive: true })
const browser = await chromium.launch({ headless: true })
const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'zh-CN' })
const page = await context.newPage()
await page.goto(BASE + '/login', { waitUntil: 'domcontentloaded' })
await page.waitForSelector('h1.login-title')
await page.waitForTimeout(1200)
await page.screenshot({ path: join(OUT, 'prod-post-restart-1440x900-login-setup-required.png'), animations: 'disabled' })
const authStatus = await page.evaluate(async () => (await fetch('/api/auth/status', { credentials: 'same-origin' })).json())
const health = await page.evaluate(async () => (await fetch('/healthz', { credentials: 'same-origin' })).json())
const probe = await page.evaluate(async () => {
  const result = {}
  for (const path of ['/api/runtime/status', '/api/calibration/jobs']) {
    const res = await fetch(path, { credentials: 'same-origin' })
    result[path] = { status: res.status, body: await res.text() }
  }
  return result
})
const loginText = await page.locator('.login-card').innerText()
await browser.close()
const payload = { capturedAt: new Date().toISOString(), base: BASE, authStatus, health, probe, loginText: loginText.slice(0, 1500) }
writeFileSync(join(OUT, 'prod-post-restart-state.json'), JSON.stringify(payload, null, 2))
console.log(JSON.stringify({ authStatus, health, probe: Object.fromEntries(Object.entries(probe).map(([k, v]) => [k, v.status])) }, null, 2))
