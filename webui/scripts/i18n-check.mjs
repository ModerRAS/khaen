#!/usr/bin/env node
/**
 * i18n 键集校验（PLAN §12.2 门禁；lead 任务要求 node scripts/i18n-check.mjs 全绿）。
 *
 * 消息来源：
 *  1. 基础文件 src/i18n/zh-CN.ts（源语言，唯一事实来源）+ src/i18n/en-US.ts；
 *  2. 分片目录 src/i18n/messages/<area>.zh-CN.ts + <area>.en-US.ts（并行开发时各页面只改自己的分片，
 *     避免多人同时改两个巨型文件）。分片必须成对存在，且由 src/i18n/index.ts 实际 import 并合并。
 *
 * 校验项：
 *  1. zh-CN 与 en-US 合并后的键集**完全对等**（缺失 / 多余）；同一语言内跨文件不得重复定义同一个键；
 *  2. 同一文件内无重复键（TS AST 检测对象字面量的重复属性）；
 *  3. 所有叶子值都是非空字符串（不允许空串 / 数字 / 嵌套对象漂移）；
 *  4. 源码里出现的 i18n 键字面量（以 zh-CN 顶层命名空间开头）在两种语言里都存在；
 *  5. src 下 .vue / .ts 不得硬编码中日韩文字（文案必须走 t()，注释不算）。
 *
 * 用法：node scripts/i18n-check.mjs
 * 退出码：0 = 通过；1 = 有错误（逐条打印）。
 */
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, extname, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const I18N_DIR = join(ROOT, 'src', 'i18n')
const PARTS_DIR = join(I18N_DIR, 'messages')
const SOURCE_LOCALE = 'zh-CN'
const TARGET_LOCALES = ['en-US']
const LOCALES = [SOURCE_LOCALE, ...TARGET_LOCALES]
const BASE_FILES = { 'zh-CN': 'zh-CN.ts', 'en-US': 'en-US.ts' }
const INDEX_FILE = join(I18N_DIR, 'index.ts')

const errors = []
const notes = []

function fail(message) {
  errors.push(message)
}

/* ------------------------------------------------------------------ 文件发现 */

/**
 * 分片文件：messages/<area>.zh-CN.ts / messages/<area>.en-US.ts。
 * 返回 { areas: string[], files: Map<locale, string[]> }；不成对的分片直接报错。
 */
function discoverParts() {
  let names = []
  try {
    names = readdirSync(PARTS_DIR).filter((name) => name.endsWith('.ts') && !name.endsWith('.d.ts'))
  } catch {
    return { areas: [], files: new Map(LOCALES.map((locale) => [locale, []])) }
  }
  const byArea = new Map()
  for (const name of names) {
    const match = /^(.+)\.(zh-CN|en-US)\.ts$/.exec(name)
    if (!match) {
      fail('messages/' + name + ': 文件名必须是 <area>.<locale>.ts（支持 zh-CN / en-US）')
      continue
    }
    const [, area, locale] = match
    if (!byArea.has(area)) {
      byArea.set(area, new Set())
    }
    byArea.get(area).add(locale)
  }
  const areas = [...byArea.keys()].sort()
  const files = new Map(LOCALES.map((locale) => [locale, []]))
  for (const area of areas) {
    const locales = byArea.get(area)
    for (const locale of LOCALES) {
      if (!locales.has(locale)) {
        fail('messages/' + area + '.' + locale + '.ts 缺失（分片必须 zh-CN / en-US 成对）')
        continue
      }
      files.get(locale).push(join(PARTS_DIR, area + '.' + locale + '.ts'))
    }
  }
  return { areas, files }
}

/* ------------------------------------------------------------------ 解析 */

/** 找到 export default <identifier> 或 export default { ... } 的对象字面量。 */
function findRootObject(source, filePath) {
  let rootExpression = null
  source.forEachChild((node) => {
    if (ts.isExportAssignment(node) && node.isExportEquals !== true) {
      rootExpression = node.expression
    }
  })
  if (rootExpression && ts.isIdentifier(rootExpression)) {
    const name = rootExpression.text
    source.forEachChild((node) => {
      if (ts.isVariableStatement(node)) {
        for (const declaration of node.declarationList.declarations) {
          if (
            ts.isIdentifier(declaration.name) &&
            declaration.name.text === name &&
            declaration.initializer
          ) {
            rootExpression = declaration.initializer
          }
        }
      }
    })
  }
  if (!rootExpression || !ts.isObjectLiteralExpression(rootExpression)) {
    fail(relative(ROOT, filePath).replace(/\\/g, '/') + ': 找不到导出的消息对象字面量（export default { ... }）')
    return null
  }
  return rootExpression
}

