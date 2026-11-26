import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

/**
 * 中央契约测试：webui 实际使用的端点必须与 docs/API.md（唯一 API 合同）一致。
 *
 * 设计：
 * - 从 docs/API.md 的 markdown 表格解析出「方法 + 路径」端点集合（支持 POST/PUT 双方法、`{id}` 占位符）；
 * - 下方 USED 是 webui 代码里真实发出的端点清单（逐页面/逐模块核对过；各区域的 mock 契约测试
 *   进一步断言 path/method/body 与 docs 完全一致）；
 * - 任何 USED 端点不在 docs/API.md 里 → 失败（合同漂移/偷偷加端点）；
 * - 反向不强制（后端存在前端尚未用的端点，例如 /api/auth/setup 之外的管理端点）。
 *
 * 前端不得单边改 docs/API.md：缺端点先消息 lead 补文档，再改这里。
 */

const HERE = dirname(fileURLToPath(import.meta.url))
const DOC_PATH = resolve(HERE, '..', '..', 'docs', 'API.md')

/** 文档里声明的端点（method + path，保留 `{id}` 占位符）。 */
function documentedEndpoints(markdown: string): Set<string> {
  const endpoints = new Set<string>()
  const pathPattern = /`(\/[A-Za-z0-9_\/{}.-]*)/g
  const add = (methods: string[], path: string): void => {
    for (const method of methods) {
      endpoints.add(method + ' ' + path.split('?')[0])
    }
  }
  const methodsOf = (cell: string): string[] =>
    cell
      .toUpperCase()
      .split('/')
      .filter((method) => /^(GET|POST|PUT|PATCH|DELETE)$/.test(method))
  for (const line of markdown.split(/\r?\n/)) {
    // 表格行：| GET | `/api/...` | ...
    if (line.startsWith('|')) {
      const cells = line.split('|').map((cell) => cell.trim())
      if (cells.length < 3) {
        continue
      }
      const methods = methodsOf(cells[1])
      if (methods.length === 0) {
        continue
      }
      for (const match of cells[2].matchAll(pathPattern)) {
        add(methods, match[1])
      }
      continue
    }
    // 列表行：- GET `/api/...`：说明 / - POST/PUT `/api/...`
    const bullet = /^\s*[-*]\s+([A-Za-z/]+)\s+`(\/[A-Za-z0-9_\/{}.-]*)/.exec(line)
    if (bullet !== null) {
      const methods = methodsOf(bullet[1])
      if (methods.length > 0) {
        add(methods, bullet[2])
      }
    }
  }
  return endpoints
}

