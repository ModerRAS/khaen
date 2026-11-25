// 内容布局面板契约验证：隔离实例真实登录壳 + Playwright 拦截 /api/content/layout 模拟 task-316 各状态。
// 生产 200 路径因 host graph=stereo_identity（501）且 auth 缺失 → NOT_RUN；此处验证 UI 对契约的处理。
import { chromium } from 'playwright'
import { writeFileSync, mkdirSync } from 'node:fs'
import { join } from 'node:path'

const BASE = process.env.KHAEN_BASE_URL || 'http://192.168.63.222'
const PASSWORD = process.env.KHAEN_PASSWORD || ''
const OUT = process.env.KHAEN_EVIDENCE_DIR || './artifacts'
mkdirSync(OUT, { recursive: true })
const summary = { startedAt: new Date().toISOString(), base: BASE, scenarios: {}, errors: [], putBodies: [] }

const currentBase = {
  configured: true,
  input_layout: 'nsp-5.1.2',
  output_layout: '4.1.2',
  center: 'auto',
  program_peak_dbfs: -18,
  program_peak_measured_dbfs: -21.4,
  adapter_status: 'applied',
  applied: true,
  source_channels: 8,
  center_channel: 2,
  applied_blocks: 3,
  applied_block: 174233
}
const wiredBody = {
  ok: true, schema: 'khaen-host-content/v1', engine: 'rt-host', actual: true,
  wired: ['status', 'set'], not_wired: ['presets_edit'],
  current: currentBase,
  presets_path: '/etc/khaen-rt-stack/layout-presets.yml',
  presets: { available: true, input: ['nsp-5.1.2', 'h16-5.1', '4.1.2'], output: ['4.1.2', '5.1.2'] },
  plan_input_channels: 8, plan_output_channels: 8
}
const presetsUnavailable = {
  ok: true, schema: 'khaen-host-content/v1', engine: 'rt-host', actual: true,
  wired: ['status', 'set'], not_wired: ['presets_edit'],
  current: currentBase,
  presets_path: '/etc/khaen-rt-stack/layout-presets.yml',
  presets: { available: false, error: 'layout-presets.yml unreadable: ENOENT' },
  plan_input_channels: 8, plan_output_channels: 8
}
const fixtures = {
  'not-wired': { status: 501, body: { error: { code: 'not_wired', message: 'host has no content adapter (graph=stereo_identity); content layout requires plan-arena graph' } } },
  'presets-503': { status: 503, body: { error: { code: 'layout_presets_unavailable', message: 'layout-presets.yml unreadable: ENOENT' } } },
  wired: { status: 200, body: wiredBody },
  'busy-put': { status: 409, body: { error: { code: 'content_busy', message: 'content adapter busy: block switch in progress' } } },
  'host-unavailable': { status: 503, body: { error: { code: 'host_unavailable', message: 'rt-host socket not reachable' } } },
  'content-timeout': { status: 504, body: { error: { code: 'content_timeout', message: 'content apply ack timeout after 2000ms' } } }
}

let mode = 'not-wired'
const browser = await chromium.launch({ headless: true })
const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'zh-CN', timezoneId: 'Asia/Shanghai' })
const page = await context.newPage()
page.on('pageerror', (error) => { summary.errors.push(String(error)) })

await page.route('**/api/content/layout', async (route) => {
  const request = route.request()
  if (request.method() === 'PUT') {
    summary.putBodies.push(request.postData() || '')
    if (mode === 'busy-put') {
      const fixture = fixtures['busy-put']
      await route.fulfill({ status: fixture.status, contentType: 'application/json', body: JSON.stringify(fixture.body) })
      return
    }
    const ackCurrent = { ...currentBase, center: 'present', program_peak_dbfs: -20, applied_block: 174240, applied_blocks: 4 }
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ ok: true, actual: true, applied: true, applied_block: 174240, current: ackCurrent })
    })
    return
  }
  if (mode === 'presets-unavailable') {
    // GET 本身 200，但 presets.available=false（后端也可能 503 layout_presets_unavailable，两种都覆盖：这里用 200 对象形态）
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(presetsUnavailable) })
    return
  }
  if (mode === 'wired') {
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(wiredBody) })
    return
  }
  const fixture = fixtures[mode] ?? fixtures['not-wired']
  await route.fulfill({ status: fixture.status, contentType: 'application/json', body: JSON.stringify(fixture.body) })
})

