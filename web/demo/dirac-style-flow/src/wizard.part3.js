
/* =========================================================
 * Step 5–6 renderers
 * ======================================================= */
const activePoints = () => state.points.filter(p => p.on);
function renderStep5() {
  const pts = activePoints();
  const n = pts.length;
  const done = state.measure.done.filter(id => pts.some(p => p.id === id));
  const low = state.measure.low.filter(id => pts.some(p => p.id === id));
  if (state.measure.done.length !== done.length || state.measure.low.length !== low.length) {
    state.measure.done = done; state.measure.low = low;
  }
  const pct = n ? Math.round(done.length / n * 100) : 0;
  const lowFirst = low[0];
  $('#step-body').innerHTML =
    '<div class="two-col">' +
      '<div class="col-main"><div class="panel" style="flex:1"><div class="panel-h">逐点测量 <span class="hint">扫频 20 Hz – 20 kHz · 每点约 0.6 s（演示动画）</span></div><div class="panel-b" style="display:flex;flex-direction:column;gap:14px">' +
        '<div class="prog-wrap">' +
          '<div class="prog-top"><div class="prog-big" id="m-prog">' + done.length + ' <small>/ ' + n + ' 点</small></div><div class="chip" id="m-live">待开始</div></div>' +
          '<div class="prog-status" id="m-status">' + (done.length && done.length === n && !low.length ? '全部测点完成 · SNR 均达标' : low.length ? '存在低 SNR 测点，请重测后继续' : done.length ? '测量进行中 …' : '点击“开始测量”按顺序完成所有测点') + '</div>' +
          '<div class="bar"><div class="fill" id="m-bar" style="width:' + pct + '%"></div></div>' +
          '<div style="display:flex;justify-content:space-between;font-size:10px;color:var(--muted2);font-family:var(--mono)"><span>SNR 阈值 ≥ 15 dB</span><span id="m-pct">' + pct + '%</span></div>' +
        '</div>' +
        '<div style="display:flex;gap:9px;align-items:center">' +
          '<button class="btn primary" id="btn-measure" data-test="btn-measure">开始测量</button>' +
          '<button class="btn ghost" id="btn-remeasure" data-test="btn-remeasure" ' + (lowFirst && !state.measure.running ? '' : 'disabled style="display:none"') + '>重测 ' + (lowFirst || '') + '</button>' +
          '<span class="hint" id="m-hint">测量序列与 SNR 为演示；pilot 每通道实测 SNR 11.9–29.0 dB。</span>' +
        '</div>' +
        '<div class="param-grid"><div class="kv"><span>测量信号</span><b>Sweep 20 Hz–20 kHz</b></div><div class="kv"><span>采样率</span><b>48 kHz / 24 bit</b></div><div class="kv"><span>每点用时</span><b>约 0.6 s（演示）</b></div><div class="kv"><span>平均次数</span><b>3 次</b></div><div class="kv"><span>SNR 阈值</span><b>≥ 15 dB</b></div><div class="kv"><span>测量点</span><b>' + n + ' 点（含甜点）</b></div></div>' +
        '<div id="m-warn"></div>' +
        '<div style="margin-top:auto;display:flex;gap:10px;font-size:10px;color:var(--muted2)">' +
          '<span class="badge3 real">pilot 实测统计</span><span>9 点 / 1 甜点 + 8 环绕</span><span>平均 SNR ≈ 28 dB（示意）</span></div>' +
      '</div></div></div>' +
      '<div class="col-side"><div class="panel" style="flex:1"><div class="panel-h">测点状态 <span class="hint">' + n + ' 点</span></div><div class="panel-b scroll"><div class="meas-list" id="meas-list">' +
        pts.map((p, i) => {
          const isDone = done.includes(p.id), isLow = low.includes(p.id);
          const snr = state.measure.snr && state.measure.snr[p.id];
          return '<div class="meas-row ' + (isLow ? 'low' : isDone ? 'ok' : '') + (state.measure.current === p.id ? ' active' : '') + '" id="mr-' + p.id + '"><span class="pt">' + p.id + '</span><span class="st">' + (isLow ? '⚠ 低SNR' : isDone ? '✓ 完成' : state.measure.current === p.id ? '测量中' : '等待') + '</span><span style="color:var(--muted);font-size:10px">' + (p.sweet ? '听音位' : '多点 ' + i) + '</span><span class="snr">' + (snr ? snr.toFixed(1) + ' dB' : '—') + '</span></div>';
        }).join('') +
      '</div></div></div></div>' +
    '</div>';
  $('#btn-measure').addEventListener('click', () => runMeasure(false));
  const rb = $('#btn-remeasure');
  if (rb) rb.addEventListener('click', () => runMeasure(true));
  refreshMeasureWarn();
}
function refreshMeasureWarn() {
  const w = $('#m-warn'); if (!w) return;
  const pts = activePoints(); const n = pts.length; const done = state.measure.done.length, low = state.measure.low;
  const pct = n ? Math.round(done / n * 100) : 0;
  const bar = $('#m-bar'); if (bar) bar.style.width = pct + '%';
  const pctEl = $('#m-pct'); if (pctEl) pctEl.textContent = pct + '%';
  const prog = $('#m-prog'); if (prog) prog.innerHTML = done + ' <small>/ ' + n + ' 点</small>';
  const live = $('#m-live');
  if (live) {
    live.textContent = state.measure.running ? '测量中…' : low.length ? low.length + ' 点待重测' : done === n && n > 0 ? '测量完成' : '待开始';
    live.className = 'chip ' + (state.measure.running ? 'model' : low.length ? 'sim' : done === n && n > 0 ? 'real' : '');
  }
  if (low.length && !state.measure.running) {
    w.innerHTML = '<div class="warn-card"><div class="warn-ico">!</div><div class="warn-body"><b>SNR 偏低：' + low.join(' / ') + '</b><p>' + low.map(id => { const s = state.measure.snr && state.measure.snr[id]; return id + ' = ' + (s ? s.toFixed(1) : '—') + ' dB'; }).join(' · ') + '（阈值 15 dB）。建议点击“重测该点”，或检查环境噪声/麦克风增益后重测。</p></div></div>';
  } else if (done === n && n > 0) {
    w.innerHTML = '<div class="role-ok"><b>✓</b><span style="color:var(--teal)">' + n + ' 个测点全部完成，SNR 均 ≥ 15 dB，可以进入分析</span></div>';
  } else w.innerHTML = '';
  renderHeader();
}
function updateMeasureLive() {
  const pts = activePoints(); const n = pts.length;
  const done = state.measure.done.filter(id => pts.some(p => p.id === id));
  const low = state.measure.low;
  const pct = n ? Math.round(done.length / n * 100) : 0;
  const bar = $('#m-bar'); if (bar) bar.style.width = pct + '%';
  const pctEl = $('#m-pct'); if (pctEl) pctEl.textContent = pct + '%';
  const prog = $('#m-prog'); if (prog) prog.innerHTML = done.length + ' <small>/ ' + n + ' 点</small>';
  const live = $('#m-live'); if (live) live.textContent = state.measure.running ? '测量中…' : '待开始';
  const st = $('#m-status');
  if (st && state.measure.current) st.textContent = '正在测量 ' + state.measure.current + ' · 扫频 20 Hz – 20 kHz …';
  for (const p of pts) {
    const row = $('#mr-' + p.id); if (!row) continue;
    const isDone = state.measure.done.includes(p.id);
    const isLow = state.measure.low.includes(p.id);
    const cls = 'meas-row ' + (isLow ? 'low' : isDone ? 'ok' : '') + (state.measure.current === p.id ? ' active' : '');
    if (row.className !== cls) row.className = cls;
    const stel = row.querySelector('.st');
    if (stel) stel.textContent = isLow ? '⚠ 低SNR' : isDone ? '✓ 完成' : state.measure.current === p.id ? '测量中' : '等待';
    const snrEl = row.querySelector('.snr');
    const snr = state.measure.snr && state.measure.snr[p.id];
    if (snrEl) snrEl.textContent = snr ? snr.toFixed(1) + ' dB' : '—';
  }
}
async function runMeasure(isRemeasure) {
  if (state.measure.running) return;
  if (KHAEN_LIVE && !isRemeasure) {
    state.measure.running = true;
    try {
      const response = await khaenPostJob('measure');
      state.measure.liveAck = !!response && response.state !== 'failed';
      state.measure.progressText = response && response.progress_text ? response.progress_text : '';
      const sweet = state.points.find(p => p.sweet && p.on);
      if (sweet && state.measure.liveAck && !state.measure.done.includes(sweet.id)) state.measure.done.push(sweet.id);
    } catch (error) {
      state.measure.liveAck = false;
    }
    state.measure.running = false;
    renderStep5();
    return;
  }
  const pts = activePoints();
  if (!pts.length) return;
  state.measure.running = true;
  const btn = $('#btn-measure'), rb = $('#btn-remeasure');
  if (btn) btn.disabled = true; if (rb) rb.disabled = true;
  if (!isRemeasure) {
    state.measure.done = []; state.measure.low = []; state.measure.snr = {};
    renderStep5();
    await sleep(120);
  }
  const targets = isRemeasure ? pts.filter(p => state.measure.low.includes(p.id)) : pts.filter(p => !state.measure.done.includes(p.id));
  for (const p of targets) {
    state.measure.current = p.id;
    updateMeasureLive();
    await sleep(isRemeasure ? 1100 : 620);
    // deterministic demo SNR; P5 slightly low on first pass
    let snr;
    if (!isRemeasure && p.id === 'P5') snr = 13.4;
    else snr = 26 + ((p.id.charCodeAt(1) * 7) % 9) + 1.5;
    state.measure.snr = state.measure.snr || {};
    state.measure.snr[p.id] = snr;
    if (!state.measure.done.includes(p.id)) state.measure.done.push(p.id);
    state.measure.low = state.measure.low.filter(id => id !== p.id);
    if (snr < 15) state.measure.low.push(p.id);
    updateMeasureLive();
  }
  state.measure.current = null;
  state.measure.running = false;
  if (!state.measure.low.length) {
    const status = $('#m-status'); if (status) status.textContent = '全部测点完成 · SNR 均达标';
  }
  renderStep5();
}
function finishMeasurementUi() { renderStep5(); }

