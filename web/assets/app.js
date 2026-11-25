'use strict';

/* 泰山派音频管理前端：无构建步骤、无外部依赖，轮询 REST JSON。 */

const state = {
  csrf: null,
  page: 'overview',
  timer: null,
  mock: false,
  lastErrorAt: 0,
  modeFastTimer: null,
  motuBusy: false,
};

/* 页面数据变化时才重渲染，避免轮询打断点击/表单操作（元素 refs 也保持稳定）。 */
const pageCache = {};

function skipRenderIfUnchanged(page, data) {
  const key = JSON.stringify(data);
  if (pageCache[page] === key) return true;
  pageCache[page] = key;
  return false;
}

const POLL_MS = {
  overview: 3000,
  mode: 1500,
  camilladsp: 2500,
  ptp: 3000,
  aes67: 5000,
  inferno: 5000,
  alsa: 5000,
  motu: 8000,
  channelmap: 5000,
  system: 10000,
};

const $ = (sel, root = document) => root.querySelector(sel);

/* ---------- DOM 构造工具（只用 textContent，避免 XSS） ---------- */
function el(tag, attrs, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs || {})) {
    if (value === null || value === undefined || value === false) continue;
    if (key === 'class') node.className = value;
    else if (key === 'text') node.textContent = value;
    else if (key === 'dataset') Object.assign(node.dataset, value);
    else if (key.startsWith('on') && typeof value === 'function') node.addEventListener(key.slice(2), value);
    else if (value === true) node.setAttribute(key, '');
    else node.setAttribute(key, String(value));
  }
  for (const child of children.flat(Infinity)) {
    if (child === null || child === undefined || child === false) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

function clear(node) { node.replaceChildren(); return node; }
function text(value, cls) { return el('span', cls ? { class: cls } : {}, value == null ? '—' : String(value)); }
function badge(label, kind = 'muted') { return el('span', { class: 'badge ' + kind }, label); }
function kv(pairs) {
  const dl = el('dl', { class: 'kv' });
  for (const [k, v] of pairs) {
    if (v === undefined) continue;
    dl.append(el('dt', {}, k), el('dd', {}, v instanceof Node ? v : (v == null || v === '' ? '—' : String(v))));
  }
  return dl;
}
function alertBox(kind, content) {
  return el('div', { class: 'alert ' + kind }, content);
}
function warnList(warnings) {
  if (!warnings || !warnings.length) return null;
  return alertBox('warn', el('ul', {}, ...warnings.map((w) => el('li', {}, String(w)))));
}
function card(title, body, opts = {}) {
  const head = el('div', { class: 'card-head' },
    el('h2', {}, title),
    opts.hint ? el('span', { class: 'hint' }, opts.hint) : null,
  );
  return el('div', { class: 'card ' + (opts.className || '') }, head,
    ...(Array.isArray(body) ? body : [body]),
    opts.actions ? el('div', { class: 'btn-row' }, ...opts.actions) : null);
}
function table(headers, rows) {
  if (!rows.length) return el('div', { class: 'empty' }, '暂无数据');
  return el('div', { class: 'table-wrap' }, el('table', {},
    el('thead', {}, el('tr', {}, ...headers.map((h) => el('th', {}, h)))),
    el('tbody', {}, ...rows.map((row) => el('tr', {},
      ...row.map((cell) => el('td', {}, cell instanceof Node ? cell : (cell == null ? '—' : String(cell))))))),
  ));
}
function meter(label, percent, extra) {
  const p = Math.max(0, Math.min(100, Number(percent) || 0));
  return el('div', { class: 'meter-row' },
    el('span', { class: 'muted', style: 'min-width:52px' }, label),
    el('div', { class: 'progress' + (p > 85 ? ' danger' : p > 65 ? ' warn' : '') },
      el('span', { style: 'width:' + p + '%' })),
    el('span', { class: 'val' }, (extra != null ? extra : p.toFixed(1) + '%')));
}
function serviceBadge(svc) {
  if (!svc) return badge('未知', 'muted');
  if (!svc.available) return badge('不可用', 'danger');
  return svc.active ? badge('运行中', 'ok') : badge('已停止', 'muted');
}
function fmtUptime(secs) {
  const s = Math.max(0, Number(secs) || 0);
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  if (d) return d + ' 天 ' + h + ' 小时 ' + m + ' 分';
  if (h) return h + ' 小时 ' + m + ' 分';
  if (m) return m + ' 分 ' + Math.floor(s % 60) + ' 秒';
  return Math.floor(s) + ' 秒';
}
function fmtTime(unix) {
  if (!unix) return '—';
  try { return new Date(unix * 1000).toLocaleString('zh-CN', { hour12: false }); } catch { return '—'; }
}
function fmtDb(v) { return v == null ? '—' : Number(v).toFixed(1) + ' dB'; }
function fmtBytes(v) {
  if (v == null) return '—';
  if (v < 1024) return v + ' B';
  if (v < 1024 * 1024) return (v / 1024).toFixed(1) + ' KB';
  return (v / 1024 / 1024).toFixed(1) + ' MB';
}
function fmtHz(v) { return v == null ? '—' : (v / 1000) + ' kHz'; }
function stepStatusText(s) {
  return ({ pending: '等待', running: '进行中', done: '完成', failed: '失败', skipped: '跳过' })[s] || s;
}

/* ---------- API ---------- */
class ApiError extends Error {
  constructor(message, status, data) { super(message); this.status = status; this.data = data; }
}
async function api(path, options = {}) {
  const method = options.method || 'GET';
  const headers = {};
  if (options.body !== undefined) headers['Content-Type'] = 'application/json';
  if (state.csrf && method !== 'GET') headers['X-CSRF-Token'] = state.csrf;
  let response;
  try {
    response = await fetch(path, {
      method,
      headers,
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
      credentials: 'same-origin',
      cache: 'no-store',
    });
  } catch (e) {
    throw new ApiError('网络请求失败：' + e.message, 0, null);
  }
  const contentType = response.headers.get('content-type') || '';
  let data = null;
  if (contentType.includes('application/json')) {
    try { data = await response.json(); } catch { data = null; }
  } else {
    data = await response.text();
  }
  if (!response.ok) {
    const message = (data && data.error && data.error.message) ? data.error.message
      : (typeof data === 'string' && data ? data : '请求失败（HTTP ' + response.status + '）');
    if (response.status === 401 && !path.startsWith('/api/auth/login') && !path.startsWith('/api/auth/setup')) onUnauthorized();
    throw new ApiError(message, response.status, data);
  }
  return data;
}

/* ---------- Toast / 弹窗 ---------- */
function toast(message, kind = 'info') {
  const node = el('div', { class: 'toast ' + kind }, String(message));
  $('#toasts').append(node);
  setTimeout(() => { node.style.opacity = '0'; node.style.transition = 'opacity .3s'; }, 4200);
  setTimeout(() => node.remove(), 4700);
}

function confirmDialog({ title = '确认操作', message = '', requireText = null, forText = null, confirmLabel = '确认', danger = true }) {
  return new Promise((resolve) => {
    const modal = $('#modal');
    const inputWrap = $('#modal-input-wrap');
    const input = $('#modal-input');
    $('#modal-title').textContent = title;
    $('#modal-message').textContent = message;
    $('#modal-error').textContent = '';
    $('#modal-ok').textContent = confirmLabel;
    $('#modal-ok').className = 'btn ' + (danger ? 'danger' : 'primary');
    if (requireText) {
      inputWrap.hidden = false;
      $('#modal-input-label').textContent = forText || ('请输入 “' + requireText + '” 以确认');
      input.value = '';
    } else {
      inputWrap.hidden = true;
      input.value = '';
    }
    modal.hidden = false;
    if (requireText) setTimeout(() => input.focus(), 30);
    let done = false;
    const finish = (result) => {
      if (done) return;
      done = true;
      modal.hidden = true;
      $('#modal-ok').onclick = null;
      $('#modal-cancel').onclick = null;
      input.onkeydown = null;
      document.removeEventListener('keydown', onKey);
      resolve(result);
    };
    const onKey = (e) => { if (e.key === 'Escape') finish({ ok: false }); };
    document.addEventListener('keydown', onKey);
    $('#modal-cancel').onclick = () => finish({ ok: false });
    $('#modal-ok').onclick = () => {
      if (requireText && input.value.trim() !== requireText) {
        $('#modal-error').textContent = '输入不匹配，请输入 “' + requireText + '”';
        return;
      }
      finish({ ok: true, text: input.value.trim() });
    };
    input.onkeydown = (e) => { if (e.key === 'Enter' && !inputWrap.hidden) $('#modal-ok').click(); };
  });
}

/* ---------- 登录 / 导航 ---------- */
function setConn(ok) {
  const node = $('#conn-status');
  node.textContent = ok ? '已连接' : '连接异常';
  node.className = 'conn ' + (ok ? 'ok' : 'bad');
}
function onUnauthorized() {
  state.csrf = null;
  stopPolling();
  showLogin();
}
function showLogin() {
  for (const key of Object.keys(pageCache)) delete pageCache[key];
  stopPolling();
  $('#app-view').hidden = true;
  $('#setup-view').hidden = true;
  $('#login-view').hidden = false;
  $('#login-password').value = '';
  $('#login-token').value = '';
  const input = $('#login-password');
  setTimeout(() => input.focus(), 30);
}
function showSetup() {
  for (const key of Object.keys(pageCache)) delete pageCache[key];
  stopPolling();
  $('#app-view').hidden = true;
  $('#login-view').hidden = true;
  $('#setup-view').hidden = false;
  $('#setup-password').value = '';
  $('#setup-confirm').value = '';
  $('#setup-error').textContent = '';
  updatePasswordStrength();
  setTimeout(() => $('#setup-password').focus(), 30);
}
function updatePasswordStrength() {
  const value = $('#setup-password').value || '';
  const node = $('#setup-strength');
  let score = 0;
  if (value.length >= 8) score += 1;
  if (value.length >= 10) score += 1;
  if (value.length >= 14) score += 1;
  if (/[a-zA-Z]/.test(value) && /[0-9]/.test(value)) score += 1;
  if (/[^a-zA-Z0-9]/.test(value)) score += 1;
  const labels = ['太短（至少 8 位）', '弱', '一般', '较强', '强', '很强'];
  node.textContent = '强度：' + labels[Math.min(score, labels.length - 1)] + (value.length ? '（' + value.length + ' 位）' : '');
}
function showApp() {
  $('#login-view').hidden = true;
  $('#setup-view').hidden = true;
  $('#app-view').hidden = false;
  $('#mock-badge').hidden = !state.mock;
  if (state.mock) $('#mock-badge').textContent = 'MOCK';
}
async function doLogin(event) {
  event.preventDefault();
  const button = $('#login-submit');
  const errorNode = $('#login-error');
  errorNode.textContent = '';
  button.disabled = true;
  const password = $('#login-password').value;
  const token = $('#login-token').value.trim();
  if (!password && !token) {
    errorNode.textContent = '请输入管理密码（或展开下方使用 token）';
    button.disabled = false;
    return;
  }
  try {
    const body = password ? { password: password } : { token: token };
    const result = await api('/api/auth/login', { method: 'POST', body: body });
    state.csrf = result.csrf_token;
    await loadHealth();
    showApp();
    switchPage('overview');
  } catch (e) {
    errorNode.textContent = e.message;
  } finally {
    button.disabled = false;
  }
}
async function doSetup(event) {
  event.preventDefault();
  const button = $('#setup-submit');
  const errorNode = $('#setup-error');
  errorNode.textContent = '';
  const password = $('#setup-password').value;
  const confirm = $('#setup-confirm').value;
  if (password.length < 8) { errorNode.textContent = '密码至少 8 位（推荐 10 位以上）'; return; }
  if (password !== confirm) { errorNode.textContent = '两次输入的密码不一致'; return; }
  button.disabled = true;
  try {
    const result = await api('/api/auth/setup', { method: 'POST', body: { password: password, confirm: confirm } });
    state.csrf = result.csrf_token;
    await loadHealth();
    showApp();
    switchPage('overview');
    toast('管理密码已设置，已登录', 'ok');
  } catch (e) {
    errorNode.textContent = e.message;
  } finally {
    button.disabled = false;
  }
}
async function loadHealth() {
  try {
    const health = await api('/healthz');
    state.mock = !!health.mock;
    $('#app-version').textContent = 'v' + health.version;
    $('#footer-version').textContent = 'v' + health.version;
  } catch { /* 忽略 */ }
}
async function doLogout() {
  try { await api('/api/auth/logout', { method: 'POST' }); } catch { /* 忽略 */ }
  state.csrf = null;
  showLogin();
}
function bindNavigation() {
  $('#login-form').addEventListener('submit', doLogin);
  $('#setup-form').addEventListener('submit', doSetup);
  $('#setup-password').addEventListener('input', updatePasswordStrength);
  $('#logout-btn').addEventListener('click', doLogout);
  $('#nav').addEventListener('click', (event) => {
    const tab = event.target.closest('.tab');
    if (!tab) return;
    switchPage(tab.dataset.page);
  });
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden && state.page) refreshCurrent().catch(() => {});
  });
}
function switchPage(page) {
  if (!POLL_MS[page]) return;
  state.page = page;
  for (const tab of document.querySelectorAll('.tab')) tab.classList.toggle('active', tab.dataset.page === page);
  for (const section of document.querySelectorAll('.page')) section.classList.toggle('active', section.id === 'page-' + page);
  startPolling();
}
function stopPolling() {
  if (state.timer) { clearInterval(state.timer); state.timer = null; }
  if (state.modeFastTimer) { clearInterval(state.modeFastTimer); state.modeFastTimer = null; }
}
function startPolling() {
  stopPolling();
  const page = state.page;
  refreshCurrent().catch(() => {});
  state.timer = setInterval(() => {
    if (!document.hidden) refreshCurrent().catch(() => {});
  }, POLL_MS[page] || 5000);
}
async function refreshCurrent() {
  // 用户正在当前页输入/拖拽时跳过本轮刷新，避免控件被重建
  const active = document.activeElement;
  if (active && active.matches && active.matches('input, select, textarea') &&
      active.closest('#page-' + state.page)) {
    return;
  }
  const page = state.page;
  const loader = {
    overview: loadOverview,
    mode: loadMode,
    camilladsp: loadCamilla,
    ptp: loadPtp,
    aes67: () => loadAoip('aes67'),
    inferno: () => loadAoip('inferno'),
    alsa: loadAlsa,
    motu: loadMotu,
    channelmap: loadChannelmap,
    system: loadSystem,
  }[page];
  if (!loader) return;
  try {
    await loader();
    setConn(true);
  } catch (e) {
    setConn(false);
    if (e.status === 401) return;
    if (Date.now() - state.lastErrorAt > 8000) {
      state.lastErrorAt = Date.now();
      toast(e.message, 'error');
    }
    renderPageError(page, e);
  }
}
function renderPage(page, ...nodes) {
  const root = $('#page-' + page);
  clear(root).append(...nodes);
}
function renderPageError(page, error) {
  renderPage(page, el('div', { class: 'grid' }, card('加载失败', alertBox('error', error.message))));
}

