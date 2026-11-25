// 校验平板/手机触控目标（<1200px 断点）是否均 >=44px。
import { chromium } from 'playwright'
const BASE = process.env.KHAEN_BASE_URL || 'http://192.168.63.222'
const PASSWORD = process.env.KHAEN_PASSWORD || ''
const browser = await chromium.launch({ headless: true })
const results = []
for (const viewport of [{ width: 768, height: 1024 }, { width: 390, height: 844 }]) {
  const context = await browser.newContext({ viewport, locale: 'zh-CN' })
  const page = await context.newPage()
  await page.goto(BASE + '/login', { waitUntil: 'domcontentloaded' })
  await page.waitForSelector('h1.login-title')
  await page.locator('input[type=password]').first().fill(PASSWORD)
  await Promise.all([page.waitForURL((url) => !url.pathname.startsWith('/login')), page.getByRole('button', { name: '登录' }).click()])
  for (const path of ['/', '/volume', '/routing', '/format', '/calibration', '/system']) {
    await page.goto(BASE + path, { waitUntil: 'domcontentloaded' })
    await page.waitForTimeout(1200)
    const metric = await page.evaluate(() => {
      const rect = (el) => el.getBoundingClientRect()
      const resolve = (el) => (el.tagName === 'INPUT' || el.tagName === 'SELECT')
        ? (el.closest('.el-input__wrapper, .el-select__wrapper, .el-input-number, .el-textarea') || el) : el
      const set = Array.from(new Set(Array.from(document.querySelectorAll('button, a[href], input, select, textarea, .el-switch, .el-slider__button-wrapper')).map(resolve)))
      const small = set.filter((el) => { const r = rect(el); return r.width > 0 && r.height > 0 && r.height < 43.5 })
        .map((el) => ({ tag: el.tagName, cls: String(el.className || '').slice(0, 50), h: Math.round(rect(el).height) }))
      return { overflowX: document.documentElement.scrollWidth - window.innerWidth, smallCount: small.length, small: small.slice(0, 5) }
    })
    results.push({ viewport: viewport.width, path, ...metric })
  }
  await context.close()
}
await browser.close()
console.log(JSON.stringify(results, null, 1))
