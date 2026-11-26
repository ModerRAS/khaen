import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import zhCN from '../src/i18n/zh-CN.ts'
import enUS from '../src/i18n/en-US.ts'

/**
 * 导航/路由注册契约（task-62 前端地区）：
 * - 侧栏每一项都必须有对应 route record（防"菜单加了但路由没注册"）；
 * - 五个语义界面（volume/mode/routing/layout/format）与校准/系统页全部注册；
 * - 每个 nav.* title key 在 zh-CN / en-US 都存在（i18n-check 之外的二道防线，明确 fail-fast）；
 * - 路由守卫仍是 requiresAuth（除 /login 公开）。
 *
 * 说明：router/index.ts 引 .vue 组件（需要 Vite 别名），Node 测试不 import 它；
 * 这里只读源码做静态注册检查，导航逻辑本身由 guard.test.ts 覆盖。
 */

const HERE = dirname(fileURLToPath(import.meta.url))
const ROUTER_SOURCE = readFileSync(resolve(HERE, '..', 'src', 'router', 'index.ts'), 'utf8')

/** 解析 route record 的 (path, name) 对（含嵌套 children 与 /login）。 */
function parseRouteNames(): Map<string, string> {
  const routes = new Map<string, string>()
  const pattern = /path:\s*'([^']*)',\s*\r?\n\s*name:\s*'([^']+)'/g
  for (const match of ROUTER_SOURCE.matchAll(pattern)) {
    routes.set(match[2], match[1])
  }
  return routes
}

/** 解析侧栏 navItems（name/path/titleKey/group）。 */
function parseNavItems(): Array<{ name: string; path: string; titleKey: string; group: string }> {
  const items: Array<{ name: string; path: string; titleKey: string; group: string }> = []
  const pattern =
    /\{\s*name:\s*'([^']+)',\s*path:\s*'([^']*)',\s*titleKey:\s*'([^']+)',\s*icon:\s*'[^']+',\s*group:\s*'([^']+)'\s*\}/g
  for (const match of ROUTER_SOURCE.matchAll(pattern)) {
    items.push({ name: match[1], path: match[2], titleKey: match[3], group: match[4] })
  }
  return items
}

function getMessage(messages: unknown, key: string): unknown {
  let node: unknown = messages
  for (const segment of key.split('.')) {
    if (typeof node !== 'object' || node === null) {
      return undefined
    }
    node = (node as Record<string, unknown>)[segment]
  }
  return node
}

test('every sidebar item has a registered route and a paired i18n title', () => {
  const routes = parseRouteNames()
  const navItems = parseNavItems()
  assert.ok(navItems.length >= 10, 'expected the full sidebar (>=10 items), got ' + navItems.length)
  for (const item of navItems) {
    assert.ok(routes.has(item.name), 'nav item ' + item.name + ' has no route record')
    assert.equal('/' + routes.get(item.name), item.path === '/' ? '/' : '/' + routes.get(item.name), 'route path for ' + item.name)
    for (const [locale, messages] of [
      ['zh-CN', zhCN],
      ['en-US', enUS]
    ] as const) {
      const value = getMessage(messages, item.titleKey)
      assert.equal(typeof value, 'string', locale + ' is missing nav title ' + item.titleKey)
      assert.ok((value as string).trim().length > 0, locale + ' nav title ' + item.titleKey + ' is empty')
    }
  }
})

test('the five semantic surfaces and auxiliary pages are all registered', () => {
  const names = parseRouteNames()
  for (const expected of [
    'overview',
    'volume',
    'mode',
    'routing',
    'layout',
    'speakers',
    'format',
    'channels',
    'calibration',
    'calibration-wizard',
    'calibration-job',
    'system',
    'logs'
  ]) {
    assert.ok(names.has(expected), 'route ' + expected + ' is not registered')
  }
  assert.equal(names.get('login'), '/login')
})

test('protected pages keep requiresAuth while login stays public', () => {
  // 每个业务 route record 的 meta 必须带 requiresAuth: true；/login 带 public: true。
  const routeBlocks = ROUTER_SOURCE.split('name:').slice(1)
  let checked = 0
  for (const block of routeBlocks) {
    const name = /^\s*'([^']+)'/.exec(block)
    if (name === null) {
      continue
    }
    const metaStart = block.indexOf('meta:')
    if (metaStart < 0) {
      continue
    }
    const meta = block.slice(metaStart, metaStart + 160)
    if (name[1] === 'login') {
      assert.match(meta, /public:\s*true/, 'login route must be public')
    } else if (meta.includes('titleKey')) {
      assert.match(meta, /requiresAuth:\s*true/, 'route ' + name[1] + ' must require auth')
    }
    checked += 1
  }
  assert.ok(checked >= 12, 'expected all routes checked, got ' + checked)
})