/* ---------- 总览 ---------- */
function modeBadgeFor(modeState, failed) {
  if (modeState === 'switching') return badge('切换中', 'warn');
  if (modeState === 'failed' || failed) return badge('失败', 'danger');
  return badge('空闲', 'ok');
}
async function loadOverview() {
  const data = await api('/api/overview');
  state.mock = !!data.mock;
  const mode = data.mode;
  const system = data.system;
  const camilla = data.camilla;
  const ptp = data.ptp;
  const alsa = data.alsa;

  const modeCard = card('运行模式', [
    el('div', { class: 'spread' },
      el('div', {},
        el('div', { class: 'mode-name', style: 'font-size:24px;font-weight:700' }, mode.mode_label),
        el('div', { class: 'mode-sub' }, '状态：', modeBadgeFor(mode.state, !!mode.last_error),
          mode.target ? ' 切换目标：' + (mode.target_label || mode.target) : ''),
      ),
      el('div', { class: 'btn-row', style: 'margin:0' },
        el('button', { class: 'btn small info', onclick: () => switchPage('mode') }, '前往切换'))),
    el('div', { class: 'mode-chip' },
      mode.ptp_version ? badge('PTP ' + mode.ptp_version, 'info') : null,
      mode.ptp_source ? badge('PTP 来源: ' + mode.ptp_source, 'muted') : null,
      mode.ptp_role ? badge('角色: ' + mode.ptp_role, 'muted') : null,
      data.aoip && data.aoip.service ? serviceBadge(data.aoip.service) : null,
      camilla.live && camilla.live.state ? badge('CamillaDSP ' + camilla.live.state, camilla.live.state === 'Running' ? 'ok' : 'warn') : null),
    mode.last_error ? alertBox('error', '上次切换错误：' + mode.last_error) : null,
    el('p', { class: 'muted', style: 'font-size:12.5px;margin-top:10px' },
      '切换策略：' + (mode.strategy === 'script' ? 'audio-stack 脚本' : '内置编排') +
      (mode.input_format ? ' · 输入 ' + mode.input_format : '') +
      (mode.output_format ? ' · 输出 ' + mode.output_format : '')),
  ]);

  const systemCard = card('系统', [
    kv([
      ['主机名', system.hostname],
      ['内核', system.kernel],
      ['系统', system.os],
      ['运行时间', fmtUptime(system.uptime_secs)],
      ['负载', [system.cpu.load1, system.cpu.load5, system.cpu.load15].map((v) => Number(v || 0).toFixed(2)).join(' / ')],
      ['温度', system.temperature_c != null ? (system.temperature_c + ' °C' + (system.temperature_zone ? '（' + system.temperature_zone + '）' : '')) : '—'],
      ['CPU', system.cpu.model + ' · ' + system.cpu.cores + ' 核'],
      ['网络', (system.network || []).filter((n) => n.ipv4).map((n) => n.name + ' ' + n.ipv4 + '/' + n.prefix).join('；') || '—'],
    ]),
    el('div', { style: 'margin-top:12px;display:flex;flex-direction:column;gap:6px' },
      meter('CPU', system.cpu.usage_percent),
      meter('内存', system.memory.used_percent, system.memory.used_percent.toFixed(1) + '%')),
  ]);

  const realtime = system.realtime;
  const rtCard = card('内核 / 实时', [
    el('div', { class: 'row' },
      realtime.realtime_sysfs === true ? badge('PREEMPT_RT 已生效', 'ok')
        : realtime.realtime_sysfs === false ? badge('RT 未生效', 'warn') : badge('非 RT /sys 不可用', 'muted'),
      system.warnings && system.warnings.length ? badge(system.warnings.length + ' 条告警', 'warn') : null),
    el('p', { style: 'margin-top:10px' }, realtime.summary),
    el('details', {}, el('summary', {}, '内核 cmdline'), el('pre', {}, realtime.kernel_cmdline || '—')),
  ]);

  const camillaCard = card('CamillaDSP', [
    el('div', { class: 'mode-chip' }, serviceBadge(camilla.service),
      camilla.live.state ? badge(camilla.live.state, camilla.live.state === 'Running' ? 'ok' : 'warn') : null,
      camilla.live.version ? badge('v' + camilla.live.version, 'info') : null),
    kv([
      ['状态', camilla.live.state || (camilla.live.reachable ? '—' : '不可达')],
      ['停止原因', camilla.live.stop_reason],
      ['音量', fmtDb(camilla.live.volume_db)],
      ['采集率', camilla.live.capture_rate ? fmtHz(camilla.live.capture_rate) : '—'],
      ['缓冲', camilla.live.buffer_level != null ? camilla.live.buffer_level + ' 帧' : '—'],
      ['削波样本', camilla.live.clipped_samples],
      ['当前配置', camilla.live.config_path || camilla.config_file],
      ['xrun 线索', camilla.xrun.recent_count + ' 条'],
    ]),
    camilla.live.error ? alertBox('warn', camilla.live.error) : null,
    camilla.xrun.last_line ? el('pre', {}, camilla.xrun.last_line) : null,
  ], { actions: [el('button', { class: 'btn small', onclick: () => switchPage('camilladsp') }, '详情与配置')] });

  const ptpCard = card('PTP', [
    el('div', { class: 'mode-chip' },
      ptp.locked ? badge('已锁定', 'ok') : badge('未锁定/未知', 'warn'),
      ptp.version ? badge(ptp.version, 'info') : null,
      ptp.source ? badge('来源: ' + ptp.source, 'muted') : null,
      ptp.role ? badge('角色: ' + ptp.role, 'muted') : null),
    kv([
      ['offset', ptp.offset_ns != null ? ptp.offset_ns + ' ns' : '—'],
      ['portState', ptp.port_state],
      ['GM', ptp.gm_present == null ? '—' : (ptp.gm_present ? '存在' : '未收到')],
      ['GM 身份', ptp.gm_identity],
      ['锁定详情', ptp.detail],
    ]),
  ], { actions: [el('button', { class: 'btn small', onclick: () => switchPage('ptp') }, '详情')] });

  const alsaCard = card('声卡 / MOTU', [
    el('div', { class: 'mode-chip' },
      alsa.motu_present ? badge('MOTU UltraLite-mk5 已连接', 'ok') : badge('未检测到 MOTU', 'warn'),
      badge(alsa.cards.length + ' 张声卡', 'muted')),
    table(['卡号', 'ID', '名称'], (alsa.cards || []).map((c) => [c.index, c.id, c.name])),
    el('p', { class: 'muted', style: 'font-size:12.5px;margin-top:8px' },
      (alsa.pcm || []).filter((p) => p.active).map((p) => p.card_id + ':' + p.device + ' ' + p.pcm_type).join('；') || '当前无活动 PCM'),
  ], { actions: [el('button', { class: 'btn small', onclick: () => switchPage('alsa') }, '声卡详情')] });

  const servicesCard = card('服务状态', [
    table(['单元', '描述', '状态'], (data.services || []).map((svc) => [
      svc.unit, svc.status ? svc.status.description : (svc.error || '—'), serviceBadge(svc),
    ])),
    (data.service_notes && data.service_notes.length)
      ? alertBox('info', el('ul', {}, ...data.service_notes.map((n) => el('li', {}, n))))
      : null,
  ], { className: 'wide' });

  renderPage('overview',
    el('div', { class: 'grid' }, modeCard, systemCard, rtCard, camillaCard, ptpCard, alsaCard),
    el('div', { style: 'margin-top:16px' }, servicesCard));
}

/* ---------- 运行模式 ---------- */
function modeTargetCard(profile, current, busy, enabled, onSwitch) {
  const isCurrent = current === profile.mode;
  return el('div', { class: 'mode-target' + (isCurrent ? ' current' : '') },
    el('div', { class: 'title' }, profile.label || profile.mode),
    el('div', { class: 'desc' }, 'PTP ' + profile.ptp_version + ' · ' + profile.ptp_source + ' / ' + profile.ptp_role),
    el('div', { class: 'desc' }, '输入 ' + profile.input_format + ' → 输出 ' + profile.output_format),
    el('div', { class: 'desc mono', style: 'font-size:11.5px' }, profile.camilla_config),
    el('div', { class: 'btn-row', style: 'margin-top:8px' },
      el('button', {
        class: 'btn ' + (isCurrent ? 'ghost' : 'primary') + ' small',
        disabled: !enabled || busy || isCurrent,
        onclick: onSwitch,
      }, isCurrent ? '当前模式' : (busy ? '切换中…' : '切换到此模式'))));
}
function offModeCard(current, busy, enabled, onSwitch) {
  return el('div', { class: 'mode-target' + (current === 'off' ? ' current' : '') },
    el('div', { class: 'title' }, '关闭音频链路（off）'),
    el('div', { class: 'desc' }, '停止 CamillaDSP 与当前 AoIP/PTP 服务，保留 Web 管理'),
    el('div', { class: 'desc' }, '切换后不会自动播放任何音频'),
    el('div', { class: 'btn-row', style: 'margin-top:8px' },
      el('button', {
        class: 'btn ' + (current === 'off' ? 'ghost' : 'danger') + ' small',
        disabled: !enabled || busy || current === 'off',
        onclick: onSwitch,
      }, current === 'off' ? '当前模式' : (busy ? '切换中…' : '关闭音频链路'))));
}
function scriptStatusBlock(st) {
  const nested = ['ptp', 'aoip', 'camilladsp', 'alsa', 'last_error']
    .filter((k) => st[k] != null)
    .map((k) => el('pre', {}, k + ':\n' + JSON.stringify(st[k], null, 2)));
  return el('details', { style: 'margin-top:12px' },
    el('summary', {}, 'audio-stack 脚本状态（tspi-mode status --json，schema=tspi-audio/v1）'),
    kv([
      ['schema', st.schema],
      ['mode', st.mode],
      ['pid', st.pid],
      ['PTP 角色', st.ptp_role],
      ['PTP 锁定', st.ptp_lock == null ? '—' : (st.ptp_lock ? '是' : '否')],
      ['message', st.message],
    ]),
    ...nested);
}
async function loadMode() {
  const data = await api('/api/mode');
  if (skipRenderIfUnchanged('mode', data)) return;
  const status = data;
  const components = data.components || {};
  const busy = status.state === 'switching';
  const current = status.mode;
  const enabled = status.enabled;

  const doSwitch = async (mode, label) => {
    const answer = await confirmDialog({
      title: '切换到 ' + label,
      message: '将按以下顺序切换运行模式：\n' +
        '1. 停止 CamillaDSP\n2. 停止当前 AoIP 服务\n3. 停止当前 PTP 服务\n' +
        '4. 启动目标 PTP（如该模式需要 linuxptp）\n5. 启动目标 AoIP\n' +
        '6. 应用目标 CamillaDSP 配置\n7. 启动 CamillaDSP 并校验（PTP/声卡/状态/xrun）\n\n' +
        '切换失败会自动回滚到当前模式。确认继续？',
      confirmLabel: '开始切换',
    });
    if (!answer.ok) return;
    try {
      await api('/api/mode', { method: 'PUT', body: { mode } });
      toast('已开始切换到 ' + label, 'ok');
      await loadMode();
      startModeFastPoll();
    } catch (e) {
      toast(e.message, 'error');
      await loadMode().catch(() => {});
    }
  };
  const fast = () => { startModeFastPoll(); };

  const profileByMode = {};
  for (const p of status.profiles || []) profileByMode[p.mode] = p;

  const hero = card('当前运行模式', [
    el('div', { class: 'mode-hero' },
      el('div', {},
        el('div', { class: 'mode-name' }, status.mode_label),
        el('div', { class: 'mode-sub' },
          '状态：', modeBadgeFor(status.state, !!status.last_error),
          ' · 切换策略：' + (status.strategy === 'script' ? 'audio-stack 脚本（tspi-mode）' : '内置编排'),
          ' · 启动自动恢复：' + (status.restore_on_start ? '开' : '关（D4 默认）')),
        status.persisted ? el('div', { class: 'muted', style: 'font-size:12.5px;margin-top:6px' },
          '上次持久化模式：' + (status.persisted.mode_label || status.persisted.mode) +
          '（' + fmtTime(status.persisted.updated_at_unix) + '）· 未自动加载时页面显示 off') : null),
      el('div', { style: 'margin-left:auto;text-align:right' },
        el('div', { class: 'muted' }, '进度 ' + status.progress + '%'),
        el('div', { class: 'progress' + (status.state === 'failed' ? ' danger' : ''), style: 'width:180px' },
          el('span', { style: 'width:' + status.progress + '%' })))),
    status.target ? alertBox('info', '正在切换到：' + (status.target_label || status.target)) : null,
    status.last_error ? alertBox('error', '上次切换失败：' + status.last_error) : null,
    warnList(status.warnings),
    status.steps && status.steps.length ? el('ol', { class: 'steps' }, ...status.steps.map((step) =>
      el('li', { class: step.status },
        el('span', { class: 'dot' }),
        el('div', {},
          el('div', { class: 'label' }, step.label + '（' + stepStatusText(step.status) + '）'),
          step.detail ? el('div', { class: 'detail' }, step.detail) : null)))) : null,
    status.script_status ? scriptStatusBlock(status.script_status) : null,
  ], {
    className: 'wide',
    actions: [el('button', {
      class: 'btn small ghost',
      onclick: async () => {
        try {
          const result = await api('/api/mode/script-status');
          if (!result.available) {
            toast(result.message || 'tspi-mode 脚本不可用', 'warn');
          } else {
            toast('已读取 tspi-mode status（schema=' + (result.script && result.script.schema || '?') + '）', 'ok');
            await loadMode();
          }
        } catch (e) { toast(e.message, 'error'); }
      },
    }, '读取 tspi-mode 状态')],
  });

  const targets = card('手动切换', [
    el('div', { class: 'mode-targets' },
      modeTargetCard(profileByMode.dante || {}, current, busy, enabled, fast ? () => doSwitch('dante', 'Dante') : null),
      modeTargetCard(profileByMode.aes67 || {}, current, busy, enabled, () => doSwitch('aes67', 'AES67')),
      offModeCard(current, busy, enabled, () => doSwitch('off', '关闭（off）'))),
    alertBox('info', 'D4：开机默认音频 off，不自动调用 tspi-mode 恢复上次模式；需要时由用户手动切换。切换失败自动回滚，并记录到 state.json。'),
    !enabled ? alertBox('warn', '运行模式切换已在配置中关闭（[mode] enabled = false）') : null,
  ]);

  const ptpComp = components.ptp || {};
  const ptpCard = card('PTP（按模式）', [
    el('div', { class: 'mode-chip' },
      ptpComp.locked ? badge('已锁定', 'ok') : badge('未锁定/未知', 'warn'),
      badge('版本 ' + (ptpComp.version || '—'), 'info'),
      badge('来源 ' + (ptpComp.source || '—'), 'muted'),
      badge('角色 ' + (ptpComp.role || '—'), 'muted')),
    kv([
      ['服务', ptpComp.service ? (ptpComp.service.unit + '：' + (ptpComp.service.active ? '运行中' : '未运行')) : '由 AoIP 内置'],
      ['offset', ptpComp.offset_ns != null ? ptpComp.offset_ns + ' ns' : '—'],
      ['portState', ptpComp.port_state],
      ['GM 存在', ptpComp.gm_present == null ? '—' : (ptpComp.gm_present ? '是' : '否')],
      ['说明', ptpComp.detail],
    ]),
    status.ptp_note ? null : null,
  ]);

  const aoipComp = components.aoip || {};
  const aoipCard = card('AoIP 服务', [
    el('div', { class: 'mode-chip' },
      aoipComp.kind ? badge(aoipComp.kind === 'aes67' ? 'AES67 daemon' : 'Inferno（Dante）', 'info') : null,
      aoipComp.service ? serviceBadge(aoipComp.service) : null),
    kv([
      ['单元', aoipComp.service ? aoipComp.service.unit : '—'],
      ['运行', aoipComp.service ? (aoipComp.service.active ? '是' : '否') : '—'],
    ]),
    aoipComp.label ? el('p', { class: 'muted', style: 'font-size:12.5px' }, aoipComp.label) : null,
  ]);

  const camillaComp = components.camilla || {};
  const camillaCard = card('CamillaDSP 链路', [
    el('div', { class: 'mode-chip' },
      camillaComp.service ? serviceBadge(camillaComp.service) : null,
      camillaComp.state ? badge(camillaComp.state, camillaComp.state === 'Running' ? 'ok' : 'warn') : badge('不可达', 'warn')),
    kv([
      ['输入设备', camillaComp.input_device],
      ['输出设备', camillaComp.output_device],
      ['采集声道', camillaComp.capture_channels],
      ['播放声道', camillaComp.playback_channels],
      ['xrun 线索', camillaComp.xrun_recent + ' 条'],
      ['当前配置', camillaComp.config_path],
    ]),
    camillaComp.xrun_hint ? el('p', { class: 'muted', style: 'font-size:12.5px' }, camillaComp.xrun_hint) : null,
  ]);

  const audioComp = components.audio || {};
  const audioCard = card('当前模式音频格式', [
    kv([
      ['采样率', fmtHz(audioComp.sample_rate)],
      ['位深', audioComp.bit_depth != null ? audioComp.bit_depth + '-bit' : '—'],
      ['声道', audioComp.channels],
      ['chunksize', audioComp.chunksize],
      ['target_level', audioComp.target_level],
      ['输入', audioComp.input_format],
      ['输出', audioComp.output_format],
    ]),
    warnList(audioComp.warnings),
    el('p', { class: 'muted', style: 'font-size:12.5px;margin-top:8px' },
      '可在 CamillaDSP 页「音频格式与缓冲」中有限编辑（采样率 / chunksize / target_level），改动会先备份再应用。'),
  ]);

  renderPage('mode', el('div', { class: 'grid' }, hero),
    el('div', { style: 'margin-top:16px' }, targets),
    el('div', { class: 'grid', style: 'margin-top:16px' }, ptpCard, aoipCard, camillaCard, audioCard));

  if (busy && !state.modeFastTimer) startModeFastPoll();
}
function startModeFastPoll() {
  if (state.modeFastTimer) clearInterval(state.modeFastTimer);
  state.modeFastTimer = setInterval(async () => {
    try {
      if (state.page !== 'mode') { clearInterval(state.modeFastTimer); state.modeFastTimer = null; return; }
      const data = await api('/api/mode');
      if (data.state !== 'switching') {
        clearInterval(state.modeFastTimer);
        state.modeFastTimer = null;
        await loadMode();
        if (data.state === 'idle') toast('模式切换完成：' + data.mode_label, 'ok');
        else if (data.state === 'failed') toast('模式切换失败：' + (data.last_error || '未知错误'), 'error');
      } else {
        await loadMode();
      }
    } catch { /* 忽略轮询错误 */ }
  }, 800);
}

