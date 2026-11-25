// 证据采集：登录（生产 8081 / 隔离 8082）+ 三档视口截图 + 布局/触控指标 + 控制台错误。
// 密码只从环境变量读入（不落盘、不打印）。
import { chromium } from 'playwright'
import { mkdirSync, writeFileSync, appendFileSync } from 'node:fs'
import { join } from 'node:path'

const TARGET = process.env.KHAEN_EVIDENCE_TARGET || 'prod'
const BASE = process.env.KHAEN_BASE_URL || 'http://192.168.63.222'
const PASSWORD = process.env.KHAEN_PASSWORD || ''
const OUT = process.env.KHAEN_EVIDENCE_DIR || './artifacts'
const prefix = TARGET === 'prod' ? 'prod' : TARGET

mkdirSync(OUT, { recursive: true })
const logPath = join(OUT, prefix + '-console-log.txt')
writeFileSync(logPath, '')
const summary = { target: TARGET, base: BASE, startedAt: new Date().toISOString(), viewports: {}, pages: [], navChecks: [], errors: [] }
const httpErrorCounts = {}

function log(line) {
  const stamp = new Date().toISOString()
  appendFileSync(logPath, '[' + stamp + '] ' + line + '\n')
}

async function shoot(page, name, options = {}) {
  const path = join(OUT, name + '.png')
  await page.screenshot({ path, fullPage: options.fullPage === true, animations: 'disabled' })
  log('screenshot ' + path + ' fullPage=' + (options.fullPage === true))
  return path
}

async function collectMetrics(page) {
  return page.evaluate(() => {
    const doc = document.documentElement
    const rect = (el) => el.getBoundingClientRect()
    const resolveTarget = (el) => {
      if (el.tagName === 'INPUT' || el.tagName === 'SELECT' || el.tagName === 'TEXTAREA') {
        return el.closest('.el-input__wrapper, .el-select__wrapper, .el-input-number, .el-textarea') || el
      }
      return el
    }
    const raw = Array.from(document.querySelectorAll('button, a[href], input, select, textarea, .el-switch, .el-slider__button-wrapper'))
      .map(resolveTarget)
    const interactive = Array.from(new Set(raw)).filter((el) => { const r = rect(el); return r.width > 0 && r.height > 0 })
    const smallTargets = interactive
      .filter((el) => { const r = rect(el); return r.height < 43.5 })
      .map((el) => ({ tag: el.tagName, cls: String(el.className || '').slice(0, 70), w: Math.round(rect(el).width), h: Math.round(rect(el).height) }))
    const header = document.querySelector('.app-header')
    const bottom = document.querySelector('.app-bottom')
    const aside = document.querySelector('.app-aside')
    const main = document.querySelector('.app-main')
    return {
      innerWidth: window.innerWidth,
      innerHeight: window.innerHeight,
      scrollWidth: doc.scrollWidth,
      overflowX: doc.scrollWidth - window.innerWidth,
      scrollHeight: doc.scrollHeight,
      bottomNavVisible: !!bottom && getComputedStyle(bottom).display !== 'none',
      asideVisible: !!aside && getComputedStyle(aside).display !== 'none',
      headerHeight: header ? Math.round(rect(header).height) : 0,
      bottomHeight: bottom && getComputedStyle(bottom).display !== 'none' ? Math.round(rect(bottom).height) : 0,
      mainTop: main ? Math.round(rect(main).top) : 0,
      smallTargetCount: smallTargets.length,
      smallTargets: smallTargets.slice(0, 12),
      title: document.title,
      path: location.pathname
    }
  })
}

const browser = await chromium.launch({ headless: true })
const consoleLines = []
const pageErrors = []

