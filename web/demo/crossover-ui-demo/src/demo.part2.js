
/* =========================================================
 * 导出 / 配置
 * ======================================================= */
let exportMode = 'bc';
let exportData = { json: '', yaml: '' };

function buildBcConfig() {
  const now = new Date().toISOString();
  const m = state.groups.mains, s = state.groups.surround;
  const hp = (g) => ({ freq_hz: g.freq, slope_db_oct: g.slope, order: g.slope / 6, type: 'LinkwitzRiley' });
  return {
    schema: 'tspi-crossover-demo/v1',
    generated_at: now,
    note: '演示导出，不写入设备。由 2026-11-18 板端实测 FR 驱动的分频设计。',
    measurement: { source: 'board-window-20261118-calib/analysis/board-pilot/B/fr/UMIK', seat: 'B', mic: 'UMIK-1' },
    bass_management: {
      enabled: true,
      lfe_role: 'SUB1',
      sub_gain_db: state.sub.gain,
      mains_highpass_hz: m.freq,
      mains_highpass_order: m.slope / 6,
      mains_highpass_type: 'LinkwitzRiley',
      sub_lowpass_hz: m.freq,
      sub_lowpass_order: m.slope / 6,
      sub_lowpass_type: 'LinkwitzRiley',
    },
    groups: [
      { id: 'mains', channels: ['FL', 'FR'], highpass: hp(m), detected_rolloff_hz: round1(GROUPS.mains.detected) },
      { id: 'surround', channels: ['SL', 'SR'], highpass: hp(s), detected_rolloff_hz: round1(GROUPS.surround.detected) },
    ],
    roles: [
      { id: 'FL', highpass: hp(m) }, { id: 'FR', highpass: hp(m) },
      { id: 'SL', highpass: hp(s) }, { id: 'SR', highpass: hp(s) },
      { id: 'SUB1', lowpass: { freq_hz: m.freq, slope_db_oct: m.slope, order: m.slope / 6, type: 'LinkwitzRiley', gain_db: state.sub.gain } },
    ],
  };
}
function buildBcYaml() {
  const m = state.groups.mains, s = state.groups.surround;
  const o = g => g.slope / 6;
  const roles = [
    ['FL', m], ['FR', m], ['SL', s], ['SR', s],
  ].map(([id, g]) => '  - id: ' + id + '\n    highpass: {freq: ' + fmtHz(g.freq) + ', order: ' + o(g) + ', type: LinkwitzRiley}').join('\n');
  return '# tspi channelmap 片段（演示导出，未写入设备）\n' +
    'bass_management:\n  enabled: true\n  lfe_role: SUB1\n  sub_gain_db: ' + state.sub.gain.toFixed(1) + '\n' +
    '  mains_highpass_hz: ' + fmtHz(m.freq) + '\n  mains_highpass_order: ' + o(m) + '\n' +
    '  sub_lowpass_hz: ' + fmtHz(m.freq) + '\n  sub_lowpass_order: ' + o(m) + '\n' +
    'roles:\n' + roles + '\n' +
    '  - id: SUB1\n    lowpass: {freq: ' + fmtHz(m.freq) + ', order: ' + o(m) + ', type: LinkwitzRiley}\n';
}

/* ---------------- ART state helpers ---------------- */
const ART_BANDS = [20, 25, 31.5, 40, 50, 63, 80, 100, 125, 160];
const RT_BEFORE = [1.55, 1.48, 1.40, 1.28, 1.12, 0.95, 0.80, 0.68, 0.58, 0.50];
let artTargets = RT_BEFORE.slice();
let artShown = RT_BEFORE.slice();
let artAnim = null;

