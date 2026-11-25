
/* =========================================================
 * Step 7–8 renderers + export + hooks + init（修正版）
 * - 目标曲线：全带 20Hz→20k 真正 −10dB（tilt10）
 * - 低切：默认主箱 55 / 环绕 60；下潜取实测滚降表
 * - ART：0…−24 dB 衰减语义（0=关闭，−24=最强）
 * - MIMO：0…−24 dB 衰减/支持（示意）
 * - Step8：实测 / 示意 / 目标 分列
 * ======================================================= */
function tiltDb(f) { return -10 * Math.log10(clamp(f, 20, 20000) / 20) / 3; }
function smoothArr(a, r) {
  return a.map((_, i) => {
    let s = 0, n = 0;
    for (let j = i - r; j <= i + r; j++) if (j >= 0 && j < a.length) { s += a[j]; n++; }
    return s / n;
  });
}
function designPrediction(group) {
  const d = state.design;
  const g = group || d.group || 'mains';
  const freqs = FR_BASS.FL.freq.filter(f => f >= 20 && f <= 200);
  const idx = FR_BASS.FL.freq.map((f, i) => f >= 20 && f <= 200 ? i : -1).filter(i => i >= 0);
  const raw = idx.map(i => CURVE_BASS[g][i]);
  const meas = smoothArr(raw, 1);
  const corrBase = smoothArr(raw, 2);
  const stages = d.slope[g] / 12;
  const out = { freq: freqs, meas, target: [], pred: [] };
  for (let k = 0; k < freqs.length; k++) {
    const f = freqs[k];
    const tgt = d.target === 'tilt10' ? tiltDb(f) : 0;
    let corr = corrBase[k] + (tgt - corrBase[k]) * 0.9;
    corr = clamp(corr, tgt - 3, tgt + 3.5);
    const hp = cdb(cpow(bwHP(f / d.cut[g]), stages));
    out.target.push(tgt);
    out.pred.push(corr + hp);
  }
  return out;
}
function rolloffText() {
  return '实测滚降：主箱 ' + GROUPS.mains.detected + ' Hz · 环绕 ' + GROUPS.surround.detected + ' Hz';
}
function designWarnings() {
  const d = state.design, w = [];
  if (d.cut.mains < GROUPS.mains.detected) w.push({ k: 'bad', t: '主箱低切低于实测滚降', m: d.cut.mains + ' Hz < 主箱实测滚降 ' + GROUPS.mains.detected + ' Hz（FR 1/6-oct，−10 dB 峰值法；见下方滚降表）' });
  if (d.cut.surround < GROUPS.surround.detected) w.push({ k: 'warn', t: '环绕低切低于实测滚降', m: d.cut.surround + ' Hz < 环绕实测滚降 ' + GROUPS.surround.detected + ' Hz（建议 ≥ ' + GROUPS.surround.detected + ' Hz）' });
  let risky = [];
  for (const r of ART_ROWS) for (const c of ART_COLS) {
    if (r === c) continue;
    const k = getConn(r, c);
    if (k.on && r === 'LFE' && (c === 'SL' || c === 'SR') && k.low < GROUPS.surround.detected) risky.push(c + '（' + k.level + ' dB）');
  }
  if (risky.length) w.push({ k: 'warn', t: 'ART 小箱过载风险（示意）', m: risky.join('/') + ' 支持 LFE 且 F-support Low 低于 ' + GROUPS.surround.detected + ' Hz' });
  if (d.mimo.height <= -18) w.push({ k: 'warn', t: 'MIMO 天空衰减设置过强（示意）', m: '天空 MIMO 衰减 ' + d.mimo.height + ' dB，注意小箱负载；该数值为演示模型，未上板验证' });
  return w;
}
function renderStep7() {
  const d = state.design;
  const c = getConn(d.artCell.row, d.artCell.col);
  $('#step-body').innerHTML =
    '<div class="design-grid">' +
      '<div class="design-main">' +
        '<div class="panel" style="flex:1"><div class="panel-h">低切 / 分频局部图（20–200 Hz） <span class="hint" id="design-sub"></span></div>' +
          '<div class="panel-b" style="padding:0"><canvas id="design-canvas" class="room-canvas"></canvas></div>' +
          '<div class="chart-foot" style="flex:0 0 32px"><span class="hint" style="font-size:10px">主箱 55 / 环绕 60 默认，可拖动/输入；目标曲线与 ART/MIMO 衰减实时联动。</span><span style="display:flex;align-items:center;gap:9px"><span class="foot-right" id="design-foot" style="font-size:10px"></span><button class="btn ghost mini" id="btn-recommend" data-test="btn-recommend">一键推荐</button></span></div>' +
        '</div>' +
        '<div class="panel" style="flex:0 0 148px"><div class="panel-h">目标曲线（全带 20 Hz–20 kHz） <span class="hint" id="target-band-hint"></span></div>' +
          '<div class="panel-b" style="padding:0"><canvas id="target-band-canvas" class="room-canvas"></canvas></div>' +
        '</div>' +
      '</div>' +
      '<div class="design-side">' +
        '<div class="panel"><div class="panel-h">目标曲线 <span class="badge3 model">设计目标</span></div><div class="panel-b">' +
          '<div class="target-row">' +
            '<button class="target-btn ' + (d.target === 'flat' ? 'active' : '') + '" data-target="flat" data-test="target-flat"><b>Flat</b><small>全带 0 dB</small></button>' +
            '<button class="target-btn ' + (d.target === 'tilt10' ? 'active' : '') + '" data-target="tilt10" data-test="target-tilt10"><b>Tilt 10</b><small>20 Hz 0 dB → 20 kHz −10 dB</small></button>' +
          '</div>' +
          '<div style="font-size:9px;color:var(--muted2);margin-top:4px;line-height:1.4">tilt10：20 Hz 0 dB → 20 kHz −10 dB（全带）；局部图仅显示 20–200 段。' + (d.target ? '' : ' <span style="color:var(--amber)">未选择</span>') + '</div>' +
        '</div></div>' +
        '<div class="panel"><div class="panel-h">低切 / 分频 <span class="hint">主箱 55 / 环绕 60（可改）</span></div><div class="panel-b">' +
          '<div class="mini-seg" id="lc-group" style="margin-bottom:7px"><button data-group="mains" class="' + (d.group === 'mains' ? 'active' : '') + '" data-test="lc-group-mains">主箱 L/R</button><button data-group="surround" class="' + (d.group === 'surround' ? 'active' : '') + '" data-test="lc-group-surround">环绕 L/R</button></div>' +
          '<div class="lc-row"><span class="lbl">低切</span><input type="number" id="lc-freq" data-test="lc-freq" min="20" max="200" step="1" value="' + d.cut[d.group] + '"><span class="unit">Hz</span>' +
            '<div class="mini-seg" id="lc-slope" style="max-width:120px"><button data-slope="24" class="' + (d.slope[d.group] === 24 ? 'active' : '') + '">24</button><button data-slope="48" class="' + (d.slope[d.group] === 48 ? 'active' : '') + '">48</button></div></div>' +
          '<div style="display:flex;gap:6px;margin-top:7px;flex-wrap:wrap"><button class="btn ghost mini" data-lc="55" data-test="lc-55">主箱 55 Hz</button><button class="btn ghost mini" data-lc="60" data-test="lc-60">环绕 60 Hz</button></div>' +
          '<div id="lc-warn" style="margin-top:7px;font-size:10px"></div>' +
          '<div style="font-size:8.5px;color:var(--muted2);line-height:1.4;margin-top:4px;font-family:var(--mono)">实测滚降：主箱 45 / 环绕 60 Hz（FL 43.6 / SL 59.9 / SR 61.7）</div>' +
        '</div></div>' +
        '<div class="panel"><div class="panel-h">ART 衰减（示意） <span class="badge3 sim">0…−24 dB</span></div><div class="panel-b">' +
          '<div class="art-matrix-mini" id="art-mini"></div>' +
          '<div class="lc-row" style="margin-top:6px"><span class="lbl" id="art-cell-lbl">FL ← SUB1</span><input type="range" id="art-level" data-test="art-level" min="-24" max="0" step="1" value="' + c.level + '"><span class="unit" id="art-level-out">' + (c.level < 0 ? c.level + ' dB' : '0（关闭）') + '</span></div>' +
          '<div class="mimo-row" style="margin:0;font-size:9px;color:var(--muted2);line-height:1.4">0 = 关闭 · −24 dB = 最强（示意）</div>' +
        '</div></div>' +
        (KHAEN_LIVE
          ? '<div class="panel"><div class="panel-h">皇帝位侧路 <span class="badge3 model">f_hi / 从喇叭</span></div><div class="panel-b">' +
              '<div class="lc-row"><span class="lbl">上沿</span><input type="number" id="f-hi" data-test="f-hi" min="20" max="150" step="1" value="' + d.fHi + '"><span class="unit">Hz</span></div>' +
              '<div style="display:flex;gap:8px;flex-wrap:wrap;margin-top:8px">' +
                ['FL', 'FR', 'SL', 'SR', 'HFL', 'HFR', 'SUB1'].map(name => '<label style="font-size:11px"><input type="checkbox" data-support="' + name + '" ' + (d.supports.includes(name) ? 'checked' : '') + '> ' + name + '</label>').join('') +
              '</div>' +
              '<div style="font-size:9px;color:var(--muted2);line-height:1.4;margin-top:6px">缺省不勾选。ART 滑杆留在上面，不进入求解请求。</div>' +
            '</div></div>'
          : '<div class="panel"><div class="panel-h">MIMO 衰减 / 支持（示意） <span class="badge3 sim">0…−24 dB</span></div><div class="panel-b">' +
              '<div class="mimo-grid">' + ['mains', 'surround', 'height', 'sub'].map(k => '<div class="mimo-cell"><div class="mimo-cap"><span>' + ({ mains: '主箱', surround: '环绕', height: '天空', sub: 'SUB' })[k] + '</span><output>' + d.mimo[k] + ' dB</output></div><input id="mimo-' + k + '" type="range" min="-24" max="0" step="1" value="' + d.mimo[k] + '" data-mimo="' + k + '" data-test="mimo-' + k + '"></div>').join('') + '</div>' +
              '<div style="font-size:9px;color:var(--muted2);line-height:1.4;margin-top:2px">0 = 不参与 · −24 dB = 最强（示意）</div>' +
            '</div></div>') +
        '<div id="design-warn"></div>' +
      '</div>' +
    '</div>';
  $$('#step-body [data-target]').forEach(b => b.addEventListener('click', () => { d.target = b.dataset.target; d.touched = true; renderStep7(); }));
  $$('#lc-group button').forEach(b => b.addEventListener('click', () => { d.group = b.dataset.group; renderStep7(); }));
  $$('#lc-slope button').forEach(b => b.addEventListener('click', () => { d.slope[d.group] = Number(b.dataset.slope); d.touched = true; renderStep7(); }));
  const lcIn = $('#lc-freq');
  lcIn.addEventListener('change', () => { d.cut[d.group] = clamp(Number(lcIn.value) || 60, 20, 200); d.touched = true; refreshDesign(); });
  $$('#step-body [data-lc]').forEach(b => b.addEventListener('click', () => {
    const v = Number(b.dataset.lc);
    if (v === 55) { d.group = 'mains'; d.cut.mains = 55; }
    else { d.group = 'surround'; d.cut.surround = 60; }
    d.touched = true; renderStep7();
  }));
  renderArtMini();
  const artLevel = $('#art-level');
  artLevel.addEventListener('input', () => {
    const conn = getConn(d.artCell.row, d.artCell.col);
    conn.level = Number(artLevel.value);
    conn.on = conn.level < 0;
    const out = $('#art-level-out'); if (out) out.textContent = conn.level < 0 ? conn.level + ' dB' : '0（关闭）';
    d.touched = true;
    renderArtMini();
    refreshDesign();
  });
  const fHi = $('#f-hi');
  if (fHi) fHi.addEventListener('change', () => { d.fHi = Math.min(150, Math.max(20, Number(fHi.value) || 150)); });
  $$('#step-body [data-support]').forEach(box => box.addEventListener('change', () => {
    const name = box.dataset.support;
    if (box.checked && !d.supports.includes(name)) d.supports.push(name);
    if (!box.checked) d.supports = d.supports.filter(item => item !== name);
  }));
  $$('#step-body [data-mimo]').forEach(inp => inp.addEventListener('input', () => {
    d.mimo[inp.dataset.mimo] = Number(inp.value);
    const out = inp.parentElement.querySelector('output'); if (out) out.textContent = inp.value + ' dB';
    d.touched = true; refreshDesign();
  }));
  $('#btn-recommend').addEventListener('click', () => {
    d.target = 'tilt10'; d.cut.mains = 55; d.cut.surround = 60; d.slope.mains = 24; d.slope.surround = 24;
    d.mimo = { mains: -6, surround: -6, height: -12, sub: -12 };
    d.art = defaultArtConns(); d.artCell = { row: 'FL', col: 'SUB1' };
    d.touched = true;
    renderStep7();
    toast('已套用推荐参数：tilt10 · 主箱 55 / 环绕 60 · ART/MIMO 默认（示意）');
  });
  refreshDesign();
  requestAnimationFrame(() => { drawDesignChart(); drawTargetBand(); });
}
function renderArtMini() {
  const box = $('#art-mini'); if (!box) return;
  const d = state.design;
  let html = '<div class="amm-row">' + '<div class="h">主组</div>' + ART_COLS.map(c => '<div class="h">' + c + '</div>').join('') + '</div>';
  for (const r of ART_ROWS) {
    html += '<div class="amm-row"><div class="h" style="text-align:left;padding-left:6px;color:#a8b8ce">' + r + '</div>';
    for (const c of ART_COLS) {
      if (r === c) { html += '<button class="amm-cell" disabled>—</button>'; continue; }
      const k = getConn(r, c);
      const sel = d.artCell.row === r && d.artCell.col === c;
      html += '<button class="amm-cell ' + (k.on ? 'on' : '') + (sel ? ' sel' : '') + '" data-cell="' + r + '_' + c + '" data-test="art-cell-' + r + '_' + c + '">' + (k.on ? k.level : '0') + '</button>';
    }
    html += '</div>';
  }
  box.innerHTML = html;
  $$('#art-mini .amm-cell[data-cell]').forEach(btn => btn.addEventListener('click', () => {
    const [r, c] = btn.dataset.cell.split('_');
    d.artCell = { row: r, col: c };
    renderStep7();
  }));
  const lbl = $('#art-cell-lbl'); if (lbl) lbl.textContent = d.artCell.row + ' ← ' + d.artCell.col;
}
function refreshDesign() {
  const d = state.design;
  const lw = $('#lc-warn');
  if (lw) {
    const det = GROUPS[d.group].detected;
    if (d.cut[d.group] < det) lw.innerHTML = '<span style="color:var(--red)">✗ 低于' + GROUPS[d.group].label + '实测滚降 ' + det + ' Hz</span>';
    else lw.innerHTML = '<span style="color:var(--teal)">✓ 高于实测滚降 ' + det + ' Hz · SUB 低通跟随主箱 ' + d.cut.mains + ' Hz</span>';
  }
  const sub = $('#design-sub');
  if (sub) sub.textContent = '当前组 ' + GROUPS[d.group].label + ' · 主箱 ' + d.cut.mains + ' / 环绕 ' + d.cut.surround + ' Hz';
  const th = $('#target-band-hint');
  if (th) th.textContent = d.target === 'tilt10' ? 'tilt10 · −10 dB @ 20 kHz' : d.target === 'flat' ? 'flat · 全带 0 dB' : '未选择';
  const foot = $('#design-foot');
  if (foot) foot.textContent = '目标 ' + (d.target || '未选') + ' · 主箱 ' + d.cut.mains + ' / 环绕 ' + d.cut.surround + ' Hz · ART ' + countArt() + ' 条';
  const w = $('#design-warn');
  if (w) {
    const ws = designWarnings();
    w.innerHTML = ws.length ? ws.map(x => '<div class="warn-card ' + (x.k === 'bad' ? '' : 'info') + '" style="margin-top:6px"><div class="warn-ico">' + (x.k === 'bad' ? '!' : 'i') + '</div><div class="warn-body"><b>' + x.t + '</b><p>' + x.m + '</p></div></div>').join('') : '<div class="role-ok"><b>✓</b><span style="color:var(--teal)">当前设计无风险警告（低切高于实测滚降）</span></div>';
  }
  drawDesignChart();
  drawTargetBand();
  renderHeader();
}
function countArt() {
  let n = 0;
  for (const r of ART_ROWS) for (const c of ART_COLS) if (r !== c && getConn(r, c).on) n++;
  return n;
}
function drawDesignChart() {
  const cv = $('#design-canvas'); if (!cv) return;
  const { ctx, w, h } = setupCanvas(cv);
  const d = state.design;
  const p = designPrediction(d.group);
  const L = 54, R = w - 18, T = 20, B = h - 44;
  const yMin = -30, yMax = 16;
  const my = db => T + (yMax - clamp(db, yMin, yMax)) / (yMax - yMin) * (B - T);
  const mx = f => L + fToU(f) * (R - L);
  ctx.fillStyle = '#0a111a'; ctx.fillRect(0, 0, w, h);
  gridFreq(ctx, L, R, T, B, 20, 200, [20, 25, 31.5, 40, 50, 63, 80, 100, 125, 160, 200], null);
  ctx.font = '9.5px "Cascadia Mono", monospace'; ctx.textAlign = 'right'; ctx.textBaseline = 'middle';
  for (let v = -20; v <= 10; v += 10) { const y = my(v); ctx.strokeStyle = 'rgba(120,140,170,0.1)'; ctx.beginPath(); ctx.moveTo(L, y); ctx.lineTo(R, y); ctx.stroke(); ctx.fillStyle = 'rgba(140,158,184,0.5)'; ctx.fillText((v > 0 ? '+' : '') + v, L - 8, y); }
  // detected zone & line
  const detX = mx(GROUPS[d.group].detected);
  const grd = ctx.createLinearGradient(L, 0, detX, 0); grd.addColorStop(0, 'rgba(255,123,114,0.12)'); grd.addColorStop(1, 'rgba(255,123,114,0.015)');
  ctx.fillStyle = grd; ctx.fillRect(L, T, detX - L, B - T);
  const mapPts = arr => p.freq.map((f, i) => [f, arr[i]]);
  drawLine(ctx, mapPts(p.meas), mx, my, '#9fb0c8', 1.6, null, 0.85);
  if (d.target) drawLine(ctx, mapPts(p.target), mx, my, '#2ee6c8', 1.5, [6, 4], 0.9);
  ctx.save(); ctx.shadowColor = 'rgba(243,247,255,0.5)'; ctx.shadowBlur = 9;
  drawLine(ctx, mapPts(p.pred), mx, my, '#f3f7ff', 2.4, null, 1); ctx.restore();
  // both group cutoffs: current solid, other dashed
  const drawCut = (g, solid) => {
    const x = mx(d.cut[g]);
    ctx.save();
    ctx.strokeStyle = solid ? '#f7ab4a' : 'rgba(160,176,200,0.5)';
    ctx.lineWidth = solid ? 1.8 : 1.2; ctx.setLineDash(solid ? [] : [4, 4]);
    if (solid) { ctx.shadowColor = '#f7ab4a'; ctx.shadowBlur = 7; }
    ctx.beginPath(); ctx.moveTo(x, T); ctx.lineTo(x, B); ctx.stroke();
    ctx.restore();
    if (solid) {
      ctx.fillStyle = '#0d1420'; ctx.beginPath(); ctx.arc(x, T + 2, 6, 0, Math.PI * 2); ctx.fill(); ctx.strokeStyle = '#f7ab4a'; ctx.lineWidth = 2; ctx.stroke();
      ctx.fillStyle = 'rgba(64,45,18,0.95)'; const lbl = GROUPS[g].label.split(' ')[0] + ' ' + d.cut[g] + ' Hz'; ctx.font = '600 10.5px "Cascadia Mono", monospace';
      const lw = ctx.measureText(lbl).width + 14; let lx = clamp(x - lw / 2, L + 2, R - lw - 2);
      roundRect(ctx, lx, T - 18, lw, 16, 5); ctx.fill(); ctx.strokeStyle = '#f7ab4a'; ctx.lineWidth = 1; ctx.stroke();
      ctx.fillStyle = '#ffd39a'; ctx.textAlign = 'left'; ctx.textBaseline = 'middle'; ctx.fillText(lbl, lx + 7, T - 10);
    } else {
      ctx.fillStyle = 'rgba(180,196,216,0.75)'; ctx.font = '9px "Microsoft YaHei", sans-serif'; ctx.textAlign = 'left'; ctx.textBaseline = 'middle';
      ctx.fillText(GROUPS[g].label.split(' ')[0] + ' ' + d.cut[g], x + 4, B - 8);
    }
  };
  drawCut(d.group === 'mains' ? 'surround' : 'mains', false);
  drawCut(d.group, true);
  // detected label
  ctx.fillStyle = 'rgba(255,179,173,0.9)'; ctx.font = '9px "Microsoft YaHei", sans-serif'; ctx.textAlign = 'right'; ctx.textBaseline = 'top';
  ctx.fillText('实测滚降 ' + GROUPS[d.group].detected + ' Hz', detX - 4, T + 4);
  // legend
  const legend = [['#9fb0c8', '实测 FR'], ['#2ee6c8', '目标曲线（设计）'], ['#f3f7ff', '预测（示意）'], ['#f7ab4a', '低切点'], ['#ff7b72', '实测滚降']];
  ctx.font = '9.5px "Microsoft YaHei", sans-serif'; ctx.textAlign = 'left'; ctx.textBaseline = 'middle';
  let lx0 = L + 8;
  for (const [col, name] of legend) {
    ctx.fillStyle = col; ctx.fillRect(lx0, B + 22, 12, 2.5);
    ctx.fillStyle = 'rgba(180,196,216,0.75)'; ctx.fillText(name, lx0 + 16, B + 23);
    lx0 += ctx.measureText(name).width + 30;
  }
}
function drawTargetBand() {
  const cv = $('#target-band-canvas'); if (!cv) return;
  const { ctx, w, h } = setupCanvas(cv);
  const d = state.design;
  const L = 54, R = w - 18, T = 12, B = h - 24;
  const yMin = -12, yMax = 2;
  const my = v => T + (yMax - clamp(v, yMin, yMax)) / (yMax - yMin) * (B - T);
  const mx = f => L + fToU(f) * (R - L);
  ctx.fillStyle = '#0a111a'; ctx.fillRect(0, 0, w, h);
  ctx.font = '9px "Cascadia Mono", monospace';
  for (let v = -10; v <= 0; v += 2) {
    const y = my(v); ctx.strokeStyle = 'rgba(120,140,170,0.1)'; ctx.beginPath(); ctx.moveTo(L, y); ctx.lineTo(R, y); ctx.stroke();
    ctx.fillStyle = 'rgba(140,158,184,0.5)'; ctx.textAlign = 'right'; ctx.textBaseline = 'middle'; ctx.fillText(v + ' dB', L - 6, y);
  }
  for (const f of [20, 100, 1000, 10000, 20000]) {
    const x = mx(f); ctx.strokeStyle = 'rgba(120,140,170,0.08)'; ctx.beginPath(); ctx.moveTo(x, T); ctx.lineTo(x, B); ctx.stroke();
    ctx.fillStyle = 'rgba(140,158,184,0.55)'; ctx.textAlign = 'center'; ctx.textBaseline = 'top'; ctx.fillText(f >= 1000 ? (f / 1000) + 'k' : String(f), x, B + 5);
  }
  ctx.fillStyle = 'rgba(140,158,184,0.4)'; ctx.textAlign = 'right'; ctx.fillText('Hz', R, B + 15);
  // target curve
  const n = 240, pts = [];
  for (let i = 0; i <= n; i++) { const f = uToF(i / n); pts.push([f, d.target === 'tilt10' ? tiltDb(f) : 0]); }
  ctx.strokeStyle = d.target ? '#2ee6c8' : 'rgba(120,140,170,0.35)';
  ctx.lineWidth = 2; ctx.setLineDash(d.target ? [] : [4, 4]);
  ctx.beginPath(); pts.forEach((p, i) => { const x = mx(p[0]), y = my(p[1]); if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y); }); ctx.stroke(); ctx.setLineDash([]);
  if (d.target === 'tilt10') {
    // -10 dB @ 20k marker
    const x = mx(20000), y = my(-10);
    ctx.fillStyle = '#2ee6c8'; ctx.beginPath(); ctx.arc(x - 2, y, 3.4, 0, Math.PI * 2); ctx.fill();
    ctx.fillStyle = '#9ff2e3'; ctx.font = '600 9.5px "Cascadia Mono", monospace'; ctx.textAlign = 'right'; ctx.textBaseline = 'bottom';
    ctx.fillText('−10 dB @ 20 kHz', x - 6, y - 4);
  } else if (d.target === 'flat') {
    ctx.fillStyle = 'rgba(46,230,200,0.75)'; ctx.font = '9.5px "Microsoft YaHei", sans-serif'; ctx.textAlign = 'left'; ctx.textBaseline = 'bottom'; ctx.fillText('flat：全带 0 dB', L + 8, my(0) - 5);
  } else {
    ctx.fillStyle = 'rgba(255,201,138,0.85)'; ctx.font = '10px "Microsoft YaHei", sans-serif'; ctx.textAlign = 'center'; ctx.textBaseline = 'middle'; ctx.fillText('未选择目标曲线', (L + R) / 2, (T + B) / 2);
  }
  ctx.fillStyle = 'rgba(160,180,205,0.65)'; ctx.font = '9px "Microsoft YaHei", sans-serif'; ctx.textAlign = 'left'; ctx.textBaseline = 'top';
  ctx.fillText('目标曲线（设计）· tilt10 由 20 Hz 0 dB 线性（对数频率）缓降至 20 kHz −10 dB', L + 4, T + 1);
}