async function newPage(viewport, storageState) {
  const context = await browser.newContext({
    viewport,
    deviceScaleFactor: 1,
    storageState: storageState || undefined,
    locale: 'zh-CN',
    timezoneId: 'Asia/Shanghai'
  })
  const page = await context.newPage()
  page.on('console', (msg) => {
    const line = msg.type() + ': ' + msg.text()
    consoleLines.push(line)
    if (msg.type() === 'error') { log('[console.error] ' + msg.text()) }
  })
  page.on('pageerror', (error) => {
    pageErrors.push(String(error))
    log('[pageerror] ' + String(error))
  })
  page.on('response', (response) => {
    if (response.status() >= 400) {
      const key = response.status() + ' ' + new URL(response.url()).pathname
      httpErrorCounts[key] = (httpErrorCounts[key] || 0) + 1
      log('[http ' + response.status() + '] ' + response.url())
    }
  })
  return { context, page }
}

async function login(page) {
  await page.goto(BASE + '/login', { waitUntil: 'domcontentloaded', timeout: 30000 })
  await page.waitForSelector('h1.login-title', { timeout: 15000 })
  await page.locator('input[type=password]').first().fill(PASSWORD)
  await Promise.all([
    page.waitForURL((url) => !url.pathname.startsWith('/login'), { timeout: 25000 }),
    page.getByRole('button', { name: '登录' }).click()
  ])
  await page.waitForTimeout(1800)
  if (!(await page.locator('.app-header').count())) { throw new Error('登录后未进入应用外壳') }
  return page.url()
}

async function gotoPage(page, path) {
  try {
    await page.goto(BASE + path, { waitUntil: 'domcontentloaded', timeout: 25000 })
    await page.waitForSelector('.app-main, .login-card', { timeout: 15000 })
    await page.waitForTimeout(1400)
  } catch (error) {
    log('[goto warn] ' + path + ' -> ' + String(error).slice(0, 160))
  }
}

const PAGES = [
  { name: 'overview', path: '/', title: '概览' },
  { name: 'volume', path: '/volume', title: '音量与静音' },
  { name: 'routing', path: '/routing', title: '路由矩阵' },
  { name: 'format', path: '/format', title: '音频格式' },
  { name: 'calibration', path: '/calibration', title: '校准' },
  { name: 'system', path: '/system', title: '系统信息' }
]

