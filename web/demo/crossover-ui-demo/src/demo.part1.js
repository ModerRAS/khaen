/* =========================================================
 * KHAEN Audio · 低切 / 分频 + ART 支持 交互演示
 * 单文件演示页（数据内嵌，可双击打开）
 * 交互参考 Dirac Live Bass Control / ART 公开文档；界面与代码均为自制
 * ======================================================= */
'use strict';

const FR_DATA = /*__FR_DATA__*/ null;

/* ---------------- small utils ---------------- */
const $ = (s, root) => (root || document).querySelector(s);
const $$ = (s, root) => Array.from((root || document).querySelectorAll(s));
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const round1 = v => Math.round(v * 10) / 10;
const LOG_MIN = Math.log(20), LOG_MAX = Math.log(200);
const fToU = f => (Math.log(clamp(f, 20, 200)) - LOG_MIN) / (LOG_MAX - LOG_MIN);
const uToF = u => Math.exp(LOG_MIN + clamp(u, 0, 1) * (LOG_MAX - LOG_MIN));
const DB_MIN = -24, DB_MAX = 14;
const dbToY = (db, top, h) => top + (DB_MAX - clamp(db, DB_MIN, DB_MAX)) / (DB_MAX - DB_MIN) * h;
const fmtHz = f => (Math.abs(f - Math.round(f)) < 0.05 ? String(Math.round(f)) : f.toFixed(1));
const mean = arr => arr.reduce((s, x) => s + x, 0) / arr.length;

