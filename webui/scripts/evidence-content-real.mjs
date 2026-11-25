// 真实 200 路径验证：隔离 daemon 实例（默认 18083，隔离 state/auth）。
// 用 KHAEN_BASE_URL 指向该实例（默认板端 80 端口 daemon），真实 GET/PUT /api/content/layout，截图 + 原始回执存档。
import { chromium } from 'playwright'
import { writeFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'

const BASE = process.env.KHAEN_BASE_URL || 'http://192.168.63.222'
const PASSWORD = process.env.KHAEN_PASSWORD || ''
const OUT = process.env.KHAEN_EVIDENCE_DIR || './artifacts'
mkdirSync(OUT, { recursive: true })
const summary = { startedAt: new Date().toISOString(), base: BASE, steps: [], putBodies: [], raw: {}, errors: [] }
const log = (line) => { summary.steps.push(line); console.log(String(line)) }

const browser = await chromium.launch({ headless: true })
const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'zh-CN', timezoneId: 'Asia/Shanghai' })
const page = await context.newPage()
page.on('pageerror', (error) => { summary.errors.push(String(error)) })
page.on('request', (request) => {
  if (request.url().includes('/api/content/layout') && request.method() === 'PUT') {
    summary.putBodies.push(request.postData() || '')
  }
})

const panel = () => page.locator('.panel', { hasText: '内容布局 / 中置下混' }).first()
async function getContent() {
  return page.evaluate(async () => {
    const response = await fetch('/api/content/layout', { credentials: 'same-origin' })
    return { status: response.status, body: await response.json() }
  })
}
async function panelState() {
  const target = panel()
  return target.evaluate((root) => {
    const selects = Array.from(root.querySelectorAll('.el-select'))
    const save = Array.from(root.querySelectorAll('button')).find((button) => button.textContent.includes('保存并应用'))
    const kv = {}
    for (const item of Array.from(root.querySelectorAll('.kv__item'))) {
      const label = item.querySelector('.kv__label')?.textContent?.trim() || ''
      const value = item.querySelector('.kv__value')?.textContent?.trim() || ''
      kv[label] = value
    }
    return {
      chip: (root.querySelector('.panel__extra .chip')?.textContent || '').trim(),
      bannerTitle: (root.querySelector('.banner .banner__title')?.textContent || '').trim(),
      selectsDisabled: selects.map((select) => {
        const wrapper = select.querySelector('.el-select__wrapper')
        return select.className.includes('is-disabled') || (wrapper ? wrapper.className.includes('is-disabled') : false)
      }),
      saveDisabled: save ? save.disabled : null,
      kv
    }
  })
}
async function shoot(name, target = page) {
  await target.screenshot({ path: join(OUT, 'content-layout-real-' + name + '.png'), animations: 'disabled' })
}
async function openRouting() {
  await page.goto(BASE + '/routing', { waitUntil: 'domcontentloaded' })
  await page.waitForSelector('.panel', { timeout: 15000 })
  await page.waitForTimeout(1500)
  const target = panel()
  await target.scrollIntoViewIfNeeded().catch(() => {})
  await page.waitForTimeout(400)
  return target
}
async function clickSave(target) {
  await target.getByRole('button', { name: '保存并应用' }).click()
  await page.waitForTimeout(2500)
}