/* ---------- CamillaDSP ---------- */
let camillaCtx = { live: null, configs: [] };
function camillaStatusCard(data) {
  const live = data.live || {};
  return card('状态', [
    el('div', { class: 'mode-chip' }, serviceBadge(data.service),
      live.state ? badge(live.state, live.state === 'Running' ? 'ok' : 'warn') : badge('不可达', 'warn'),
      live.version ? badge('v' + live.version, 'info') : null),
    kv([
      ['停止原因', live.stop_reason],
      ['采集率', live.capture_rate ? fmtHz(live.capture_rate) : '—'],
      ['处理负载', live.processing_load != null ? live.processing_load.toFixed(1) + '%' : '—'],
      ['缓冲层级', live.buffer_level != null ? live.buffer_level + ' 帧' : '—'],
      ['rate_adjust', live.rate_adjust],
      ['削波样本', live.clipped_samples],
      ['静音', live.mute == null ? '—' : (live.mute ? '是' : '否')],
      ['当前配置', live.config_path || data.config_file],
      ['配置标题', live.config_title],
      ['websocket', data.ws_url],
    ]),
    live.error ? alertBox('warn', live.error) : null,
  ], {
    actions: [
      el('button', { class: 'btn small primary', onclick: () => camillaOp('start', '启动') }, '启动'),
      el('button', { class: 'btn small', onclick: () => camillaOp('stop', '停止', true) }, '停止'),
      el('button', { class: 'btn small', onclick: () => camillaOp('restart', '重启', true) }, '重启'),
    ],
  });
}
async function camillaOp(action, label, confirm = false) {
  if (confirm) {
    const a = await confirmDialog({ title: label + ' CamillaDSP', message: '确认' + label + ' CamillaDSP 服务？', confirmLabel: label });
    if (!a.ok) return;
  }
  try {
    await api('/api/camilladsp/' + action, { method: 'POST' });
    toast('CamillaDSP ' + label + '成功', 'ok');
    await loadCamilla();
  } catch (e) { toast(e.message, 'error'); }
}
function volumeCard(live) {
  const current = live.volume_db == null ? -12 : Number(live.volume_db);
  const input = el('input', { type: 'range', min: '-60', max: '0', step: '0.5', value: String(current) });
  const value = el('span', { class: 'val' }, fmtDb(current));
  input.addEventListener('input', () => { value.textContent = Number(input.value).toFixed(1) + ' dB'; });
  return card('音量（主控）', [
    el('div', { class: 'slider-row' }, input, value),
    el('div', { class: 'muted', style: 'font-size:12.5px;margin-top:6px' },
      'CamillaDSP 主音量，范围 -150 ~ +50 dB（页面范围 -60 ~ 0 dB）。调整立即生效。'),
    live && live.mute ? alertBox('warn', '当前处于静音（mute）状态') : null,
  ], {
    actions: [el('button', {
      class: 'btn small primary',
      onclick: async () => {
        try {
          const result = await api('/api/camilladsp/volume', { method: 'POST', body: { db: Number(input.value) } });
          toast('音量已设为 ' + fmtDb(result.volume_db), 'ok');
          await loadCamilla();
        } catch (e) { toast(e.message, 'error'); }
      },
    }, '应用音量')],
  });
}
function xrunCard(xrun) {
  return card('xrun / 缓冲线索', [
    el('div', { class: 'mode-chip' },
      xrun.recent_count > 0 ? badge(xrun.recent_count + ' 条线索', 'warn') : badge('最近日志未发现', 'ok')),
    el('p', { class: 'muted', style: 'margin-top:10px' }, xrun.hint || '—'),
    xrun.last_line ? el('pre', {}, xrun.last_line) : null,
    el('p', { class: 'muted', style: 'font-size:12.5px' },
      '说明：CamillaDSP 4.x websocket 没有独立 xrun 计数命令，这里统计 journalctl 最近日志中的 xrun/underrun/overrun 关键词，并配合 StopReason 判断。'),
  ]);
}
function configSwitchCard(data) {
  const select = el('select', {});
  for (const c of data.configs || []) {
    select.append(el('option', { value: c.name }, c.name + (c.current ? '（当前）' : '') + ' · ' + fmtBytes(c.size)));
  }
  if (!(data.configs || []).length) select.append(el('option', { value: '' }, '（无可用配置）'));
  return card('切换配置', [
    el('label', { class: 'field' }, '选择 /etc/camilladsp/configs 下的配置',
      select),
    el('p', { class: 'muted', style: 'font-size:12.5px;margin-top:8px' },
      '服务端先做 serde_yaml 语法校验，再优先用 websocket SetConfigJson 应用；失败自动回退 systemd restart。'),
    el('p', { class: 'muted', style: 'font-size:12.5px' },
      '配置会同步写入 ' + data.config_file + '，重启后仍然生效。'),
  ], {
    actions: [el('button', {
      class: 'btn small primary',
      onclick: async () => {
        const name = select.value;
        if (!name) return toast('没有可用配置', 'warn');
        const a = await confirmDialog({
          title: '切换 CamillaDSP 配置',
          message: '切换到 ' + name + ' 并立即应用？原配置文件会自动备份。',
          confirmLabel: '应用配置',
        });
        if (!a.ok) return;
        try {
          const result = await api('/api/camilladsp/configs/switch', { method: 'POST', body: { name } });
          toast('配置已应用（' + result.outcome.applied_via + '）', 'ok');
          for (const w of result.warnings || []) toast(w, 'warn');
          await loadCamilla();
        } catch (e) { toast(e.message, 'error'); }
      },
    }, '读取并应用')],
  });
}
function configViewCard(data) {
  const select = el('select', {});
  select.append(el('option', { value: '' }, '（当前激活配置）'));
  for (const c of data.configs || []) select.append(el('option', { value: c.name }, c.name));
  const pre = el('pre', {}, '点击「读取」查看配置 YAML');
  return card('配置内容（只读）', [el('label', { class: 'field' }, '配置', select), pre], {
    hint: '只读预览',
    actions: [el('button', {
      class: 'btn small',
      onclick: async () => {
        try {
          const query = select.value ? '?name=' + encodeURIComponent(select.value) : '';
          const result = await api('/api/camilladsp/config' + query);
          pre.textContent = result.content;
          toast('已读取 ' + (result.name || '当前配置'), 'ok');
        } catch (e) { toast(e.message, 'error'); }
      },
    }, '读取'), el('button', {
      class: 'btn small ghost',
      onclick: async () => {
        try {
          const result = await api('/api/camilladsp/config');
          pre.textContent = result.content;
        } catch (e) { toast(e.message, 'error'); }
      },
    }, '刷新当前')],
  });
}
function audioFormatCard(fmt) {
  const srSelect = el('select', {});
  for (const sr of fmt.limits.sample_rates) srSelect.append(el('option', { value: String(sr), selected: sr === fmt.sample_rate }, fmtHz(sr)));
  const csSelect = el('select', {});
  for (const cs of fmt.limits.chunksizes) csSelect.append(el('option', { value: String(cs), selected: cs === fmt.chunksize }, String(cs) + (cs === 32 ? '（高风险）' : cs === 128 ? '（推荐）' : '')));
  const tlInput = el('input', { type: 'number', min: String(fmt.limits.target_level_min), max: String(fmt.limits.target_level_max), value: fmt.target_level == null ? '' : String(fmt.target_level) });
  const status = el('div', {});
  const readOnly = el('div', { class: 'mode-chip' },
    badge('位深 ' + (fmt.bit_depth == null ? '—' : fmt.bit_depth + '-bit'), 'info'),
    badge('声道 ' + (fmt.channels == null ? '—' : fmt.channels), 'info'),
    badge(fmt.mode_label, 'muted'),
    badge(fmt.source, 'muted'));
  return card('音频格式与缓冲', [
    readOnly,
    el('div', { class: 'form-grid', style: 'margin-top:12px' },
      el('label', { class: 'field' }, '采样率', srSelect),
      el('label', { class: 'field' }, 'chunksize（帧）', csSelect),
      el('label', { class: 'field' }, 'target_level', tlInput)),
    kv([
      ['输入格式', fmt.input_format],
      ['输出格式', fmt.output_format],
      ['配置文件', fmt.config_path],
      ['xrun 线索', (fmt.xrun ? fmt.xrun.recent_count : 0) + ' 条'],
    ]),
    warnList(fmt.warnings),
    status,
  ], {
    actions: [el('button', {
      class: 'btn small primary',
      onclick: async () => {
        const body = {};
        if (srSelect.value) body.sample_rate = Number(srSelect.value);
        if (csSelect.value) body.chunksize = Number(csSelect.value);
        if (tlInput.value !== '') body.target_level = Number(tlInput.value);
        const a = await confirmDialog({
          title: '修改音频格式 / 缓冲',
          message: '将修改当前模式 CamillaDSP 配置并立即应用：\n' +
            '采样率 ' + (body.sample_rate || '-') + ' Hz，chunksize ' + (body.chunksize || '-') +
            '，target_level ' + (body.target_level == null ? '-' : body.target_level) + '\n\n' +
            'chunksize=32 属高风险档位，需实测 xrun；默认建议 128。失败会自动回滚。',
          confirmLabel: '应用并重载',
        });
        if (!a.ok) return;
        try {
          const result = await api('/api/audio/format', { method: 'PUT', body });
          toast('格式已应用（' + result.outcome.applied_via + '）', 'ok');
          clear(status).append(...(result.warnings || []).map((w) => alertBox('warn', w)), alertBox('info', '请观察 10~30 秒 xrun 与播放状态'));
          await loadCamilla();
        } catch (e) {
          clear(status).append(alertBox('error', e.message));
          toast(e.message, 'error');
        }
      },
    }, '应用格式修改')],
  });
}
async function loadCamilla() {
  const data = await api('/api/camilladsp');
  camillaCtx = data;
  let fmt = null;
  try { fmt = await api('/api/audio/format'); } catch { /* 忽略 */ }
  if (skipRenderIfUnchanged('camilladsp', { data, fmt })) return;
  renderPage('camilladsp',
    el('div', { class: 'grid' },
      camillaStatusCard(data),
      volumeCard(data.live || {}),
      xrunCard(data.xrun || { recent_count: 0, hint: '' }),
      configSwitchCard(data)),
    el('div', { class: 'grid', style: 'margin-top:16px' },
      configViewCard(data),
      fmt ? audioFormatCard(fmt) : card('音频格式与缓冲', alertBox('warn', '读取格式失败'))));
}

/* ---------- PTP ---------- */
async function loadPtp() {
  const data = await api('/api/ptp');
  if (skipRenderIfUnchanged('ptp', data)) return;
  const st = data.status || {};
  const conf = data.conf || {};
  const mode = data.mode || {};
  const notes = data.notes || [];

  const statusCard = card('PTP 状态', [
    el('div', { class: 'mode-chip' },
      st.locked ? badge('已锁定', 'ok') : badge('未锁定/未知', 'warn'),
      badge('版本 ' + (st.ptp_version || mode.ptp_version || '—'), 'info'),
      badge('来源 ' + (st.ptp_source || mode.ptp_source || '—'), 'muted'),
      badge('角色 ' + (st.ptp_role || mode.ptp_role || '—'), 'muted')),
    kv([
      ['offset', st.offset_ns != null ? st.offset_ns + ' ns' : '—'],
      ['portState', st.port_state],
      ['GM 存在', st.gm_present == null ? '—' : (st.gm_present ? '是' : '否')],
      ['GM 身份', st.gm_identity],
      ['portIdentity', st.port_identity],
      ['stepsRemoved', st.steps_removed],
      ['clockClass', st.clock_class],
      ['锁定详情', st.lock_detail],
    ]),
    st.error ? alertBox('warn', st.error) : null,
    st.raw_time_status ? el('details', {}, el('summary', {}, 'pmc TIME_STATUS_NP 原始输出'), el('pre', {}, st.raw_time_status)) : null,
    st.raw_port_data_set ? el('details', {}, el('summary', {}, 'pmc PORT_DATA_SET 原始输出'), el('pre', {}, st.raw_port_data_set)) : null,
  ], {
    actions: [
      el('button', { class: 'btn small primary', onclick: () => ptpOp('start', '启动') }, '启动 ptp4l/phc2sys'),
      el('button', { class: 'btn small', onclick: () => ptpOp('stop', '停止', true) }, '停止'),
      el('button', { class: 'btn small', onclick: () => ptpOp('restart', '重启', true) }, '重启'),
    ],
  });

  const noteCard = card('按模式说明', [
    ...notes.map((n) => alertBox('info', n)),
    kv([
      ['当前模式', mode.mode],
      ['PTP 来源', mode.ptp_source],
      ['PTP 角色', mode.ptp_role],
      ['PTP 版本', mode.ptp_version],
      ['模式 PTP 服务', (mode.ptp_services || []).length ? mode.ptp_services.join(', ') : '无（方案 B1：AoIP 内置）'],
    ]),
    el('p', { class: 'muted', style: 'font-size:12.5px;margin-top:8px' },
      'AES67 默认方案 B1 不启动 ptp4l；只有配置为 B2（[mode.aes67].ptp_services = ["ptp4l.service","phc2sys.service"]）时本页操作才影响链路。'),
  ]);

  const servicesCard = card('PTP 相关服务', table(['单元', '描述', '状态'],
    (data.services || []).map((svc) => [svc.unit, svc.status ? svc.status.description : (svc.error || '—'), serviceBadge(svc)])));

  const masterOnly = el('input', { type: 'checkbox', checked: conf.master_only === true });
  const priority1 = el('input', { type: 'number', min: '0', max: '255', value: conf.priority1 == null ? '' : String(conf.priority1) });
  const configCard = card('linuxptp 配置（方案 B2）', [
    el('p', { class: 'muted', style: 'font-size:12.5px' }, '配置文件：' + (conf.path || '—')),
    el('div', { class: 'form-grid', style: 'margin-top:10px' },
      el('label', { class: 'field' }, 'masterOnly', el('span', { class: 'inline' }, masterOnly, '本机作为 PTP 主时钟')),
      el('label', { class: 'field' }, 'priority1（0-255）', priority1)),
    el('p', { class: 'muted', style: 'font-size:12.5px;margin-top:8px' },
      '修改会先备份 /etc/ptp4l.conf，再重启 ptp4l/phc2sys；重启失败自动恢复备份。'),
    conf.error ? alertBox('warn', conf.error) : null,
  ], {
    actions: [el('button', {
      class: 'btn small primary',
      onclick: async () => {
        const body = { master_only: masterOnly.checked };
        if (priority1.value !== '') body.priority1 = Number(priority1.value);
        const a = await confirmDialog({
          title: '修改 PTP 配置',
          message: '将修改 linuxptp 配置（masterOnly=' + masterOnly.checked +
            (body.priority1 != null ? '，priority1=' + body.priority1 : '') +
            '）并重启 ptp4l/phc2sys。\n注意：AES67 方案 B1 不依赖 linuxptp，此修改对当前链路无效。',
          confirmLabel: '保存并重启',
        });
        if (!a.ok) return;
        try {
          const result = await api('/api/ptp/config', { method: 'PUT', body });
          toast(result.outcome.message || 'PTP 配置已更新', 'ok');
          for (const w of result.warnings || []) toast(w, 'warn');
          await loadPtp();
        } catch (e) { toast(e.message, 'error'); }
      },
    }, '保存并重启')],
  });

  renderPage('ptp',
    el('div', { class: 'grid' }, statusCard, noteCard, servicesCard, configCard),
    el('div', { style: 'margin-top:16px' }, card('配置文件原文', [conf.raw ? el('pre', {}, conf.raw) : el('div', { class: 'empty' }, conf.error || '读取失败')])));
}
async function ptpOp(action, label, confirm = false) {
  if (confirm) {
    const a = await confirmDialog({ title: label + ' PTP 服务', message: '确认' + label + ' ptp4l/phc2sys？', confirmLabel: label });
    if (!a.ok) return;
  }
  try {
    await api('/api/ptp/' + action, { method: 'POST' });
    toast('PTP 服务' + label + '成功', 'ok');
    await loadPtp();
  } catch (e) { toast(e.message, 'error'); }
}

