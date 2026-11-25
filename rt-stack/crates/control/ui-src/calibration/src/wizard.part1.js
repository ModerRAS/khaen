/* =========================================================
 * KHAEN Audio · Dirac 式完整校准流程向导（演示）
 * 11 步：设备检查 → 布局分配 → 麦克风/校准设置 → Level 对齐 →
 *        Delay 对齐 → Bass management → 测点 → 测量 → 分析 →
 *        设计（目标/低切/ART/MIMO） → 回顾/应用/回滚
 * 数据：pilot 板端实测 FR/RT60/waterfall/SNR + 明确标注的示意数据
 * 交互参考 Dirac Live 公开文档，界面/代码自制，不使用其商标素材
 * ======================================================= */
'use strict';

const PILOT = /*__PILOT__*/ null;
const CH_ALL = ['FL', 'FR', 'SL', 'SR', 'HFL', 'HFR', 'SUB1'];
const CH_LABEL = { FL: '主箱 L', FR: '主箱 R', SL: '环绕 L', SR: '环绕 R', HFL: '天空 L', HFR: '天空 R', SUB1: '低音炮' };
const CH_POS = { FL: '前左', FR: '前右', SL: '侧左', SR: '侧右', HFL: '前上左', HFR: '前上右', SUB1: '前下' };

/* ---------- tiny utils ---------- */
const $ = (s, r) => (r || document).querySelector(s);
const $$ = (s, r) => Array.from((r || document).querySelectorAll(s));
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const round = (v, n) => Math.round(v * Math.pow(10, n || 0)) / Math.pow(10, n || 0);
const fmt = (v, n) => Number(v).toFixed(n == null ? 1 : n);
const log2 = Math.log2;
const LOG_MIN = Math.log(20), LOG_MAX = Math.log(20000);
const fToU = f => (Math.log(clamp(f, 20, 20000)) - LOG_MIN) / (LOG_MAX - LOG_MIN);
const uToF = u => Math.exp(LOG_MIN + clamp(u, 0, 1) * (LOG_MAX - LOG_MIN));
function mean(a) { return a.reduce((s, x) => s + x, 0) / a.length; }
function sleep(ms) { return new Promise(r => setTimeout(r, ms)); }
function roundRect(ctx, x, y, w, h, r) {
  r = Math.min(r, Math.abs(w) / 2, Math.abs(h) / 2);
  ctx.beginPath(); ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r); ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r); ctx.arcTo(x, y, x + w, y, r); ctx.closePath();
}

/* ---------- pilot data → curves ---------- */
const FR = {};        // 归一化到 200–2000 Hz 均值的全频曲线（分析用）
const FR_BASS = {};   // 归一化到 100–200 Hz 均值的 20–200 Hz 曲线（设计用）
for (const ch of CH_ALL) {
  const c = PILOT.fr[ch];
  const band = c.freq.map((f, i) => [f, c.db_smooth[i]]).filter(p => p[0] >= 200 && p[0] <= 2000).map(p => p[1]);
  const ref = mean(band);
  FR[ch] = { freq: c.freq, db: c.db_smooth.map(v => round(v - ref, 2)), raw: c.db_raw.map(v => round(v - ref, 2)) };
  const bassRef = c.ref100_200;
  FR_BASS[ch] = { freq: c.freq, db: c.db_smooth.map(v => round(v - bassRef, 2)) };
}
const detectedOf = ch => PILOT.fr[ch].detected10_hz;
const GROUPS = {
  mains: { label: '主箱 L/R', channels: ['FL', 'FR'], detected: (PILOT.rolloff && PILOT.rolloff.groups.mains) || 45 },
  surround: { label: '环绕 L/R', channels: ['SL', 'SR'], detected: (PILOT.rolloff && PILOT.rolloff.groups.surround) || 60 },
  height: { label: '天空 L/R', channels: ['HFL', 'HFR'], detected: (PILOT.rolloff && PILOT.rolloff.groups.height) || 60 },
};
const CURVE_BASS = {};
for (const k in GROUPS) {
  const [a, b] = GROUPS[k].channels;
  CURVE_BASS[k] = FR_BASS[a].db.map((v, i) => 10 * Math.log10((Math.pow(10, v / 10) + Math.pow(10, FR_BASS[b].db[i] / 10)) / 2));
}
function dbAt(freqs, arr, f) {
  let bi = 0; for (let i = 0; i < freqs.length; i++) if (Math.abs(freqs[i] - f) < Math.abs(freqs[bi] - f)) bi = i;
  return arr[bi];
}