function decayFor(row) {
  const before = RT_BEFORE.slice();
  const after = before.map((b, bi) => {
    const band = ART_BANDS[bi];
    let sumW = 0;
    for (const col of ART_COLS) {
      if (col === row) continue;
      const c = getConn(row, col);
      if (!c.on) continue;
      if (band < c.low || band > c.high) continue;
      if (band > state.art.fsiso) continue;
      const edgeLo = clamp(Math.log2(band / c.low) / 0.28, 0, 1);
      const edgeHi = clamp(Math.log2(c.high / band) / 0.28, 0, 1);
      const edge = Math.min(1, edgeLo, edgeHi);
      const w = clamp((-c.level - 1) / 23, 0, 1) * edge;
      sumW += w;
    }
    const reduction = 0.62 * (sumW / (sumW + 0.85));
    return Math.max(0.16, b * (1 - reduction));
  });
  return { before, after };
}
function artStats(row) {
  const d = decayFor(row);
  const idx = ART_BANDS.map((b, i) => i).filter(i => ART_BANDS[i] <= 125);
  const bAvg = mean(idx.map(i => d.before[i])), aAvg = mean(idx.map(i => d.after[i]));
  return { d, bAvg, aAvg, improve: bAvg > 0 ? (1 - aAvg / bAvg) * 100 : 0 };
}
function countConns() {
  let n = 0;
  for (const r of ART_ROWS) for (const c of ART_COLS) if (r.id !== c) n += getConn(r.id, c).on ? 1 : 0;
  return n;
}
function artWarnings() {
  const out = [];
  let hot = 0, surroundsToLfe = [];
  for (const r of ART_ROWS) for (const c of ART_COLS) {
    if (r.id === c) continue;
    const k = getConn(r.id, c);
    if (!k.on) continue;
    if (r.id === 'LFE' && (c === 'SL' || c === 'SR') && k.low < GROUPS.surround.detected) surroundsToLfe.push(c);
    if (k.level === -24) hot++;
  }
  if (surroundsToLfe.length) {
    out.push({
      kind: 'warn', title: '小箱过载风险',
      text: surroundsToLfe.join(' / ') + ' 正在支持 LFE，且 F-support Low 低于环绕箱实测下潜（' + fmtHz(GROUPS.surround.detected) + ' Hz）。小箱承受 LFE 低频有失真、过冲风险；建议停用该支持，或把 F-support Low 抬到 ' + fmtHz(Math.ceil(GROUPS.surround.detected / 5) * 5) + ' Hz 以上。',
    });
  }
  if (hot >= 3) {
    out.push({ kind: 'warn', title: '支持量过强', text: hot + ' 条连接设为 -24 dB（最强支持）。注意功放负载与扬声器冲程，建议先只改一组、计算后复测。' });
  }
  const row = state.art.row;
  const own = ART_COLS.filter(c => c !== row && getConn(row, c).on).length;
  if (own === 0) {
    out.push({ kind: 'info', title: '当前主组无支持', text: row + ' 当前没有启用任何支持组，ART 将只做常规房间校正；可在矩阵中开启 SUB1 等支持。' });
  }
  return out;
}

