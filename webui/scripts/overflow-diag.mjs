// 诊断：哪些元素在移动端视口下横向溢出。
import { chromium } from 'playwright'
const BASE = process.env.KHAEN_BASE_URL || 'http://192.168.63.222'
const PASSWORD = process.env.KHAEN_PASSWORD || ''
const browser = await chromium.launch({ headless: true })
const context = await browser.newContext({ viewport: { width: 390, height: 844 }, locale: 'zh-CN' })
const page = await context.newPage()
await page.goto(BASE + '/login', { waitUntil: 'domcontentloaded' })
await page.waitForSelector('h1.login-title')
await page.locator('input[type=password]').first().fill(PASSWORD)
await Promise.all([page.waitForURL((url) => !url.pathname.startsWith('/login')), page.getByRole('button', { name: '登录' }).click()])
await page.waitForTimeout(1500)
for (const path of ['/calibration', '/system', '/routing']) {
  await page.goto(BASE + path, { waitUntil: 'domcontentloaded' })
  await page.waitForTimeout(1600)
  const offenders = await page.evaluate(() => {
    const vw = window.innerWidth
    const out = []
    for (const el of document.querySelectorAll('*')) {
      const r = el.getBoundingClientRect()
      if (r.width === 0 && r.height === 0) continue
      if (r.right > vw + 0.5) {
        out.push({
          tag: el.tagName,
          cls: String(el.className || '').slice(0, 80),
          right: Math.round(r.right),
          width: Math.round(r.width),
          text: (el.textContent || '').trim().slice(0, 40)
        })
      }
    }
    // 只保留最外层（去掉被父元素包含的重复项）
    return out.slice(0, 25)
  })
  console.log('=== ' + path + ' offenders=' + offenders.length)
  for (const item of offenders) console.log(JSON.stringify(item))
}
await browser.close()
