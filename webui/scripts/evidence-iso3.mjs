// 隔离实例：完整校准 job 从启动到 prepared（合成状态机约 1.5–3 分钟），随后 apply 并抓回执。
import { chromium } from 'playwright'
import { writeFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'
const BASE = process.env.KHAEN_BASE_URL || 'http://192.168.63.222'
const PASSWORD = process.env.KHAEN_PASSWORD || ''
const OUT = process.env.KHAEN_EVIDENCE_DIR || './artifacts'
mkdirSync(OUT, { recursive: true })
const summary = { startedAt: new Date().toISOString(), steps: [], errors: [] }
const log = (line) => { summary.steps.push(line); console.log(String(line)) }
const browser = await chromium.launch({ headless: true })
const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'zh-CN' })
const page = await context.newPage()
page.on('pageerror', (error) => { summary.errors.push(String(error)) })
const shoot = async (name, target = page) => { await target.screenshot({ path: join(OUT, 'iso-' + name + '.png'), animations: 'disabled' }) }
const ackText = async () => (await page.waitForTimeout(400), page.locator('.panel', { hasText: '操作回执' }).first().innerText().catch(() => ''))
try {
  await page.goto(BASE + '/login', { waitUntil: 'domcontentloaded' })
  await page.waitForSelector('h1.login-title')
  await page.locator('input[type=password]').first().fill(PASSWORD)
  await Promise.all([page.waitForURL((url) => !url.pathname.startsWith('/login')), page.getByRole('button', { name: '登录' }).click()])
  await page.waitForTimeout(1500)
  await page.goto(BASE + '/calibration', { waitUntil: 'domcontentloaded' })
  await page.waitForTimeout(1500)

  // 清理非终态 job
  const terminal = ['complete', 'failed', 'cancelled', 'rolled_back']
  summary.cleanup = await page.evaluate(async (terminalStates) => {
    const csrf = (await (await fetch('/api/auth/session', { credentials: 'same-origin' })).json()).csrf_token
    const list = await (await fetch('/api/calibration/jobs', { credentials: 'same-origin' })).json()
    const results = []
    for (const job of (list.jobs || []).filter((item) => !terminalStates.includes(item.state))) {
      const response = await fetch('/api/calibration/jobs/' + encodeURIComponent(job.job_id) + '/cancel', {
        method: 'POST', credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json', 'X-KHAEN-CSRF': csrf }, body: '{}'
      })
      results.push({ job_id: job.job_id, state: job.state, http: response.status })
      await new Promise((resolve) => setTimeout(resolve, 1500))
    }
    return results
  }, terminal)
  log('cleanup: ' + JSON.stringify(summary.cleanup))
  await page.waitForTimeout(1500)

  await page.getByRole('button', { name: /启动完整校准/ }).first().click()
  await page.waitForTimeout(2500)
  summary.startAck = await ackText()
  log('start ack: ' + summary.startAck.replace(/\n/g, ' | ').slice(0, 220))

  let preparedRow = null
  let rowIndex = -1
  for (let attempt = 0; attempt < 75; attempt += 1) {
    const rows = page.locator('.data-table tbody tr')
    const count = await rows.count()
    for (let i = 0; i < count; i += 1) {
      const text = await rows.nth(i).innerText().catch(() => '')
      if (text.includes('prepared')) { preparedRow = rows.nth(i); rowIndex = i; break }
    }
    if (preparedRow) { break }
    await page.waitForTimeout(4000)
  }
  summary.reachedPrepared = Boolean(preparedRow)
  summary.preparedSeconds = (Date.now() - new Date(summary.startedAt).getTime()) / 1000
  await shoot('desktop-1440x900-12-calibration-prepared')
  if (!preparedRow) {
    summary.jobTable = await page.locator('.data-table tbody').innerText().catch(() => '')
    log('prepared NOT reached within 300s')
  } else {
    summary.preparedRowText = (await preparedRow.innerText()).replace(/\n/g, ' | ')
    log('prepared row: ' + summary.preparedRowText.slice(0, 160))
    await preparedRow.locator('a.mono').click().catch(() => {})
    await page.waitForTimeout(1200)
    await shoot('desktop-1440x900-13-calibration-timeline-prepared')
    summary.preparedDetail = await page.locator('.el-dialog').first().innerText().catch(() => '')
    await page.keyboard.press('Escape')
    await page.waitForTimeout(700)
    await preparedRow.getByRole('button', { name: '应用' }).click()
    await page.waitForTimeout(3000)
    summary.applyAck = await ackText()
    await shoot('desktop-1440x900-14-calibration-apply-ack')
    log('apply ack: ' + summary.applyAck.replace(/\n/g, ' | ').slice(0, 260))
  }
} catch (error) {
  summary.errors.push(String(error))
  log('[fatal] ' + String(error))
} finally {
  await browser.close()
  writeFileSync(join(OUT, 'iso3-run-summary.json'), JSON.stringify(summary, null, 2))
  console.log(JSON.stringify({ errors: summary.errors, reachedPrepared: summary.reachedPrepared, applyAck: String(summary.applyAck || '').replace(/\n/g, ' | ').slice(0, 320) }, null, 2))
}