const panel = () => page.locator('.panel', { hasText: '内容布局 / 中置下混' }).first()
async function openRouting() {
  await page.goto(BASE + '/routing', { waitUntil: 'domcontentloaded' })
  await page.waitForSelector('.panel', { timeout: 15000 })
  await page.waitForTimeout(1200)
  const target = panel()
  await target.scrollIntoViewIfNeeded().catch(() => {})
  await page.waitForTimeout(400)
  return target
}
async function stateOf(target) {
  return target.evaluate((root) => {
    const selects = Array.from(root.querySelectorAll('.el-select'))
    const save = Array.from(root.querySelectorAll('button')).find((button) => button.textContent.includes('保存并应用'))
    const disableButton = Array.from(root.querySelectorAll('button')).find((button) => button.textContent.includes('停用内容适配'))
    const banner = root.querySelector('.banner')
    return {
      chip: (root.querySelector('.panel__extra .chip')?.textContent || '').trim(),
      bannerTitle: (banner?.querySelector('.banner__title')?.textContent || '').trim(),
      bannerBody: (banner?.querySelector('.banner__body')?.textContent || '').trim().slice(0, 200),
      selectsDisabled: selects.map((select) => {
        const wrapper = select.querySelector('.el-select__wrapper')
        return select.className.includes('is-disabled') || (wrapper ? wrapper.className.includes('is-disabled') : false)
      }),
      saveDisabled: save ? save.disabled : null,
      disableDisabled: disableButton ? disableButton.disabled : null
    }
  })
}
async function shoot(name) {
  const target = panel()
  await target.screenshot({ path: join(OUT, 'content-layout-' + name + '.png'), animations: 'disabled' })
}

try {
  await page.goto(BASE + '/login', { waitUntil: 'domcontentloaded' })
  await page.waitForSelector('h1.login-title')
  await page.locator('input[type=password]').first().fill(PASSWORD)
  await Promise.all([page.waitForURL((url) => !url.pathname.startsWith('/login')), page.getByRole('button', { name: '登录' }).click()])
  await page.waitForTimeout(1500)

  // 1) 501 not_wired（生产当前预期形态）
  mode = 'not-wired'
  let target = await openRouting()
  summary.scenarios['not-wired'] = await stateOf(target)
  await shoot('01-not-wired')

  // 2a) 200 但 presets.available=false（对象形态）
  mode = 'presets-unavailable'
  target = await openRouting()
  summary.scenarios['presets-unavailable-object'] = await stateOf(target)
  await shoot('02-presets-unavailable-object')
  // 2b) 503 layout_presets_unavailable（错误形态）
  mode = 'presets-503'
  target = await openRouting()
  summary.scenarios['presets-unavailable-503'] = await stateOf(target)
  await shoot('02b-presets-unavailable-503')

  // 3) 200 wired + 保存（PUT 200 ack）
  mode = 'wired'
  target = await openRouting()
  summary.scenarios.wired = await stateOf(target)
  await shoot('03-wired')
  // 记录下拉选项（presets.input）
  const selects = target.locator('.el-select')
  await selects.first().click()
  await page.waitForTimeout(600)
  summary.scenarios.wired.inputOptions = await page
    .locator('.el-select-dropdown:visible .el-select-dropdown__item')
    .allInnerTexts()
    .catch(() => [])
  await page.keyboard.press('Escape')
  await page.waitForTimeout(300)
  // 改 center=present、peak=-20 后保存
  await target.locator('.el-radio', { hasText: 'present' }).first().click().catch(() => {})
  const peakInput = target.locator('.el-input-number input').first()
  await peakInput.fill('-20')
  await peakInput.press('Enter')
  await target.getByRole('button', { name: '保存并应用' }).click()
  await page.waitForTimeout(1500)
  await target.scrollIntoViewIfNeeded().catch(() => {})
  await shoot('04-put-ack')
  summary.scenarios['put-ack'] = await stateOf(target)

  // 4) 停用（PUT {enabled:false}）→ 200 ack
  await target.getByRole('button', { name: '停用内容适配' }).click()
  await page.waitForTimeout(1200)
  await shoot('05-disable')

  // 5) PUT 409 content_busy
  mode = 'busy-put'
  await target.getByRole('button', { name: '保存并应用' }).click()
  await page.waitForTimeout(1200)
  await shoot('06-put-busy')
  summary.scenarios['put-busy'] = await stateOf(target)

  // 6) 503 host_unavailable / 504 content_timeout（GET 错误态）
  mode = 'host-unavailable'
  target = await openRouting()
  summary.scenarios['host-unavailable'] = await stateOf(target)
  await shoot('07-host-unavailable')
  mode = 'content-timeout'
  target = await openRouting()
  summary.scenarios['content-timeout'] = await stateOf(target)
  await shoot('08-content-timeout')

  // 7) 概览小标签（wired 形态）
  mode = 'wired'
  await page.goto(BASE + '/', { waitUntil: 'domcontentloaded' })
  await page.waitForTimeout(1500)
  summary.overviewChip = await page.locator('.panel .chip', { hasText: '内容布局' }).first().innerText().catch(() => '')
  await page.screenshot({ path: join(OUT, 'content-layout-09-overview-chip.png'), animations: 'disabled' })

  summary.finishedAt = new Date().toISOString()
} catch (error) {
  summary.errors.push(String(error))
} finally {
  await browser.close()
  writeFileSync(join(OUT, 'content-layout-contract-summary.json'), JSON.stringify(summary, null, 2))
  console.log(JSON.stringify({
    errors: summary.errors,
    scenarios: summary.scenarios,
    putBodies: summary.putBodies,
    overviewChip: summary.overviewChip
  }, null, 2))
}