/* ---------------- ART UI ---------------- */
function renderArtMatrix() {
  const box = $('#art-matrix');
  box.innerHTML = '';
  const head = document.createElement('div');
  head.className = 'm-row m-head';
  head.innerHTML = '<div class="m-cell rowh">主组 \ 支持</div>' + ART_COLS.map(c => '<div class="m-cell">' + c + '</div>').join('');
  box.appendChild(head);
  for (const r of ART_ROWS) {
    const row = document.createElement('div');
    row.className = 'm-row';
    row.innerHTML = '<div class="m-cell rowh">' + r.id + '</div>';
    for (const c of ART_COLS) {
      const cell = document.createElement('div');
      cell.className = 'm-cell';
      const btn = document.createElement('button');
      btn.dataset.cell = r.id + '_' + c;
      if (r.id === c) { btn.disabled = true; btn.textContent = '—'; }
      else {
        btn.addEventListener('click', () => selectCell(r.id, c));
      }
      cell.appendChild(btn);
      row.appendChild(cell);
    }
    box.appendChild(row);
  }
}
function refreshArtMatrix() {
  $$('#art-matrix button[data-cell]').forEach(btn => {
    const [r, c] = btn.dataset.cell.split('_');
    if (r === c) return;
    const conn = getConn(r, c);
    btn.classList.toggle('on', conn.on);
    btn.classList.toggle('sel', state.art.cell.row === r && state.art.cell.col === c);
    const risky = conn.on && r === 'LFE' && (c === 'SL' || c === 'SR') && conn.low < GROUPS.surround.detected;
    btn.classList.toggle('risk', risky);
    btn.innerHTML = conn.on ? '<span class="lvl">' + conn.level + '</span>' + c : '·';
  });
  $$('#art-row-tabs .seg-btn').forEach(b => b.classList.toggle('active', b.dataset.row === state.art.row));
}
function refreshArtCard() {
  const { row, col } = state.art.cell;
  const conn = getConn(row, col);
  $('#cell-title').textContent = row + ' ← ' + col;
  const en = $('#cell-enabled');
  en.checked = conn.on;
  en.disabled = row === col;
  $('#support-level').value = conn.level;
  $('#support-level-out').textContent = conn.level + ' dB';
  $('#fs-low').value = conn.low;
  $('#fs-low-out').textContent = conn.low + ' Hz';
  $('#fs-high').value = conn.high;
  $('#fs-high-out').textContent = conn.high + ' Hz';
  $('#fsiso').value = state.art.fsiso;
  $('#fsiso-out').textContent = state.art.fsiso + ' Hz';
}
function refreshArtWarn() {
  const ws = artWarnings();
  const card = $('#art-warn');
  if (!ws.length) { card.hidden = true; return; }
  const w = ws[0];
  card.hidden = false;
  card.classList.toggle('info', w.kind === 'info');
  $('#art-warn-title').textContent = w.title;
  $('#art-warn-text').textContent = w.text;
}
function setArtTargets(animate) {
  const st = artStats(state.art.row);
  artTargets = st.d.after;
  if (!animate) { artShown = artTargets.slice(); drawArt(); return; }
  if (artAnim) cancelAnimationFrame(artAnim);
  const from = artShown.slice(), t0 = performance.now(), dur = 420, to = artTargets.slice();
  const step = (t) => {
    const p = clamp((t - t0) / dur, 0, 1), e = 1 - Math.pow(1 - p, 3);
    artShown = from.map((v, i) => v + (to[i] - v) * e);
    drawArt();
    if (p < 1) artAnim = requestAnimationFrame(step); else artAnim = null;
  };
  artAnim = requestAnimationFrame(step);
}
function refreshArt() {
  refreshArtMatrix();
  refreshArtCard();
  refreshArtWarn();
  const st = artStats(state.art.row);
  $('#art-group-label').textContent = '主组 ' + state.art.row + ' · 20–160 Hz';
  $('#art-before-rt').textContent = st.bAvg.toFixed(2) + ' s';
  $('#art-after-rt').textContent = st.aAvg.toFixed(2) + ' s';
  $('#art-improve').textContent = '-' + st.improve.toFixed(0) + '%';
  $('#art-conn').textContent = countConns() + ' 条';
  $('#art-foot-status').textContent = '支持连接 ' + countConns() + ' · Fsiso ' + state.art.fsiso + ' Hz · ' + (state.art.applied ? '已应用 ✓' : '未应用');
  setArtTargets(true);
}
function selectCell(row, col) {
  state.art.cell = { row, col };
  state.art.row = row;
  refreshArt();
}
function selectArtRow(row) { state.art.row = row; state.art.cell = { row, col: state.art.cell.col || 'SUB1' }; refreshArt(); }