/* ---------- AES67 / Inferno ---------- */
async function loadAoip(kind) {
  const data = await api('/api/' + kind);
  if (skipRenderIfUnchanged('aoip-' + kind, data)) return;
  const svc = data.service || {};
  const files = data.config || [];
  const page = kind;

  const serviceCard = card('服务状态', [
    el('div', { class: 'mode-chip' }, serviceBadge(svc), badge(kind === 'aes67' ? 'AES67' : 'Inferno', 'info')),
    kv([
      ['单元', svc.unit],
      ['状态', svc.status ? svc.status.active_state : (svc.error || '—')],
      ['描述', svc.status ? svc.status.description : '—'],
      ['PTP', data.ptp_hint],
    ]),
    svc.error ? alertBox('warn', svc.error) : null,
  ], {
    actions: [
      el('button', { class: 'btn small primary', onclick: () => aoipOp(kind, 'start', '启动') }, '启动'),
      el('button', { class: 'btn small', onclick: () => aoipOp(kind, 'stop', '停止', true) }, '停止'),
      el('button', { class: 'btn small', onclick: () => aoipOp(kind, 'restart', '重启', true) }, '重启'),
    ],
  });

  const notesCard = card('说明', [
    ...(data.notes || []).map((n) => alertBox('info', n)),
    data.webui_url ? el('p', {}, '自带 Web UI：', el('a', { href: data.webui_url, target: '_blank', rel: 'noopener noreferrer' }, data.webui_url)) : null,
  ]);

  const configCard = card('配置（只读）', [
    ...files.map((f) => el('div', { style: 'margin-top:10px' },
      el('div', { class: 'spread' },
        el('span', { class: 'mono', style: 'font-size:12.5px;word-break:break-all' }, f.path),
        f.exists ? badge(fmtBytes(f.size), 'ok') : badge('不存在', 'muted')),
      f.content ? el('pre', {}, f.content + (f.truncated ? '\n…（已截断）' : '')) : null,
      f.error ? alertBox('warn', f.error) : null)),
    !files.length ? el('div', { class: 'empty' }, '未配置展示文件') : null,
  ]);

  const linesSelect = el('select', {}, el('option', { value: '100' }, '最近 100 行'), el('option', { value: '200' }, '最近 200 行'), el('option', { value: '500' }, '最近 500 行'));
  const logPre = el('pre', {}, '点击「读取日志」查看 journalctl 输出');
  const logsCard = card('最近日志', [el('div', { class: 'row' }, linesSelect, el('span', { class: 'muted' }, 'journalctl -u ' + (svc.unit || ''))), logPre], {
    actions: [el('button', {
      class: 'btn small',
      onclick: async () => {
        try {
          const result = await api('/api/' + kind + '/logs?lines=' + linesSelect.value);
          logPre.textContent = result.text || '（空）';
        } catch (e) { toast(e.message, 'error'); }
      },
    }, '读取日志')],
  });

  renderPage(page, el('div', { class: 'grid' }, serviceCard, notesCard, configCard, logsCard));
}
async function aoipOp(kind, action, label, confirm = false) {
  if (confirm) {
    const a = await confirmDialog({ title: label + '服务', message: '确认' + label + ' ' + kind + ' 服务？', confirmLabel: label });
    if (!a.ok) return;
  }
  try {
    await api('/api/' + kind + '/' + action, { method: 'POST' });
    toast(label + '成功', 'ok');
    await loadAoip(kind);
  } catch (e) { toast(e.message, 'error'); }
}

/* ---------- ALSA / MOTU ---------- */
function alsaCards(data) {
  return card('声卡列表', table(['卡号', 'ID', '驱动', '名称'],
    (data.cards || []).map((c) => [c.index, c.id, c.driver, c.name])));
}
function alsaPcmTable(data) {
  const rows = (data.pcm || []).map((p) => [
    p.card, p.card_id, p.device, p.pcm_type, p.name,
    p.subdevices == null ? '—' : p.subdevices_available + '/' + p.subdevices,
    p.active ? badge('活动', 'ok') : badge('空闲', 'muted'),
  ]);
  return card('PCM 设备', table(['卡', 'ID', '设备', '方向', '名称', '可用/总数', '状态'], rows));
}
function hwParamsTable(data) {
  const rows = (data.hw_params || []).map((h) => [
    h.card, h.device, h.pcm_type, h.subdevice, h.format, h.channels, h.rate ? fmtHz(h.rate) : '—', h.period_size, h.buffer_size,
  ]);
  return card('活动 hw_params', table(['卡', '设备', '方向', '子设备', '格式', '声道', '采样率', 'period', 'buffer'], rows),
    { hint: '来自 /proc/asound 的实时参数' });
}
function usbTable(data) {
  const rows = (data.usb_devices || []).map((d) => [
    d.bus + ':' + d.device, d.id, d.product || '—', d.manufacturer || '—', d.speed ? d.speed + ' Mbps' : '—',
    d.is_motu ? badge('MOTU', 'ok') : (d.is_audio ? badge('音频', 'info') : badge('其他', 'muted')),
  ]);
  return card('USB 设备', table(['总线', 'VID:PID', '产品', '厂商', '速率', '类型'], rows));
}
function toneCard(data) {
  const select = el('select', {});
  for (const p of (data.pcm || []).filter((x) => x.pcm_type === 'playback')) {
    select.append(el('option', { value: 'plughw:' + p.card + ',' + p.device }, 'plughw:' + p.card + ',' + p.device + ' · ' + p.card_id + ' · ' + p.name));
  }
  const custom = el('input', { type: 'text', placeholder: '或手填设备名，例如 plughw:1,0' });
  const result = el('div', {});
  return card('测试音', [
    el('p', { class: 'muted', style: 'font-size:12.5px' },
      '播放内置 1 kHz / 48 kHz / 16-bit / 1 秒 WAV（带淡入淡出），用于确认 MOTU 输出链路。'),
    el('label', { class: 'field', style: 'margin-top:8px' }, '目标设备', select),
    el('label', { class: 'field', style: 'margin-top:8px' }, '自定义设备（优先）', custom),
    result,
  ], {
    actions: [el('button', {
      class: 'btn small primary',
      onclick: async () => {
        const device = custom.value.trim() || select.value;
        const a = await confirmDialog({
          title: '播放测试音',
          message: '将在 ' + device + ' 上播放 1 秒 1 kHz 测试音，可能产生较大音量，请确认已连接并调低监听音量。',
          confirmLabel: '播放',
          danger: false,
        });
        if (!a.ok) return;
        try {
          const response = await api('/api/alsa/test-tone', { method: 'POST', body: { device } });
          const r = response.result || {};
          clear(result).append(alertBox(r.success ? 'info' : 'error',
            (r.message || '') + (r.stderr ? ' · stderr: ' + r.stderr : '')));
          toast(r.success ? '测试音已播放' : '测试音播放失败', r.success ? 'ok' : 'error');
        } catch (e) {
          clear(result).append(alertBox('error', e.message));
          toast(e.message, 'error');
        }
      },
    }, '播放测试音')],
  });
}
async function loadAlsa() {
  const data = await api('/api/alsa');
  if (skipRenderIfUnchanged('alsa', data)) return;
  const raw = el('details', {}, el('summary', {}, '/proc/asound/cards 与 aplay -l 原始输出'),
    el('pre', {}, (data.raw_cards || '') + '\n\n' + (data.raw_aplay_l || '')));
  renderPage('alsa',
    el('div', { class: 'grid' }, alsaCards(data), alsaPcmTable(data), hwParamsTable(data), usbTable(data), toneCard(data)),
    el('div', { style: 'margin-top:16px' }, card('原始信息', raw,
      { hint: data.motu_present ? '已检测到 MOTU' : '未检测到 MOTU' })),
    warnList(data.warnings));
}

let motuRawText = '';
async function motuWrite(path, body, okText) {
  if (state.motuBusy) return;
  state.motuBusy = true;
  try {
    const result = await api(path, { method: 'POST', body });
    delete pageCache.motu;
    const check = result.verified ? '读回校验通过' : '未校验';
    toast((okText || result.message || '已写入') + '（' + check + '，written=' + result.written + '）', 'ok');
    await loadMotu();
  } catch (e) {
    delete pageCache.motu;
    const data = e.data || {};
    if (data.rollback) {
      toast((data.message || e.message) + (data.rollback_verified ? '；回滚已校验' : '；回滚未确认'), 'error');
    } else {
      toast(e.message, 'error');
    }
    try { await loadMotu(); } catch { /* 状态刷新失败时保留上面的提示 */ }
  } finally {
    state.motuBusy = false;
  }
}
function motuDeviceCard(status) {
  const online = !!status.online;
  const device = status.device || {};
  const found = status.discovery || {};
  return card('设备', [
    el('div', { class: 'spread' },
      online ? badge('在线', 'ok') : badge('离线', 'danger'),
      status.mock ? badge('MOCK', 'warn') : badge('板端直连', 'info')),
    online ? null : alertBox('warn', status.reason || '设备离线'),
    status.last_error ? alertBox('error', status.last_error) : null,
    kv([
      ['型号', device.model || found.model || found.name],
      ['固件', device.version || found.version],
      ['UID', device.uid || found.uid],
      ['IP', found.ip],
      ['网口', found.iface],
      ['采样率', device.sample_rate ? (device.sample_rate + ' Hz') : null],
      ['Main 衰减', status.main_trim == null ? null : (status.main_trim + '（' + fmtDb(status.main_db) + '）')],
      ['直通条数', status.direct_count],
      ['幻象 Mic 1', phantomText(status, 0)],
      ['幻象 Mic 2', phantomText(status, 1)],
      ['最近读取', fmtTime(status.last_read_unix)],
      ['最近快照', status.latest_snapshot_id],
    ]),
  ], { hint: '只读设备信息；幻象电源本页不写' });
}
function phantomText(status, index) {
  const row = (status.phantom || []).find((item) => item.index === index);
  if (!row || row.on == null) return '未知';
  return row.on ? '开' : '关';
}
function motuVolumeCard(status) {
  const current = status.main_trim == null ? 0 : Number(status.main_trim);
  const input = el('input', { type: 'range', min: '0', max: '100', step: '1', value: String(current), disabled: !status.online });
  const value = el('span', { class: 'val' }, fmtDb(-(Number(input.value) || 0)));
  input.addEventListener('input', () => { value.textContent = fmtDb(-Number(input.value)); });
  return card('Main Out 监听音量', [
    el('div', { class: 'slider-row' }, input, value),
    el('p', { class: 'muted', style: 'font-size:12.5px;margin-top:6px' },
      'kMainTrim：滑块数值 n 写入设备后表示 -n dB（0 = 0 dB）。范围 0–100（依据 CueMix 源码 kMainVolCtrlInfo，0 = 0 dB，100 = −100 dB）。不改混音矩阵。'),
  ], {
    actions: [el('button', {
      class: 'btn small primary',
      disabled: !status.online,
      onclick: async () => {
        const trim = Number(input.value);
        const answer = await confirmDialog({
          title: '设置 Main Out 监听音量',
          message: '将把 MOTU Main Out 衰减写为 ' + trim + '（' + fmtDb(-trim) + '）。这只改 kMainTrim。',
          confirmLabel: '写入设备',
          danger: false,
        });
        if (!answer.ok) return;
        await motuWrite('/api/motu/monitor', { trim_db: trim, confirm: true }, 'Main 衰减已写入');
      },
    }, '应用')],
  });
}
function motuDirectCard(status) {
  const rows = (status.direct || []).map((row) => [
    row.bus_name,
    row.input_name,
    row.alias || '—',
    Number(row.fader).toFixed(3),
  ]);
  const canRestore = !!status.latest_snapshot_id && !!status.online;
  return card('输入直通', [
    alertBox('info', '直通 = 物理输入 0–17 与混响返回 30/31 的推子大于 0 且未静音。USB 播放 18–29 是 CamillaDSP → MOTU，关闭直通时不会改。'),
    table(['总线', '输入', '别名', '推子'], rows),
  ], {
    actions: [
      el('button', {
        class: 'btn small danger',
        disabled: !status.online,
        onclick: async () => {
          const answer = await confirmDialog({
            title: '关闭全部输入直通',
            message: '会关闭 CueMix 输入直通：把物理输入 0–17 与混响返回 30/31 在 14 条总线上的推子写为 0（final insert，候选 280 格，只写有差异的）。不会改动 USB 播放路径 18–29。写前保存快照，校验失败会自动回滚。',
            confirmLabel: '关闭直通',
          });
          if (!answer.ok) return;
          await motuWrite('/api/motu/direct-mute', { mute: true, confirm: true }, '已关闭 CueMix 输入直通');
        },
      }, '全部静音（会关闭 CueMix 输入直通）'),
      el('button', {
        class: 'btn small',
        disabled: !canRestore,
        onclick: async () => {
          const answer = await confirmDialog({
            title: '恢复快照',
            message: '将按最近一次写入前的快照恢复混音矩阵，包括已被关闭的 CueMix 输入直通。监听衰减 kMainTrim 不在这批键里。',
            confirmLabel: '恢复快照',
            danger: false,
          });
          if (!answer.ok) return;
          await motuWrite('/api/motu/restore', { confirm: true }, '已按快照恢复');
        },
      }, '恢复快照'),
    ],
  });
}
function motuMatrixCard(snap) {
  const rawPre = el('pre', {}, motuRawText || '尚未加载。原始 datastore 默认不随轮询返回。');
  const rows = ((snap && snap.nonzero) || []).map((row) => [
    row.host ? badge('USB', 'info') : (row.direct ? badge('直通', 'warn') : badge('其他', 'muted')),
    row.bus_name,
    row.input_name,
    row.alias || '—',
    Number(row.fader).toFixed(3),
  ]);
  return card('矩阵 / JSON（只读）', [
    el('details', {},
      el('summary', {}, '非零 crosspoint 与原始 datastore'),
      el('div', { style: 'margin-top:10px' }, table(['类型', '总线', '输入', '别名', '推子'], rows)),
      el('div', { class: 'btn-row' }, el('button', {
        class: 'btn small',
        type: 'button',
        onclick: async () => {
          try {
            const raw = await api('/api/motu/snapshot?raw=1');
            motuRawText = JSON.stringify(raw.raw, null, 2);
            rawPre.textContent = motuRawText;
          } catch (e) {
            rawPre.textContent = e.message;
          }
        },
      }, '加载原始 datastore')),
      rawPre,
    ),
  ]);
}
async function loadMotu() {
  const status = await api('/api/motu/status');
  let snap = null;
  try { snap = await api('/api/motu/snapshot'); } catch (e) { snap = null; }
  if (skipRenderIfUnchanged('motu', { status, snap })) return;
  renderPage('motu', el('div', { class: 'grid' },
    motuDeviceCard(status),
    motuVolumeCard(status),
    motuDirectCard(status),
    motuMatrixCard(snap),
  ));
}

