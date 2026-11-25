// 冒烟：打开登录页 → 检查页面错误/控制台错误 → 登录 → 抓概览文本 → 打印布局指标。
import { chromium } from 'playwright'

const BASE = process.env.KHAEN_BASE_URL || 'http://192.168.63.222'
const PASSWORD = process.env.KHAEN_PASSWORD || ''

const browser = await chromium.launch({ headless: true })
const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 })
const page = await context.newPage()
const consoleErrors = []
const pageErrors = []
page.on('console', (msg) => { if (msg.type() === 'error') { consoleErrors.push(msg.text()) } })
page.on('pageerror', (error) => { pageErrors.push(String(error)) })

await page.goto(BASE + '/login', { waitUntil: 'networkidle', timeout: 30000 })
console.log('title:', await page.title())
console.log('login h1:', await page.locator('h1.login-title').textContent().catch(() => '(missing)'))
const statusChips = await page.locator('.login-meta .chip').allTextContents().catch(() => [])
console.log('auth chips:', JSON.stringify(statusChips))

if (PASSWORD) {
  await page.locator('input[type=password]').first().fill(PASSWORD)
  await page.getByRole('button', { name: '登录' }).click()
  await page.waitForURL((url) => !url.pathname.startsWith('/login'), { timeout: 20000 })
  await page.waitForTimeout(2500)
  console.log('after login url:', page.url())
  const chip = await page.locator('.app-header .chip').first().textContent().catch(() => '(none)')
  console.log('header chip:', chip)
  const kv = await page.locator('.kv__item').first().textContent().catch(() => '(none)')
  console.log('first kv:', kv)
  const metrics = await page.evaluate(() => ({
    innerWidth: window.innerWidth,
    scrollWidth: document.documentElement.scrollWidth,
    bodyScrollWidth: document.body.scrollWidth,
    scrollHeight: document.documentElement.scrollHeight
  }))
  console.log('metrics:', JSON.stringify(metrics))
}
console.log('consoleErrors:', JSON.stringify(consoleErrors.slice(0, 10)))
console.log('pageErrors:', JSON.stringify(pageErrors.slice(0, 10)))
await browser.close()