/** webui 实际使用的端点（按 docs/API.md 章节顺序）。 */
const USED: Array<{ method: string; path: string; where: string }> = [
  // §2 认证与元信息
  { method: 'GET', path: '/healthz', where: 'AppShell/版本 store（release 版本）' },
  { method: 'GET', path: '/api/auth/status', where: 'LoginView/session store' },
  { method: 'POST', path: '/api/auth/setup', where: 'LoginView 初始化' },
  { method: 'POST', path: '/api/auth/login', where: 'LoginView 登录' },
  { method: 'GET', path: '/api/auth/session', where: 'session store' },
  { method: 'POST', path: '/api/auth/logout', where: 'AppShell 登出' },
  { method: 'POST', path: '/api/auth/password', where: '旧改密接口（兼容）' },
  { method: 'GET', path: '/api/meta', where: 'stores/meta 能力快照' },
  // §3 只读视图
  { method: 'GET', path: '/api/status', where: '概览/顶栏/各页面轮询' },
  { method: 'GET', path: '/api/plan/history', where: '方案历史/回滚' },
  { method: 'POST', path: '/api/plan/inspect', where: '方案上传后只读预览' },
  { method: 'GET', path: '/api/motu', where: 'VolumeView MOTU 设备状态' },
  { method: 'GET', path: '/api/calibration/wizard', where: '校准向导配置' },
  { method: 'GET', path: '/api/calibration/wizard/state', where: '校准向导 8 步状态' },
  { method: 'GET', path: '/api/system/info', where: '系统信息页' },
  { method: 'GET', path: '/api/system/logs', where: '系统日志页' },
  { method: 'GET', path: '/api/system/diagnostics', where: '诊断包下载' },
  // §4 控制面（两阶段）
  { method: 'POST', path: '/api/mute', where: '静音（立即生效）' },
  { method: 'POST', path: '/api/prepare', where: '方案草稿' },
  { method: 'POST', path: '/api/commit', where: '两阶段提交' },
  { method: 'POST', path: '/api/abort', where: '两阶段丢弃' },
  { method: 'POST', path: '/api/surface', where: 'volume/routing/layout/mode 第一阶段' },
  { method: 'POST', path: '/api/rollback', where: '方案回滚草稿' },
  { method: 'POST', path: '/api/format', where: '格式两阶段' },
  { method: 'POST', path: '/api/device/retry', where: '设备监督器重试' },
  // §4.1 MOTU（无 abort；本地取消 + TTL）
  { method: 'PUT', path: '/api/motu/volume', where: 'MOTU 音量两阶段（prepare/commit）' },
  // §5 /api/channels 数据面热重配
  { method: 'GET', path: '/api/channels', where: '方案与声道页/路由当前值' },
  { method: 'PUT', path: '/api/channels', where: '热重配 prepare/commit/abort' },
  // §6 方案上传
  { method: 'POST', path: '/api/plan/upload', where: '方案文件上传' },
  // §7 校准
  { method: 'POST', path: '/api/calibration/uploads', where: '校准 WAV 上传' },
  { method: 'GET', path: '/api/calibration/jobs', where: '校准 job 列表' },
  { method: 'POST', path: '/api/calibration/jobs', where: '校准 job 创建' },
  { method: 'GET', path: '/api/calibration/jobs/{id}', where: '校准 job 详情' },
  { method: 'POST', path: '/api/calibration/jobs/{id}/cancel', where: '校准 job 取消' },
  { method: 'POST', path: '/api/calibration/jobs/{id}/apply', where: '校准结果 apply/rollback 第一阶段' },
  { method: 'POST', path: '/api/calibration/jobs/{id}/rollback', where: '校准结果 rollback' },
  { method: 'GET', path: '/api/calibration/jobs/{id}/artifacts/{name}', where: '校准产物下载' },
  { method: 'PUT', path: '/api/calibration/wizard', where: '校准向导配置保存' },
  { method: 'POST', path: '/api/calibration/wizard/apply', where: '校准向导 apply' },
  // §12 音频通道 profile / Speaker Layout（v1.2 新功能，后端未接线时如实显示 not_wired）
  { method: 'GET', path: '/api/audio-profile', where: '音频映射页 profile/校验/能力' },
  { method: 'PUT', path: '/api/audio-profile', where: '音频映射两步保存（prepare/commit/abort）' },
  { method: 'POST', path: '/api/audio-profile/plan', where: 'profile → 方案生成（之后走 prepare/commit）' },
  { method: 'GET', path: '/api/device/inputs', where: '声卡输入能力（只读）' },
  { method: 'POST', path: '/api/speaker-test', where: '通道识别测试音（需 audible+confirm）' },
  { method: 'POST', path: '/api/speaker-test/stop', where: '立即停止测试音（幂等）' },
  // §8 系统
  { method: 'POST', path: '/api/system/password', where: '系统页改密' },
  { method: 'POST', path: '/api/system/restart', where: '系统页重启 rt-host' }
]

/**
 * 文档路径占位符（{id} 等）与声明路径比较：占位符匹配一个非空路径段。
 * 两侧都允许占位符时按正则匹配。
 */
function matches(documented: Set<string>, method: string, path: string): boolean {
  const key = method + ' ' + path
  if (documented.has(key)) {
    return true
  }
  const pattern = new RegExp(
    '^' +
      key
        .split('/')
        .map((segment) => {
          if (/^\{[^}]+\}$/.test(segment)) {
            return '[^/]+'
          }
          return segment.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
        })
        .join('/') +
      '$'
  )
  for (const entry of documented) {
    if (pattern.test(entry)) {
      return true
    }
  }
  return false
}

test('docs/API.md is readable and declares endpoints', () => {
  const markdown = readFileSync(DOC_PATH, 'utf8')
  const documented = documentedEndpoints(markdown)
  assert.ok(documented.size >= 30, 'expected the API doc to declare its endpoint tables (got ' + documented.size + ')')
  assert.ok(documented.has('GET /api/meta'), 'GET /api/meta must be documented')
  assert.ok(documented.has('GET /api/motu'), 'GET /api/motu must be documented')
  assert.ok(documented.has('PUT /api/motu/volume'), 'PUT /api/motu/volume must be documented')
  assert.ok(documented.has('PUT /api/channels'), 'PUT /api/channels must be documented')
  assert.ok(documented.has('POST /api/calibration/jobs/{id}/apply'), 'job-scoped apply must be documented')
})

test('every endpoint the webui calls is declared in docs/API.md', () => {
  const documented = documentedEndpoints(readFileSync(DOC_PATH, 'utf8'))
  const missing = USED.filter((entry) => !matches(documented, entry.method, entry.path))
  assert.deepEqual(
    missing.map((entry) => entry.method + ' ' + entry.path + ' (' + entry.where + ')'),
    [],
    'endpoints used by the webui but missing from docs/API.md'
  )
})

test('the webui does not call the removed mock-only calibration endpoints', () => {
  const usedPaths = new Set(USED.map((entry) => entry.path))
  assert.equal(usedPaths.has('/api/calibration/apply'), false)
  assert.equal(usedPaths.has('/api/calibration/rollback'), false)
})