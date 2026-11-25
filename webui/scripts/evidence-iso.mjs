// 隔离实例（N150:18081，同一 daemon 构建 / memory engine）上的写操作证据：
// 校准 job：列表 / 详情时间线 / apply / rollback / retest / 通过 UI 启动新 job；模式切换回执。
// 生产 80 不执行这些写操作（只读截图）。
import { chromium } from 'playwright'
import { writeFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'

const BASE = process.env.KHAEN_BASE_URL || 'http://192.168.63.222'
const PASSWORD = process.env.KHAEN_PASSWORD || ''
const OUT = process.env.KHAEN_EVIDENCE_DIR || './artifacts'
mkdirSync(OUT, { recursive: true })
const summary = { target: 'iso-18081', base: BASE, startedAt: new Date().toISOString(), steps: [], errors: [], shots: [] }
const log = (line) => { summary.steps.push({ at: new Date().toISOString(), line: String(line) }); console.log(String(line)) }

const browser = await chromium.launch({ headless: true })
const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'zh-CN', timezoneId: 'Asia/Shanghai' })
const page = await context.newPage()
page.on('pageerror', (error) => { summary.errors.push(String(error)) })
page.on('response', (response) => {
  if (response.status() >= 400 && !response.url().includes('/api/auth/session')) {
    log('[http ' + response.status() + '] ' + new URL(response.url()).pathname)
  }
})
async function shoot(name, target = page) {
  await target.screenshot({ path: join(OUT, 'iso-' + name + '.png'), animations: 'disabled' })
  summary.shots.push('iso-' + name + '.png')
}
async function ackText() {
  await page.waitForTimeout(400)
  return page.locator('.panel', { hasText: '操作回执' }).first().innerText().catch(() => '')
}
async function clickJobAction(label) {
  const button = page.locator('.data-table tbody tr').first().getByRole('button', { name: label })
  const enabled = await button.isEnabled().catch(() => false)
  if (!enabled) { log(label + ': disabled（按状态机规则禁用）'); return false }
  await button.click()
  await page.waitForTimeout(300)
  return true
}
async function confirmDialog() {
  await page.locator('.el-message-box__btns .el-button--primary').click().catch(() => {})
  await page.waitForTimeout(1500)
}