function roundRect(ctx, x, y, w, h, r) {
  r = Math.min(r, Math.abs(w) / 2, Math.abs(h) / 2);
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

/* ---------------- measured FR data ---------------- */
const CH_IDS = ['FL', 'FR', 'SL', 'SR', 'SUB1'];
const FREQ = FR_DATA.channels.FL.freq.slice();

function baselineDb(ch) {
  const c = FR_DATA.channels[ch];
  const lo = ch === 'SUB1' ? 30 : 100, hi = ch === 'SUB1' ? 60 : 200;
  const vals = c.freq.map((f, i) => [f, c.db_smooth[i]]).filter(p => p[0] >= lo && p[0] <= hi).map(p => p[1]);
  return vals.length ? mean(vals) : 0;
}
const CURVE = {};          // 每通道按自身通带归零后的实测幅度曲线
CH_IDS.forEach(ch => { CURVE[ch] = FR_DATA.channels[ch].db_smooth.map(v => round1(v - baselineDb(ch))); });

const detectedOf = ch => {
  const v = FR_DATA.channels[ch].detected_10db_hz;
  return (typeof v === 'number' && v >= 20) ? v : 60;
};
const GROUPS = {
  mains: {
    label: '主箱 L/R（FL + FR）', short: '主箱 L/R', channels: ['FL', 'FR'],
    detected: Math.max(detectedOf('FL'), detectedOf('FR')),
  },
  surround: {
    label: '环绕 L/R（SL + SR）', short: '环绕 L/R', channels: ['SL', 'SR'],
    detected: Math.max(detectedOf('SL'), detectedOf('SR')),
  },
};

function pairAvg(a, b) {
  return a.map((v, i) => 10 * Math.log10((Math.pow(10, v / 10) + Math.pow(10, b[i] / 10)) / 2));
}

/* ---------------- LR / Butterworth crossover math ---------------- */
// 2nd-order Butterworth prototype evaluated on jΩ (Ω = f/fc)
function bwLP(Om) {
  const d0 = 1 - Om * Om, d1 = Math.SQRT2 * Om, dd = d0 * d0 + d1 * d1;
  return [d0 / dd, -d1 / dd];
}
function bwHP(Om) {
  const d0 = 1 - Om * Om, d1 = Math.SQRT2 * Om, dd = d0 * d0 + d1 * d1;
  return [-Om * Om * d0 / dd, Om * Om * d1 / dd];
}
function cmul(a, b) { return [a[0] * b[0] - a[1] * b[1], a[0] * b[1] + a[1] * b[0]]; }
function cpow(a, n) { let r = [1, 0]; for (let i = 0; i < n; i++) r = cmul(r, a); return r; }
const cdb = c => 20 * Math.log10(Math.hypot(c[0], c[1]) + 1e-12);

/* ---------------- state ---------------- */
const DEFAULT_STATE = () => ({
  tab: 'bc',
  group: 'mains',
  groups: {
    mains: { freq: 80, slope: 24 },
    surround: { freq: 80, slope: 24 },
  },
  sub: { gain: 0 },
  legend: { meas: true, hp: true, sub: true, pred: true, det: true },
  art: {
    row: 'FL',
    cell: { row: 'FL', col: 'SUB1' },
    fsiso: 150,
    applied: false,
    conns: {},
  },
});
let state = DEFAULT_STATE();

const ART_ROWS = [
  { id: 'FL', label: '主箱 L' }, { id: 'FR', label: '主箱 R' },
  { id: 'SL', label: '环绕 L' }, { id: 'SR', label: '环绕 R' },
  { id: 'LFE', label: 'LFE' },
];
const ART_COLS = ['FL', 'FR', 'SL', 'SR', 'SUB1'];
const connKey = (row, col) => row + '<-' + col;

function initArtConns() {
  const c = {};
  const def = (row, col, level, low, high) => { c[connKey(row, col)] = { on: true, level, low, high }; };
  def('FL', 'FR', -20, 80, 150); def('FL', 'SUB1', -18, 20, 120);
  def('FR', 'FL', -20, 80, 150); def('FR', 'SUB1', -18, 20, 120);
  def('SL', 'SR', -20, 80, 150); def('SL', 'SUB1', -18, 20, 120);
  def('SR', 'SL', -20, 80, 150); def('SR', 'SUB1', -18, 20, 120);
  def('LFE', 'SUB1', -14, 20, 120);
  return c;
}
function connDefaults(row, col) {
  return col === 'SUB1' ? { on: false, level: -18, low: 20, high: 120 } : { on: false, level: -20, low: 80, high: 150 };
}
function getConn(row, col) {
  const k = connKey(row, col);
  if (!state.art.conns[k]) state.art.conns[k] = connDefaults(row, col);
  return state.art.conns[k];
}

/* ---------------- prediction ---------------- */
function predict(groupKey) {
  const g = state.groups[groupKey];
  const chans = GROUPS[groupKey].channels;
  const mains = pairAvg(CURVE[chans[0]], CURVE[chans[1]]);
  const sub = CURVE.SUB1.map(v => v + state.sub.gain);
  const stages = g.slope / 12;
  const fc = g.freq;
  const hp = [], lp = [], sum = [];
  for (let i = 0; i < FREQ.length; i++) {
    const Om = FREQ[i] / fc;
    const h = cpow(bwHP(Om), stages);
    const l = cpow(bwLP(Om), stages);
    const a = Math.pow(10, mains[i] / 20), b = Math.pow(10, sub[i] / 20);
    const Hs = [a * h[0], a * h[1]];
    const Hl = [b * l[0], b * l[1]];
    hp.push(cdb(Hs)); lp.push(cdb(Hl));
    sum.push(cdb([Hs[0] + Hl[0], Hs[1] + Hl[1]]));
  }
  return { freq: FREQ, meas: mains, hp, lp, sum, fc };
}
function dbAt(f, arr) {
  let bi = 0;
  for (let i = 0; i < FREQ.length; i++) if (Math.abs(FREQ[i] - f) < Math.abs(FREQ[bi] - f)) bi = i;
  return arr[bi];
}

/* ---------------- BC canvas ---------------- */
const bcCanvas = $('#bc-canvas');
const bcCtx = bcCanvas.getContext('2d');
let bcHoverF = null, bcDragging = false, bcNearLine = false;
let bcRects = null;

function resizeCanvas(cv, ctx) {
  const r = cv.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  cv.width = Math.max(1, Math.round(r.width * dpr));
  cv.height = Math.max(1, Math.round(r.height * dpr));
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  return r;
}

function bcLayout() {
  const r = bcCanvas.getBoundingClientRect();
  return { L: 56, R: r.width - 20, T: 26, B: r.height - 40, W: r.width, H: r.height };
}
const xOfF = (f, lay) => lay.L + fToU(f) * (lay.R - lay.L);

function drawBcGrid(ctx, lay) {
  // background
  ctx.fillStyle = '#0b111b';
  ctx.fillRect(0, 0, lay.W, lay.H);
  // warning zone below detected
  const g = GROUPS[state.group];
  const det = g.detected;
  const detX = xOfF(det, lay);
  const grd = ctx.createLinearGradient(lay.L, 0, detX, 0);
  grd.addColorStop(0, 'rgba(255,123,114,0.10)');
  grd.addColorStop(1, 'rgba(255,123,114,0.015)');
  ctx.fillStyle = grd;
  ctx.fillRect(lay.L, lay.T, Math.max(0, detX - lay.L), lay.B - lay.T);
  // horizontal grid + labels
  ctx.strokeStyle = 'rgba(120,140,170,0.10)';
  ctx.lineWidth = 1;
  ctx.font = '10px "Cascadia Mono", Consolas, monospace';
  ctx.textAlign = 'right'; ctx.textBaseline = 'middle';
  for (let db = -20; db <= 10; db += 10) {
    const y = dbToY(db, lay.T, lay.B - lay.T);
    ctx.beginPath(); ctx.moveTo(lay.L, y); ctx.lineTo(lay.R, y); ctx.stroke();
    ctx.fillStyle = db === 0 ? 'rgba(220,232,248,0.55)' : 'rgba(140,158,184,0.45)';
    ctx.fillText((db > 0 ? '+' : '') + db, lay.L - 9, y);
  }
  // vertical grid + freq labels
  const ticks = [20, 25, 31.5, 40, 50, 63, 80, 100, 125, 160, 200];
  ctx.textAlign = 'center'; ctx.textBaseline = 'top';
  for (const f of ticks) {
    const x = xOfF(f, lay);
    ctx.strokeStyle = f === 100 ? 'rgba(120,140,170,0.16)' : 'rgba(120,140,170,0.08)';
    ctx.beginPath(); ctx.moveTo(x, lay.T); ctx.lineTo(x, lay.B); ctx.stroke();
    ctx.fillStyle = 'rgba(140,158,184,0.55)';
    ctx.fillText(f >= 1000 ? (f / 1000) + 'k' : String(f), x, lay.B + 9);
  }
  ctx.fillStyle = 'rgba(140,158,184,0.4)';
  ctx.textAlign = 'right'; ctx.fillText('Hz', lay.R, lay.B + 24);
  ctx.textAlign = 'left';
  ctx.fillText('dB', lay.L - 38, lay.T - 16);
  // frame
  ctx.strokeStyle = 'rgba(120,140,170,0.18)';
  ctx.strokeRect(lay.L + 0.5, lay.T + 0.5, lay.R - lay.L, lay.B - lay.T);
}

function drawCurve(ctx, arr, lay, color, width, dash, alpha) {
  ctx.save();
  ctx.globalAlpha = alpha == null ? 1 : alpha;
  ctx.strokeStyle = color;
  ctx.lineWidth = width;
  ctx.lineJoin = 'round';
  if (dash) ctx.setLineDash(dash);
  ctx.beginPath();
  for (let i = 0; i < FREQ.length; i++) {
    const x = xOfF(FREQ[i], lay), y = dbToY(arr[i], lay.T, lay.B - lay.T);
    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  }
  ctx.stroke();
  ctx.restore();
}

function drawBc() {
  const r = resizeCanvas(bcCanvas, bcCtx);
  const lay = bcLayout();
  bcRects = lay;
  const ctx = bcCtx;
  drawBcGrid(ctx, lay);
  const pred = predict(state.group);
  const g = GROUPS[state.group];
  const lg = state.legend;
  // measured per-channel + pair average
  if (lg.meas) {
    for (const ch of g.channels) drawCurve(ctx, CURVE[ch], lay, '#9fb0c8', 1, null, 0.28);
    drawCurve(ctx, pred.meas, lay, '#a8b8cf', 1.7, null, 0.9);
    drawCurve(ctx, CURVE.SUB1.map(v => v + state.sub.gain), lay, '#f7ab4a', 1, [2, 3], 0.30);
  }
  if (lg.hp) drawCurve(ctx, pred.hp, lay, '#35c4ff', 1.8, null, 0.95);
  if (lg.sub) drawCurve(ctx, pred.lp, lay, '#f7ab4a', 1.7, [6, 4], 0.9);
  if (lg.pred) {
    // glow
    ctx.save();
    ctx.shadowColor = 'rgba(243,247,255,0.55)';
    ctx.shadowBlur = 9;
    drawCurve(ctx, pred.sum, lay, '#f3f7ff', 2.5, null, 1);
    ctx.restore();
  }
  // detected lower cutoff line
  if (lg.det) {
    const detX = xOfF(g.detected, lay);
    ctx.save();
    ctx.strokeStyle = 'rgba(255,123,114,0.85)';
    ctx.lineWidth = 1.4;
    ctx.setLineDash([5, 5]);
    ctx.beginPath(); ctx.moveTo(detX, lay.T); ctx.lineTo(detX, lay.B); ctx.stroke();
    ctx.restore();
    ctx.save();
    ctx.font = '10px "Microsoft YaHei", sans-serif';
    const txt = '实测下潜 ' + fmtHz(g.detected) + ' Hz';
    const tw = ctx.measureText(txt).width + 12;
    const bx = Math.min(detX + 6, lay.R - tw);
    roundRect(ctx, bx, lay.T + 6, tw, 19, 5);
    ctx.fillStyle = 'rgba(70,24,26,0.9)'; ctx.fill();
    ctx.strokeStyle = 'rgba(255,123,114,0.55)'; ctx.stroke();
    ctx.fillStyle = '#ffb3ad'; ctx.textAlign = 'left'; ctx.textBaseline = 'middle';
    ctx.fillText(txt, bx + 6, lay.T + 16);
    ctx.restore();
  }
  // crossover marker
  const cg = state.groups[state.group];
  const mx = xOfF(cg.freq, lay);
  const warn = cg.freq < g.detected;
  const col = warn ? '#ff8f86' : '#f7ab4a';
  ctx.save();
  ctx.strokeStyle = col;
  ctx.lineWidth = bcDragging || bcNearLine ? 2.4 : 1.9;
  ctx.shadowColor = col; ctx.shadowBlur = bcDragging ? 14 : 8;
  ctx.beginPath(); ctx.moveTo(mx, lay.T); ctx.lineTo(mx, lay.B); ctx.stroke();
  // handle
  ctx.shadowBlur = bcDragging ? 18 : 10;
  ctx.fillStyle = '#0d1420';
  ctx.beginPath(); ctx.arc(mx, lay.T + 2, bcDragging ? 8 : 7, 0, Math.PI * 2); ctx.fill();
  ctx.strokeStyle = col; ctx.lineWidth = 2.4; ctx.stroke();
  ctx.beginPath(); ctx.arc(mx, lay.T + 2, 2.4, 0, Math.PI * 2); ctx.fillStyle = col; ctx.fill();
  // label pill
  ctx.shadowBlur = 0;
  ctx.font = '600 11.5px "Cascadia Mono", Consolas, monospace';
  const label = (warn ? '⚠ ' : '') + '低切 ' + fmtHz(cg.freq) + ' Hz';
  const lw = ctx.measureText(label).width + 16;
  let lx = mx - lw / 2;
  lx = clamp(lx, lay.L + 2, lay.R - lw - 2);
  roundRect(ctx, lx, lay.T - 21, lw, 19, 6);
  ctx.fillStyle = warn ? 'rgba(86,28,30,0.95)' : 'rgba(64,45,18,0.95)'; ctx.fill();
  ctx.strokeStyle = col; ctx.lineWidth = 1; ctx.stroke();
  ctx.fillStyle = warn ? '#ffc0ba' : '#ffd39a'; ctx.textAlign = 'left'; ctx.textBaseline = 'middle';
  ctx.fillText(label, lx + 8, lay.T - 11.5);
  ctx.restore();
  // hover crosshair
  if (bcHoverF != null && !bcDragging) {
    const hx = xOfF(bcHoverF, lay);
    ctx.save();
    ctx.strokeStyle = 'rgba(200,220,245,0.22)'; ctx.lineWidth = 1;
    ctx.setLineDash([2, 4]);
    ctx.beginPath(); ctx.moveTo(hx, lay.T); ctx.lineTo(hx, lay.B); ctx.stroke();
    ctx.restore();
    const y = dbToY(dbAt(bcHoverF, pred.sum), lay.T, lay.B - lay.T);
    ctx.save();
    ctx.fillStyle = '#f3f7ff'; ctx.shadowColor = '#f3f7ff'; ctx.shadowBlur = 8;
    ctx.beginPath(); ctx.arc(hx, y, 3.2, 0, Math.PI * 2); ctx.fill();
    ctx.restore();
  }
  updateBcCorner(pred);
}

function updateBcCorner(pred) {
  const f = bcHoverF != null ? bcHoverF : state.groups[state.group].freq;
  $('#corner-f').textContent = fmtHz(f) + ' Hz';
  $('#corner-db').textContent = round1(dbAt(f, pred.sum)).toFixed(1) + ' dB';
}

function markBcDirty() {
  if (bcRaf) return;
  bcRaf = requestAnimationFrame(() => { bcRaf = null; drawBc(); });
}
let bcRaf = null;

/* ---------------- BC UI ---------------- */
function syncBcUI() {
  const g = GROUPS[state.group];
  const cur = state.groups[state.group];
  $('#bc-group-label').textContent = g.label;
  $$('#bc-group-tabs .seg-btn').forEach(b => b.classList.toggle('active', b.dataset.group === state.group));
  const input = $('#cutoff-input');
  if (document.activeElement !== input) input.value = fmtHz(cur.freq);
  $$('#slope-tabs [data-slope]').forEach(b => b.classList.toggle('active', Number(b.dataset.slope) === cur.slope));
  $('#sub-gain').value = state.sub.gain;
  $('#sub-gain-out').textContent = (state.sub.gain > 0 ? '+' : '') + state.sub.gain.toFixed(1) + ' dB';
  // range meter
  const uDet = fToU(g.detected) * 100, uCur = fToU(cur.freq) * 100;
  $('#range-warn').style.width = uDet + '%';
  $('#range-safe').style.left = uDet + '%';
  $('#range-safe').style.width = (100 - uDet) + '%';
  $('#range-det').style.left = uDet + '%';
  $('#range-cur').style.left = uCur + '%';
  $('#range-text').textContent = '下潜 ≥ ' + fmtHz(g.detected) + ' Hz';
  const note = $('#range-note');
  if (cur.freq < g.detected) {
    note.innerHTML = '低于实测下潜 <b>' + fmtHz(g.detected) + ' Hz</b>：低频失真 / 摆位风险，建议 ≥ ' + fmtHz(Math.ceil(g.detected / 5) * 5) + ' Hz';
  } else {
    note.innerHTML = '当前低切高于实测下潜，处于安全范围（主箱可按标称 ±3 dB 取 55 Hz）';
  }
  // warning card
  const warn = $('#bc-warn');
  if (cur.freq < g.detected) {
    warn.hidden = false;
    $('#bc-warn-title').textContent = '低于实测下潜 · ' + fmtHz(cur.freq) + ' Hz';
    $('#bc-warn-text').textContent = g.short + ' 在 ' + fmtHz(cur.freq) + ' Hz 附近已低于实测可用下潜（' + fmtHz(g.detected) + ' Hz）。继续下探会增加低频失真与摆位风险；建议抬高到 ' + fmtHz(Math.ceil(g.detected / 5) * 5) + ' Hz 以上，或改用 SUB 承担低频。';
  } else {
    warn.hidden = true;
  }
  $('#bc-foot-status').textContent = g.short + ' · ' + fmtHz(cur.freq) + ' Hz · ' + (cur.slope === 12 ? 'LR2' : cur.slope === 24 ? 'LR4' : 'LR8') + '（' + cur.slope + ' dB/oct）';
  markBcDirty();
}

function setCutoff(f, opts) {
  const g = state.groups[state.group];
  g.freq = clamp(round1(f), 20, 200);
  syncBcUI();
  if (opts && opts.input) {
    const el = $('#cutoff-input'); el.value = fmtHz(g.freq);
  }
}
function setSlope(slope) { state.groups[state.group].slope = slope; syncBcUI(); }
function setGroup(key) { state.group = key; syncBcUI(); }
function setPreset(key) {
  state.group = key;
  state.groups[key].freq = key === 'mains' ? 55 : 60;
  syncBcUI();
  toast(key === 'mains' ? '已应用主箱预设：55 Hz' : '已应用环绕预设：60 Hz');
}

/* BC canvas pointer interaction */
function canvasFreq(e) {
  const r = bcCanvas.getBoundingClientRect();
  const lay = bcLayout();
  const x = e.clientX - r.left;
  if (x < lay.L - 10 || x > lay.R + 10) return null;
  return uToF((x - lay.L) / (lay.R - lay.L));
}
bcCanvas.addEventListener('pointermove', e => {
  const f = canvasFreq(e);
  if (f == null) { bcHoverF = null; bcNearLine = false; bcCanvas.style.cursor = 'default'; markBcDirty(); return; }
  bcHoverF = f;
  const lay = bcLayout();
  bcNearLine = Math.abs(xOfF(state.groups[state.group].freq, lay) - (e.clientX - bcCanvas.getBoundingClientRect().left)) < 20;
  bcCanvas.style.cursor = bcDragging ? 'grabbing' : (bcNearLine ? 'ew-resize' : 'crosshair');
  if (bcDragging) setCutoff(f);
  markBcDirty();
});
bcCanvas.addEventListener('pointerdown', e => {
  if (e.button !== 0) return;
  const f = canvasFreq(e);
  if (f == null) return;
  bcDragging = true;
  bcCanvas.setPointerCapture && bcCanvas.setPointerCapture(e.pointerId);
  setCutoff(f);
  bcCanvas.style.cursor = 'grabbing';
  markBcDirty();
});
window.addEventListener('pointerup', () => {
  if (!bcDragging) return;
  bcDragging = false;
  if (bcNearLine === false) bcCanvas.style.cursor = 'crosshair';
  markBcDirty();
});
bcCanvas.addEventListener('pointerleave', () => { bcHoverF = null; bcNearLine = false; bcCanvas.style.cursor = 'default'; markBcDirty(); });
bcCanvas.addEventListener('dblclick', e => {
  const f = canvasFreq(e);
  if (f == null) return;
  setCutoff(Math.round(f));
});

/* BC controls */
$$('#bc-group-tabs .seg-btn').forEach(b => b.addEventListener('click', () => setGroup(b.dataset.group)));
$$('#slope-tabs [data-slope]').forEach(b => b.addEventListener('click', () => setSlope(Number(b.dataset.slope))));
$$('#bc-legend .item').forEach(it => it.addEventListener('click', () => {
  const k = it.dataset.toggle;
  state.legend[k] = !state.legend[k];
  it.classList.toggle('off', !state.legend[k]);
  markBcDirty();
}));
$('#cutoff-input').addEventListener('input', e => {
  const v = parseFloat(e.target.value);
  if (Number.isFinite(v)) setCutoff(v);
});
$('#cutoff-input').addEventListener('blur', () => syncBcUI());
$('#sub-gain').addEventListener('input', e => { state.sub.gain = Number(e.target.value); syncBcUI(); });
$$('[data-preset]').forEach(b => b.addEventListener('click', () => setPreset(b.dataset.preset)));
$('#btn-reset').addEventListener('click', () => {
  state.groups = { mains: { freq: 80, slope: 24 }, surround: { freq: 80, slope: 24 } };
  state.sub.gain = 0;
  state.group = 'mains';
  syncBcUI();
  toast('已重置为默认 80 Hz / LR4');
});