/* ---------------- STEP 8 ---------------- */
function renderStep8() {
  const d = state.design;
  const applied = d.applied, rolled = d.rolledBack;
  const snrVals = CH_ALL.map(ch => PILOT.summary[ch].snr_db);
  const snrMin = Math.min(...snrVals), snrMax = Math.max(...snrVals);
  const rtLow = PILOT.rt60low ? PILOT.rt60low.mean_s : 1.31;
  const rtFull = PILOT.summary.FL.rt60_mean_500_2k;
  const sumGroup = (kind, title, rows) => '<div class="sum-group"><div class="sum-h"><span class="badge3 ' + kind + '">' + (kind === 'real' ? '实测' : kind === 'sim' ? '示意' : '目标') + '</span>' + title + '</div>' +
    rows.map(r => '<div class="kv"><span>' + r[0] + '</span><b>' + r[1] + '</b></div>').join('') + '</div>';
  $('#step-body').innerHTML =
    '<div class="review-grid">' +
      '<div class="review-main"><div class="panel" style="flex:1"><div class="panel-h">低频 Before / After（20–200 Hz） <span class="hint">Before 实测 / After 预测（示意）/ Target 设计目标</span></div>' +
        '<div class="panel-b" style="padding:0"><canvas id="review-canvas" class="rev-canvas"></canvas></div>' +
        '<div class="chart-foot" style="flex:0 0 30px"><span class="hint" style="font-size:10px">坐标范围 20–200 Hz；RT60 数字见右侧分组，按 40–200 Hz（低频）/ 500–2k（全带）分别标注。</span></div></div></div>' +
      '<div class="review-side">' +
        '<div class="panel"><div class="panel-h">本次校准摘要 <span class="hint">按来源分列</span></div><div class="panel-b">' +
          sumGroup('real', 'pilot 实测', [
            ['每通道 SNR（7ch）', fmt(snrMin, 1) + '–' + fmt(snrMax, 1) + ' dB'],
            ['RT60 低频 40–200 / 全带 500–2k', fmt(rtLow, 2) + ' / ' + fmt(rtFull, 2) + ' s'],
            ['实测滚降（主箱 / 环绕）', GROUPS.mains.detected + ' / ' + GROUPS.surround.detected + ' Hz'],
          ]) +
          sumGroup('sim', '演示 / 示意', [
            ['测点布局 / 逐点 SNR / 测量动画', state.points.filter(p => p.on).length + ' 点 + 示意'],
            ['After 曲线 / 衰减改善', '预测（示意）'],
            ['ART 衰减 / MIMO 衰减', countArt() + ' 条 / 主箱 ' + d.mimo.mains + ' dB'],
          ]) +
          sumGroup('model', '设计目标', [
            ['目标曲线 / 低切', (d.target === 'tilt10' ? 'tilt10 · −10 dB@20k' : d.target === 'flat' ? 'flat' : '未选') + ' · ' + d.cut.mains + '/' + d.cut.surround + ' Hz'],
            ['目标 RT60', '0.45 s（目标值，示意）'],
            ['ART / MIMO 衰减语义', '0 = 关闭 → −24 dB = 最强'],
          ]) +
          '<div style="font-size:8.8px;color:var(--muted2);line-height:1.45;margin-top:4px">实测：2026-11-18 pilot（SNR/全带 RT60/FR）+ 2026-11-19 mimo-kit（低频 RT60/模态/CSD）。示意：测点/测量动画/ART/MIMO/After 预测。</div>' +
        '</div></div>' +
        '<div class="apply-box"><h4>应用与回滚</h4>' +
          '<p>应用到设备前会创建回滚快照；演示环境不写设备、不出声。</p>' +
          '<div class="st" id="apply-status">' + (applied ? '已应用（演示）· 快照 #FLOW-' + (state.points.filter(p => p.on).length) : rolled ? '已回滚（演示）· 配置恢复应用前快照' : '未应用 · 等待确认') + '</div>' +
          '<div class="btns"><button class="btn primary" id="btn-apply" data-test="btn-apply" ' + (applied ? 'disabled' : '') + '>' + (applied ? '已应用' : '应用并导出') + '</button>' +
            '<button class="btn ghost" id="btn-rollback" data-test="btn-rollback" ' + (applied ? '' : 'disabled style="opacity:.45"') + '>回滚</button></div>' +
          '<button class="btn ghost" id="btn-export" data-test="btn-export" style="width:100%;margin-top:7px">导出配置（JSON / YAML）</button>' +
          '<div style="font-size:8.8px;color:var(--muted2);line-height:1.45;margin-top:5px">导出含布局、实测/示意/目标分组、设计参数、ART/MIMO 衰减与回滚标记（演示字段）。</div>' +
        '</div>' +
      '</div>' +
    '</div>';
  $('#btn-apply').addEventListener('click', openApplyConfirm);
  $('#btn-export').addEventListener('click', openFlowExport);
  const rb = $('#btn-rollback');
  if (rb && !rb.disabled) rb.addEventListener('click', openRollbackConfirm);
  requestAnimationFrame(drawReviewChart);
  renderHeader();
}
function drawReviewChart() {
  const cv = $('#review-canvas'); if (!cv) return;
  const { ctx, w, h } = setupCanvas(cv);
  const p = designPrediction('mains');
  const L = 54, R = w - 20, T = 22, B = h - 50;
  const yMin = -30, yMax = 16;
  const my = db => T + (yMax - clamp(db, yMin, yMax)) / (yMax - yMin) * (B - T);
  const mx = f => L + fToU(f) * (R - L);
  ctx.fillStyle = '#0a111a'; ctx.fillRect(0, 0, w, h);
  gridFreq(ctx, L, R, T, B, 20, 200, [20, 25, 31.5, 40, 50, 63, 80, 100, 125, 160, 200], null);
  ctx.font = '9.5px "Cascadia Mono", monospace'; ctx.textAlign = 'right'; ctx.textBaseline = 'middle';
  for (let v = -20; v <= 10; v += 10) { const y = my(v); ctx.strokeStyle = 'rgba(120,140,170,0.1)'; ctx.beginPath(); ctx.moveTo(L, y); ctx.lineTo(R, y); ctx.stroke(); ctx.fillStyle = 'rgba(140,158,184,0.5)'; ctx.fillText((v > 0 ? '+' : '') + v, L - 8, y); }
  const mapPts = arr => p.freq.map((f, i) => [f, arr[i]]);
  drawLine(ctx, mapPts(p.meas), mx, my, '#8a98ae', 1.6, null, 0.8);
  if (state.design.target) drawLine(ctx, mapPts(p.target), mx, my, '#2ee6c8', 1.4, [6, 4], 0.85);
  ctx.save(); ctx.shadowColor = 'rgba(243,247,255,0.55)'; ctx.shadowBlur = 10;
  drawLine(ctx, mapPts(p.pred), mx, my, '#f3f7ff', 2.6, null, 1); ctx.restore();
  const cg = mx(state.design.cut.mains);
  ctx.strokeStyle = '#f7ab4a'; ctx.lineWidth = 1.6; ctx.setLineDash([3, 4]); ctx.beginPath(); ctx.moveTo(cg, T); ctx.lineTo(cg, B); ctx.stroke(); ctx.setLineDash([]);
  ctx.fillStyle = '#ffd39a'; ctx.font = '10px "Microsoft YaHei", sans-serif'; ctx.textAlign = 'left'; ctx.textBaseline = 'top'; ctx.fillText('低切 主箱 ' + state.design.cut.mains + ' / 环绕 ' + state.design.cut.surround + ' Hz', cg + 5, T + 3);
  const legend = [['#8a98ae', 'Before · 实测 FR'], ['#f3f7ff', 'After · 预测（示意）'], ['#2ee6c8', 'Target · 设计目标']];
  ctx.font = '10px "Microsoft YaHei", sans-serif'; ctx.textAlign = 'left'; ctx.textBaseline = 'middle';
  let lx0 = L + 8;
  for (const [col, name] of legend) { ctx.fillStyle = col; ctx.fillRect(lx0, B + 30, 14, 3); ctx.fillStyle = 'rgba(190,206,225,0.8)'; ctx.fillText(name, lx0 + 19, B + 31); lx0 += ctx.measureText(name).width + 34; }
}
function openApplyConfirm() {
  const d = state.design;
  const rows = STEPS.map((s, i) => '<tr><td><span class="okmark">✓</span>' + s.title + '</td><td>完成（演示）</td></tr>').join('');
  showModal({
    title: '应用前确认',
    wide: true,
    body: '<div class="confirm-note" style="margin-top:0">将把本次设计应用到设备，并先创建回滚快照（演示，不写设备）。请确认以下摘要：</div>' +
      '<table class="confirm-table">' + rows +
      '<tr><td>目标曲线 / 低切</td><td>' + (d.target || '未选') + ' · 主箱 ' + d.cut.mains + ' / 环绕 ' + d.cut.surround + ' Hz</td></tr>' +
      '<tr><td>ART 衰减 / MIMO 衰减（示意）</td><td>' + countArt() + ' 条 · ' + d.mimo.mains + '/' + d.mimo.surround + '/' + d.mimo.height + ' dB</td></tr>' +
      '<tr><td>导出槽位</td><td>Slot 3 · KHAEN-DiracFlow-Demo</td></tr>' +
      '<tr><td>回滚快照</td><td>will create · #FLOW-SNAP-' + Date.now().toString().slice(-5) + '</td></tr></table>' +
      '<div class="confirm-note">应用后可通过“回滚”恢复应用前状态；演示环境不会修改任何设备或 CueMix。</div>',
    buttons: [
      { label: '取消', cls: 'ghost', act: hideModal },
      { label: '应用并导出', cls: 'primary', test: 'btn-apply-confirm', act: applyFlow },
    ],
  });
}
function applyFlow() {
  hideModal();
  khaenPostJob('remeasure').catch(() => {});
  saveSnapshot();
  state.design.applied = true; state.design.rolledBack = false;
  renderStep8();
  toast('已下发应用/复测（daemon）；是否生效以 job 回执为准');
  renderRail(); renderHeader();
}
function openRollbackConfirm() {
  showModal({
    title: '回滚确认',
    narrow: true,
    body: '<div class="confirm-note" style="margin-top:0">将回滚到应用前快照，恢复 channelmap 与设计参数（演示）。确定继续？</div>' +
      '<table class="confirm-table"><tr><td>当前状态</td><td>已应用</td></tr><tr><td>回滚目标</td><td>应用前快照</td></tr><tr><td>影响</td><td>设计参数恢复，不写设备</td></tr></table>',
    buttons: [
      { label: '取消', cls: 'ghost', act: hideModal },
      { label: '确认回滚', cls: 'primary', test: 'btn-rollback-confirm', act: rollbackFlow },
    ],
  });
}
function rollbackFlow() {
  hideModal();
  if (state.jobId) {
    khaenApi('POST', CAL_API.jobs + '/' + encodeURIComponent(state.jobId) + '/rollback', {}).catch(() => {});
  }
  const snap = state.design.snapshotBeforeApply;
  if (snap && snap.design) {
    const keep = state.design.snapshotBeforeApply;
    state.design = { ...JSON.parse(JSON.stringify(snap.design)), snapshotBeforeApply: keep, applied: false, rolledBack: true };
  } else {
    state.design.applied = false; state.design.rolledBack = true;
  }
  renderStep8();
  toast('已下发回滚/leave；是否恢复以 host 回执为准');
  renderRail(); renderHeader();
}
function buildFlowConfig() {
  const d = state.design;
  const artConns = [];
  for (const r of ART_ROWS) for (const c of ART_COLS) {
    if (r === c) continue;
    const k = getConn(r, c);
    if (k.on) artConns.push({ main: r, support: c, attenuation_db: k.level, f_low_hz: k.low, f_high_hz: k.high });
  }
  return {
    schema: 'khaen-dirac-flow-demo/v2',
    generated_at: new Date().toISOString(),
    demo: true,
    note: '演示导出，不写设备。实测数据与示意/目标数据分列；ART/MIMO/After 为示意模型。',
    device: { audio_interface: 'MOTU UltraLite-mk5', mic: 'UMIK-1 718-7696', mic_orientation_deg: 90, sample_rate: 48000, mic_gain_db: 18 },
    layout: { preset: '4.1.2', roles: state.layout.roles.map(r => ({ id: r.id, output_channel: r.out })), note: 'ch6 保留空；SUB1 = ch7（Line Out 8）' },
    measured: {
      source_fr: 'board-window-20261118-calib · B/UMIK',
      per_channel_snr_db: Object.fromEntries(CH_ALL.map(ch => [ch, PILOT.summary[ch].snr_db])),
      per_channel_polarity: Object.fromEntries(CH_ALL.map(ch => [ch, PILOT.summary[ch].polarity])),
      rt60_low_40_200_s: PILOT.rt60low ? PILOT.rt60low.mean_s : null,
      rt60_full_500_2k_s: Object.fromEntries(CH_ALL.map(ch => [ch, PILOT.summary[ch].rt60_mean_500_2k])),
      rolloff_hz: PILOT.rolloff,
      modes3mic: PILOT.modes3mic ? PILOT.modes3mic.rows : [],
    },
    simulated: {
      measurements_animation: true,
      points_layout: state.points.filter(p => p.on).length,
      art_mimo_model: true,
      after_curve: 'predicted',
    },
    design: {
      target_curve: d.target,
      target_curve_fullband: d.target === 'tilt10' ? { at_20hz_db: 0, at_20khz_db: -10 } : { at_20hz_db: 0, at_20khz_db: 0 },
      low_cut: { mains: { freq_hz: d.cut.mains, slope_db_oct: d.slope.mains }, surround: { freq_hz: d.cut.surround, slope_db_oct: d.slope.surround } },
      sub_lowpass: { follows: 'mains', freq_hz: d.cut.mains },
      art_attenuation: { fsiso_hz: d.fsiso, connections: artConns, note: '0=关闭，−24dB=最强（示意）' },
      mimo_attenuation_db: d.mimo,
      target_rt60_s: 0.45,
    },
    apply: { slot: 3, name: 'KHAEN-DiracFlow-Demo', applied: d.applied, rolled_back: !!d.rolledBack, rollback_snapshot: 'FLOW-SNAP' },
  };
}
function buildFlowYaml() {
  const d = state.design;
  const lines = ['# khaen dirac-style flow 导出片段（演示，不写设备）', 'device:', '  audio_interface: MOTU UltraLite-mk5', '  mic: UMIK-1 718-7696', '  mic_orientation_deg: 90', '  sample_rate: 48000', 'layout:', '  preset: 4.1.2'];
  for (const r of state.layout.roles) lines.push('  - {id: ' + r.id + ', output_channel: ' + r.out + '}');
  lines.push('design:', '  target_curve: ' + d.target, '  target_curve_fullband: ' + (d.target === 'tilt10' ? '{at_20hz_db: 0, at_20khz_db: -10}' : '{at_20hz_db: 0, at_20khz_db: 0}'), '  low_cut:', '    mains: {freq_hz: ' + d.cut.mains + ', slope_db_oct: ' + d.slope.mains + '}', '    surround: {freq_hz: ' + d.cut.surround + ', slope_db_oct: ' + d.slope.surround + '}', '    sub_lowpass: {follows: mains, freq_hz: ' + d.cut.mains + '}', '  art_attenuation:', '    fsiso_hz: ' + d.fsiso);
  for (const r of ART_ROWS) for (const c of ART_COLS) { if (r === c) continue; const k = getConn(r, c); if (k.on) lines.push('    - {main: ' + r + ', support: ' + c + ', attenuation_db: ' + k.level + ', f_low: ' + k.low + ', f_high: ' + k.high + '}'); }
  lines.push('  mimo_attenuation_db: {mains: ' + d.mimo.mains + ', surround: ' + d.mimo.surround + ', height: ' + d.mimo.height + ', sub: ' + d.mimo.sub + '}', 'apply: {slot: 3, name: KHAEN-DiracFlow-Demo, applied: ' + d.applied + ', rolled_back: ' + !!d.rolledBack + '}');
  return lines.join('\n') + '\n';
}
function openFlowExport() {
  const json = JSON.stringify(buildFlowConfig(), null, 2);
  const yaml = buildFlowYaml();
  showModal({
    title: '导出配置',
    wide: true,
    body: '<div class="tabs-mini" id="exp-tabs"><button class="active" data-view="json" data-test="exp-json">flow-config.json</button><button data-view="yaml" data-test="exp-yaml">channelmap 片段（YAML）</button></div><pre class="code" id="exp-code"></pre><div class="code-note">演示导出，不写入设备；实测/示意/目标分列，ART/MIMO 为示意参数。</div>',
    buttons: [
      { label: '关闭', cls: 'ghost', act: hideModal },
      { label: '下载 JSON', cls: 'primary', test: 'btn-download', act: () => {
          const blob = new Blob([json], { type: 'application/json' });
          const a = document.createElement('a'); a.href = URL.createObjectURL(blob); a.download = 'flow-config.json'; document.body.appendChild(a); a.click(); a.remove();
          setTimeout(() => URL.revokeObjectURL(a.href), 3000);
          toast('已下载 flow-config.json');
        } },
    ],
  });
  const code = $('#exp-code'); code.textContent = json;
  $$('#exp-tabs button').forEach(b => b.addEventListener('click', () => {
    $$('#exp-tabs button').forEach(x => x.classList.toggle('active', x === b));
    code.textContent = b.dataset.view === 'json' ? json : yaml;
  }));
}