/* ---------------- STEP 6 analysis ---------------- */
const AN_TABS = [
  { id: 'fr', label: 'FR 频响' }, { id: 'ir', label: 'IR 脉冲' },
  { id: 'waterfall', label: '瀑布 CSD' }, { id: 'rt60', label: 'RT60（低频）' }, { id: 'modal', label: '模态' },
];
function anRangeText(tab) {
  if (tab === 'fr') return '20 Hz–20 kHz（低频区 20–200 高亮）';
  if (tab === 'ir') return '0–60 ms（重建示意）';
  if (tab === 'waterfall') return '20–200 Hz × 0–600 ms（实测 CSD 有效 40–190 Hz / 0–400 ms）';
  if (tab === 'rt60') return '40–200 Hz（实测逐带 T20）';
  if (tab === 'modal') return '20–200 Hz（实测 5 模态）';
  return '';
}
function renderStep6() {
  const a = state.analysis;
  if (!a.seen.includes(a.tab)) a.seen.push(a.tab);
  const editableCh = a.tab === 'fr' || a.tab === 'ir';
  $('#step-body').innerHTML =
    '<div class="two-col">' +
      '<div class="col-main"><div class="panel" style="flex:1"><div class="panel-h" style="padding:7px 10px"><div class="tabbar" id="an-tabs" style="margin:0;flex:1">' +
        AN_TABS.map(t => '<button data-tab="' + t.id + '" data-test="an-tab-' + t.id + '" class="' + (a.tab === t.id ? 'active' : '') + (a.seen.includes(t.id) ? ' seen' : '') + '">' + t.label + '<span class="seen"></span></button>').join('') +
      '</div></div><div class="panel-b" style="padding:0"><canvas id="an-canvas" class="room-canvas"></canvas></div></div></div>' +
      '<div class="col-side">' +
        '<div class="panel"><div class="panel-h">分析信息 <span class="hint" id="an-badge"></span></div><div class="panel-b" id="an-side"></div></div>' +
        '<div class="panel" style="flex:0 0 auto"><div class="panel-h">范围与数据来源</div><div class="panel-b"><div id="an-range" style="font-family:var(--mono);font-size:10px;color:#9fd8c8;background:rgba(46,230,200,.06);border:1px solid rgba(46,230,200,.25);border-radius:6px;padding:6px 8px;margin-bottom:7px"></div>' +
          '<div style="font-size:10px;color:var(--muted);line-height:1.6">FR / RT60（低频）/ 瀑布 CSD / 模态：实测数据（来源见每个视图说明）；IR：由实测模态+RT60 重建（示意）；Q/PEQ 为估计值。</div>' +
          '<div style="display:flex;gap:5px;margin-top:7px;flex-wrap:wrap"><span class="badge3 real">实测</span><span class="badge3 model">重建示意</span><span class="badge3 sim">估计</span></div></div></div>' +
      '</div>' +
    '</div>';
  $('#an-tabs').addEventListener('click', e => {
    const b = e.target.closest('[data-tab]'); if (!b) return;
    a.tab = b.dataset.tab;
    if (!a.seen.includes(a.tab)) a.seen.push(a.tab);
    renderStep6();
  });
  const rg = $('#an-range'); if (rg) rg.textContent = '当前范围：' + anRangeText(a.tab);
  drawAnalysis();
  renderHeader();
}
function anChannelSelect(show, note) {
  if (!show) return '<div class="lc-row" style="margin:0 0 8px"><span class="lbl">数据</span><b style="font-family:var(--mono);font-size:11px;color:#cfe0f4">' + (note || 'FL（固定）') + '</b></div>';
  return '<div class="lc-row" style="margin:0 0 8px"><span class="lbl">通道</span><select id="an-ch" style="background:#0b1320;border:1px solid #2a3a52;color:#cfe0f4;border-radius:6px;padding:4px 7px;font-family:var(--mono);font-size:11px">' +
    CH_ALL.map(ch => '<option ' + (state.analysis.channel === ch ? 'selected' : '') + '>' + ch + '</option>').join('') + '</select></div>';
}
function drawAnalysis() {
  const cv = $('#an-canvas'); if (!cv) return;
  const a = state.analysis;
  const { ctx, w, h } = setupCanvas(cv);
  const side = $('#an-side');
  const ch = a.channel;
  const L = 54, R = w - 20, T = 18, B = h - 36;
  ctx.fillStyle = '#0a111a'; ctx.fillRect(0, 0, w, h);
  const badge = $('#an-badge');
  if (a.tab === 'fr') {
    badge.innerHTML = '<span class="badge3 real">实测</span>';
    const yMin = -26, yMax = 14;
    const my = db => T + (yMax - clamp(db, yMin, yMax)) / (yMax - yMin) * (B - T);
    const mx = f => L + fToU(f) * (R - L);
    gridFreq(ctx, L, R, T, B, 20, 20000, [20, 30, 50, 80, 100, 200, 500, 1000, 2000, 5000, 10000, 20000], { 1000: '1k', 2000: '2k', 5000: '5k', 10000: '10k', 20000: '20k' });
    ctx.font = '9.5px "Cascadia Mono", monospace'; ctx.textAlign = 'right'; ctx.textBaseline = 'middle';
    for (let y = -20; y <= 10; y += 10) {
      const yy = my(y); ctx.strokeStyle = 'rgba(120,140,170,0.1)'; ctx.beginPath(); ctx.moveTo(L, yy); ctx.lineTo(R, yy); ctx.stroke();
      ctx.fillStyle = 'rgba(140,158,184,0.5)'; ctx.fillText((y > 0 ? '+' : '') + y, L - 8, yy);
    }
    const x1 = mx(20), x2 = mx(200);
    ctx.fillStyle = 'rgba(247,171,74,0.06)'; ctx.fillRect(x1, T, x2 - x1, B - T);
    ctx.strokeStyle = 'rgba(247,171,74,0.25)'; ctx.setLineDash([3, 4]); ctx.beginPath(); ctx.moveTo(x2, T); ctx.lineTo(x2, B); ctx.stroke(); ctx.setLineDash([]);
    ctx.fillStyle = 'rgba(247,171,74,0.7)'; ctx.textAlign = 'center'; ctx.textBaseline = 'top'; ctx.font = '9px "Microsoft YaHei", sans-serif';
    ctx.fillText('低频分析区 20–200 Hz', (x1 + x2) / 2, T + 4);
    const pts = FR[ch].freq.map((f, i) => [f, FR[ch].db[i]]);
    drawLine(ctx, pts, mx, my, 'rgba(67,177,255,0.35)', 4, null, 0.4);
    drawLine(ctx, pts, mx, my, '#a8b8cf', 1.8, null, 0.95);
    side.innerHTML = anChannelSelect(true) +
      '<div class="kv"><span>通道</span><b>' + ch + ' · ' + CH_LABEL[ch] + '</b></div>' +
      '<div class="kv"><span>范围</span><b>20 Hz–20 kHz（全带）</b></div>' +
      '<div class="kv"><span>有效下潜（实测滚降）</span><b>' + (PILOT.rolloff.per_channel[ch] != null ? fmt(PILOT.rolloff.per_channel[ch]) + ' Hz' : '低架不可判') + '</b></div>' +
      '<div class="kv"><span>200–2000 Hz 平均</span><b>0.0 dB（归一）</b></div>' +
      '<div style="font-size:10px;color:var(--muted2);line-height:1.6;margin-top:7px">实测 FR（1/12 oct 平滑，归一化到 200–2000 Hz 均值）；低频区用于低切/ART/MIMO 设计。</div>';
  } else if (a.tab === 'ir') {
    badge.innerHTML = '<span class="badge3 model">重建示意</span>';
    const modes = PILOT.modes3mic.rows;
    const ir = synthIR(ch, modes);
    const N = ir.length;
    ctx.strokeStyle = 'rgba(120,140,170,0.12)'; ctx.beginPath(); ctx.moveTo(L, (T + B) / 2); ctx.lineTo(R, (T + B) / 2); ctx.stroke();
    for (let i = 1; i < N; i++) {
      const x = L + i / (N - 1) * (R - L);
      const y = (T + B) / 2 - ir[i] * (B - T) * 0.45;
      ctx.strokeStyle = 'rgba(46,230,200,0.8)'; ctx.lineWidth = 1;
      ctx.beginPath(); ctx.moveTo(L + (i - 1) / (N - 1) * (R - L), (T + B) / 2 - ir[i - 1] * (B - T) * 0.45); ctx.lineTo(x, y); ctx.stroke();
    }
    ctx.fillStyle = 'rgba(160,180,205,0.55)'; ctx.font = '9px "Cascadia Mono", monospace'; ctx.textAlign = 'left'; ctx.textBaseline = 'top';
    for (let ms = 0; ms <= 60; ms += 10) ctx.fillText(ms + ' ms', L + ms / 60 * (R - L), B + 7);
    side.innerHTML = anChannelSelect(true) +
      '<div class="kv"><span>合成依据</span><b>实测模态 + 实测 RT60</b></div>' +
      '<div class="kv"><span>模态数</span><b>' + modes.length + ' 个（20–200 Hz）</b></div>' +
      '<div class="kv"><span>RT60（500 Hz，全带）</span><b>' + fmt(PILOT.rt60[ch].median[3] || 0, 2) + ' s</b></div>' +
      '<div style="font-size:10px;color:var(--muted2);line-height:1.6;margin-top:7px">IR 为演示重建（示意）：用实测模态频率与 RT60 包络合成，仅示意时域形态；不是实测 IR。</div>';
  } else if (a.tab === 'waterfall') {
    badge.innerHTML = '<span class="badge3 real">实测 CSD</span><span class="badge3 model">有效窗 40–190Hz/0–400ms</span>';
    drawCSD3D(ctx, w, h);
    side.innerHTML = anChannelSelect(false, 'UMIK · FL（CSD 数据）') +
      '<div class="kv"><span>类型</span><b>3D CSD 瀑布（频率 × 时间 × dB）</b></div>' +
      '<div class="kv"><span>频率范围</span><b>20–200 Hz（数据 40–190）</b></div>' +
      '<div class="kv"><span>时间范围</span><b>0–600 ms（数据 0–400）</b></div>' +
      '<div class="kv"><span>动态范围</span><b>−45 … 0 dB</b></div>' +
      '<div class="kv"><span>数据</span><b>mimo-kit-fix 2026-11-19 · UMIK FL</b></div>' +
      '<div style="font-size:10px;color:var(--muted2);line-height:1.6;margin-top:7px">实测 CSD：由反卷积 IR 做 1/12-oct 滤波/短时包络；0 dB = 公共峰值。CSD 有效窗 0–400 ms（IR 有效窗 0.55 s），坐标轴按 20–200 Hz / 0–600 ms 显示。</div>';
  } else if (a.tab === 'rt60') {
    badge.innerHTML = '<span class="badge3 real">实测 T20</span><span class="badge3 sim">目标 0.45s 示意</span>';
    drawRT60Low(ctx, w, h, L, R, T, B);
    side.innerHTML = anChannelSelect(false, 'UMIK · FL（低频 RT60）') +
      '<div class="kv"><span>范围</span><b>40–200 Hz（8 个 1/3-oct 带）</b></div>' +
      '<div class="kv"><span>方法</span><b>T20 · 噪声校正 EDC · 0.55s 窗</b></div>' +
      '<div class="kv"><span>40–200 Hz 平均</span><b>' + fmt(PILOT.rt60low.mean_s, 2) + ' s（实测）</b></div>' +
      '<div class="kv"><span>目标</span><b>0.45 s（设计目标，示意）</b></div>' +
      '<div style="font-size:10px;color:var(--muted2);line-height:1.6;margin-top:7px">实测 rt60_before_after.csv（mimo-kit-fix 2026-11-19）；500 Hz 以上全带 RT60 不在本图范围，见 Step8 标注。</div>';
  } else {
    badge.innerHTML = '<span class="badge3 real">实测模态</span><span class="badge3 sim">PEQ 估计</span>';
    drawModalChart(ctx, w, h, L, R, T, B);
    side.innerHTML = '<div style="font-size:10.5px;color:var(--muted);margin-bottom:6px">实测低频模态（3mic pilot，2026-11-19）</div>' +
      PILOT.modes3mic.rows.slice().sort((x, y) => x.hz - y.hz).map(m => '<div class="mode-row" style="align-items:flex-start"><span class="f">' + fmt(m.hz, 1) + ' Hz</span><span class="g">+' + fmt(m.prominence_db, 1) + ' dB</span><span class="q" style="width:auto">T60 ' + fmt(m.t60_s, 2) + 's</span><span class="q" style="width:auto">BW ' + fmt(m.bw_hz, 1) + 'Hz</span></div>' +
        '<div style="font-size:9px;color:#ffc98a;font-family:var(--mono);margin:-2px 0 6px 2px">建议 PEQ：Peaking ' + fmt(m.hz, 1) + ' Hz · Q ' + fmt(m.peq.q, 1) + ' · ' + fmt(m.peq.gain_db, 1) + ' dB（估计）</div>').join('') +
      '<div style="font-size:10px;color:var(--muted2);line-height:1.6;margin-top:6px">模态频率/突出度/T60 来自实测 3mic 分析（mimo-objective-v2/predicted/modes-3mic.csv）；PEQ 为按突出度与半功率带宽估算的建议，未上板验证。</div>';
  }
  const sel = $('#an-ch');
  if (sel) sel.addEventListener('change', () => { state.analysis.channel = sel.value; drawAnalysis(); });
  renderHeader();
}
function drawRT60Low(ctx, w, h, L, R, T, B) {
  const bands = PILOT.rt60low.bands_hz, vals = PILOT.rt60low.rt60_s;
  const yMax = 2.0;
  const my = v => T + (1 - clamp(v, 0, yMax) / yMax) * (B - T);
  const slot = (R - L) / bands.length, bw = Math.min(56, slot * 0.52);
  ctx.font = '9.5px "Cascadia Mono", monospace';
  for (let v = 0; v <= 2.0; v += 0.5) {
    const y = my(v); ctx.strokeStyle = 'rgba(120,140,170,0.1)'; ctx.beginPath(); ctx.moveTo(L, y); ctx.lineTo(R, y); ctx.stroke();
    ctx.fillStyle = 'rgba(140,158,184,0.5)'; ctx.textAlign = 'right'; ctx.textBaseline = 'middle'; ctx.fillText(v.toFixed(1), L - 8, y);
  }
  ctx.save(); ctx.strokeStyle = 'rgba(46,230,200,0.5)'; ctx.setLineDash([5, 5]); ctx.beginPath(); ctx.moveTo(L, my(0.45)); ctx.lineTo(R, my(0.45)); ctx.stroke(); ctx.restore();
  ctx.fillStyle = 'rgba(120,240,218,0.8)'; ctx.textAlign = 'right'; ctx.textBaseline = 'bottom'; ctx.font = '9px "Microsoft YaHei", sans-serif';
  ctx.fillText('目标 RT60 0.45 s（设计目标，示意）', R - 6, my(0.45) - 3);
  bands.forEach((b, i) => {
    const cx = L + slot * (i + 0.5), y = my(vals[i]);
    const grd = ctx.createLinearGradient(0, y, 0, B); grd.addColorStop(0, 'rgba(67,177,255,0.92)'); grd.addColorStop(1, 'rgba(30,80,140,0.4)');
    ctx.fillStyle = grd; roundRect(ctx, cx - bw / 2, y, bw, B - y, 4); ctx.fill();
    ctx.fillStyle = '#bfe0ff'; ctx.font = '600 10px "Cascadia Mono", monospace'; ctx.textAlign = 'center'; ctx.textBaseline = 'bottom';
    ctx.fillText(fmt(vals[i], 2), cx, y - 3);
    ctx.fillStyle = 'rgba(160,180,205,0.65)'; ctx.font = '9.5px "Cascadia Mono", monospace'; ctx.textBaseline = 'top';
    ctx.fillText(String(b), cx, B + 7);
  });
  ctx.fillStyle = 'rgba(140,158,184,0.5)'; ctx.font = '9.5px "Cascadia Mono", monospace'; ctx.textAlign = 'right'; ctx.fillText('Hz', R, B + 24);
  ctx.textAlign = 'left'; ctx.fillText('s', L - 28, T - 12);
}
function drawModalChart(ctx, w, h, L, R, T, B) {
  const rows = PILOT.modes3mic.rows.slice().sort((a, b) => a.hz - b.hz);
  const yMax = 9;
  const my = v => T + (1 - v / yMax) * (B - T);
  const mx = f => L + (Math.log(clamp(f, 20, 200)) - Math.log(20)) / (Math.log(200) - Math.log(20)) * (R - L);
  ctx.font = '9.5px "Cascadia Mono", monospace';
  for (let v = 0; v <= 8; v += 2) {
    const y = my(v); ctx.strokeStyle = 'rgba(120,140,170,0.1)'; ctx.beginPath(); ctx.moveTo(L, y); ctx.lineTo(R, y); ctx.stroke();
    ctx.fillStyle = 'rgba(140,158,184,0.5)'; ctx.textAlign = 'right'; ctx.textBaseline = 'middle'; ctx.fillText(v + ' dB', L - 8, y);
  }
  for (const f of [20, 30, 40, 50, 63, 80, 100, 125, 160, 200]) {
    const x = mx(f); ctx.strokeStyle = 'rgba(120,140,170,0.08)'; ctx.beginPath(); ctx.moveTo(x, T); ctx.lineTo(x, B); ctx.stroke();
    ctx.fillStyle = 'rgba(140,158,184,0.55)'; ctx.font = '9px "Cascadia Mono", monospace'; ctx.textAlign = 'center'; ctx.textBaseline = 'top'; ctx.fillText(String(f), x, B + 7);
  }
  rows.forEach(m => {
    const x = mx(m.hz), y1 = my(m.prominence_db);
    const grd = ctx.createLinearGradient(0, y1, 0, B); grd.addColorStop(0, 'rgba(247,171,74,0.95)'); grd.addColorStop(1, 'rgba(120,70,20,0.35)');
    ctx.fillStyle = grd; ctx.fillRect(x - 10, y1, 20, B - y1);
    ctx.fillStyle = '#ffd39a'; ctx.font = '600 9.5px "Cascadia Mono", monospace'; ctx.textAlign = 'center'; ctx.textBaseline = 'bottom';
    ctx.fillText(fmt(m.hz, 0), x, y1 - 20);
    ctx.fillText('+' + fmt(m.prominence_db, 1), x, y1 - 9);
  });
  ctx.fillStyle = 'rgba(140,158,184,0.5)'; ctx.font = '9.5px "Cascadia Mono", monospace'; ctx.textAlign = 'right'; ctx.fillText('Hz', R, B + 24);
  ctx.textAlign = 'left'; ctx.fillText('突出度', L + 4, T + 2);
}
function drawCSD3D(ctx, w, h) {
  const T = 18, L = 44;
  const d = PILOT.csd3d;
  if (!d) return;
  const freqs = d.freqs_hz, times = d.times_ms, db = d.db;
  // layout
  const x0 = 92, y0 = h - 62;             // front-left corner
  const Wx = w * 0.52;                     // freq axis length
  const depthX = w * 0.30, depthY = -h * 0.30; // time axis (up-right)
  const zH = h * 0.42;                     // amplitude height
  const fU = f => (Math.log(clamp(f, 20, 200)) - Math.log(20)) / (Math.log(200) - Math.log(20));
  const tU = t => clamp(t, 0, 600) / 600;
  const zN = v => clamp((v + 45) / 45, 0, 1);
  const color = v => { const t = zN(v); return 'hsl(' + (212 - 196 * t) + ',' + (82 + 12 * t) + '%,' + (16 + 48 * t) + '%)'; };
  const P = (f, t, v) => {
    const u = fU(f), tt = tU(t), z = zN(v);
    return { x: x0 + u * Wx + tt * depthX, y: y0 + tt * depthY - z * zH };
  };
  // floor grid (time × freq)
  ctx.strokeStyle = 'rgba(120,140,170,0.13)'; ctx.lineWidth = 1;
  const fTicks = [20, 40, 80, 120, 200];
  for (const t of [0, 200, 400, 600]) {
    const a = P(20, t, -45), b = P(200, t, -45);
    ctx.beginPath(); ctx.moveTo(a.x, a.y); ctx.lineTo(b.x, b.y); ctx.stroke();
  }
  for (const f of fTicks) {
    const a = P(f, 0, -45), b = P(f, 600, -45);
    ctx.beginPath(); ctx.moveTo(a.x, a.y); ctx.lineTo(b.x, b.y); ctx.stroke();
  }
  // amplitude axis at left
  const zx = P(20, 0, -45);
  ctx.strokeStyle = 'rgba(160,180,205,0.5)';
  ctx.beginPath(); ctx.moveTo(zx.x, zx.y); ctx.lineTo(zx.x, zx.y - zH); ctx.stroke();
  ctx.font = '9px "Cascadia Mono", monospace'; ctx.fillStyle = 'rgba(160,180,205,0.65)'; ctx.textAlign = 'right'; ctx.textBaseline = 'middle';
  for (const v of [0, -15, -30, -45]) { const p = P(20, 0, v); ctx.fillText(v, zx.x - 6, p.y); }
  // curves: one per time slice (draw back→front)
  const tStep = 4; // every 20 ms
  for (let ti = times.length - 1; ti >= 0; ti -= tStep) {
    const t = times[ti];
    const slice = db[ti];
    // fill under curve
    ctx.beginPath();
    let first = null, last = null;
    freqs.forEach((f, fi) => {
      const p = P(f, t, clamp(slice[fi], -45, 0));
      if (!first) { first = p; ctx.moveTo(p.x, p.y); } else ctx.lineTo(p.x, p.y);
      last = p;
    });
    const baseL = P(freqs[0], t, -45), baseR = P(freqs[freqs.length - 1], t, -45);
    ctx.lineTo(baseR.x, baseR.y); ctx.lineTo(baseL.x, baseL.y); ctx.closePath();
    ctx.fillStyle = 'rgba(30,90,140,0.10)'; ctx.fill();
    // colored line segments
    for (let fi = 1; fi < freqs.length; fi++) {
      const p1 = P(freqs[fi - 1], t, clamp(slice[fi - 1], -45, 0));
      const p2 = P(freqs[fi], t, clamp(slice[fi], -45, 0));
      ctx.strokeStyle = color((slice[fi] + slice[fi - 1]) / 2); ctx.lineWidth = 1.5;
      ctx.beginPath(); ctx.moveTo(p1.x, p1.y); ctx.lineTo(p2.x, p2.y); ctx.stroke();
    }
    // vertical connector at first freq
    if (ti % 20 === 0) {
      const ptop = P(freqs[0], t, clamp(slice[0], -45, 0)), pbot = P(freqs[0], t, -45);
      ctx.strokeStyle = 'rgba(120,140,170,0.15)'; ctx.beginPath(); ctx.moveTo(ptop.x, ptop.y); ctx.lineTo(pbot.x, pbot.y); ctx.stroke();
    }
  }
  // axes labels/ticks
  ctx.font = '10px "Cascadia Mono", monospace'; ctx.fillStyle = 'rgba(190,208,228,0.9)'; ctx.textAlign = 'center'; ctx.textBaseline = 'top';
  for (const f of fTicks) { const p = P(f, 0, -45); ctx.fillText(String(f), p.x, p.y + 7); }
  ctx.fillText('频率 Hz（对数，20–200）', x0 + Wx * 0.55, y0 + 26);
  ctx.textAlign = 'right'; ctx.font = '10px "Cascadia Mono", monospace';
  for (const t of [0, 200, 400, 600]) { const p = P(200, t, -45); ctx.fillText(String(t), p.x + 40, p.y + 2); }
  ctx.save(); ctx.translate(x0 + Wx + depthX * 0.92, y0 + depthY * 0.55); ctx.rotate(-0.5); ctx.textAlign = 'center'; ctx.fillStyle = 'rgba(190,208,228,0.9)'; ctx.fillText('时间 ms（0–600）', 0, 0); ctx.restore();
  ctx.textAlign = 'left'; ctx.fillStyle = 'rgba(190,208,228,0.9)'; ctx.fillText('dB', zx.x - 44, zx.y - zH - 8);
  // colorbar
  const cbx = w - 44, cby = T + 12, cbh = h * 0.42;
  const grd = ctx.createLinearGradient(0, cby, 0, cby + cbh);
  grd.addColorStop(0, color(0)); grd.addColorStop(1, color(-45));
  ctx.fillStyle = grd; ctx.fillRect(cbx, cby, 12, cbh);
  ctx.strokeStyle = 'rgba(120,140,170,0.3)'; ctx.strokeRect(cbx, cby, 12, cbh);
  ctx.font = '8.5px "Cascadia Mono", monospace'; ctx.fillStyle = 'rgba(160,180,205,0.75)'; ctx.textAlign = 'left'; ctx.textBaseline = 'middle';
  for (const v of [0, -15, -30, -45]) ctx.fillText(v, cbx + 16, cby + (1 - zN(v)) * cbh);
  ctx.fillText('dB', cbx + 16, cby - 8);
  // source note
  ctx.fillStyle = 'rgba(140,158,184,0.6)'; ctx.font = '9px "Microsoft YaHei", sans-serif'; ctx.textAlign = 'left'; ctx.textBaseline = 'bottom';
  ctx.fillText('实测 CSD · UMIK FL · 有效 40–190 Hz / 0–400 ms（轴 20–200 Hz / 0–600 ms）· 0 dB=公共峰值', L + 4, h - 6);
}
function synthIR(ch, modes) {
  const fs = 48000, N = 2880;
  const ir = new Float32Array(N);
  for (const m of modes) {
    const tau = Math.max(0.05, m.t60_s / 6.9);
    const amp = Math.pow(10, m.prominence_db / 20) * 0.22;
    let ph = (m.hz * 37) % (Math.PI * 2);
    for (let i = 0; i < N; i++) {
      const t = i / fs;
      ir[i] += amp * Math.exp(-t / tau) * Math.sin(2 * Math.PI * m.hz * t + ph);
    }
  }
  for (let i = 0; i < N; i++) ir[i] += (Math.random() * 2 - 1) * 0.006 * Math.exp(-i / (fs * 0.02));
  let mx = 0; for (let i = 0; i < N; i++) mx = Math.max(mx, Math.abs(ir[i]));
  for (let i = 0; i < N; i++) ir[i] /= (mx || 1);
  return ir;
}