/** 把一个消息对象文件解析成扁平键表（并检测重复键 / 非字符串叶子）。 */
function parseMessageFile(filePath) {
  const label = relative(ROOT, filePath).replace(/\\/g, '/')
  const text = readFileSync(filePath, 'utf8')
  const source = ts.createSourceFile(filePath, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS)
  const entries = new Map()
  const duplicates = []
  const rootExpression = findRootObject(source, filePath)
  if (rootExpression === null) {
    return { entries, duplicates, label }
  }

  function walk(objectLiteral, prefix) {
    for (const property of objectLiteral.properties) {
      let key = null
      if (ts.isIdentifier(property.name) || ts.isStringLiteral(property.name)) {
        key = property.name.text
      } else {
        fail(label + ': ' + (prefix || '<root>') + ' 有无法识别的键（仅支持标识符/字符串）')
        continue
      }
      const fullKey = prefix ? prefix + '.' + key : key
      if (!ts.isPropertyAssignment(property)) {
        fail(label + ': ' + fullKey + ' 不是普通属性赋值')
        continue
      }
      const value = property.initializer
      if (ts.isObjectLiteralExpression(value)) {
        walk(value, fullKey)
        continue
      }
      if (entries.has(fullKey)) {
        duplicates.push(fullKey)
      }
      if (!ts.isStringLiteral(value) && !ts.isNoSubstitutionTemplateLiteral(value)) {
        fail(label + ': ' + fullKey + ' 的值必须是字符串字面量')
        continue
      }
      if (value.text.trim().length === 0) {
        fail(label + ': ' + fullKey + ' 的值不能为空')
      }
      entries.set(fullKey, value.text)
    }
  }

  walk(rootExpression, '')
  for (const key of duplicates) {
    fail(label + ': 重复键 ' + key)
  }
  return { entries, duplicates, label }
}

/** 解析一个语言的全部文件并合并；跨文件重复定义直接报错（分片不得覆盖基础文件）。 */
function parseLocale(locale, files) {
  const merged = new Map()
  const origin = new Map()
  for (const filePath of files) {
    const { entries, label } = parseMessageFile(filePath)
    for (const [key, value] of entries) {
      if (merged.has(key)) {
        fail(
          locale + ': 键 ' + key + ' 同时定义在 ' + origin.get(key) + ' 与 ' + label +
            '（分片不得重复定义基础键，也不得与其它分片重名）'
        )
        continue
      }
      merged.set(key, value)
      origin.set(key, label)
    }
  }
  return { entries: merged, origin }
}

/* -------------------------------------------------------------- 源码扫描 */

function walkFiles(dir, predicate) {
  const result = []
  const stack = [dir]
  while (stack.length > 0) {
    const current = stack.pop()
    for (const name of readdirSync(current)) {
      const full = join(current, name)
      if (statSync(full).isDirectory()) {
        stack.push(full)
        continue
      }
      if (predicate(full)) {
        result.push(full)
      }
    }
  }
  return result.sort()
}

const CJK_PATTERN = /[\u3000-\u303f\u3400-\u4dbf\u4e00-\u9fff\uff00-\uffef]/

/** 去掉注释后再查硬编码文案（注释里的中文是合法的）。 */
function stripComments(text) {
  return text
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|[^:\\])\/\/[^\n]*$/gm, '$1')
}

function lineOf(text, index) {
  return text.slice(0, index).split('\n').length
}

/* ------------------------------------------------------------------ 主流程 */

const parts = discoverParts()
const localeFiles = new Map()
for (const locale of LOCALES) {
  localeFiles.set(locale, [join(I18N_DIR, BASE_FILES[locale]), ...(parts.files.get(locale) ?? [])])
}