try {
  await page.goto(BASE + '/login', { waitUntil: 'domcontentloaded' })
  await page.waitForSelector('h1.login-title')
  await page.locator('input[type=password]').first().fill(PASSWORD)
  await Promise.all([page.waitForURL((url) => !url.pathname.startsWith('/login')), page.getByRole('button', { name: '登录' }).click()])
  await page.waitForTimeout(1500)
  log('logged in: ' + page.url())

  // 真实 GET（200 actual）
  summary.raw.getBefore = await getContent()
  log('GET before: http=' + summary.raw.getBefore.status + ' engine=' + summary.raw.getBefore.body?.engine + ' actual=' + summary.raw.getBefore.body?.actual)

  // 1) wired 面板：若起始为 disabled（上一次测试遗留），先用 UI 重新启用（真实 PUT）
  let target = await openRouting()
  const initialState = await panelState()
  summary.initialPanel = initialState
  if (initialState.kv['已应用 applied'] === 'false') {
    const inputSelect0 = target.locator('.el-select').first()
    await inputSelect0.click()
    await page.waitForTimeout(500)
    await page.locator('.el-select-dropdown:visible .el-select-dropdown__item', { hasText: 'nsp-5.1.2' }).first().click().catch(() => {})
    await page.waitForTimeout(300)
    const outputSelect0 = target.locator('.el-select').nth(1)
    await outputSelect0.click()
    await page.waitForTimeout(500)
    await page.locator('.el-select-dropdown:visible .el-select-dropdown__item', { hasText: '4.1.2' }).first().click().catch(() => {})
    await page.waitForTimeout(300)
    await clickSave(target)
    summary.raw.putEnableInitial = await getContent()
    log('initial enable -> applied=' + summary.raw.putEnableInitial.body?.current?.applied)
    target = await openRouting()
  }
  summary.wiredPanel = await panelState()
  summary.raw.getPanel = await getContent()
  await shoot('01-wired-desktop')
  // 真实 presets 选项
  await target.locator('.el-select').first().click()
  await page.waitForTimeout(600)
  summary.inputOptions = await page.locator('.el-select-dropdown:visible .el-select-dropdown__item').allInnerTexts().catch(() => [])
  await page.keyboard.press('Escape')
  await page.waitForTimeout(300)
  await target.locator('.el-select').nth(1).click()
  await page.waitForTimeout(600)
  summary.outputOptions = await page.locator('.el-select-dropdown:visible .el-select-dropdown__item').allInnerTexts().catch(() => [])
  await page.keyboard.press('Escape')
  await page.waitForTimeout(300)

  // 2) 真实 PUT：保存（照回调 default：center=auto / peak=-18）
  await target.locator('.el-radio', { hasText: 'auto' }).first().click().catch(() => {})
  const peak = target.locator('.el-input-number input').first()
  await peak.fill('-18')
  await peak.press('Enter')
  await clickSave(target)
  summary.raw.putSave = await getContent()
  summary.putPanel = await panelState()
  await shoot('02-put-ack')
  log('PUT save -> http=' + summary.raw.putSave.status + ' applied=' + summary.raw.putSave.body?.current?.applied + ' applied_block=' + summary.raw.putSave.body?.current?.applied_block)

  // 3) 真实 PUT：停用 enabled:false
  await target.getByRole('button', { name: '停用内容适配' }).click()
  await page.waitForTimeout(2500)
  summary.raw.putDisable = await getContent()
  summary.disablePanel = await panelState()
  await shoot('03-disable')
  log('PUT disable -> applied=' + summary.raw.putDisable.body?.current?.applied + ' adapter_status=' + summary.raw.putDisable.body?.current?.adapter_status + ' applied_block=' + summary.raw.putDisable.body?.current?.applied_block)

  // 4) 恢复 applied：停用后后端 current 布局为空，UI 保留上次选择；显式重选 presets 再保存
  const inputSelect = target.locator('.el-select').first()
  await inputSelect.click()
  await page.waitForTimeout(500)
  await page.locator('.el-select-dropdown:visible .el-select-dropdown__item', { hasText: 'nsp-5.1.2' }).first().click().catch(() => {})
  await page.waitForTimeout(300)
  const outputSelect = target.locator('.el-select').nth(1)
  await outputSelect.click()
  await page.waitForTimeout(500)
  await page.locator('.el-select-dropdown:visible .el-select-dropdown__item', { hasText: '4.1.2' }).first().click().catch(() => {})
  await page.waitForTimeout(300)
  await clickSave(target)
  summary.raw.putRestore = await getContent()
  summary.restorePanel = await panelState()
  await shoot('04-restored')
  log('PUT restore -> applied=' + summary.raw.putRestore.body?.current?.applied + ' applied_block=' + summary.raw.putRestore.body?.current?.applied_block)

  // 5) 概览小标签（真实）
  await page.goto(BASE + '/', { waitUntil: 'domcontentloaded' })
  await page.waitForTimeout(1600)
  summary.overviewChip = await page.locator('.panel .chip', { hasText: '内容布局' }).first().innerText().catch(() => '')
  await shoot('05-overview-chip')
  log('overview chip: ' + summary.overviewChip)

  // 6) 手机端面板
  const mobileContext = await browser.newContext({ viewport: { width: 390, height: 844 }, locale: 'zh-CN', storageState: await context.storageState() })
  const mobile = await mobileContext.newPage()
  await mobile.goto(BASE + '/routing', { waitUntil: 'domcontentloaded' })
  await mobile.waitForTimeout(1800)
  const mobilePanel = mobile.locator('.panel', { hasText: '内容布局 / 中置下混' }).first()
  await mobilePanel.scrollIntoViewIfNeeded().catch(() => {})
  await mobile.waitForTimeout(400)
  await mobilePanel.screenshot({ path: join(OUT, 'content-layout-real-06-wired-mobile.png'), animations: 'disabled' })
  summary.mobileOverflowX = await mobile.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
  await mobileContext.close()

  summary.finishedAt = new Date().toISOString()
} catch (error) {
  summary.errors.push(String(error))
  log('[fatal] ' + String(error))
} finally {
  await browser.close()
  writeFileSync(join(OUT, 'content-layout-real-summary.json'), JSON.stringify(summary, null, 2))
  console.log(JSON.stringify({
    errors: summary.errors,
    putBodies: summary.putBodies,
    inputOptions: summary.inputOptions,
    outputOptions: summary.outputOptions,
    overviewChip: summary.overviewChip,
    wiredChip: summary.wiredPanel?.chip,
    wiredKv: summary.wiredPanel?.kv,
    getStatus: summary.raw.getBefore?.status,
    saveApplied: summary.raw.putSave?.body?.current?.applied,
    saveBlock: summary.raw.putSave?.body?.current?.applied_block,
    disableApplied: summary.raw.putDisable?.body?.current?.applied,
    restoreBlock: summary.raw.putRestore?.body?.current?.applied_block,
    mobileOverflowX: summary.mobileOverflowX
  }, null, 2))
}