/* ---------------- ART canvas ---------------- */
const artCanvas = $('#art-canvas');
const artCtx = artCanvas.getContext('2d');
function artLayout() {
  const r = artCanvas.getBoundingClientRect();
  return { L: 60, R: r.width - 24, T: 34, B: r.height - 44, W: r.width, H: r.height };
}
function drawArt() {
  resizeCanvas(artCanvas, artCtx);
  const lay = artLayout(), ctx = artCtx;
  const RW = lay.R - lay.L, RH = lay.B - lay.T;
  ctx.fillStyle = '#0b111b'; ctx.fillRect(0, 0, lay.W, lay.H);
  const maxRT = 1.8;
  const yOf = v => lay.T + (1 - clamp(v, 0, maxRT) / maxRT) * RH;
  // grid
  ctx.font = '10px "Cascadia Mono", Consolas, monospace';
  ctx.textAlign = 'right'; ctx.textBaseline = 'middle';
  for (let v = 0; v <= 1.8; v += 0.3) {
    const y = yOf(v);
    ctx.strokeStyle = v === 0 ? 'rgba(120,140,170,0.22)' : 'rgba(120,140,170,0.09)';
    ctx.beginPath(); ctx.moveTo(lay.L, y); ctx.lineTo(lay.R, y); ctx.stroke();
    ctx.fillStyle = 'rgba(140,158,184,0.5)';
    ctx.fillText(v.toFixed(1), lay.L - 10, y);
  }
  ctx.fillStyle = 'rgba(140,158,184,0.45)'; ctx.textAlign = 'left'; ctx.textBaseline = 'top';
  ctx.fillText('s', lay.L - 34, lay.T - 18);
  // bars
  const n = ART_BANDS.length;
  const slot = RW / n, bw = Math.min(30, slot * 0.34);
  for (let i = 0; i < n; i++) {
    const cx = lay.L + slot * (i + 0.5);
    const before = RT_BEFORE[i], after = artShown[i];
    // before bar
    const xb = cx - bw - 2, yb = yOf(before);
    ctx.fillStyle = 'rgba(138,152,174,0.16)';
    roundRect(ctx, xb, yb, bw, lay.B - yb, 4); ctx.fill();
    ctx.strokeStyle = 'rgba(138,152,174,0.45)'; ctx.lineWidth = 1;
    roundRect(ctx, xb + .5, yb + .5, bw - 1, lay.B - yb - 1, 4); ctx.stroke();
    ctx.fillStyle = 'rgba(160,176,200,0.62)';
    ctx.font = '10px "Cascadia Mono", Consolas, monospace';
    ctx.textAlign = 'center'; ctx.textBaseline = 'bottom';
    ctx.fillText(before.toFixed(2), xb + bw / 2, yb - 3);
    // after bar
    const xa = cx + 2, ya = yOf(after);
    const grd = ctx.createLinearGradient(0, ya, 0, lay.B);
    grd.addColorStop(0, 'rgba(46,230,200,0.95)');
    grd.addColorStop(1, 'rgba(24,120,110,0.55)');
    ctx.fillStyle = grd;
    ctx.shadowColor = 'rgba(46,230,200,0.35)'; ctx.shadowBlur = 9;
    roundRect(ctx, xa, ya, bw, lay.B - ya, 4); ctx.fill();
    ctx.shadowBlur = 0;
    // after value
    ctx.fillStyle = '#9ff2e3';
    ctx.font = '600 10px "Cascadia Mono", Consolas, monospace';
    ctx.textAlign = 'center'; ctx.textBaseline = 'bottom';
    ctx.fillText(after.toFixed(2), xa + bw / 2, ya - 3);
    // band label
    ctx.fillStyle = 'rgba(160,176,200,0.6)';
    ctx.font = '10px "Cascadia Mono", Consolas, monospace';
    ctx.textAlign = 'center'; ctx.textBaseline = 'top';
    ctx.fillText(String(ART_BANDS[i]), cx, lay.B + 9);
  }
  ctx.fillStyle = 'rgba(140,158,184,0.4)'; ctx.textAlign = 'right';
  ctx.fillText('Hz', lay.R, lay.B + 27);
  // target line + pill label (drawn on top so it stays readable)
  ctx.save();
  ctx.strokeStyle = 'rgba(46,230,200,0.45)'; ctx.setLineDash([5, 5]); ctx.lineWidth = 1;
  ctx.beginPath(); ctx.moveTo(lay.L, yOf(0.4)); ctx.lineTo(lay.R, yOf(0.4)); ctx.stroke();
  const tl = '目标 RT60 ≈ 0.4 s（示意）';
  ctx.font = '10px "Microsoft YaHei", sans-serif';
  const tw = ctx.measureText(tl).width + 14;
  roundRect(ctx, lay.R - tw - 4, yOf(0.4) - 19, tw, 17, 5);
  ctx.fillStyle = 'rgba(10,16,26,0.92)'; ctx.fill();
  ctx.strokeStyle = 'rgba(46,230,200,0.35)'; ctx.stroke();
  ctx.fillStyle = 'rgba(120,240,218,0.8)'; ctx.textAlign = 'left'; ctx.textBaseline = 'middle';
  ctx.fillText(tl, lay.R - tw + 3, yOf(0.4) - 10);
  ctx.restore();
  // frame
  ctx.strokeStyle = 'rgba(120,140,170,0.18)'; ctx.lineWidth = 1;
  ctx.strokeRect(lay.L + .5, lay.T + .5, RW, RH);
  // legend in-canvas? handled by HTML legend
}
function markArtDirty() { requestAnimationFrame(drawArt); }