/* ---------- 系统 ---------- */
async function loadSystem() {
  const data = await api('/api/system');
  if (skipRenderIfUnchanged('system', data)) return;
  const infoCard = card('服务信息', kv([
    ['版本', data.version],
    ['运行模式', data.mock ? 'MOCK（内存模拟）' : '真机'],
    ['运行时间', fmtUptime(data.uptime_secs)],
    ['监听地址', data.listen],
    ['会话有效期', fmtUptime(data.session_ttl_secs)],
    ['运行模式切换', data.mode_enabled ? '已启用' : '已关闭'],
    ['启动恢复上次模式', data.restore_on_start ? '是' : '否'],
    ['token', data.token_configured ? '已配置' : '未配置'],
  ]));

  const authCard = card('认证与安全', [
    alertBox('info', '除 /healthz 外，所有 API 都需要 token 认证；浏览器会话写操作需要 CSRF token。'),
    el('p', {}, '修改 token：编辑设备上的 ', el('code', {}, '/etc/tspi-audio-web/config.toml'), ' 中 token 行，然后：'),
    el('pre', {}, 'sudo systemctl restart tspi-audio-web\n# 查看当前 token：\nsudo tspi-audio-web --config /etc/tspi-audio-web/config.toml --print-token'),
    el('p', { class: 'muted' }, '建议仅在内网/可信网络使用；如需 HTTPS，请在前置反向代理终结 TLS 并在配置中开启 cookie_secure = true。'),
  ]);

  const rebootAllowed = data.allow_reboot;
  const shutdownAllowed = data.allow_shutdown;
  const powerCard = card('电源操作（危险）', [
    alertBox('warn', '默认关闭；需在配置 [system] 中显式开启 allow_reboot / allow_shutdown，且操作需要输入确认短语。'),
    kv([
      ['允许重启', rebootAllowed ? '是' : '否'],
      ['允许关机', shutdownAllowed ? '是' : '否'],
    ]),
  ], {
    className: 'danger-zone',
    actions: [
      el('button', {
        class: 'btn danger small',
        disabled: !rebootAllowed,
        onclick: async () => {
          const a = await confirmDialog({
            title: '重启设备',
            message: '将立即重启整台泰山派音频设备，所有音频链路会中断。',
            requireText: 'REBOOT',
            confirmLabel: '确认重启',
          });
          if (!a.ok) return;
          try {
            const result = await api('/api/system/reboot', { method: 'POST', body: { confirm: a.text } });
            toast(result.message || '重启命令已下发', 'ok');
          } catch (e) { toast(e.message, 'error'); }
        },
      }, '重启设备'),
      el('button', {
        class: 'btn danger small',
        disabled: !shutdownAllowed,
        onclick: async () => {
          const a = await confirmDialog({
            title: '关闭设备',
            message: '将立即关机，需要现场重新上电才能恢复。',
            requireText: 'SHUTDOWN',
            confirmLabel: '确认关机',
          });
          if (!a.ok) return;
          try {
            const result = await api('/api/system/shutdown', { method: 'POST', body: { confirm: a.text } });
            toast(result.message || '关机命令已下发', 'ok');
          } catch (e) { toast(e.message, 'error'); }
        },
      }, '关闭设备'),
    ],
  });

  renderPage('system', el('div', { class: 'grid' }, infoCard, authCard, powerCard));
}

/* ---------- 声道分配 / 校准（channelmap v2） ---------- */

const CM = {
  view: null,
  doc: null,
  dirty: false,
  wizard: null,
  applyError: null,
  renderText: null,
  inputMode: null,
  inputModePending: false,
};

function cmDeep(obj) { return JSON.parse(JSON.stringify(obj)); }
function cmPresets() { return (CM.view && CM.view.presets) || { input_layouts: {}, output_layouts: {}, remap_presets: {} }; }
function cmInputLayouts() { return cmPresets().input_layouts || {}; }
function cmOutputLayouts() { return cmPresets().output_layouts || {}; }
function cmRemapPresets() { return cmPresets().remap_presets || {}; }
function cmProbe() { return (CM.view && CM.view.probe) || { ok: false, notes: [], cards: [] }; }
function cmRoles() { return (CM.doc && CM.doc.output && CM.doc.output.roles) || []; }
function cmInputRoles() { return (CM.view && CM.view.input_roles) || []; }
function cmFirFiles() { return (CM.view && CM.view.fir_files) || []; }
function cmProfiles() { return (CM.view && CM.view.profiles) || []; }
function cmMarkDirty() { CM.dirty = true; CM.applyError = null; }
function cmChLabel(cc) {
  if (cc === undefined || cc === null) return 'auto';
  if (typeof cc === 'number') return String(cc);
  return String(cc);
}
function cmSetChannelField(section, value) {
  const v = String(value || '').trim();
  if (v === '' || v.toLowerCase() === 'auto') CM.doc[section].channels = 'auto';
  else {
    const n = Number(v);
    if (!Number.isFinite(n) || n < 1 || n > 64) { toast('通道数必须是 1..64 或 auto', 'warn'); return; }
    CM.doc[section].channels = n;
  }
  cmMarkDirty();
}
function cmOutputChannelNames(count) {
  const out = [];
  for (let i = 0; i < count; i += 1) {
    if (i === 0) out.push('Main1');
    else if (i === 1) out.push('Main2');
    else out.push('Line' + (i + 1));
  }
  return out;
}
function cmFindRole(id) { return cmRoles().find(function (r) { return r.id === id; }) || null; }
function cmSources(role) { if (!role) return []; if (!role.sources) role.sources = []; return role.sources; }
function cmDefaultPresetName() {
  const input = (CM.doc && CM.doc.input && CM.doc.input.layout) || 'input';
  const output = (CM.doc && CM.doc.output && CM.doc.output.layout) || 'output';
  return input + '__to__' + output;
}

async function loadChannelmap() {
  if (CM.dirty || CM.wizard) return;
  const data = await api('/api/channelmap');
  state.mock = !!data.mock;
  CM.view = data;
  CM.doc = data.document ? cmDeep(data.document) : null;
  if (!CM.doc) { renderChannelmapWizard(); return; }
  if (skipRenderIfUnchanged('channelmap', { doc: CM.doc, validation: data.validation, probe: data.probe })) return;
  renderChannelmap();
  if (!CM.inputMode && !CM.inputModePending) cmLoadInputMode();
}
async function cmReloadAll() {
  CM.dirty = false;
  pageCache.channelmap = null;
  await loadChannelmap();
}

/* ===== 首启向导 ===== */
function renderChannelmapWizard() {
  const probe = cmProbe();
  const wiz = CM.wizard || {
    input_layout: Object.keys(cmInputLayouts())[0] || '',
    output_layout: Object.keys(cmOutputLayouts())[0] || '',
    capture_channels: probe.capture && probe.capture.channels_max ? probe.capture.channels_max : '',
    playback_channels: probe.playback && probe.playback.channels_max ? probe.playback.channels_max : '',
  };
  CM.wizard = wiz;
  const inputSel = el('select', { id: 'cm-wiz-input' }, Object.keys(cmInputLayouts()).map(function (id) {
    const def = cmInputLayouts()[id] || {};
    return el('option', { value: id, selected: id === wiz.input_layout }, (def.label || id) + '（' + id + '）');
  }));
  const outputSel = el('select', { id: 'cm-wiz-output' }, Object.keys(cmOutputLayouts()).map(function (id) {
    const def = cmOutputLayouts()[id] || {};
    return el('option', { value: id, selected: id === wiz.output_layout }, (def.label || id) + '（' + id + '）');
  }));
  const capInput = el('input', { id: 'cm-wiz-cap', type: 'number', min: '1', max: '64', value: wiz.capture_channels, placeholder: 'auto' });
  const pbInput = el('input', { id: 'cm-wiz-pb', type: 'number', min: '1', max: '64', value: wiz.playback_channels, placeholder: 'auto' });
  const probeCard = card('① probe 检测结果', [
    probe.ok ? badge('probe 成功', 'ok') : badge('probe 失败/需手动选择', 'warn'),
    kv([
      ['播放设备', probe.playback ? (probe.playback.device + ' · ' + (probe.playback.channels_min || '?') + '~' + (probe.playback.channels_max || '?') + 'ch · ' + (probe.playback.formats || []).join('/')) : '未探测到'],
      ['采集设备', probe.capture ? (probe.capture.device + ' · ' + (probe.capture.channels_min || '?') + '~' + (probe.capture.channels_max || '?') + 'ch') : '未探测到'],
      ['声卡', (probe.cards || []).join('；') || '—'],
    ]),
    probe.notes && probe.notes.length ? warnList(probe.notes) : null,
    el('p', { class: 'muted', style: 'font-size:12px' }, 'probe 来自 tspi-channelmap probe --json（aplay/arecord --dump-hw-params）；失败时可手动填通道数。'),
  ]);
  const layoutCard = card('② 选择输入 / 输出布局', [
    el('div', { class: 'split-two' },
      el('div', {}, el('label', {}, '输入布局（H16/AES67/Dante 实际布局）'), inputSel,
        el('p', { class: 'muted', style: 'font-size:11.5px' }, 'H16 仅支持 5.1/7.1/5.1.2/5.1.4/7.1.2/7.1.4；每流 ≤8ch，7.1.4 需 2 流（SDP 只给通道数，角色需选）。'),
        el('label', { style: 'margin-top:6px' }, '输入通道数（auto=按布局 streams 合计/probe）'), capInput),
      el('div', {}, el('label', {}, '输出布局（MOTU / 其他声卡实际接法）'), outputSel,
        el('p', { class: 'muted', style: 'font-size:11.5px' }, '输出通道以 ALSA 实测为准；任何 USB 声卡都可选（先确认设备）。'),
        el('label', { style: 'margin-top:6px' }, '输出通道数（auto=按布局 outputs/probe）'), pbInput)),
  ]);
  const noteCard = card('③ 说明', [
    el('p', {}, '生成草稿会创建显式 output.roles + sources（与 audio-stack 契约一致），之后可编辑 remap 矩阵/每角色校准/低音管理，再保存或应用。'),
    el('p', {}, 'D4：默认上电音频 off；apply 在 off 下只校验/渲染/持久化，不启动 CamillaDSP。'),
    el('p', {}, 'H16 已知问题：待机再开机可能回 Multi CH，每次开机请在 H16 上确认 Pure。'),
  ]);
  renderPage('channelmap', speakerRouteCard(), el('div', { class: 'wizard' },
    el('h2', {}, '首次配置向导：检测 → 选布局 → 生成草稿'),
    el('p', { class: 'muted' }, '当前没有 active channelmap（首启正常）。按步骤生成后再编辑/保存/应用；全部参数之后都可改，不写死 5.3.2/16ch/22ch。'),
    el('div', { class: 'grid' }, probeCard, layoutCard, noteCard),
    el('div', { class: 'btn-row' },
      el('button', { class: 'btn primary', onclick: async function () {
        const input = $('#cm-wiz-input').value;
        const output = $('#cm-wiz-output').value;
        const cap = $('#cm-wiz-cap').value.trim();
        const pb = $('#cm-wiz-pb').value.trim();
        try {
          const body = { input_layout: input, output_layout: output, profile_name: input + '__to__' + output };
          if (cap) body.capture_channels = Number(cap);
          if (pb) body.playback_channels = Number(pb);
          const result = await api('/api/channelmap/draft', { method: 'POST', body: body });
          CM.doc = result.document;
          CM.view.probe = result.probe;
          CM.view.presets = result.presets;
          CM.wizard = null;
          CM.dirty = true;
          toast('草稿已生成：' + input + ' → ' + output + '，请检查 remap/校准后保存', 'ok');
          renderChannelmap();
        } catch (e) { toast(e.message, 'error'); }
      } }, '生成草稿并进入编辑'),
      el('button', { class: 'btn ghost', onclick: function () { CM.wizard = null; pageCache.channelmap = null; loadChannelmap().catch(function (e) { toast(e.message, 'error'); }); } }, '重新检测'),
    )));
  mountSpeakerRoute();
}

const SR = {
  loaded: false,
  loading: false,
  roles: [],
  gainLabel: '−3 dB',
  hasCenter: false,
  subCount: 1,
  missingInput: 'silence',
  inputs: [],
  outputs: [],
  presetInput: [],
  presetOutput: null,
  generation: 0,
};

function speakerRouteCard() {
  return card('声道位置', [
    el('div', { id: 'speaker-route' }, '正在读取角色表…'),
  ], { className: 'wide', hint: '输入角色到输出角色。中置下混系数固定，不在这里改。' });
}

function srCopySlots(slots) {
  return (slots || []).map(function (slot) {
    return { channel: slot.channel, role: slot.role || '' };
  });
}

function mountSpeakerRoute() {
  const host = document.getElementById('speaker-route');
  if (!host) return;
  if (!SR.loaded) {
    host.textContent = '正在读取角色表…';
    if (SR.loading) return;
    SR.loading = true;
    api('/api/channelmap/speaker-route').then(function (data) {
      SR.loaded = true;
      SR.loading = false;
      SR.roles = data.roles || [];
      SR.gainLabel = data.center_gain_label || '−3 dB';
      SR.presetInput = srCopySlots(data.input_512);
      SR.presetOutput = data.output_412 || null;
      srApplyPreset('both');
      mountSpeakerRoute();
    }).catch(function (error) {
      SR.loading = false;
      host.textContent = error.message;
    });
    return;
  }
  host.replaceChildren(srBuild());
  srRefresh();
}

function srApplyPreset(which) {
  if ((which === 'input' || which === 'both') && SR.presetInput.length) {
    SR.inputs = srCopySlots(SR.presetInput);
  }
  if ((which === 'output' || which === 'both') && SR.presetOutput) {
    SR.hasCenter = !!SR.presetOutput.has_center;
    SR.subCount = Number(SR.presetOutput.sub_count || 0);
    SR.missingInput = SR.presetOutput.missing_input || 'silence';
    SR.outputs = srCopySlots(SR.presetOutput.slots);
  }
}

function srRoleOptions(selected) {
  const options = [el('option', { value: '', selected: !selected }, '空')];
  SR.roles.forEach(function (role) {
    options.push(el('option', {
      value: role.id,
      selected: role.id === selected,
    }, role.zh + ' ' + role.en));
  });
  return options;
}

function srReindex(slots) {
  slots.forEach(function (slot, index) { slot.channel = index; });
}

function srRow(side, slot) {
  const title = (side === 'in' ? '输入' : '输出') + ' 槽 ' + slot.channel + ' · 口 ' + (slot.channel + 1);
  return el('label', { class: 'sr-row', id: 'sr-' + side + '-' + slot.channel },
    el('span', {}, title),
    el('select', {
      onchange: function (event) {
        slot.role = event.target.value;
        srRefresh();
      },
    }, srRoleOptions(slot.role)),
    el('button', {
      class: 'btn ghost',
      type: 'button',
      onclick: function (event) {
        event.preventDefault();
        const list = side === 'in' ? SR.inputs : SR.outputs;
        const index = list.indexOf(slot);
        if (index >= 0) list.splice(index, 1);
        srReindex(list);
        mountSpeakerRoute();
      },
    }, '去掉'));
}