try {
  // ---------- 桌面 1440x900 ----------
  const desktop = await newPage({ width: 1440, height: 900 })
  let storageState = null
  try {
    const loginShotName = prefix + '-desktop-1440x900-01-login'
    await desktop.page.goto(BASE + '/login', { waitUntil: 'networkidle', timeout: 30000 })
    await desktop.page.waitForSelector('h1.login-title', { timeout: 15000 })
    await shoot(desktop.page, loginShotName)
    const loginMetrics = await collectMetrics(desktop.page)
    summary.pages.push({ viewport: '1440x900', page: 'login', metrics: loginMetrics })
    summary.viewports['1440x900'] = { loginUrl: await login(desktop.page) }
    storageState = await desktop.context.storageState()
    summary.apiFacts = await desktop.page.evaluate(async () => {
      const get = async (url) => {
        try {
          const res = await fetch(url, { credentials: 'same-origin' })
          return { status: res.status, json: await res.json() }
        } catch (error) { return { status: 0, error: String(error) } }
      }
      const status = await get('/api/runtime/status')
      const format = await get('/api/audio/format')
      const mode = await get('/api/runtime/mode')
      const target = await get('/api/runtime/target')
      const jobs = await get('/api/calibration/jobs')
      const content = await get('/api/content/layout')
      const health = await get('/healthz')
      return {
        runtime: status.json && { engine: status.json.engine, synthetic: status.json.synthetic, actual: status.json.actual, mode: status.json.mode, mode_services: status.json.mode_services, sample_rate: status.json.sample_rate, chunksize: status.json.chunksize, volume_db: status.json.volume_db, mute: status.json.mute, actual_version: status.json.actual_version, epoch: status.json.epoch },
        audioFormat: format.json && { engine: format.json.engine, synthetic: format.json.synthetic, source: format.json.source, limits: format.json.limits },
        mode: mode.json && { mode_controller: mode.json.mode_controller, mode_services: mode.json.mode_services, mode_actual: mode.json.mode_actual, profiles: (mode.json.profiles || []).map((p) => p.mode) },
        target: target.json && { requires_commit: target.json.requires_commit, layout: target.json.target && target.json.target.layout, roles: target.json.target && target.json.target.roles },
        calJobs: jobs.json && { count: jobs.json.count, engine: jobs.json.engine, synthetic: jobs.json.synthetic, real_audio: jobs.json.real_audio, cal_engine: jobs.json.cal_engine, jobs: (jobs.json.jobs || []).map((j) => ({ job_id: j.job_id, kind: j.kind, state: j.state, progress: j.progress, applied_version: j.applied_version, last_good_version: j.last_good_version })) },
        contentLayout: { status: content.status, json: content.json ?? content.error },
        health: health.json
      }
    })
    let index = 1
    for (const pageDef of PAGES) {
      index += 1
      await gotoPage(desktop.page, pageDef.path)
      await desktop.page.waitForTimeout(1200)
      const metrics = await collectMetrics(desktop.page)
      await shoot(desktop.page, prefix + '-desktop-1440x900-' + String(index).padStart(2, '0') + '-' + pageDef.name)
      if (pageDef.name === 'overview') {
        await shoot(desktop.page, prefix + '-desktop-1440x900-' + String(index).padStart(2, '0') + '-overview-full', { fullPage: true })
      }
      summary.pages.push({ viewport: '1440x900', page: pageDef.name, metrics })
      log('page ' + pageDef.name + ' metrics ' + JSON.stringify(metrics))
    }

    // 音量：记住实际值 → 应用到 -18.0 dB → 截图回执 → 还原（生产状态不留痕）
    await gotoPage(desktop.page, '/volume')
    const volumeInput = desktop.page.locator('.el-input-number input')
    const originalVolume = await volumeInput.inputValue().catch(() => '')
    summary.volumeFlow = { original: originalVolume }
    if (await volumeInput.count()) {
      await volumeInput.fill('-18')
      await volumeInput.press('Enter')
    }
    await desktop.page.getByRole('button', { name: '应用音量' }).click()
    await desktop.page.waitForTimeout(1600)
    await shoot(desktop.page, prefix + '-desktop-1440x900-09-volume-ack')
    const ackText = await desktop.page.locator('.banner', { hasText: '音量请求' }).first().innerText().catch(() => '')
    summary.pages.push({ viewport: '1440x900', page: 'volume-ack', ackText })
    if (originalVolume) {
      await volumeInput.fill(originalVolume)
      await volumeInput.press('Enter')
      await desktop.page.getByRole('button', { name: '应用音量' }).click()
      await desktop.page.waitForTimeout(1500)
      summary.volumeFlow.restored = await volumeInput.inputValue().catch(() => '')
      summary.volumeFlow.restoreAck = await desktop.page.locator('.banner', { hasText: '音量请求' }).first().innerText().catch(() => '')
    }
    // 静音切换两次（回到原状态）
    const muteStateBefore = await desktop.page.locator('.el-switch').first().getAttribute('class').catch(() => '')
    await desktop.page.getByRole('button', { name: /静音|取消静音/ }).first().click().catch(() => {})
    await desktop.page.waitForTimeout(1200)
    await desktop.page.getByRole('button', { name: /静音|取消静音/ }).first().click().catch(() => {})
    await desktop.page.waitForTimeout(800)
    summary.volumeFlow.muteClassBefore = muteStateBefore

    // 路由页：仅查看（不在生产 80 上提交 prepare/commit，避免改变生产控制面状态）
    await gotoPage(desktop.page, '/routing')
    await shoot(desktop.page, prefix + '-desktop-1440x900-10-routing-detail')
    await desktop.page.locator('.el-collapse-item__header').first().click().catch(() => {})
    await desktop.page.waitForTimeout(500)
    // 内容布局 / 中置下混 面板（后端未接线 → 待接线 + 禁用）
    const contentPanel = desktop.page.locator('.panel', { hasText: '内容布局 / 中置下混' }).first()
    if (await contentPanel.count()) {
      await contentPanel.scrollIntoViewIfNeeded().catch(() => {})
      await desktop.page.waitForTimeout(600)
      await contentPanel.screenshot({ path: join(OUT, prefix + '-desktop-1440x900-11-content-layout.png'), animations: 'disabled' }).catch(() => {})
      const phase = await contentPanel.locator('.panel__extra .chip').first().innerText().catch(() => '')
      const disabledSave = await contentPanel.getByRole('button', { name: '保存内容布局' }).isDisabled().catch(() => null)
      const infoText = await contentPanel.locator('.banner').first().innerText().catch(() => '')
      summary.contentLayoutPanel = { phase: phase.trim(), saveDisabled: disabledSave, banner: infoText.slice(0, 300) }
      log('content layout panel phase=' + phase + ' saveDisabled=' + disabledSave)
    }
    // 校准 job 详情/时间线（只读 GET，不在生产上触发 apply/rollback）
    await gotoPage(desktop.page, '/calibration')
    const jobLink = desktop.page.locator('.data-table a.mono').first()
    if (await jobLink.count()) {
      await jobLink.click()
      await desktop.page.waitForSelector('.el-dialog', { state: 'visible', timeout: 5000 }).catch(() => {})
      await desktop.page.waitForTimeout(900)
      await shoot(desktop.page, prefix + '-desktop-1440x900-12-calibration-detail')
      summary.calibrationDetail = await desktop.page.locator('.el-dialog').first().innerText().catch(() => '')
      await desktop.page.keyboard.press('Escape').catch(() => {})
      await desktop.page.waitForTimeout(400)
    }
  } finally {
    await desktop.context.close()
  }

  // ---------- 平板 768x1024 ----------
  const tablet = await newPage({ width: 768, height: 1024 }, storageState)
  try {
    for (const pageDef of [{ name: 'overview', path: '/' }, { name: 'calibration', path: '/calibration' }, { name: 'volume', path: '/volume' }, { name: 'system', path: '/system' }]) {
      await gotoPage(tablet.page, pageDef.path)
      const metrics = await collectMetrics(tablet.page)
      await shoot(tablet.page, prefix + '-tablet-768x1024-' + pageDef.name)
      summary.pages.push({ viewport: '768x1024', page: pageDef.name, metrics })
      log('tablet page ' + pageDef.name + ' metrics ' + JSON.stringify(metrics))
    }
  } finally {
    await tablet.context.close()
  }

  // ---------- 手机 390x844（未登录：登录页；登录后：导航可达性） ----------
  const mobileAnon = await newPage({ width: 390, height: 844 })
  try {
    await mobileAnon.page.goto(BASE + '/login', { waitUntil: 'domcontentloaded', timeout: 30000 })
    await mobileAnon.page.waitForSelector('h1.login-title')
    await shoot(mobileAnon.page, prefix + '-mobile-390x844-01-login')
    const metrics = await collectMetrics(mobileAnon.page)
    summary.pages.push({ viewport: '390x844', page: 'login', metrics })
    log('mobile login metrics ' + JSON.stringify(metrics))
  } finally {
    await mobileAnon.context.close()
  }

  const mobile = await newPage({ width: 390, height: 844 }, storageState)
  try {
    await gotoPage(mobile.page, '/')
    await shoot(mobile.page, prefix + '-mobile-390x844-02-overview')
    let metrics = await collectMetrics(mobile.page)
    summary.pages.push({ viewport: '390x844', page: 'overview', metrics })
    log('mobile overview metrics ' + JSON.stringify(metrics))

    // 底部导航可达性：逐项点击并记录 URL/标题
    const bottomItems = mobile.page.locator('.app-bottom .bottom-item')
    const bottomCount = await bottomItems.count()
    for (let i = 0; i < bottomCount; i += 1) {
      const label = (await bottomItems.nth(i).innerText()).replace(/\s+/g, ' ').trim()
      await bottomItems.nth(i).click()
      await mobile.page.waitForTimeout(900)
      summary.navChecks.push({ kind: 'bottom', label, path: new URL(mobile.page.url()).pathname, title: await mobile.page.title() })
    }
    // 抽屉导航：逐项点击（每次重新打开抽屉，等遮罩消失后再操作底部导航）
    async function closeDrawerIfOpen() {
      const overlay = mobile.page.locator('.el-overlay.is-drawer')
      if (await overlay.isVisible().catch(() => false)) {
        await mobile.page.keyboard.press('Escape').catch(() => {})
        await overlay.waitFor({ state: 'hidden', timeout: 4000 }).catch(() => {})
        await mobile.page.waitForTimeout(300)
      }
    }
    async function openDrawer() {
      await closeDrawerIfOpen()
      await mobile.page.locator('.app-bottom .bottom-item').last().click()
      await mobile.page.waitForSelector('.el-drawer .nav-item', { state: 'visible', timeout: 5000 })
      await mobile.page.waitForTimeout(500)
    }
    await openDrawer()
    await shoot(mobile.page, prefix + '-mobile-390x844-03-drawer')
    const drawerCount = await mobile.page.locator('.el-drawer .nav-item').count()
    for (let i = 0; i < drawerCount; i += 1) {
      await openDrawer()
      const item = mobile.page.locator('.el-drawer .nav-item').nth(i)
      const label = (await item.innerText()).replace(/\s+/g, ' ').trim()
      await item.click()
      await mobile.page.waitForTimeout(1100)
      summary.navChecks.push({ kind: 'drawer', label, path: new URL(mobile.page.url()).pathname, title: await mobile.page.title() })
      await closeDrawerIfOpen()
    }

    // 手机关键页面截图
    for (const pageDef of [{ name: 'volume', path: '/volume' }, { name: 'routing', path: '/routing' }, { name: 'format', path: '/format' }, { name: 'calibration', path: '/calibration' }, { name: 'system', path: '/system' }]) {
      await gotoPage(mobile.page, pageDef.path)
      const m = await collectMetrics(mobile.page)
      await shoot(mobile.page, prefix + '-mobile-390x844-' + pageDef.name)
      summary.pages.push({ viewport: '390x844', page: pageDef.name, metrics: m })
      log('mobile page ' + pageDef.name + ' metrics ' + JSON.stringify(m))
    }
  } finally {
    await mobile.context.close()
  }
} catch (error) {
  summary.errors.push(String(error))
  log('[fatal] ' + String(error))
} finally {
  await browser.close()
  summary.finishedAt = new Date().toISOString()
  const allErrors = consoleLines.filter((line) => line.startsWith('error:'))
  const expected = allErrors.filter((line) => /401 \(Unauthorized\)|501 \(Not Implemented\)|404 \(Not Found\)/.test(line))
  summary.consoleErrorCount = allErrors.length
  summary.expectedConsoleErrors = expected
  summary.expectedConsoleErrorNote = '401 = 未登录时 /api/auth/session 探针（预期）；501 = 预览代理未反代 /ws（预期，前端已降级 REST）；404 = 后端未实现的 /api/content/layout（设计先行契约，UI 显示待接线）'
  summary.httpErrorSummary = httpErrorCounts
  summary.unexpectedConsoleErrors = allErrors.filter((line) => !expected.includes(line)).slice(0, 30)
  summary.pageErrorCount = pageErrors.length
  summary.pageErrors = pageErrors.slice(0, 20)
  writeFileSync(join(OUT, prefix + '-run-summary.json'), JSON.stringify(summary, null, 2))
  log('summary written; consoleErrors=' + summary.consoleErrorCount + ' pageErrors=' + summary.pageErrorCount)
  console.log(JSON.stringify({
    target: TARGET,
    consoleErrors: summary.consoleErrorCount,
    pageErrors: summary.pageErrorCount,
    pages: summary.pages.length,
    navChecks: summary.navChecks.length,
    errors: summary.errors,
    sampleMetrics: summary.pages.slice(0, 3).map((p) => ({ page: p.page, viewport: p.viewport, overflowX: p.metrics.overflowX, smallTargets: p.metrics.smallTargetCount, bottomNav: p.metrics.bottomNavVisible, aside: p.metrics.asideVisible }))
  }, null, 2))
}