/* ---------------- ART interactions ---------------- */
$('#art-row-tabs').addEventListener('click', e => {
  const b = e.target.closest('[data-row]');
  if (!b) return;
  selectArtRow(b.dataset.row);
});
$('#cell-enabled').addEventListener('change', e => {
  const { row, col } = state.art.cell;
  if (row === col) return;
  getConn(row, col).on = e.target.checked;
  refreshArt();
});
$('#support-level').addEventListener('input', e => {
  const { row, col } = state.art.cell;
  getConn(row, col).level = Number(e.target.value);
  refreshArt();
});
$('#fs-low').addEventListener('input', e => {
  const { row, col } = state.art.cell;
  const c = getConn(row, col);
  c.low = clamp(Number(e.target.value), 20, c.high);
  refreshArt();
});
$('#fs-high').addEventListener('input', e => {
  const { row, col } = state.art.cell;
  const c = getConn(row, col);
  c.high = clamp(Number(e.target.value), c.low, 150);
  refreshArt();
});
$('#fsiso').addEventListener('input', e => {
  state.art.fsiso = Number(e.target.value);
  refreshArt();
});
$('#btn-calc').addEventListener('click', () => openConfirm());
$('#btn-art-export').addEventListener('click', () => openExport('art'));
$('#btn-save').addEventListener('click', () => openExport('bc'));