function srBuild() {
  return el('div', {},
    el('div', { class: 'sr-toolbar' },
      el('button', { class: 'btn', type: 'button', onclick: function () { srApplyPreset('input'); mountSpeakerRoute(); } }, '输入预设 5.1.2'),
      el('button', { class: 'btn', type: 'button', onclick: function () { srApplyPreset('output'); mountSpeakerRoute(); } }, '输出预设 4.1.2'),
      el('label', {}, '中置 ',
        el('select', {
          onchange: function (event) {
            SR.hasCenter = event.target.value === 'yes';
            if (!SR.hasCenter) {
              SR.outputs.forEach(function (slot) { if (slot.role === 'C') slot.role = ''; });
            }
            mountSpeakerRoute();
          },
        },
          el('option', { value: 'no', selected: !SR.hasCenter }, '无中置'),
          el('option', { value: 'yes', selected: SR.hasCenter }, '有中置'))),
      el('label', {}, '低音炮 ',
        el('select', {
          onchange: function (event) {
            SR.subCount = Number(event.target.value);
            mountSpeakerRoute();
          },
        }, [0, 1, 2, 3].map(function (count) {
          return el('option', { value: String(count), selected: SR.subCount === count }, String(count) + ' 个');
        }))),
      el('label', {}, '缺输入 ',
        el('select', {
          onchange: function (event) {
            SR.missingInput = event.target.value;
            srRefresh();
          },
        },
          el('option', { value: 'silence', selected: SR.missingInput !== 'upmix' }, '静音（当前）'),
          el('option', { value: 'upmix', selected: SR.missingInput === 'upmix' }, '上混（口子，拒绝启动）')))),
    el('p', { class: 'muted' },
      '折叠系数固定 ' + SR.gainLabel + '（0.7079458），图上只读。没有中置时中置内容折进前左和前右；有中置则直通。其它没有对上的输入只画成保留，这次不混。'),
    el('div', { class: 'sr-flow', id: 'sr-flow' },
      el('svg', { id: 'sr-svg', class: 'sr-svg' }),
      el('div', { class: 'sr-col' },
        el('h3', {}, '输入'),
        SR.inputs.map(function (slot) { return srRow('in', slot); }),
        el('button', {
          class: 'btn ghost',
          type: 'button',
          onclick: function () {
            SR.inputs.push({ channel: SR.inputs.length, role: '' });
            mountSpeakerRoute();
          },
        }, '加一条输入')),
      el('div', { class: 'sr-mid', id: 'sr-edges' }, el('p', { class: 'muted' }, '正在计算流向…')),
      el('div', { class: 'sr-col' },
        el('h3', {}, '输出'),
        SR.outputs.map(function (slot) { return srRow('out', slot); }),
        el('button', {
          class: 'btn ghost',
          type: 'button',
          onclick: function () {
            SR.outputs.push({ channel: SR.outputs.length, role: '' });
            mountSpeakerRoute();
          },
        }, '加一条输出'))),
    el('div', { id: 'sr-errors' }),
    el('pre', { id: 'sr-paths', class: 'sr-paths' }, ''));
}

function srPayload() {
  function slots(list) {
    return list.map(function (slot) {
      return { channel: slot.channel, role: slot.role ? slot.role : null };
    });
  }
  return {
    input: slots(SR.inputs),
    output: {
      has_center: SR.hasCenter,
      sub_count: SR.subCount,
      missing_input: SR.missingInput,
      slots: slots(SR.outputs),
    },
  };
}

function srRefresh() {
  const gen = ++SR.generation;
  api('/api/channelmap/speaker-route', { method: 'POST', body: srPayload() }).then(function (result) {
    if (gen !== SR.generation) return;
    srPaint(result);
  }).catch(function (error) {
    if (gen !== SR.generation) return;
    const box = document.getElementById('sr-errors');
    if (box) box.textContent = error.message;
  });
}

function srPaint(result) {
  const box = document.getElementById('sr-errors');
  const edgeHost = document.getElementById('sr-edges');
  const pathHost = document.getElementById('sr-paths');
  if (!box || !edgeHost) return;
  box.replaceChildren();
  if (result.errors && result.errors.length) {
    box.append(alertBox('warn', el('ul', {}, result.errors.map(function (item) { return el('li', {}, item); }))));
  }
  edgeHost.replaceChildren(el('h3', {}, '流向'), ...(result.edges || []).map(function (edge) {
    return el('div', { class: 'sr-edge sr-' + edge.strategy }, edge.label);
  }));
  if (pathHost) {
    const lines = (result.paths || []).map(function (path) {
      return 'out ' + path.out_index + ' ← in ' + path.in_index;
    });
    pathHost.textContent = result.ok
      ? ('可执行直通\n' + (lines.join('\n') || '（没有直通）'))
      : '当前绑定还不能生成可执行 path';
  }
  srDrawSvg(result.edges || []);
}

function srDrawSvg(edges) {
  const svg = document.getElementById('sr-svg');
  const flow = document.getElementById('sr-flow');
  if (!svg || !flow) return;
  const root = flow.getBoundingClientRect();
  const width = Math.max(flow.clientWidth, 1);
  const height = Math.max(flow.clientHeight, 1);
  svg.setAttribute('viewBox', '0 0 ' + width + ' ' + height);
  svg.setAttribute('width', String(width));
  svg.setAttribute('height', String(height));
  while (svg.firstChild) svg.removeChild(svg.firstChild);
  edges.forEach(function (edge) {
    if (edge.from_channel == null || edge.to_channel == null) return;
    const from = document.getElementById('sr-in-' + edge.from_channel);
    const to = document.getElementById('sr-out-' + edge.to_channel);
    if (!from || !to) return;
    const a = from.getBoundingClientRect();
    const b = to.getBoundingClientRect();
    const x1 = a.right - root.left;
    const y1 = a.top + a.height / 2 - root.top;
    const x2 = b.left - root.left;
    const y2 = b.top + b.height / 2 - root.top;
    const mid = (x1 + x2) / 2;
    const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
    path.setAttribute('d', 'M ' + x1 + ' ' + y1 + ' C ' + mid + ' ' + y1 + ', ' + mid + ' ' + y2 + ', ' + x2 + ' ' + y2);
    path.setAttribute('class', 'sr-wire sr-wire-' + edge.strategy);
    svg.appendChild(path);
  });
}

/* ===== 主页面 ===== */
function renderChannelmap() {
  if (!CM.view) { renderPage('channelmap', card('加载中', el('p', {}, '正在读取 channelmap…'))); return; }
  if (!CM.doc) { renderChannelmapWizard(); return; }
  const doc = CM.doc;
  const validation = (CM.view && CM.view.validation) || { valid: true, errors: [], warnings: [] };
  const probe = cmProbe();
  const roles = cmRoles();
  const inputs = cmInputRoles();
  const modeOff = !!CM.view.mode_off;
  const pbChannels = (probe.playback && probe.playback.channels_max) || (typeof doc.output.channels === 'number' ? doc.output.channels : 0);
  const pbNames = cmOutputChannelNames(Math.max(pbChannels, 1));

  const statusCard = card('状态 / 应用', [
    el('div', { class: 'mode-chip' },
      badge(modeOff ? '模式 off（只保存/渲染）' : '音频模式 active', modeOff ? 'warn' : 'ok'),
      validation.valid ? badge('校验通过', 'ok') : badge(validation.errors.length + ' 个校验错误', 'danger'),
      badge('输入 ' + (doc.input.layout || '—'), 'info'),
      badge('输出 ' + (doc.output.layout || '—'), 'info'),
      badge('probe ' + (probe.playback ? probe.playback.channels_max + 'ch' : '?') + ' / ' + (probe.capture ? probe.capture.channels_max + 'ch' : '?'), 'muted'),
      CM.dirty ? badge('未保存修改', 'warn') : null,
      CM.view.mock ? badge('MOCK', 'warn') : null),
    kv([
      ['active 文件', CM.view.path],
      ['probe 输出设备', probe.playback ? probe.playback.device : '—'],
      ['probe 采集设备', probe.capture ? probe.capture.device : '—'],
      ['渲染输出', CM.view.render_file + '（只读）'],
      ['input.channels / output.channels', cmChLabel(doc.input.channels) + ' / ' + cmChLabel(doc.output.channels)],
    ]),
    validation.errors && validation.errors.length ? alertBox('error', el('ul', {}, ...validation.errors.map(function (e) { return el('li', {}, e); }))) : null,
    validation.warnings && validation.warnings.length ? warnList(validation.warnings) : null,
    CM.applyError ? alertBox('error', '应用失败：' + CM.applyError) : null,
    el('div', { class: 'btn-row' },
      el('button', { class: 'btn small', onclick: cmValidate }, '校验'),
      el('button', { class: 'btn small primary', onclick: function () { cmSave(false); } }, '保存 active'),
      el('button', { class: 'btn small ghost', onclick: function () { cmSave(true); } }, '强制保存草稿'),
      el('button', { class: 'btn small ok', onclick: cmApply }, '保存并应用（tspi-channelmap apply）'),
      el('button', { class: 'btn small danger', onclick: cmRollback }, '回滚 last-good'),
      el('a', { class: 'btn small ghost', href: '/api/channelmap/export', target: '_blank' }, '导出 YAML'),
      el('button', { class: 'btn small ghost', onclick: cmResetWizard }, '重新向导')),
  ], { className: 'wide' });

  const probeCard = card('probe / 通道', [
    table(['方向', '设备', '通道', '格式', '采样率'], [
      ['播放', probe.playback ? probe.playback.device : '—', probe.playback ? ((probe.playback.channels_min || '?') + '~' + (probe.playback.channels_max || '?')) : '—', probe.playback ? (probe.playback.formats || []).join(',') : '—', probe.playback ? (probe.playback.rates || []).join(',') : '—'],
      ['采集', probe.capture ? probe.capture.device : '—', probe.capture ? ((probe.capture.channels_min || '?') + '~' + (probe.capture.channels_max || '?')) : '—', probe.capture ? (probe.capture.formats || []).join(',') : '—', probe.capture ? (probe.capture.rates || []).join(',') : '—'],
    ]),
    probe.needs_user_choice ? alertBox('warn', 'probe 未能自动确定通道数：请在下方布局里手动填写输入/输出通道数。') : null,
    (probe.notes || []).length ? warnList(probe.notes) : null,
    el('div', { class: 'btn-row' },
      el('button', { class: 'btn small ghost', onclick: async function () {
        try {
          const result = await api('/api/channelmap/draft', { method: 'POST', body: { input_layout: doc.input.layout, output_layout: doc.output.layout, profile_name: doc.profile.name, capture_channels: probe.capture && probe.capture.channels_max, playback_channels: probe.playback && probe.playback.channels_max } });
          CM.doc = result.document;
          CM.dirty = true;
          toast('已按 probe 通道数重建草稿（原编辑被重置）', 'warn');
          renderChannelmap();
        } catch (e) { toast(e.message, 'error'); }
      } }, '按 probe 重建草稿'),
      el('button', { class: 'btn small ghost', onclick: function () { CM.renderText = null; cmLoadRender(); } }, '查看渲染配置')),
  ], { className: 'wide' });

  const layoutCard = card('布局 / 通道数 / profile', [
    el('div', { class: 'split-two' },
      el('div', {},
        el('label', {}, '输入布局'),
        el('select', { onchange: function (e) { cmRegenerate(e.target.value, doc.output.layout); } },
          Object.keys(cmInputLayouts()).map(function (id) {
            const def = cmInputLayouts()[id] || {};
            return el('option', { value: id, selected: id === doc.input.layout }, (def.label || id) + '（' + id + '）');
          })),
        el('label', { style: 'margin-top:6px' }, 'input.channels（auto 或 1..64）'),
        el('input', { value: cmChLabel(doc.input.channels), onchange: function (e) { cmSetChannelField('input', e.target.value); } }),
        el('p', { class: 'muted', style: 'font-size:11.5px' }, 'H16 16ch 容器顺序：1 L、2 R、3 C、4 LFE、5 Ls、6 Rs、7 Lrs、8 Rrs、9/10 Lw/Rw(旧)、11/12 Ltf/Rtf、13/14 空、15/16 Ltr/Rtr；7.1.4=12 路、5.1.2=8 路。')),
      el('div', {},
        el('label', {}, '输出布局'),
        el('select', { onchange: function (e) { cmRegenerate(doc.input.layout, e.target.value); } },
          Object.keys(cmOutputLayouts()).map(function (id) {
            const def = cmOutputLayouts()[id] || {};
            return el('option', { value: id, selected: id === doc.output.layout }, (def.label || id) + '（' + id + '）');
          })),
        el('label', { style: 'margin-top:6px' }, 'output.device（auto 或 hw:1,0）'),
        el('input', { value: doc.output.device || 'auto', onchange: function (e) { doc.output.device = e.target.value.trim() || 'auto'; cmMarkDirty(); } }),
        el('label', { style: 'margin-top:6px' }, 'output.channels（auto 或 1..64）'),
        el('input', { value: cmChLabel(doc.output.channels), onchange: function (e) { cmSetChannelField('output', e.target.value); } }))),
    el('div', { style: 'margin-top:10px' },
      el('h3', {}, 'profile'),
      el('div', { class: 'role-fields', style: 'grid-template-columns:1fr 2fr' },
        el('label', {}, 'profile.name', el('input', { value: doc.profile.name, onchange: function (e) { doc.profile.name = e.target.value; cmMarkDirty(); } })),
        el('label', {}, 'description', el('input', { value: doc.profile.description, onchange: function (e) { doc.profile.description = e.target.value; cmMarkDirty(); } })))),
    el('p', { class: 'muted', style: 'font-size:11.5px;margin-top:8px' }, '切换布局会按选中的 input/output 重新生成显式 roles+sources（未保存编辑会丢）。'),
  ], { className: 'wide' });

  const remapCard = cmRenderRemapCard(inputs, roles);
  const rolesCard = cmRenderRolesCard(roles, pbNames, pbChannels);
  const bassCard = cmRenderBassCard(roles);
  const firCard = cmRenderFirCard();
  const profileCard = cmRenderProfileCard();
  const inputModeCard = cmRenderInputModeCard();
  const mimoCard = card('v2.1 · ART-like per-route FIR 矩阵（字段占位，待 benchmark）', [
    el('p', {}, 'v2.1 将支持每个输出角色的每条输入源路径各带一个 FIR（全 MIMO）：sources[].fir{path, samplerate, taps, band_limit, sparse}、'),
    el('p', {}, 'roles[].limiter{threshold_db, release_ms} / max_gain_db，以及 print --json 的 estimated{gflops_direct, gflops_fft, group_delay_ms, paths, taps_max}。'),
    el('p', { class: 'muted' }, '本页暂不编辑 per-route FIR；等 audio-stack benchmark 定稿后按 draft 增加矩阵单元（每格上传/选择系数、采样率/taps 校验、CPU/延迟估算与确认）。'),
    el('p', { class: 'muted' }, '当前每角色单 FIR 仍可用（v2 正式字段），见上方角色卡的 FIR 下拉。'),
  ], { className: 'wide' });

  renderPage('channelmap',
    speakerRouteCard(),
    el('div', { class: 'alert info', style: 'margin-bottom:12px' },
      '延迟/增益/极性/分频/PEQ/FIR 需要外部测量或优化设计后填入；本页提供结构、校验与回滚。每次开机请确认 H16 为 Pure 模式。'),
    el('div', { class: 'grid' }, statusCard, inputModeCard, probeCard, layoutCard, remapCard),
    el('div', { style: 'margin-top:16px' }, rolesCard),
    el('div', { class: 'grid', style: 'margin-top:16px' }, bassCard, firCard, profileCard),
    el('div', { style: 'margin-top:16px' }, mimoCard));
  mountSpeakerRoute();
}