/* ---------------- render registry / init ---------------- */
RENDER[STEP.device] = renderStep1;
RENDER[STEP.layout] = renderStep2;
RENDER[STEP.mic] = renderStep3;
RENDER[STEP.level] = renderStepLevel;
RENDER[STEP.delay] = renderStepDelay;
RENDER[STEP.bass] = renderStepBass;
RENDER[STEP.fir] = renderStepFir;
RENDER[STEP.modal] = renderStepModal;
RENDER[STEP.points] = renderStep4;
RENDER[STEP.measure] = renderStep5;
RENDER[STEP.analysis] = renderStep6;
RENDER[STEP.design] = renderStep7;
RENDER[STEP.review] = renderStep8;

let resizeTimer = null;
window.addEventListener('resize', () => { clearTimeout(resizeTimer); resizeTimer = setTimeout(() => { if (!state.measure.running) renderStep(); }, 250); });
document.addEventListener('keydown', e => { if (e.key === 'Escape') hideModal(); });

window.__flow = {
  ready: true,
  state: () => JSON.parse(JSON.stringify(state)),
  step: () => state.step,
  goStep, nextStep, backStep,
  caption, toast, hideModal,
  cursorVisible(v) { $('#cursor').style.display = v ? '' : 'none'; },
  elRect(sel) { const el = document.querySelector(sel); if (!el) return null; const r = el.getBoundingClientRect(); return { x: r.left, y: r.top, w: r.width, h: r.height, cx: r.left + r.width / 2, cy: r.top + r.height / 2 }; },
  rangePoint(id, value, min, max) {
    const el = document.getElementById(id); if (!el) return null;
    const r = el.getBoundingClientRect(); const t = (value - min) / (max - min);
    return { x: r.left + 7 + t * (r.width - 14), y: r.top + r.height / 2 };
  },
  designTouched() { state.design.touched = true; },
  addSeen(id) { if (!state.analysis.seen.includes(id)) state.analysis.seen.push(id); },
};

/* ---------------- boot ---------------- */
renderStep();
if (typeof khaenBoot === 'function') khaenBoot();