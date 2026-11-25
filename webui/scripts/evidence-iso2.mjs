// 隔离实例：补拍 apply 回执（回执文本已修复 job 字段回退）+ job 列表/详情。
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
  await page.waitForTimeout(2000)

  // 清掉可能仍在 active 的 job：按 API 状态找非终态 job 逐个 cancel
  const terminal = ['complete', 'failed', 'cancelled', 'rolled_back']
  const cleanup = await page.evaluate(async (terminalStates) => {
    const csrf = (await (await fetch('/api/auth/session', { credentials: 'same-origin' })).json()).csrf_token
    const list = await (await fetch('/api/calibration/jobs', { credentials: 'same-origin' })).json()
    const active = (list.jobs || []).filter((job) => !terminalStates.includes(job.state))
    const results = []
    for (const job of active) {
      const response = await fetch('/api/calibration/jobs/' + encodeURIComponent(job.job_id) + '/cancel', {
        method: 'POST', credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json', 'X-KHAEN-CSRF': csrf }, body: '{}'
      })
      results.push({ job_id: job.job_id, state: job.state, cancelHttp: response.status })
      await new Promise((resolve) => setTimeout(resolve, 1200))
    }
    return { active, results }
  }, terminal)
  summary.cleanup = cleanup
  log('cleanup active jobs: ' + JSON.stringify(cleanup.results))
  await page.waitForTimeout(1500)

  // 等任意 job 到 prepared（合成状态机需要数十秒），然后对该行 apply
  async function findPreparedRow() {
    const rows = page.locator('.data-table tbody tr')
    const count = await rows.count()
    for (let i = 0; i < count; i += 1) {
      const text = await rows.nth(i).innerText().catch(() => '')
      if (text.includes('prepared')) { return { rows, index: i } }
    }
    return null
  }
  let preparedRow = null
  for (let attempt = 0; attempt < 60; attempt += 1) {
    preparedRow = await findPreparedRow()
    if (preparedRow) { break }
    await page.waitForTimeout(3000)
  }
  summary.reachedPrepared = Boolean(preparedRow)
  await shoot('desktop-1440x900-09-calibration-list-now')
  if (!preparedRow) {
    log('no prepared job within ~180s；记录现有 job 状态')
    summary.jobTable = await page.locator('.data-table tbody').innerText().catch(() => '')
  } else {
    const row = preparedRow.rows.nth(preparedRow.index)
    summary.preparedRowText = (await row.innerText()).replace(/\n/g, ' | ')
    log('prepared row: ' + summary.preparedRowText.slice(0, 160))
    await row.locator('a.mono').click().catch(() => {})
    await page.waitForTimeout(1200)
    await shoot('desktop-1440x900-10-calibration-timeline-prepared')
    summary.preparedDetail = await page.locator('.el-dialog').first().innerText().catch(() => '')
    await page.keyboard.press('Escape')
    await page.waitForTimeout(600)
    await row.getByRole('button', { name: '应用' }).click()
    await page.waitForTimeout(2500)
    summary.applyAck = await ackText()
    await shoot('desktop-1440x900-11-calibration-apply-ack')
    log('apply ack: ' + summary.applyAck.replace(/\n/g, ' | ').slice(0, 260))
  }

  // 手机端 job 卡片
  const mobileContext = await browser.newContext({ viewport: { width: 390, height: 844 }, locale: 'zh-CN', storageState: await context.storageState() })
  const mobile = await mobileContext.newPage()
  await mobile.goto(BASE + '/calibration', { waitUntil: 'domcontentloaded' })
  await mobile.waitForTimeout(2000)
  await shoot('mobile-390x844-02-calibration-jobs', mobile)
  summary.mobileOverflowX = await mobile.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
  await mobileContext.close()
} catch (error) {
  summary.errors.push(String(error))
  log('[fatal] ' + String(error))
} finally {
  await browser.close()
  writeFileSync(join(OUT, 'iso2-run-summary.json'), JSON.stringify(summary, null, 2))
  console.log(JSON.stringify({ errors: summary.errors, reachedPrepared: summary.reachedPrepared, jobId: summary.jobId, applyAck: String(summary.applyAck || '').replace(/\n/g, ' | ').slice(0, 300), mobileOverflowX: summary.mobileOverflowX }, null, 2))
}