/* ---------- state ---------- */
const ROLE_ORDER = ['FL', 'FR', 'SL', 'SR', 'HFL', 'HFR', 'SUB1'];
const BOARD_ROLES = [
  { id: 'FL', out: 0, label: 'Front L' }, { id: 'FR', out: 1, label: 'Front R' },
  { id: 'SL', out: 2, label: 'Surround L' }, { id: 'SR', out: 3, label: 'Surround R' },
  { id: 'HFL', out: 4, label: 'Height L' }, { id: 'HFR', out: 5, label: 'Height R' },
  { id: 'SUB1', out: 7, label: 'Subwoofer' },
];
const initialPoints = () => [
  { id: 'P1', x: 0, y: 0.35, sweet: true, on: true },
  { id: 'P2', x: -0.9, y: 0.1, sweet: false, on: true },
  { id: 'P3', x: 0.9, y: 0.1, sweet: false, on: true },
  { id: 'P4', x: -0.9, y: -0.9, sweet: false, on: true },
  { id: 'P5', x: 0, y: -0.9, sweet: false, on: true },
  { id: 'P6', x: 0.9, y: -0.9, sweet: false, on: true },
  { id: 'P7', x: -1.6, y: -0.4, sweet: false, on: true },
  { id: 'P8', x: 1.6, y: -0.4, sweet: false, on: true },
  { id: 'P9', x: 0, y: -1.6, sweet: false, on: true },
];
const defaultArtConns = () => {
  const c = {};
  const add = (r, col, level, low, high) => { c[r + '<-' + col] = { on: true, level, low, high }; };
  add('FL', 'FR', -6, 80, 150); add('FL', 'SUB1', -12, 20, 120);
  add('FR', 'FL', -6, 80, 150); add('FR', 'SUB1', -12, 20, 120);
  add('SL', 'SR', -6, 80, 150); add('SL', 'SUB1', -12, 20, 120);
  add('SR', 'SL', -6, 80, 150); add('SR', 'SUB1', -12, 20, 120);
  add('LFE', 'SUB1', -18, 20, 120);
  return c;
};
const DEFAULT_STATE = () => ({
  step: 0,
  maxStep: 0,
  completed: [],
  check1: { started: false, done: false, busy: false, items: { motu: 'idle', umik: 'idle', wiring: 'idle', service: 'idle' } },
  layout: { roles: BOARD_ROLES.map(r => ({ ...r })), checked: false },
  check3: { started: false, done: false, busy: false, fixed: false, levels: {} },
  settings: {
    mic: { kind: 'usb', device: 'hw:U18dB,0', channel: 0, phantom_48v: false, gain_db: 18 },
    cal_file: null,
    orientation_deg: 90,
    target_mode: 'flat',
    bass: { enabled: true, crossover_hz: 80, main_slope: 'LR4', sub_slope: 'LR4', linear_phase: true, sub_gain_db: 0, strategy: 'multi_b', subs: [], routing: { mode: 'all_to_all', lfe_weights: [0.34, 0.33, 0.33], left_weights: [0.5, 0.25, 0.25], right_weights: [0.25, 0.25, 0.5], stereo_bias: 0 }, predicted_peak_db: 0, pre_gain_db: 0 },
    fir: { enabled: true, target_curve: 'flat', custom_curve: [], taps: 4096, phase: 'mixed' },
    modal: { enabled: true, auto_detect: true, max_notches: 4, disabled_lanes: [] }
  },
  settingsValidation: null,
  settingsSaved: false,
  fir: { jobId: null, running: false, skipped: false, applied: false, lanes: [], progressText: '', error: null },
  modal: { jobId: null, running: false, skipped: false, applied: false, lanes: [], progressText: '', error: null },
  runner: { running: false, dryRun: true, stages: [], results: [], current: null, failedStage: null, error: null, done: 0, total: 0, progressText: '' },
  level: { jobId: null, running: false, skipped: false, applied: false, lanes: [], progressText: '', error: null },
  delay: { jobId: null, running: false, skipped: false, applied: false, lanes: [], progressText: '', error: null },
  bass: { jobId: null, running: false, skipped: false, applied: false, result: null, progressText: '', error: null },
  jobId: null,
  points: initialPoints(),
  measure: { started: false, running: false, done: [], low: [], current: null, remeasuring: false },
  analysis: { seen: [], tab: 'fr', channel: 'FL' },
  design: {
    target: null, group: 'mains',
    cut: { mains: 55, surround: 60 }, slope: { mains: 24, surround: 24 },
    art: defaultArtConns(), artCell: { row: 'FL', col: 'SUB1' }, fsiso: 150,
    mimo: { mains: -6, surround: -6, height: -12, sub: -12 },
    fHi: 150, supports: [],
    touched: false, applied: false, rolledBack: false,
  },
});
// 正式页面：所有设置/测量/求解/复测都走 daemon 的 /api/calibration/*（不再有 ?live=1 演示分支）。
const KHAEN_LIVE = true;
const CAL_LANES = ['FL', 'FR', 'SL', 'SR', 'HFL', 'HFR', 'SUB1'];
const CAL_API = {
  settings: '/api/calibration/settings',
  status: '/api/calibration/status',
  jobs: '/api/calibration/jobs',
  apply: '/api/calibration/apply',
  clear: '/api/calibration/clear',
  run: '/api/calibration/run',
};
let KHAEN_CSRF = null;
function khaenErrorText(error) {
  if (!error) return 'unknown error';
  if (error.code) return error.code + ': ' + error.message;
  if (error.message) return error.message;
  return String(error);
}
async function khaenApi(method, path, body) {
  const headers = { Accept: 'application/json' };
  if (body !== undefined && body !== null) headers['Content-Type'] = 'application/json';
  if (method !== 'GET' && KHAEN_CSRF) headers['X-KHAEN-CSRF'] = KHAEN_CSRF;
  const response = await fetch(path, {
    method,
    headers,
    credentials: 'same-origin',
    body: body === undefined || body === null ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let json = null;
  if (text) { try { json = JSON.parse(text); } catch (error) { json = null; } }
  if (!response.ok) {
    const detail = json && json.error ? json.error.code + ': ' + json.error.message : 'HTTP ' + response.status;
    const error = new Error(detail);
    error.status = response.status;
    error.code = json && json.error ? json.error.code : '';
    error.body = json;
    throw error;
  }
  return json;
}
async function khaenLoadSession() {
  try {
    const session = await khaenApi('GET', '/api/auth/session');
    KHAEN_CSRF = session && session.csrf_token ? session.csrf_token : null;
    return true;
  } catch (error) {
    return false;
  }
}
function khaenJobBody(stage) {
  const body = {
    kind: 'emperor',
    stage: stage,
    audible_sweep_authorized: true,
    f_hi: Number(state.design.fHi) || 150,
    supports: state.design.supports.slice(),
    target: (state.settings && state.settings.target_mode) || state.design.target || 'flat',
    lanes: CAL_LANES.slice(),
    reference_lane: 'FL',
    target_dbfs: -12,
  };
  if (state.settings) body.settings = state.settings;
  if (state.jobId) body.job_id = state.jobId;
  return body;
}
async function khaenPostJob(stage) {
  const payload = await khaenApi('POST', CAL_API.jobs, khaenJobBody(stage));
  state.jobId = payload.job_id;
  state.lastJob = payload;
  return payload;
}
// 复用现有 job 状态机：轮询 GET /api/calibration/jobs/{id}（不另起一套 job/轮询机制）。
function khaenPollJob(jobId, onUpdate) {
  let tries = 0;
  return new Promise(function (resolve, reject) {
    function tick() {
      khaenApi('GET', CAL_API.jobs + '/' + encodeURIComponent(jobId)).then(function (job) {
        if (onUpdate) onUpdate(job);
        const terminal = ['complete', 'failed', 'cancelled', 'rolled_back'].indexOf(job.state) >= 0;
        tries += 1;
        if (terminal) { resolve(job); return; }
        if (tries > 120) { reject(new Error('poll timeout: ' + jobId)); return; }
        setTimeout(tick, 700);
      }).catch(reject);
    }
    tick();
  });
}
let state = DEFAULT_STATE();
function saveSnapshot() { state.design.snapshotBeforeApply = JSON.parse(JSON.stringify({ design: state.design, layout: state.layout, points: state.points })); }
function getConn(r, col) {
  const k = r + '<-' + col;
  if (!state.design.art[k]) state.design.art[k] = { on: false, level: 0, low: 80, high: 150 };
  return state.design.art[k];
}
const ART_ROWS = ['FL', 'FR', 'SL', 'SR', 'LFE'];
const ART_COLS = ['FL', 'FR', 'SL', 'SR', 'SUB1'];

/* ---------- step definitions ---------- */
const STEPS = [
  { id: 'device', title: '设备与房间检查', sub: '确认 MOTU UltraLite-mk5、UMIK-1、输出线序与服务状态，全部只读检测。', tag: 'Check', badges: ['只读检测'] },
  { id: 'layout', title: '扬声器布局与分配', sub: '4.1.2 七角色：FL / FR / SL / SR / HFL / HFR / SUB1 → MOTU 输出通道。', tag: 'Arrangement', badges: ['板端映射'] },
  { id: 'mic', title: '麦克风与校准设置 / 一键运行', sub: 'USB / 48V 模拟输入、device/channel、校准文件（SHA-256）、朝向、目标模式；一键按链序运行全部阶段。', tag: 'Volume Calibration', badges: ['设置下发', '一键运行'] },
  { id: 'bass', title: '① Crossover / Bass', sub: '链序第 1 步：线性相位低音炮分频 + 多炮 B 方案（subs[] + 路由矩阵）。', tag: 'Bass Management', badges: ['护栏', '多炮'] },
  { id: 'fir', title: '② 全频 FIR 校正', sub: '每 lane 全频 FIR：flat/tilt10/custom、taps、mixed/linear phase、期望幅频误差/群延迟/峰值。', tag: 'Full-band FIR', badges: ['目标曲线', 'taps 上限'] },
  { id: 'modal', title: '③ Modal EQ', sub: '每 lane 检测模态频率/Q/削减；结果表与每 lane 开关；上限校验。', tag: 'Modal EQ', badges: ['模态', '上限'] },
  { id: 'level', title: '④ Level 对齐', sub: '逐箱从静音爬坡到参考电平；每 lane 读数、SNR、锁存增益与失败原因。', tag: 'Level Alignment', badges: ['闭环爬坡', 'SNR 门'] },
  { id: 'delay', title: '⑤ Delay 对齐', sub: '逐箱 onset（双门限）；LFE 用 40–150 Hz 与 FL 互相关；显示相对/施加样本数。', tag: 'Delay Alignment', badges: ['onset', 'LFE xcorr'] },
  { id: 'points', title: '测点选择', sub: '听音位 + 多点示意；本路径只点亮皇帝位，ART 多点要求留在页面上。', tag: 'Measure Setup', badges: ['皇帝位'] },
  { id: 'measure', title: '⑥ Emperor / A-B 复测', sub: '扫频测量 + 复测；从 daemon 下发 stage=measure/remeasure。', tag: 'Measure', badges: ['真实任务'] },
  { id: 'analysis', title: '测量分析', sub: 'FR / IR / 3D 瀑布 CSD / RT60（低频）/ 模态；FR、CSD、RT60、模态为实测，IR 为重建示意。', tag: 'Analysis', badges: ['实测', '部分示意'] },
  { id: 'design', title: '滤波器设计', sub: '目标曲线 flat / tilt10、低切、ART/MIMO 衰减量，实时预测与警告。', tag: 'Filter Design', badges: ['实测驱动', '示意'] },
  { id: 'review', title: '回顾 / 应用 / 回滚', sub: 'before/after 对比、应用前确认、导出配置与回滚确认。', tag: 'Export', badges: ['应用状态'] },
];

/* ---------- shared UI ---------- */
let capTimer = null;
function caption(n, text, ms) {
  const c = $('#caption');
  c.innerHTML = (n != null ? '<span class="n">' + n + '</span>' : '') + text;
  c.hidden = false;
  c.style.animation = 'none'; void c.offsetWidth; c.style.animation = '';
  clearTimeout(capTimer);
  capTimer = setTimeout(() => { c.hidden = true; }, ms || 5200);
}
let toastTimer = null;
function toast(text) {
  const t = $('#toast'); t.textContent = text; t.hidden = false;
  clearTimeout(toastTimer); toastTimer = setTimeout(() => { t.hidden = true; }, 2800);
}
const cursor = $('#cursor');
window.addEventListener('pointermove', e => { cursor.classList.add('on'); cursor.style.left = e.clientX + 'px'; cursor.style.top = e.clientY + 'px'; });
window.addEventListener('pointerdown', () => cursor.classList.add('down'));
window.addEventListener('pointerup', () => { cursor.classList.remove('down'); cursor.classList.add('click'); setTimeout(() => cursor.classList.remove('click'), 450); });

function showModal(opts) {
  const m = $('#modal');
  const card = $('#modal-card');
  card.className = 'modal' + (opts.wide ? ' wide' : '') + (opts.narrow ? ' narrow2' : '');
  $('#modal-title').textContent = opts.title || '';
  $('#modal-body').innerHTML = opts.body || '';
  const foot = $('#modal-foot'); foot.innerHTML = '';
  (opts.buttons || [{ label: '关闭', cls: 'ghost', act: hideModal }]).forEach(b => {
    const btn = document.createElement('button');
    btn.className = 'btn ' + (b.cls || 'ghost');
    btn.dataset.test = b.test || '';
    btn.textContent = b.label;
    btn.addEventListener('click', b.act || hideModal);
    foot.appendChild(btn);
  });
  m.hidden = false;
}
function hideModal() { $('#modal').hidden = true; }
$('#modal-x').addEventListener('click', hideModal);
$('#modal').addEventListener('click', e => { if (e.target === $('#modal')) hideModal(); });

/* ---------- canvas helpers ---------- */
function setupCanvas(cv) {
  const r = cv.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  cv.width = Math.max(1, Math.round(r.width * dpr));
  cv.height = Math.max(1, Math.round(r.height * dpr));
  const ctx = cv.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  return { ctx, w: r.width, h: r.height };
}
function gridFreq(ctx, L, R, T, B, fmin, fmax, ticks, labels) {
  ctx.strokeStyle = 'rgba(120,140,170,0.10)'; ctx.lineWidth = 1;
  ctx.font = '9.5px "Cascadia Mono", Consolas, monospace';
  ctx.textAlign = 'center'; ctx.textBaseline = 'top';
  for (const f of ticks) {
    const u = (Math.log(f) - Math.log(fmin)) / (Math.log(fmax) - Math.log(fmin));
    const x = L + u * (R - L);
    ctx.beginPath(); ctx.moveTo(x, T); ctx.lineTo(x, B); ctx.stroke();
    ctx.fillStyle = 'rgba(140,158,184,0.55)';
    ctx.fillText(labels ? labels[f] || String(f) : (f >= 1000 ? (f / 1000) + 'k' : String(f)), x, B + 6);
  }
}
function drawLine(ctx, pts, mapX, mapY, color, width, dash, alpha) {
  ctx.save(); ctx.globalAlpha = alpha == null ? 1 : alpha;
  ctx.strokeStyle = color; ctx.lineWidth = width; ctx.lineJoin = 'round';
  if (dash) ctx.setLineDash(dash);
  ctx.beginPath();
  pts.forEach((p, i) => { const x = mapX(p[0]), y = mapY(p[1]); if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y); });
  ctx.stroke(); ctx.restore();
}

/* ---------- filter math (LR) ---------- */
function bwLP(Om) { const d0 = 1 - Om * Om, d1 = Math.SQRT2 * Om, dd = d0 * d0 + d1 * d1; return [d0 / dd, -d1 / dd]; }
function bwHP(Om) { const d0 = 1 - Om * Om, d1 = Math.SQRT2 * Om, dd = d0 * d0 + d1 * d1; return [-Om * Om * d0 / dd, Om * Om * d1 / dd]; }
function cmul(a, b) { return [a[0] * b[0] - a[1] * b[1], a[0] * b[1] + a[1] * b[0]]; }
function cpow(a, n) { let r = [1, 0]; for (let i = 0; i < n; i++) r = cmul(r, a); return r; }
const cdb = c => 20 * Math.log10(Math.hypot(c[0], c[1]) + 1e-12);

/* ---------- navigation ---------- */
const STEP = {};
STEPS.forEach((s, i) => { STEP[s.id] = i; });
function stepValid(i) {
  switch (i) {
    case STEP.device: return { ok: state.check1.done, msg: state.check1.done ? '设备与房间检查通过' : '请先点击“开始检测”完成 4 项检查' };
    case STEP.layout: {
      const outs = state.layout.roles.map(r => r.out);
      const unique = new Set(outs).size === outs.length;
      const noCh6 = !outs.includes(6);
      const ok = state.layout.checked && unique && noCh6 && state.layout.roles.length === 7;
      return { ok, msg: ok ? '布局映射已校验：7/7 角色唯一、ch6 保留空' : (state.layout.checked ? '存在重复输出或 ch6 被占用，请修正后重新校验' : '请点击“校验布局映射”') };
    }
    case STEP.mic: {
      const ok = state.settingsSaved === true;
      const label = state.settingsValidation && state.settingsValidation.label ? state.settingsValidation.label : '未校验';
      return { ok, msg: ok ? '校准设置已保存 · ' + label : '请先填写并保存麦克风/校准设置（绝对/相对按校准文件 SHA-256 判定）' };
    }
    case STEP.level: {
      const ok = state.level.applied || state.level.skipped;
      return { ok, msg: ok ? (state.level.applied ? 'Level 对齐已应用' : 'Level 对齐已跳过（pending host 字段；结果不标 applied）') : (state.level.error ? 'Level 对齐失败：' + state.level.error : '请启动 Level 对齐并应用/跳过') };
    }
    case STEP.delay: {
      const ok = state.delay.applied || state.delay.skipped;
      return { ok, msg: ok ? (state.delay.applied ? 'Delay 对齐已应用' : 'Delay 对齐已跳过（pending host 字段；结果不标 applied）') : (state.delay.error ? 'Delay 对齐失败：' + state.delay.error : '请启动 Delay 对齐并应用/跳过') };
    }
    case STEP.bass: {
      if (!state.settings.bass.enabled) return { ok: true, msg: 'Bass management 未启用（设置里可开启）' };
      const ok = state.bass.applied || state.bass.skipped;
      const guard = state.bass.result && state.bass.result.bass;
      return { ok, msg: ok ? (state.bass.applied ? 'Bass management 已应用' : 'Bass management 已跳过（pending host 字段；结果不标 applied）') : (state.bass.error ? 'Bass management 失败：' + state.bass.error : '请启动 Bass 分频并查看护栏' + (guard ? '' : '')) };
    }
    case STEP.fir: {
      const ok = state.fir.applied || state.fir.skipped;
      return { ok, msg: ok ? (state.fir.applied ? '全频 FIR 已应用' : '全频 FIR 已跳过（pending host 字段）') : (state.fir.error ? '全频 FIR 失败：' + state.fir.error : '请启动全频 FIR 并应用/跳过') };
    }
    case STEP.modal: {
      const ok = state.modal.applied || state.modal.skipped;
      return { ok, msg: ok ? (state.modal.applied ? 'Modal EQ 已应用' : 'Modal EQ 已跳过（pending host 字段）') : (state.modal.error ? 'Modal EQ 失败：' + state.modal.error : '请启动 Modal EQ 并应用/跳过') };
    }
    case STEP.points: {
      const sweet = state.points.some(p => p.on && p.sweet);
      const n = state.points.filter(p => p.on).length;
      return { ok: sweet, msg: sweet ? '皇帝位已点亮（' + n + ' 点）。ART 多点要求留在页面上，这次只测皇帝位' : '至少包含听音位' };
    }
    case STEP.measure: {
      const n = state.measure.done.length;
      const ok = !!state.measure.liveAck;
      return { ok, msg: ok ? '皇帝位测量任务已完成（' + n + ' 点）' : (state.jobError || '请启动测量（daemon stage=measure）') };
    }
    case STEP.analysis: {
      const seen = state.analysis.seen.length;
      return { ok: seen >= 5, msg: seen >= 5 ? '已完成 5 类分析（FR/IR/瀑布/RT60/模态）' : '请依次查看全部 5 个分析视图（已看 ' + seen + '/5）' };
    }
    case STEP.design: {
      const d = state.design;
      return { ok: !!d.target, msg: d.target ? '目标 ' + d.target + '；低切只保留滚降警告，不生成分频' : '请先选择目标曲线（flat / tilt10）' };
    }
    case STEP.review: return { ok: true, msg: '完成回顾并应用/回滚' };
    default: return { ok: false, msg: '' };
  }
}
function renderRail() {
  const ol = $('#step-list'); ol.innerHTML = '';
  STEPS.forEach((s, i) => {
    const li = document.createElement('li');
    li.className = 'step-item' + (i === state.step ? ' active' : '') + (state.completed.includes(i) ? ' done' : '') + (i > state.maxStep ? ' locked' : '');
    li.dataset.test = 'rail-step-' + i;
    li.innerHTML = '<span class="num">' + (state.completed.includes(i) ? '✓' : (i + 1)) + '</span><span class="st"><b>' + s.title + '</b><small>' + s.tag + '</small></span><span class="chk">✓</span>';
    li.addEventListener('click', () => { if (i <= state.maxStep || state.completed.includes(i)) goStep(i); });
    ol.appendChild(li);
  });
}
function renderHeader() {
  const s = STEPS[state.step];
  $('#step-crumb').innerHTML = 'STEP <b>' + (state.step + 1) + '</b> / ' + STEPS.length;
  $('#step-title').innerHTML = s.title + '<span class="tag">' + s.tag + '</span>';
  $('#step-sub').textContent = s.sub;
  $('#step-badges').innerHTML = s.badges.map(b => '<span class="chip ' + (/实测/.test(b) ? 'real' : /示意/.test(b) ? 'sim' : 'model') + '">' + b + '</span>').join('');
  $('#step-progress').innerHTML = STEPS.map((_, i) => '<i class="' + (i < state.step ? 'done' : i === state.step ? 'on' : '') + '"></i>').join('');
  const v = stepValid(state.step);
  const gate = $('#gate');
  gate.className = 'gate ' + (v.ok ? 'ok' : '');
  gate.querySelector('i').textContent = v.ok ? '✓' : '!';
  $('#gate-text').textContent = v.msg;
  $('#btn-back').disabled = state.step === 0;
  $('#btn-back').style.opacity = state.step === 0 ? '.4' : '1';
  $('#btn-next').textContent = state.step === STEPS.length - 1 ? '完成流程' : '下一步';
}
const RENDER = [];
function renderStep() { RENDER[state.step](); renderRail(); renderHeader(); }
function goStep(i) {
  if (i >= 0 && i < STEPS.length) { state.step = i; state.maxStep = Math.max(state.maxStep, i); renderStep(); }
}
function nextStep() {
  const v = stepValid(state.step);
  if (!v.ok) {
    const gate = $('#gate'); gate.className = 'gate bad';
    gate.querySelector('i').textContent = '!';
    $('#gate-text').textContent = v.msg;
    gate.animate([{ transform: 'translateX(0)' }, { transform: 'translateX(-4px)' }, { transform: 'translateX(4px)' }, { transform: 'translateX(0)' }], { duration: 240 });
    return false;
  }
  if (!state.completed.includes(state.step)) state.completed.push(state.step);
  if (state.step === STEP.design) { khaenPostJob('solve').catch(() => {}); }
  if (state.step === STEP.review) { finishFlow(); renderRail(); return true; }
  goStep(state.step + 1);
  return true;
}
function backStep() { if (state.step > 0) goStep(state.step - 1); }
function finishFlow() {
  showModal({
    title: '校准流程完成（演示）',
    narrow: true,
    body: '<div class="confirm-note" style="margin-top:0">' + STEPS.length + ' 个步骤全部完成：设备检查 → 布局分配 → 麦克风/校准设置 → Level 对齐 → Delay 对齐 → Bass management → 测点 → 测量 → 分析 → 设计 → 回顾应用。</div>' +
      '<table class="confirm-table"><tr><td>测点</td><td>' + state.points.filter(p => p.on).length + ' 点（含听音位）</td></tr>' +
      '<tr><td>低切</td><td>主箱 ' + state.design.cut.mains + ' Hz · 环绕 ' + state.design.cut.surround + ' Hz</td></tr>' +
      '<tr><td>目标曲线</td><td>' + (state.design.target === 'tilt10' ? 'tilt10' : 'flat') + '</td></tr>' +
      '<tr><td>应用状态</td><td>' + (state.design.applied ? '已应用（演示）' : '未应用') + '</td></tr></table>' +
      '<div class="confirm-note">演示页不写入设备；导出配置可在“回顾”步骤查看。</div>',
    buttons: [{ label: '关闭', cls: 'primary', act: hideModal }],
  });
}
$('#btn-next').addEventListener('click', nextStep);
$('#btn-back').addEventListener('click', backStep);
$('#btn-restart').addEventListener('click', () => {
  hideModal(); state = DEFAULT_STATE(); renderStep();
  toast('已重新开始流程');
});