function cmRenderRemapCard(inputs, roles) {
  const presets = cmRemapPresets();
  const presetSel = el('select', { id: 'cm-remap-preset' }, el('option', { value: '' }, '选择 remap preset…'),
    Object.keys(presets).map(function (id) {
      const p = presets[id] || {};
      return el('option', { value: id }, (p.label || id) + '（' + (p.input || '?') + '→' + (p.output || '?') + '）');
    }));
  const gridRows = [];
  for (const inRole of inputs) {
    const cells = [];
    for (const role of roles) {
      const src = cmSources(role).find(function (s) { return s.source === inRole.id; });
      cells.push(el('td', {}, el('button', {
        class: 'matrix-cell ' + (src ? 'on' : ''),
        title: src ? ('gain ' + src.gain_db + ' dB' + (src.inverted ? ' / 反相' : '')) : '点击添加该输入源',
        onclick: function () { cmToggleSource(role.id, inRole.id); },
      }, src ? ((src.gain_db !== 0 ? (src.gain_db > 0 ? '+' : '') + src.gain_db : '●') + (src.inverted ? 'Ø' : '')) : '·')));
    }
    gridRows.push(el('tr', {}, el('th', {}, inRole.id + ' #' + inRole.channel), ...cells));
  }
  const roleOptions = roles.map(function (r) { return el('option', { value: r.id }, r.id + (r.label ? '（' + r.label + '）' : '')); });
  const selRole = CM.selectedRole && roles.some(function (r) { return r.id === CM.selectedRole; }) ? CM.selectedRole : (roles[0] && roles[0].id);
  CM.selectedRole = selRole;
  const editorRole = cmFindRole(selRole);
  return card('Remap 矩阵（输入源 × 输出角色，多源求和）', [
    el('div', { class: 'matrix-pivot' },
      el('span', { class: 'muted' }, '一键预设：'), presetSel,
      el('button', { class: 'btn small', onclick: function () {
        const id = $('#cm-remap-preset').value;
        if (id) cmApplyRemapPreset(id);
      } }, '套用预设'),
      el('span', { class: 'muted' }, '预设仅用于生成 sources（保存前会物化，不依赖 preset 字符串）')),
    el('div', { class: 'matrix-wrap' }, el('table', { class: 'matrix-table' },
      el('thead', {}, el('tr', {}, el('th', {}, '输入 \\ 输出'), ...roles.map(function (r) {
        return el('th', {}, el('button', { class: 'matrix-cell ' + (r.id === selRole ? 'sel' : ''), onclick: function () { CM.selectedRole = r.id; renderChannelmap(); } }, r.id));
      }))),
      el('tbody', {}, ...gridRows))),
    el('h3', { style: 'margin-top:12px' }, '输出角色 ' + (selRole || '—') + ' 的输入源'),
    ...(editorRole ? cmSources(editorRole).map(function (s, idx) {
      return el('div', { class: 'source-row' },
        el('select', { onchange: function (e) { s.source = e.target.value; cmMarkDirty(); renderChannelmap(); } },
          ...inputs.map(function (r) { return el('option', { value: r.id, selected: r.id === s.source }, r.id + ' #' + r.channel); })),
        el('input', { type: 'number', step: '0.1', value: s.gain_db, title: '增益 dB', onchange: function (e) { s.gain_db = Number(e.target.value) || 0; cmMarkDirty(); } }),
        el('label', { style: 'font-size:12px' }, el('input', { type: 'checkbox', checked: !!s.inverted, onchange: function (e) { s.inverted = e.target.checked; cmMarkDirty(); } }), ' 反相'),
        el('label', { style: 'font-size:12px' }, el('input', { type: 'checkbox', checked: !!s.mute, onchange: function (e) { s.mute = e.target.checked; cmMarkDirty(); } }), ' 静音'),
        el('button', { class: 'btn small ghost', onclick: function () { editorRole.sources.splice(idx, 1); cmMarkDirty(); renderChannelmap(); } }, '删除'));
    }) : []),
    el('div', { class: 'btn-row' },
      el('button', { class: 'btn small ghost', onclick: function () {
        const role = cmFindRole(selRole);
        if (!role) return;
        role.sources.push({ source: (inputs[0] && inputs[0].id) || 'FL', gain_db: 0, inverted: false, mute: false });
        cmMarkDirty(); renderChannelmap();
      } }, '+ 添加输入源'),
      el('span', { class: 'muted', style: 'font-size:12px' }, '多源相加用于下混/低频互补偿；bass_management enabled 时 SUB 的 sources 必须为空。')),
  ], { className: 'wide' });
}

function cmRenderRolesCard(roles, pbNames, pbChannels) {
  const firFiles = cmFirFiles();
  return card('每角色校准（输出通道 / 增益 / 延迟 / 极性 / 分频 / PEQ / FIR）', el('div', { class: 'role-grid' },
    ...roles.map(function (role) {
      return el('div', { class: 'role-card' },
        el('h3', {}, role.id + (role.label ? ' · ' + role.label : ''),
          el('span', {},
            el('button', { class: 'btn small ghost', onclick: function () { cmTestTone(role.id, 'tone'); } }, '测试音'),
            el('button', { class: 'btn small ghost', onclick: function () { cmTestTone(role.id, 'sweep'); } }, '扫频'))),
        el('div', { class: 'role-fields' },
          el('label', {}, '输出通道（可多通道 fan-out）', el('input', {
            value: (role.outputs || []).join(','), placeholder: '0 或 0,1',
            onchange: function (e) {
              role.outputs = e.target.value.split(',').map(function (x) { return Number(x.trim()); }).filter(function (x) { return Number.isInteger(x) && x >= 0; });
              cmMarkDirty();
            },
          })),
          el('label', {}, 'enabled', el('select', { onchange: function (e) { role.enabled = e.target.value === 'true'; cmMarkDirty(); } },
            el('option', { value: 'true', selected: role.enabled !== false }, '启用'), el('option', { value: 'false', selected: role.enabled === false }, '禁用'))),
          el('label', {}, 'gain dB', el('input', { type: 'number', step: '0.1', value: role.gain_db, onchange: function (e) { role.gain_db = Number(e.target.value) || 0; cmMarkDirty(); } })),
          el('label', {}, 'delay ms', el('input', { type: 'number', step: '0.01', value: role.delay_ms, onchange: function (e) { role.delay_ms = Number(e.target.value) || 0; cmMarkDirty(); } })),
          el('label', {}, 'polarity', el('select', { onchange: function (e) { role.polarity = e.target.value; cmMarkDirty(); } },
            el('option', { value: 'normal', selected: role.polarity !== 'invert' }, 'normal'), el('option', { value: 'invert', selected: role.polarity === 'invert' }, 'invert'))),
          el('label', {}, 'mute', el('input', { type: 'checkbox', checked: !!role.mute, onchange: function (e) { role.mute = e.target.checked; cmMarkDirty(); } })),
          el('label', {}, 'HPF Hz / 阶', cmXoInputs(role, 'highpass')),
          el('label', {}, 'LPF Hz / 阶', cmXoInputs(role, 'lowpass')),
          el('label', {}, 'FIR 文件', el('select', { onchange: function (e) {
            const name = e.target.value;
            if (!name) role.fir = null;
            else {
              const f = firFiles.find(function (x) { return x.name === name; });
              role.fir = { path: f.path, samplerate: f.sample_rate || null };
            }
            cmMarkDirty();
          } },
            el('option', { value: '', selected: !role.fir }, '无'),
            ...firFiles.map(function (f) {
              const current = role.fir && (role.fir.path === f.path || role.fir === f.name);
              return el('option', { value: f.name, selected: !!current }, f.name + (f.taps ? ' · ' + f.taps + 'taps' : '') + (f.sample_rate ? ' · ' + f.sample_rate + 'Hz' : '') + (f.level === 'long' ? ' ⚠' : ''));
            }))),
          el('label', {}, 'link_to', el('input', { value: role.link_to || '', placeholder: '可选', onchange: function (e) { role.link_to = e.target.value.trim() || null; cmMarkDirty(); } }))),
        el('details', { style: 'margin-top:6px' },
          (function () {
            const peq = role.peq || [];
            const counts = {};
            peq.forEach(function (p) { const t = p.type || 'peaking'; counts[t] = (counts[t] || 0) + 1; });
            const parts = Object.keys(counts).map(function (t) { return t + '×' + counts[t]; });
            return el('summary', { style: 'font-size:12px;cursor:pointer' }, 'PEQ（' + peq.length + ' 段' + (parts.length ? '：' + parts.join(' / ') : '') + '）');
          })(),
          el('div', { class: 'muted', style: 'font-size:11px' }, 'type：peaking（默认）/ low_shelf / high_shelf（v1.1）。peaking 的 q ∈ [0.1, 10]；shelf 的 q 是 RBJ slope S，必须在 [0.1, 1.0]，板端渲染为 slope=12·S'),
          ...(role.peq || []).map(function (p, i) {
            const ty = p.type || 'peaking';
            const isShelf = ty === 'low_shelf' || ty === 'high_shelf';
            return el('div', { class: 'source-row', style: 'grid-template-columns:96px 1fr 1fr 1fr 40px' },
              el('select', { title: 'peq.type（v1.1；板端按 type 渲染 Peaking/Lowshelf/Highshelf）', onchange: function (e) { p.type = e.target.value; cmMarkDirty(); renderChannelmap(); } },
                el('option', { value: 'peaking', selected: ty === 'peaking' }, 'peaking'),
                el('option', { value: 'low_shelf', selected: ty === 'low_shelf' }, 'low_shelf'),
                el('option', { value: 'high_shelf', selected: ty === 'high_shelf' }, 'high_shelf')),
              el('input', { type: 'number', value: p.freq, title: 'freq Hz（20–20000）', onchange: function (e) { p.freq = Number(e.target.value) || 0; cmMarkDirty(); } }),
              el('input', { type: 'number', value: p.q, min: '0.1', max: isShelf ? '1' : '10', step: isShelf ? '0.01' : '0.1', title: isShelf ? 'RBJ slope S，板端 slope=12S（必须在 [0.1, 1.0]，CamillaDSP 上限 12 dB/oct）' : 'Q（0.1–10）', onchange: function (e) { p.q = Number(e.target.value) || 0; cmMarkDirty(); } }),
              el('input', { type: 'number', step: '0.1', value: p.gain_db, title: 'gain dB（-24–24）', onchange: function (e) { p.gain_db = Number(e.target.value) || 0; cmMarkDirty(); } }),
              el('button', { class: 'btn small ghost', onclick: function () { role.peq.splice(i, 1); cmMarkDirty(); renderChannelmap(); } }, '×'));
          }),
          el('button', { class: 'btn small ghost', onclick: function () { if (!role.peq) role.peq = []; role.peq.push({ freq: 1000, q: 1.0, gain_db: 0, type: 'peaking' }); cmMarkDirty(); renderChannelmap(); } }, '+ PEQ')),
        el('div', { class: 'fir-row' }, el('span', { class: 'muted', style: 'font-size:11px' },
          '当前 ' + (pbChannels || '?') + 'ch：' + pbNames.slice(0, 8).join(' / ') + (pbNames.length > 8 ? ' …' : ''))));
    })),
  { className: 'wide' });
}

function cmXoInputs(role, key) {
  const xo = role[key] || null;
  const freq = el('input', { type: 'number', value: xo ? xo.freq : '', placeholder: '关', style: 'width:80px', onchange: function (e) {
    const v = e.target.value.trim();
    if (!v) role[key] = null;
    else role[key] = { freq: Number(v) || 80, order: (xo && xo.order) || 4, type: 'LinkwitzRiley' };
    cmMarkDirty();
  } });
  const order = el('select', { onchange: function (e) {
    const v = Number(e.target.value);
    if (role[key]) role[key].order = v;
    cmMarkDirty();
  } }, [2, 4, 6, 8].map(function (o) { return el('option', { value: o, selected: xo && xo.order === o }, o); }));
  return el('span', { style: 'display:flex;gap:4px' }, freq, order);
}

function cmRenderBassCard(roles) {
  const bm = CM.doc.bass_management || {};
  const roleIds = roles.map(function (r) { return r.id; });
  return card('低音管理（bass_management）', [
    el('div', { class: 'mode-chip' },
      el('label', {}, el('input', { type: 'checkbox', checked: !!bm.enabled, onchange: function (e) { bm.enabled = e.target.checked; cmMarkDirty(); } }), ' enabled（启用时由渲染器自动生成 SUB 输入与 mains HP）')),
    el('div', { class: 'role-fields', style: 'max-width:720px' },
      el('label', {}, 'lfe_role', el('select', { onchange: function (e) { bm.lfe_role = e.target.value; cmMarkDirty(); } },
        ...roleIds.map(function (id) { return el('option', { value: id, selected: bm.lfe_role === id }, id); }))),
      el('label', {}, 'lfe_to（输出角色，逗号分隔）', el('input', { value: (bm.lfe_to || []).join(','), onchange: function (e) { bm.lfe_to = e.target.value.split(',').map(function (x) { return x.trim(); }).filter(Boolean); cmMarkDirty(); } })),
      el('label', {}, 'lfe_gain_db', el('input', { type: 'number', step: '0.1', value: bm.lfe_gain_db || 0, onchange: function (e) { bm.lfe_gain_db = Number(e.target.value) || 0; cmMarkDirty(); } })),
      el('label', {}, 'mains HP Hz / 阶', el('input', { type: 'number', value: bm.mains_highpass_hz || 80, onchange: function (e) { bm.mains_highpass_hz = Number(e.target.value) || 80; cmMarkDirty(); } })),
      el('label', {}, 'SUB LP Hz', el('input', { type: 'number', value: bm.sub_lowpass_hz || 80, onchange: function (e) { bm.sub_lowpass_hz = Number(e.target.value) || 80; cmMarkDirty(); } })),
      el('label', {}, 'redirect.sources（逗号分隔）', el('input', { value: (bm.redirect && bm.redirect.sources || []).join(','), onchange: function (e) { bm.redirect = bm.redirect || {}; bm.redirect.sources = e.target.value.split(',').map(function (x) { return x.trim(); }).filter(Boolean); cmMarkDirty(); } })),
      el('label', {}, 'redirect.targets（逗号分隔）', el('input', { value: (bm.redirect && bm.redirect.targets || []).join(','), onchange: function (e) { bm.redirect = bm.redirect || {}; bm.redirect.targets = e.target.value.split(',').map(function (x) { return x.trim(); }).filter(Boolean); cmMarkDirty(); } })),
      el('label', {}, 'sub_gain_db', el('input', { type: 'number', step: '0.1', value: bm.sub_gain_db || 0, onchange: function (e) { bm.sub_gain_db = Number(e.target.value) || 0; cmMarkDirty(); } }))),
    el('p', { class: 'muted', style: 'font-size:11.5px' }, 'bass_management enabled 时，lfe_to/redirect.targets 里的 SUB 角色的 sources 必须为空（渲染器接管）；关掉后再手动配 sources。'),
  ], { className: 'wide' });
}

function cmRenderFirCard() {
  const files = cmFirFiles();
  return card('FIR 文件（上传/列表；taps/采样率/群延迟）', [
    el('div', { class: 'fir-row' },
      el('input', { type: 'file', id: 'cm-fir-file', accept: '.wav,.txt,.raw,.bin,.fir,.coe' }),
      el('button', { class: 'btn small primary', onclick: cmUploadFir }, '上传 FIR'),
      el('span', { class: 'muted', style: 'font-size:12px' }, '推荐 256–2048 taps；>4096 实验性、>8192 强警告（不硬砍）；采样率必须与模式一致，否则 validate/apply 拒绝（按模式分 profile）。')),
    table(['文件', 'taps', '采样率', '格式', '群延迟', '等级'], files.map(function (f) {
      return [f.name, f.taps == null ? '—' : f.taps, f.sample_rate || '—', (f.channels || '—') + 'ch/' + (f.bits || '—') + 'bit',
        f.group_delay_ms == null ? '—' : f.group_delay_ms.toFixed(2) + ' ms',
        el('span', { class: f.level === 'long' ? 'danger-tag' : f.level === 'experimental' ? 'warn-tag' : 'muted' }, f.level)];
    })),
    (CM.view && CM.view.renderText) ? el('details', { open: true }, el('summary', {}, '渲染配置（/run/tspi/camilladsp.yml）'), el('pre', {}, CM.view.renderText)) : null,
  ], { className: 'wide' });
}