/* ---------------- confirm / export modals ---------------- */
function openConfirm() {
  const list = $('#confirm-list');
  const ws = artWarnings();
  const rowConns = ART_COLS.filter(c => c !== state.art.row && getConn(state.art.row, c).on);
  list.innerHTML = '';
  const add = (label, value, cls) => {
    const li = document.createElement('li');
    if (cls) li.className = cls;
    li.innerHTML = '<span>' + label + '</span><b>' + value + '</b>';
    list.appendChild(li);
  };
  add('已启用支持连接', countConns() + ' 条');
  add('当前主组 ' + state.art.row + ' 的支持组', rowConns.length ? rowConns.join(' / ') : '无');
  add('Fsiso（支持上限）', state.art.fsiso + ' Hz');
  add('支持量范围', '-24 ~ -1 dB（-24 = 最强）');
  ws.forEach(w => add(w.title, w.text.length > 40 ? w.text.slice(0, 40) + '…' : w.text, w.kind === 'warn' ? 'warn' : ''));
  $('#confirm-overlay').hidden = false;
}
function applyConfirm() {
  $('#confirm-overlay').hidden = true;
  state.art.applied = true;
  refreshArt();
  toast('已应用 ART 支持配置（演示，不写入设备）');
}
function openExport(mode) {
  exportMode = mode;
  exportData = mode === 'bc'
    ? { json: JSON.stringify(buildBcConfig(), null, 2), yaml: buildBcYaml() }
    : { json: JSON.stringify(buildArtConfig(), null, 2), yaml: buildArtYaml() };
  $('.modal h2', $('#export-overlay')).textContent = mode === 'bc' ? '导出分频配置' : '导出 ART 支持配置';
  const tabs = $('#export-tabs');
  tabs.innerHTML = '<button class="active" data-view="json">crossover-config.json</button><button data-view="yaml">channelmap 片段（YAML）</button>';
  $$('#export-tabs button').forEach(b => b.addEventListener('click', () => setExportView(b.dataset.view)));
  setExportView('json');
  $('#export-overlay').hidden = false;
}
function setExportView(view) {
  $$('#export-tabs button').forEach(b => b.classList.toggle('active', b.dataset.view === view));
  $('#export-code').textContent = view === 'json' ? exportData.json : exportData.yaml;
}
function closeModal(name) {
  $('#' + name + '-overlay').hidden = true;
}
function downloadConfig() {
  const blob = new Blob([exportData.json], { type: 'application/json' });
  const a = document.createElement('a');
  a.href = URL.createObjectURL(blob);
  a.download = exportMode === 'bc' ? 'crossover-config.json' : 'art-support-config.json';
  document.body.appendChild(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 4000);
  toast('已下载 ' + a.download);
}
function buildArtConfig() {
  const conns = [];
  for (const r of ART_ROWS) for (const c of ART_COLS) {
    if (r.id === c) continue;
    const k = getConn(r.id, c);
    if (k.on) conns.push({ main_group: r.id, support_group: c, support_level_db: k.level, f_support_low_hz: k.low, f_support_high_hz: k.high });
  }
  return {
    schema: 'tspi-art-support-demo/v1',
    generated_at: new Date().toISOString(),
    note: '演示导出，不写入设备；衰减预览为示意模型。',
    art: { fsiso_hz: state.art.fsiso, main_groups: ART_ROWS.map(r => r.id), support_groups: ART_COLS, connections: conns },
    warnings: artWarnings().map(w => ({ level: w.kind, title: w.title, text: w.text })),
    decay_preview: { model: 'demo-model', bands_hz: ART_BANDS, rt60_before_s: RT_BEFORE, rt60_after_selected_main_s: round1Arr(artStats(state.art.row).d.after) },
  };
}
function round1Arr(a) { return a.map(v => Math.round(v * 100) / 100); }
function buildArtYaml() {
  const lines = ['# tspi ART 支持配置片段（演示导出，未写入设备）', 'art:', '  fsiso_hz: ' + state.art.fsiso, '  main_groups: [FL, FR, SL, SR, LFE]', '  support_groups: [FL, FR, SL, SR, SUB1]', '  connections:'];
  for (const r of ART_ROWS) for (const c of ART_COLS) {
    if (r.id === c) continue;
    const k = getConn(r.id, c);
    if (k.on) lines.push('    - {main: ' + r.id + ', support: ' + c + ', level_db: ' + k.level + ', f_low: ' + k.low + ', f_high: ' + k.high + '}');
  }
  return lines.join('\n') + '\n';
}

/* ---------------- tabs / caption / cursor / toast ---------------- */
function setTab(tab) {
  state.tab = tab;
  $$('.tab').forEach(b => b.classList.toggle('active', b.dataset.tab === tab));
  $$('.page').forEach(p => p.classList.toggle('active', p.id === 'page-' + tab));
  requestAnimationFrame(() => { if (tab === 'bc') drawBc(); else { artShown = artStats(state.art.row).d.after.slice(); drawArt(); } });
}
$$('.tab').forEach(b => b.addEventListener('click', () => setTab(b.dataset.tab)));