// index.ts 必须真实引入每个分片（否则分片只是死文件，运行时拿不到文案）。
let indexSource = ''
try {
  indexSource = readFileSync(INDEX_FILE, 'utf8')
} catch {
  fail('src/i18n/index.ts 不可读')
}
for (const area of parts.areas) {
  for (const locale of LOCALES) {
    const importPath = './messages/' + area + '.' + locale
    if (!indexSource.includes(importPath)) {
      fail('src/i18n/index.ts 缺少分片 import: ' + importPath)
    }
  }
}

const source = parseLocale(SOURCE_LOCALE, localeFiles.get(SOURCE_LOCALE) ?? [])
const targets = TARGET_LOCALES.map((locale) => ({
  locale,
  parsed: parseLocale(locale, localeFiles.get(locale) ?? [])
}))

notes.push(
  'locales: ' + SOURCE_LOCALE + '=' + source.entries.size + ' keys, ' +
    targets.map((item) => item.locale + '=' + item.parsed.entries.size + ' keys').join(', ') +
    '; message files: ' + [...localeFiles.values()].reduce((sum, files) => sum + files.length, 0) +
    ' (parts: ' + parts.areas.length + ')'
)

for (const { locale, parsed } of targets) {
  for (const key of source.entries.keys()) {
    if (!parsed.entries.has(key)) {
      fail(locale + ': 缺失键 ' + key)
    }
  }
  for (const key of parsed.entries.keys()) {
    if (!source.entries.has(key)) {
      fail(locale + ': 多余键（源语言 ' + SOURCE_LOCALE + ' 没有） ' + key)
    }
  }
}

/** 已知命名空间 = 源语言顶层键，用于识别源码里的键字面量与过滤误报。 */
const namespaces = new Set([...source.entries.keys()].map((key) => key.split('.')[0]))
const KEY_LITERAL = /['"]([A-Za-z][A-Za-z0-9_]*(?:\.[A-Za-z0-9_]+)+)['"]/g

const sourceFiles = walkFiles(
  join(ROOT, 'src'),
  (file) => file.endsWith('.ts') || file.endsWith('.vue')
)
const scannedKeys = new Set()
let cjkFindings = 0

for (const file of sourceFiles) {
  const relativePath = relative(ROOT, file).replace(/\\/g, '/')
  const text = readFileSync(file, 'utf8')
  if (!relativePath.startsWith('src/i18n/')) {
    for (const match of text.matchAll(KEY_LITERAL)) {
      const key = match[1]
      if (!namespaces.has(key.split('.')[0])) {
        continue
      }
      scannedKeys.add(key)
      if (!source.entries.has(key)) {
        fail(relativePath + ': 引用了不存在的键 ' + key)
      }
      for (const { locale, parsed } of targets) {
        if (!parsed.entries.has(key)) {
          fail(relativePath + ': 键 ' + key + ' 在 ' + locale + ' 缺失')
        }
      }
    }
  }

  if (relativePath.startsWith('src/i18n/')) {
    continue
  }

  // 逐行扫描：注释行（* / // / /* / <!-- 开头）跳过。
  // 说明：不做整块注释剥离——doc 注释文本里可能出现 "*/" 序列（如 "aes67_*/jitter_frames"），
  // 正则式块注释剥离会提前终止并把剩余注释误判为代码；逐行前缀判定对代码库的行式注释足够稳。
  const lines = text.split(/\r?\n/)
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index]
    if (/^\s*(\*|\/\/|\/\*|<!--|-->)/.test(line)) {
      continue
    }
    const cjk = CJK_PATTERN.exec(line)
    if (cjk) {
      cjkFindings += 1
      fail(
        relativePath + ':' + String(index + 1) +
          ': 硬编码中文（必须改走 t()，注释除外）：' +
          JSON.stringify(line.slice(Math.max(0, cjk.index - 20), cjk.index + 20))
      )
    }
  }
}

notes.push('source files scanned: ' + sourceFiles.length + '; key literals resolved: ' + scannedKeys.size)

if (errors.length > 0) {
  console.error('i18n-check FAILED (' + errors.length + ')')
  for (const error of errors) {
    console.error('  ✗ ' + error)
  }
  process.exit(1)
}

console.log('i18n-check OK')
for (const note of notes) {
  console.log('  · ' + note)
}
console.log('  · key parity: ' + SOURCE_LOCALE + ' ↔ ' + TARGET_LOCALES.join(', '))
console.log('  · no duplicate keys, no empty values, no hardcoded CJK')