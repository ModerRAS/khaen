#!/usr/bin/env node
/**
 * i18n 键集校验（PLAN §12.2 门禁；lead 任务要求 node scripts/i18n-check.mjs 全绿）。
 *
 * 校验项：
 *  1. zh-CN（源语言）与 en-US 键集**完全对等**（缺失 / 多余）；
 *  2. 同一语言内无重复键（用 TS AST 检测对象字面量的重复属性）；
 *  3. 所有叶子值都是非空字符串（不允许空串 / 数字 / 嵌套对象漂移）；
 *  4. 源码里出现的 i18n 键字面量（以 zh-CN 顶层命名空间开头）在两种语言里都存在；
 *  5. src 下 .vue / .ts 不得硬编码中日韩文字（文案必须走 t()，注释不算）。
 *
 * 用法：node scripts/i18n-check.mjs
 * 退出码：0 = 通过；1 = 有错误（逐条打印）。
 */
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import ts from 'typescript'

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const I18N_DIR = join(ROOT, 'src', 'i18n')
const SOURCE_LOCALE = 'zh-CN'
const TARGET_LOCALES = ['en-US']

const errors = []
const notes = []

function fail(message) {
  errors.push(message)
}

/* ------------------------------------------------------------------ 解析 */

/** 把 i18n 的 TS 消息对象解析成扁平键表（并检测重复键 / 非字符串叶子）。 */
function parseLocale(fileName) {
  const file = join(I18N_DIR, fileName + '.ts')
  const text = readFileSync(file, 'utf8')
  const source = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS)
  const entries = new Map()
  const duplicates = []

  /** 找到 export default <identifier> 或 export default { ... }。 */
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
          if (ts.isIdentifier(declaration.name) && declaration.name.text === name && declaration.initializer) {
            rootExpression = declaration.initializer
          }
        }
      }
    })
  }
  if (!rootExpression || !ts.isObjectLiteralExpression(rootExpression)) {
    fail(fileName + ': 找不到导出的消息对象字面量')
    return { entries, duplicates }
  }

  function walk(objectLiteral, prefix) {
    for (const property of objectLiteral.properties) {
      let key = null
      if (ts.isIdentifier(property.name) || ts.isStringLiteral(property.name)) {
        key = property.name.text
      } else {
        fail(fileName + ': ' + (prefix || '<root>') + ' 有无法识别的键（仅支持标识符/字符串）')
        continue
      }
      const fullKey = prefix ? prefix + '.' + key : key
      if (!ts.isPropertyAssignment(property)) {
        fail(fileName + ': ' + fullKey + ' 不是普通属性赋值')
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
        fail(fileName + ': ' + fullKey + ' 的值必须是字符串字面量')
        continue
      }
      if (value.text.trim().length === 0) {
        fail(fileName + ': ' + fullKey + ' 的值不能为空')
      }
      entries.set(fullKey, value.text)
    }
  }

  walk(rootExpression, '')
  for (const key of duplicates) {
    fail(fileName + ': 重复键 ' + key)
  }
  return { entries, duplicates }
}

/* -------------------------------------------------------------- 源码扫描 */

function walkFiles(dir, predicate) {
  const result = []
  const stack = [dir]
  while (stack.length > 0) {
    const current = stack.pop()
    for (const name of readdirSync(current)) {
      const full = join(current, name)
      const stats = statSync(full)
      if (stats.isDirectory()) {
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

const source = parseLocale(SOURCE_LOCALE)
const targets = TARGET_LOCALES.map((locale) => ({ locale, parsed: parseLocale(locale) }))

notes.push('locales: ' + SOURCE_LOCALE + '=' + source.entries.size + ' keys, ' +
  targets.map((item) => item.locale + '=' + item.parsed.entries.size + ' keys').join(', '))

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

const sourceFiles = walkFiles(join(ROOT, 'src'), (file) => file.endsWith('.ts') || file.endsWith('.vue'))
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

  // i18n 目录本身就是文案存放处（含 zh-CN 源语言与 en-US 的「简体中文」标签），跳过硬编码扫描。
  if (relativePath.startsWith('src/i18n/')) {
    continue
  }

  const withoutComments = stripComments(text)
  const cjk = CJK_PATTERN.exec(withoutComments)
  if (cjk) {
    cjkFindings += 1
    fail(relativePath + ':' + lineOf(withoutComments, cjk.index) +
      ': 硬编码中文（必须改走 t()，注释除外）：' + JSON.stringify(withoutComments.slice(Math.max(0, cjk.index - 20), cjk.index + 20)))
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