let capTimer = null;
function caption(n, text, ms) {
  const c = $('#caption');
  c.innerHTML = (n ? '<span class="n">' + n + '</span>' : '') + text;
  c.hidden = false;
  c.style.animation = 'none'; void c.offsetWidth; c.style.animation = '';
  clearTimeout(capTimer);
  capTimer = setTimeout(() => { c.hidden = true; }, ms || 4600);
}
let toastTimer = null;
function toast(text) {
  const t = $('#toast');
  t.textContent = text; t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.hidden = true; }, 2800);
}
const cursor = $('#cursor');
window.addEventListener('pointermove', e => {
  cursor.classList.add('on');
  cursor.style.left = e.clientX + 'px';
  cursor.style.top = e.clientY + 'px';
});
window.addEventListener('pointerdown', () => cursor.classList.add('down'));
window.addEventListener('pointerup', () => {
  cursor.classList.remove('down');
  cursor.classList.add('click');
  setTimeout(() => cursor.classList.remove('click'), 460);
});
$$('[data-close]').forEach(b => b.addEventListener('click', () => closeModal(b.dataset.close)));
$('#btn-download').addEventListener('click', downloadConfig);
$('#btn-confirm-apply').addEventListener('click', applyConfirm);

/* ---------------- boot ---------------- */
function init() {
  state.art.conns = initArtConns();
  renderArtMatrix();
  syncBcUI();
  refreshArt();
  if (bcRaf) { cancelAnimationFrame(bcRaf); bcRaf = null; }
  requestAnimationFrame(() => { drawBc(); drawArt(); });
  window.addEventListener('resize', () => { drawBc(); drawArt(); });
  window.addEventListener('keydown', e => { if (e.key === 'Escape') { $('#export-overlay').hidden = true; $('#confirm-overlay').hidden = true; } });
}

/* ---------------- automation hooks (用于录屏/截图) ---------------- */
window.__demo = {
  ready: true,
  state: () => JSON.parse(JSON.stringify(state)),
  setTab, setGroup, setCutoff, setSlope, setPreset,
  selectArtRow, selectCell,
  setSupportLevel(db) { const { row, col } = state.art.cell; getConn(row, col).level = clamp(Math.round(db), -24, -1); refreshArt(); },
  setFsiso(hz) { state.art.fsiso = clamp(Math.round(hz), 50, 150); refreshArt(); },
  toggleCell(on) { const { row, col } = state.art.cell; if (row !== col) getConn(row, col).on = !!on; refreshArt(); },
  openExport, openConfirm, applyConfirm, closeAll() { $('#export-overlay').hidden = true; $('#confirm-overlay').hidden = true; },
  caption, toast,
  cursorVisible(v) { $('#cursor').style.display = v ? '' : 'none'; },
  markerScreen() {
    const lay = bcLayout(), r = bcCanvas.getBoundingClientRect();
    return { x: r.left + xOfF(state.groups[state.group].freq, lay), y: r.top + lay.T + 2 };
  },
  freqScreen(f) {
    const lay = bcLayout(), r = bcCanvas.getBoundingClientRect();
    return { x: r.left + xOfF(f, lay), y: r.top + lay.B - 4 };
  },
  chartCenter() { const r = bcCanvas.getBoundingClientRect(); return { x: r.left + r.width * 0.55, y: r.top + r.height * 0.5 }; },
  cellScreen(row, col) {
    const el = document.querySelector('[data-cell="' + row + '_' + col + '"]');
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
  },
  sliderScreen(id) {
    const el = document.getElementById(id);
    const r = el.getBoundingClientRect();
    return { left: r.left, right: r.right, y: r.top + r.height / 2, width: r.width };
  },
  valueScreen(id, value, min, max) {
    const el = document.getElementById(id);
    const r = el.getBoundingClientRect();
    const t = (value - min) / (max - min);
    return { x: r.left + 8 + t * (r.width - 16), y: r.top + r.height / 2 };
  },
  elScreen(sel) {
    const el = document.querySelector(sel);
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
  },
};

init();