try {
  await page.goto(BASE + '/login', { waitUntil: 'domcontentloaded' })
  await page.waitForSelector('h1.login-title')
  await page.locator('input[type=password]').first().fill(PASSWORD)
  await Promise.all([page.waitForURL((url) => !url.pathname.startsWith('/login')), page.getByRole('button', { name: '登录' }).click()])
  await page.waitForTimeout(1500)
  log('logged in: ' + page.url())

  // ---------- 模式切换（T44 状态机；隔离实例 mode_services=simulated） ----------
  await page.goto(BASE + '/system', { waitUntil: 'domcontentloaded' })
  await page.waitForTimeout(1500)
  const targetModeButton = page.getByRole('button', { name: /Target multichannel/ })
  if (await targetModeButton.count()) {
    await targetModeButton.click()
    await page.waitForTimeout(500)
    await confirmDialog()
    await page.locator('.panel', { hasText: '工作模式切换' }).first().scrollIntoViewIfNeeded().catch(() => {})
    await shoot('desktop-1440x900-01-mode-switch')
    summary.modeAck = await page.locator('.panel', { hasText: '工作模式切换' }).first().innerText().catch(() => '')
    log('mode switch ack captured')
    const back = page.getByRole('button', { name: /Toslink 2.0/ }).first()
    if (await back.count()) {
      await back.click()
      await page.waitForTimeout(400)
      await confirmDialog()
    }
  }

  // ---------- 校准 job 列表 ----------
  await page.goto(BASE + '/calibration', { waitUntil: 'domcontentloaded' })
  await page.waitForTimeout(2000)
  await shoot('desktop-1440x900-02-calibration-jobs')
  const jobRows = await page.locator('.data-table tbody tr').count()
  summary.jobRows = jobRows
  log('job rows: ' + jobRows)

  // 详情时间线（只读）
  const firstJobLink = page.locator('.data-table a.mono').first()
  if (await firstJobLink.count()) {
    summary.jobId = (await firstJobLink.innerText()).trim()
    await firstJobLink.click()
    await page.waitForTimeout(1200)
    await shoot('desktop-1440x900-03-calibration-timeline')
    summary.jobDetail = await page.locator('.el-dialog').first().innerText().catch(() => '')
    await page.keyboard.press('Escape')
    await page.waitForTimeout(500)
  }

  // apply（仅当 prepared）
  if (await clickJobAction('应用')) {
    await page.waitForTimeout(1800)
    summary.applyAck = await ackText()
    await shoot('desktop-1440x900-04-calibration-apply')
    log('apply ack: ' + summary.applyAck.slice(0, 120))
  }

  // rollback（apply 成功后 last_good 可用）
  if (await clickJobAction('回滚')) {
    await confirmDialog()
    summary.rollbackAck = await ackText()
    await shoot('desktop-1440x900-05-calibration-rollback')
    log('rollback ack: ' + summary.rollbackAck.slice(0, 120))
  }

  // retest（新 job）
  if (await clickJobAction('复测')) {
    await page.waitForTimeout(2000)
    summary.retestAck = await ackText()
    await shoot('desktop-1440x900-06-calibration-retest')
    log('retest ack: ' + summary.retestAck.slice(0, 120))
  }

  // 通过 UI 启动一个完整校准（验证 adaptive payload：当前部署版拒绝高级字段 → 最小请求重试）
  const startButton = page.getByRole('button', { name: /启动完整校准/ }).first()
  if (await startButton.count()) {
    await startButton.click()
    await page.waitForTimeout(2500)
    summary.startAck = await ackText()
    await shoot('desktop-1440x900-07-calibration-start')
    log('start ack: ' + summary.startAck.slice(0, 160))
  }
  // 取消仍在进行的 job，保持隔离实例干净
  const cancelButton = page.locator('.data-table tbody tr').first().getByRole('button', { name: '取消' })
  if (await cancelButton.isEnabled().catch(() => false)) {
    await cancelButton.click()
    await page.waitForTimeout(2500)
    summary.cancelAck = await ackText()
    await shoot('desktop-1440x900-08-calibration-cancel')
    log('cancel ack: ' + summary.cancelAck.slice(0, 120))
  }

  // ---------- 手机端：job 卡片视图 ----------
  const mobileContext = await browser.newContext({ viewport: { width: 390, height: 844 }, locale: 'zh-CN', storageState: await context.storageState() })
  const mobile = await mobileContext.newPage()
  await mobile.goto(BASE + '/calibration', { waitUntil: 'domcontentloaded' })
  await mobile.waitForTimeout(2000)
  await shoot('mobile-390x844-01-calibration-jobs', mobile)
  summary.mobileOverflowX = await mobile.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
  const cards = await mobile.locator('.job-cards .panel').count()
  summary.mobileJobCards = cards
  log('mobile job cards: ' + cards + ' overflowX=' + summary.mobileOverflowX)
  await mobileContext.close()

  summary.finishedAt = new Date().toISOString()
} catch (error) {
  summary.errors.push(String(error))
  log('[fatal] ' + String(error))
} finally {
  await browser.close()
  writeFileSync(join(OUT, 'iso-run-summary.json'), JSON.stringify(summary, null, 2))
  console.log(JSON.stringify({
    errors: summary.errors,
    jobRows: summary.jobRows,
    jobId: summary.jobId,
    hasApplyAck: Boolean(summary.applyAck),
    hasRollbackAck: Boolean(summary.rollbackAck),
    hasRetestAck: Boolean(summary.retestAck),
    hasStartAck: Boolean(summary.startAck),
    mobileJobCards: summary.mobileJobCards,
    mobileOverflowX: summary.mobileOverflowX
  }, null, 2))
}