function cmRenderProfileCard() {
  return card('多用户 profile（按输入布局 × 输出布局）', [
    el('div', { class: 'fir-row' },
      el('input', { id: 'cm-profile-name', placeholder: '自动：' + cmDefaultPresetName(), style: 'min-width:260px' }),
      el('button', { class: 'btn small primary', onclick: cmSaveProfile }, '保存 profile'),
      el('button', { class: 'btn small ghost', onclick: cmExportProfile }, '导出当前（YAML）'),
      el('span', { class: 'muted', style: 'font-size:12px' }, '目录：' + (CM.view ? CM.view.profiles_dir : ''))),
    table(['profile', '输入布局', '输出布局', '描述', '操作'], cmProfiles().map(function (p) {
      return [p.name, p.input_layout || '—', p.output_layout || '—', p.description || '—',
        el('span', {}, el('button', { class: 'btn small ghost', onclick: function () { cmLoadProfile(p.name); } }, '加载'),
          el('button', { class: 'btn small danger', onclick: function () { cmDeleteProfile(p.name); } }, '删除'))];
    })),
    el('details', { style: 'margin-top:8px' }, el('summary', {}, '导入 YAML'),
      el('textarea', { id: 'cm-import-yaml', rows: '6', placeholder: '粘贴 tspi-channelmap/v2 YAML', style: 'width:100%;margin:6px 0' }),
      el('button', { class: 'btn small', onclick: cmImportYaml }, '导入（存为 profile）')),
  ], { className: 'wide' });
}

/* ===== 输入内容检测（auto / 2.0 / multichannel） ===== */
function cmInputModeLabel(mode) {
  if (mode === 'stereo') return '2.0 内容 → 上混';
  if (mode === 'multichannel') return '多声道 → 直通';
  return '自动';
}
function cmRenderInputModeCard() {
  const im = CM.inputMode;
  if (!im) {
    return card('输入内容检测（auto / 2.0 / multichannel）', [
      el('p', { class: 'muted' }, '正在读取 /run/tspi/input-content.json…'),
    ], { className: 'wide' });
  }
  const modeSel = el('select', { id: 'cm-input-mode-select' },
    el('option', { value: 'auto', selected: (im.override || im.content_mode) === 'auto' }, '自动（override=auto，auto 默认直通）'),
    el('option', { value: 'stereo', selected: (im.override || im.content_mode) === 'stereo' }, '强制 2.0（走上混）'),
    el('option', { value: 'multichannel', selected: (im.override || im.content_mode) === 'multichannel' }, '强制多声道（直通）'));
  const active = (im.active_channels || []).join(' / ') || '—';
  const rms = Object.keys(im.rms_db || {}).slice(0, 8).map(function (k) {
    return k + ' ' + Number(im.rms_db[k]).toFixed(1) + ' dB';
  }).join('；') || '—';
  return card('输入内容检测（auto / 2.0 / multichannel）', [
    el('div', { class: 'mode-chip' },
      badge(im.state_available ? ('当前：' + cmInputModeLabel(im.content_mode)) : '检测器未运行',
        im.state_available ? (im.content_mode === 'stereo' ? 'info' : 'ok') : 'warn'),
      badge('判定：' + (im.detected || 'unknown'), im.detected === 'stereo' ? 'info' : (im.detected === 'multichannel' ? 'ok' : 'muted')),
      badge('override：' + (im.override || 'null'), im.override && im.override !== 'auto' ? 'warn' : 'muted'),
      im.daemon_running ? badge('daemon 运行中', 'ok') : badge('daemon 未运行/状态陈旧', 'warn'),
      im.h16_internal_upmix === true ? badge('H16 内部已上混', 'warn') : null,
      CM.view && CM.view.mock ? badge('MOCK', 'warn') : null),
    kv([
      ['检测/覆盖文件', im.state_path + '（只读） / ' + im.mode_path + '（覆盖）'],
      ['置信度', im.confidence != null ? Number(im.confidence).toFixed(2) : '—'],
      ['active_channels', active],
      ['RMS（前 8）', rms],
      ['阈值 / hold', (im.threshold_db != null ? im.threshold_db + ' dB' : '—') + ' / ' + (im.hold_ms != null ? im.hold_ms + ' ms' : '—')],
      ['容器布局', im.container_layout || '—'],
      ['检测器 / 更新时间', (im.detector || '—') + ' / ' + (im.updated_unix ? new Date(im.updated_unix * 1000).toLocaleString() : '—')],
      ['reason', im.reason || '—'],
    ]),
    ...(im.notes || []).map(function (n) { return alertBox('warn', n); }),
    el('div', { class: 'btn-row' },
      el('span', { class: 'muted', style: 'font-size:12px' }, '覆盖：'),
      modeSel,
      el('button', { class: 'btn small primary', onclick: cmSaveInputMode }, '保存覆盖'),
      el('button', { class: 'btn small ghost', onclick: function () { cmLoadInputMode(true); } }, '刷新状态')),
    el('p', { class: 'muted', style: 'font-size:11.5px' },
      '优先级：用户覆盖 > 检测器 > auto。auto 默认直通；检测到 2.0（仅 FL/FR 有能量）才走上混。H16 Pure/Pass-through 时板端上混；H16 已内部上混或多声道内容时禁止二次上混。文件与字段由 tspi-upmix/audio-stack 提供，本页只读运行态、写覆盖文件，不改 channelmap schema。'),
  ], { className: 'wide' });
}
async function cmLoadInputMode(force) {
  if (CM.inputModePending && !force) return;
  CM.inputModePending = true;
  try {
    CM.inputMode = await api('/api/channelmap/input-mode');
    if (force) toast('已刷新输入内容状态', 'ok');
  } catch (e) {
    if (force) toast(e.message, 'error');
  } finally {
    CM.inputModePending = false;
  }
  if (state.page === 'channelmap' && CM.doc) renderChannelmap();
}
async function cmSaveInputMode() {
  const select = $('#cm-input-mode-select');
  const mode = select ? select.value : 'auto';
  try {
    CM.inputMode = await api('/api/channelmap/input-mode', { method: 'PUT', body: { mode: mode } });
    toast('输入内容覆盖已保存：' + mode + (CM.view && CM.view.mock ? ' [mock]' : ''), 'ok');
    renderChannelmap();
  } catch (e) { toast(e.message, 'error'); }
}

/* ===== 操作 ===== */
async function cmRegenerate(input, output) {
  if (!input || !output) return;
  const answer = await confirmDialog({
    title: '切换布局并重建草稿？',
    message: '将按 ' + input + ' → ' + output + ' 重新生成显式 roles/sources，当前未保存编辑会丢失。',
    confirmLabel: '重建草稿',
    danger: false,
  });
  if (!answer.ok) return;
  try {
    const probe = cmProbe();
    const body = { input_layout: input, output_layout: output, profile_name: cmDefaultPresetName() };
    if (probe.capture && probe.capture.channels_max) body.capture_channels = probe.capture.channels_max;
    if (probe.playback && probe.playback.channels_max) body.playback_channels = probe.playback.channels_max;
    const result = await api('/api/channelmap/draft', { method: 'POST', body: body });
    CM.doc = result.document;
    CM.view.probe = result.probe;
    CM.view.presets = result.presets;
    CM.dirty = true;
    toast('已重建草稿：' + input + ' → ' + output, 'ok');
    renderChannelmap();
  } catch (e) { toast(e.message, 'error'); }
}
function cmToggleSource(roleId, inputRole) {
  const role = cmFindRole(roleId);
  if (!role) return;
  const sources = cmSources(role);
  const idx = sources.findIndex(function (s) { return s.source === inputRole; });
  if (idx >= 0) sources.splice(idx, 1);
  else sources.push({ source: inputRole, gain_db: 0, inverted: false, mute: false });
  cmMarkDirty();
  renderChannelmap();
}
function cmApplyRemapPreset(id) {
  const preset = cmRemapPresets()[id];
  if (!preset) return;
  if (preset.input !== CM.doc.input.layout || preset.output !== CM.doc.output.layout) {
    toast('该预设面向 ' + preset.input + ' → ' + preset.output + '，与当前布局不符；请先切换布局', 'warn');
    return;
  }
  const routes = preset.routes || [];
  cmRoles().forEach(function (role) {
    role.sources = routes.filter(function (r) {
      if (r.dest !== role.id) return false;
      if (r.mode === 'drop' || r.mode === 'silence') return false;
      return true;
    }).map(function (r) { return { source: r.src, gain_db: r.gain_db || 0, inverted: false, mute: !!r.mute }; });
  });
  CM.doc.remap.preset = null;
  CM.doc.remap.routes = [];
  cmMarkDirty();
  renderChannelmap();
  toast('已套用预设 ' + id + ' 并物化为 sources', 'ok');
}
async function cmValidate() {
  try {
    const result = await api('/api/channelmap/validate', { method: 'POST', body: { document: CM.doc } });
    CM.view.validation = result.validation;
    CM.view.probe = result.probe;
    CM.view.effective_roles = result.effective_roles;
    CM.view.input_roles = result.input_roles;
    toast(result.validation.valid ? '校验通过' + (result.validation.warnings.length ? '（' + result.validation.warnings.length + ' 条提示）' : '') : '校验失败：' + result.validation.errors.join('；'), result.validation.valid ? 'ok' : 'error');
    renderChannelmap();
  } catch (e) { toast(e.message, 'error'); }
}
async function cmSave(force) {
  try {
    const result = await api('/api/channelmap', { method: 'PUT', body: { document: CM.doc, force: !!force } });
    toast('已保存 active channelmap' + (result.outcome.backup ? '（旧文件已备份）' : ''), 'ok');
    await cmReloadAll();
  } catch (e) { toast(e.message, 'error'); }
}
async function cmApply() {
  const answer = await confirmDialog({
    title: '保存并应用 channelmap？',
    message: CM.view && CM.view.mode_off
      ? '当前模式 off：将校验/渲染/持久化，不启动 CamillaDSP。'
      : '将调用 tspi-channelmap apply：校验→渲染→重启 CamillaDSP；失败（exit 9）回滚 last-good 并显示错误。',
    confirmLabel: '保存并应用',
    danger: false,
  });
  if (!answer.ok) return;
  try {
    const result = await api('/api/channelmap/apply', { method: 'POST', body: { document: CM.doc } });
    CM.dirty = false;
    CM.applyError = null;
    CM.doc = result.view.document ? cmDeep(result.view.document) : CM.doc;
    CM.view = result.view;
    toast(result.message || '已应用', 'ok');
    await cmReloadAll();
  } catch (e) {
    CM.applyError = e.message;
    toast(e.message, 'error');
    renderChannelmap();
  }
}
async function cmRollback() {
  const answer = await confirmDialog({ title: '回滚 last-good？', message: '调用 tspi-channelmap rollback 恢复到上一次成功应用；mode active 时会重启 CamillaDSP。', confirmLabel: '回滚', danger: true });
  if (!answer.ok) return;
  try {
    const result = await api('/api/channelmap/rollback', { method: 'POST', body: {} });
    toast(result.message || '已回滚', 'ok');
    await cmReloadAll();
  } catch (e) { toast(e.message, 'error'); }
}
function cmResetWizard() {
  CM.doc = null;
  CM.dirty = false;
  CM.wizard = null;
  CM.selectedRole = null;
  renderChannelmapWizard();
}
async function cmTestTone(role, kind) {
  try {
    const result = await api('/api/channelmap/test-tone', { method: 'POST', body: { role: role, kind: kind } });
    toast('测试音已发送到 ' + role + '（通道 ' + result.output_channel + '，' + result.device + '）' + (result.mock ? ' [mock]' : ''), 'ok');
  } catch (e) { toast(e.message, 'error'); }
}
async function cmSaveProfile() {
  try {
    const input = $('#cm-profile-name');
    const name = (input && input.value.trim()) || (CM.doc.profile.name || cmDefaultPresetName());
    const result = await api('/api/channelmap/profiles', { method: 'POST', body: { name: name, document: CM.doc } });
    toast('profile 已保存：' + result.profile.name, 'ok');
    await cmReloadAll();
  } catch (e) { toast(e.message, 'error'); }
}
async function cmLoadProfile(name) {
  const answer = await confirmDialog({ title: '加载 profile？', message: '加载 “' + name + '” 会覆盖当前 active channelmap（旧文件会备份），之后请校验/应用。', confirmLabel: '加载', danger: false });
  if (!answer.ok) return;
  try {
    await api('/api/channelmap/profiles/load', { method: 'POST', body: { name: name } });
    toast('已加载 profile：' + name, 'ok');
    await cmReloadAll();
  } catch (e) { toast(e.message, 'error'); }
}
async function cmDeleteProfile(name) {
  const answer = await confirmDialog({ title: '删除 profile？', message: '删除 “' + name + '”（不会改动 active）。', confirmLabel: '删除', danger: true });
  if (!answer.ok) return;
  try {
    await api('/api/channelmap/profiles/delete', { method: 'POST', body: { name: name } });
    toast('已删除：' + name, 'ok');
    await cmReloadAll();
  } catch (e) { toast(e.message, 'error'); }
}
async function cmImportYaml() {
  const textarea = $('#cm-import-yaml');
  const yaml = textarea ? textarea.value.trim() : '';
  if (!yaml) { toast('请先粘贴 YAML', 'warn'); return; }
  try {
    const result = await api('/api/channelmap/import', { method: 'POST', body: { yaml: yaml, name: (CM.doc.profile.name || cmDefaultPresetName()) + '-import' } });
    toast('已导入：' + result.path, 'ok');
    await cmReloadAll();
  } catch (e) { toast(e.message, 'error'); }
}
function cmExportProfile() {
  const name = CM.doc.profile.name || cmDefaultPresetName();
  window.open('/api/channelmap/export?name=' + encodeURIComponent(name), '_blank');
}
async function cmLoadRender() {
  try {
    const result = await api('/api/channelmap/render');
    CM.view.renderText = result.text;
    toast(result.text ? '已读取渲染配置' : '渲染配置尚未生成（先 apply）', result.text ? 'ok' : 'warn');
    renderChannelmap();
  } catch (e) { toast(e.message, 'error'); }
}
function cmUploadFir() {
  const input = $('#cm-fir-file');
  const file = input && input.files && input.files[0];
  if (!file) { toast('请选择 FIR 文件', 'warn'); return; }
  const reader = new FileReader();
  reader.onload = async function () {
    try {
      const dataUrl = String(reader.result || '');
      const b64 = dataUrl.indexOf(',') >= 0 ? dataUrl.split(',').pop() : dataUrl;
      const entry = await api('/api/channelmap/fir', { method: 'POST', body: { name: file.name, data_base64: b64 } });
      toast('FIR 已上传：' + entry.name + '（' + (entry.taps || '?') + ' taps' + (entry.sample_rate ? ', ' + entry.sample_rate + ' Hz' : '') + '）', 'ok');
      if (entry.level === 'long') toast('>8192 taps，群延迟 ' + (entry.group_delay_ms || 0).toFixed(1) + ' ms，请确认 CPU/延迟预算', 'warn');
      await cmReloadAll();
    } catch (e) { toast(e.message, 'error'); }
  };
  reader.readAsDataURL(file);
}

/* ---------- 启动 ---------- */
async function boot() {
  bindNavigation();
  await loadHealth();
  if (state.mock) $('#mock-badge').hidden = false;
  try {
    const status = await api('/api/auth/status');
    if (status.setup_required) { showSetup(); return; }
  } catch { /* 状态接口异常时回退到登录页 */ }
  try {
    const session = await api('/api/auth/session');
    state.csrf = session.csrf_token || null;
    showApp();
    switchPage('overview');
  } catch {
    showLogin();
  }
}

boot();
