
/* =========================================================
 * Step 1–4 renderers
 * ======================================================= */
function roomTransform(w, h) {
  const padX = 58, padT = 34, padB = 30;
  const x0 = padX, x1 = w - padX, y0 = padT, y1 = h - padB;
  return {
    x: u => (x0 + x1) / 2 + u * ((x1 - x0) / 9),
    y: v => (y1 + y0) / 2 - v * ((y1 - y0) / 9),
    rect: [x0, y0, x1 - x0, y1 - y0],
  };
}
function drawRoom(ctx, w, h) {
  const t = roomTransform(w, h);
  const [x0, y0, rw, rh] = t.rect;
  ctx.fillStyle = '#0a111a'; ctx.fillRect(0, 0, w, h);
  // room
  const grd = ctx.createLinearGradient(x0, y0, x0, y0 + rh);
  grd.addColorStop(0, 'rgba(26,42,63,0.5)'); grd.addColorStop(1, 'rgba(14,24,38,0.35)');
  ctx.fillStyle = grd;
  roundRect(ctx, x0, y0, rw, rh, 10); ctx.fill();
  ctx.strokeStyle = 'rgba(90,120,155,0.55)'; ctx.lineWidth = 1.5; ctx.stroke();
  // screen / front wall
  ctx.strokeStyle = 'rgba(120,160,200,0.5)'; ctx.lineWidth = 2;
  ctx.beginPath(); ctx.moveTo(t.x(-2.0), y0 + 8); ctx.lineTo(t.x(2.0), y0 + 8); ctx.stroke();
  ctx.fillStyle = 'rgba(150,180,210,0.55)'; ctx.font = '9.5px "Microsoft YaHei", sans-serif';
  ctx.textAlign = 'center'; ctx.textBaseline = 'top';
  ctx.fillText('显示设备 / 幕布', t.x(0), y0 + 13);
  // grid
  ctx.strokeStyle = 'rgba(120,140,170,0.07)'; ctx.lineWidth = 1;
  for (let i = -4; i <= 4; i++) {
    ctx.beginPath(); ctx.moveTo(t.x(i), y0); ctx.lineTo(t.x(i), y0 + rh); ctx.stroke();
    ctx.beginPath(); ctx.moveTo(x0, t.y(i * 0.9)); ctx.lineTo(x0 + rw, t.y(i * 0.9)); ctx.stroke();
  }
  // listener
  const lx = t.x(0), ly = t.y(0.85);
  ctx.strokeStyle = 'rgba(247,171,74,0.65)'; ctx.setLineDash([4, 4]); ctx.beginPath(); ctx.arc(lx, ly, 34, 0, Math.PI * 2); ctx.stroke(); ctx.setLineDash([]);
  ctx.fillStyle = 'rgba(247,171,74,0.9)';
  ctx.beginPath(); ctx.arc(lx, ly, 5, 0, Math.PI * 2); ctx.fill();
  ctx.fillStyle = 'rgba(255,206,140,0.9)'; ctx.font = '10px "Microsoft YaHei", sans-serif'; ctx.textBaseline = 'top';
  ctx.fillText('听音位（甜点）', lx, ly + 12);
  return t;
}
function drawSpeaker(ctx, x, y, label, ch, color) {
  ctx.save();
  ctx.shadowColor = color; ctx.shadowBlur = 8;
  ctx.fillStyle = 'rgba(10,17,26,0.96)';
  roundRect(ctx, x - 25, y - 13, 50, 26, 6); ctx.fill();
  ctx.strokeStyle = color; ctx.lineWidth = 1.4; ctx.stroke();
  ctx.shadowBlur = 0;
  ctx.fillStyle = color; ctx.font = '600 9.5px "Cascadia Mono", Consolas, monospace';
  ctx.textAlign = 'center'; ctx.textBaseline = 'middle';
  ctx.fillText(label, x, y - 1);
  ctx.fillStyle = 'rgba(160,180,205,0.7)'; ctx.font = '8.5px "Cascadia Mono", Consolas, monospace';
  ctx.fillText('out' + (ch + 1), x, y + 9.5);
  ctx.restore();
}

/* ---------------- STEP 1 ---------------- */
const CHECK_ITEMS = [
  { key: 'motu', icon: '▤', title: 'MOTU UltraLite-mk5', desc: 'USB 音频接口 · 22 输出 · 48 kHz', log: 'USB 设备枚举：UltraLite-mk5 · 22 输出 · 48 kHz · 时钟 internal', ok: '已连接 · 22ch / 48k' },
  { key: 'umik', icon: '◉', title: 'UMIK-1 测量麦克风', desc: '序列号 718-7696 · 90° 校准文件', log: 'UMIK-1 718-7696 · 校准文件 90deg 已加载 · 输入增益 +18 dB', ok: '已识别 · 90° 校准' },
  { key: 'wiring', icon: '⌁', title: '输出线序核对（4.1.2）', desc: 'Line Out 1–6 + Line Out 8（ch6 保留空）', log: '输出线序：FL/FR=1-2 · SL/SR=3-4 · HFL/HFR=5-6 · SUB1=8 · ch7 空', ok: '8 路已核对' },
  { key: 'service', icon: '◫', title: '服务与静音状态', desc: 'tspi-audio-web 运行中 · DSP 待机 · 已静音', log: '服务状态：tspi-audio-web active · camilladsp inactive · master mute on', ok: 'Web 运行 · 已静音' },
];
function renderStep1() {
  const body = $('#step-body');
  body.innerHTML =
    '<div style="display:flex;flex-direction:column;gap:12px;height:100%">' +
      '<div class="check-actions">' +
        '<button class="btn primary" id="btn-check" data-test="btn-check">开始检测</button>' +
        '<span class="hint">只读模拟检测：设备枚举、校准文件、输出线序、服务/静音状态；不打开音频设备、不出声。</span>' +
      '</div>' +
      '<div class="check-grid">' +
        CHECK_ITEMS.map(it => '<div class="checkcard" id="cc-' + it.key + '"><div class="ico">' + it.icon + '</div><div class="body"><b>' + it.title + '</b><div class="muted">' + it.desc + '</div></div><span class="st" id="cs-' + it.key + '">待检测</span></div>').join('') +
      '</div>' +
      '<div class="panel" style="flex:1;min-height:0"><div class="panel-h">检测日志 <span class="hint">只读 · 不写设备</span></div><div class="panel-b" style="padding:0"><div class="logbox" id="check-log" style="height:100%;border:0;border-radius:0"></div></div></div>' +
    '</div>';
  $('#btn-check').addEventListener('click', runCheck1);
  renderCheck1State();
  if (!state.check1.done && !state.check1.busy) logLine('等待开始检测 · 点击上方“开始检测”', '');
  else if (state.check1.done) { CHECK_ITEMS.forEach(it => logLine('✓ ' + it.log, 'ok')); logLine('检测完成：4/4 通过 · 可以继续布局分配', 'ok'); }
}
function renderCheck1State() {
  for (const it of CHECK_ITEMS) {
    const card = $('#cc-' + it.key), st = $('#cs-' + it.key);
    if (!card) continue;
    const s = state.check1.items[it.key];
    card.className = 'checkcard' + (s === 'ok' ? ' ok' : s === 'busy' ? ' busy' : '');
    st.textContent = s === 'ok' ? '✓ ' + it.ok : s === 'busy' ? '检测中…' : state.check1.done ? '✓ ' + it.ok : '待检测';
  }
}
function logLine(text, cls) {
  const box = $('#check-log'); if (!box) return;
  const d = new Date();
  const ts = String(d.getMinutes()).padStart(2, '0') + ':' + String(d.getSeconds()).padStart(2, '0');
  const el = document.createElement('div');
  el.className = 'ln';
  el.innerHTML = '<span class="t">[' + ts + ']</span>' + (cls ? '<span class="' + cls + '">' + text + '</span>' : text);
  box.appendChild(el); box.scrollTop = box.scrollHeight;
}
async function runCheck1() {
  if (state.check1.busy) return;
  state.check1.busy = true;
  const btn = $('#btn-check'); if (btn) { btn.textContent = '检测中…'; btn.disabled = true; }
  state.check1.items = { motu: 'idle', umik: 'idle', wiring: 'idle', service: 'idle' };
  state.check1.done = false;
  const box = $('#check-log'); if (box) box.innerHTML = '';
  logLine('开始只读检测（不打开音频设备 / 不出声）', '');
  for (const it of CHECK_ITEMS) {
    state.check1.items[it.key] = 'busy'; renderCheck1State();
    logLine('→ ' + it.title + ' …', '');
    await sleep(520);
    state.check1.items[it.key] = 'ok';
    logLine('✓ ' + it.log, 'ok');
    renderCheck1State();
  }
  await sleep(360);
  logLine('检测完成：4/4 通过 · 可以继续布局分配', 'ok');
  state.check1.busy = false; state.check1.done = true;
  if (btn) { btn.textContent = '重新检测'; btn.disabled = false; }
  renderCheck1State(); renderHeader();
}

/* ---------------- STEP 2 ---------------- */
function layoutValid() {
  const outs = state.layout.roles.map(r => r.out);
  return new Set(outs).size === outs.length && !outs.includes(6) && outs.every(o => o >= 0) && state.layout.roles.length === 7;
}
function renderStep2() {
  const body = $('#step-body');
  body.innerHTML =
    '<div class="two-col">' +
      '<div class="col-main"><div class="panel" style="flex:1"><div class="panel-h">房间与扬声器布局 <span class="hint">俯视示意 · 4.1.2 七角色（板端部署映射）</span></div><div class="panel-b" style="padding:0"><canvas id="layout-canvas" class="room-canvas"></canvas></div></div></div>' +
      '<div class="col-side">' +
        '<div class="panel"><div class="panel-h">角色 → MOTU 输出通道 <span class="badge3 real">板端真相</span></div><div class="panel-b">' +
          '<table class="role-table"><thead><tr><th>角色</th><th>位置</th><th>输出</th></tr></thead><tbody>' +
          state.layout.roles.map((r, i) => '<tr id="role-row-' + r.id + '"><td class="role">' + r.id + '</td><td>' + CH_POS[r.id] + '</td><td><select data-role="' + r.id + '" data-test="role-' + r.id + '">' +
            Array.from({ length: 10 }, (_, n) => '<option value="' + n + '"' + (n === r.out ? ' selected' : '') + '>out' + (n + 1) + ' / ch' + n + '</option>').join('') +
            '<option value="-1"' + (r.out < 0 ? ' selected' : '') + '>静音</option></select></td></tr>').join('') +
          '</tbody></table>' +
          '<div class="role-ok" id="layout-msg"></div>' +
          '<button class="btn ghost mini" id="btn-check-layout" data-test="btn-check-layout" style="margin-top:10px">校验布局映射</button>' +
        '</div></div>' +
        '<div class="panel" style="flex:0 0 auto"><div class="panel-h">布局说明 <span class="hint">4.1.2</span></div><div class="panel-b">' +
          '<div class="kv"><span>输入布局</span><b>4.1.2（7 角色）</b></div>' +
          '<div class="kv"><span>地面 4</span><b>FL / FR / SL / SR</b></div>' +
          '<div class="kv"><span>高度 2</span><b>HFL / HFR</b></div>' +
          '<div class="kv"><span>低音 1</span><b>SUB1 = Line Out 8</b></div>' +
          '<div class="kv"><span>ch6</span><b>保留空（不分配）</b></div>' +
          '<div class="tik" style="font-size:9.5px;color:var(--muted2);line-height:1.6;margin-top:8px">映射来自 audio/overlay/etc/tspi-audio/layout-presets.yml 的 4.1.2；MOTU 22ch 仅使用 1-6、8 路。</div>' +
        '</div></div>' +
      '</div>' +
    '</div>';
  $$('#step-body select[data-role]').forEach(sel => sel.addEventListener('change', () => {
    const role = state.layout.roles.find(r => r.id === sel.dataset.role);
    role.out = Number(sel.value);
    state.layout.checked = false;
    updateLayoutMsg();
  }));
  $('#btn-check-layout').addEventListener('click', () => {
    state.layout.checked = true;
    logToastLayout();
    updateLayoutMsg();
    renderHeader();
  });
  updateLayoutMsg();
  requestAnimationFrame(() => {
    const cv = $('#layout-canvas'); if (!cv) return;
    const { ctx, w, h } = setupCanvas(cv);
    const t = drawRoom(ctx, w, h);
    const pos = { FL: [-2.6, -3.2], FR: [2.6, -3.2], SL: [-3.5, 0.1], SR: [3.5, 0.1], HFL: [-2.0, -2.3], HFR: [2.0, -2.3], SUB1: [-0.9, -3.7] };
    for (const role of state.layout.roles) {
      const p = pos[role.id] || [0, 0];
      const color = role.id === 'SUB1' ? '#f7ab4a' : /^H/.test(role.id) ? '#c39bff' : '#43b1ff';
      const x = t.x(p[0]), y = t.y(p[1]);
      ctx.strokeStyle = 'rgba(100,140,180,0.16)'; ctx.setLineDash([3, 5]); ctx.lineWidth = 1;
      ctx.beginPath(); ctx.moveTo(x, y); ctx.lineTo(t.x(0), t.y(0.85)); ctx.stroke(); ctx.setLineDash([]);
      drawSpeaker(ctx, x, y, role.id, role.out, color);
    }
  });
}
function logToastLayout() { /* 校验提示写入面板 */ }
function updateLayoutMsg() {
  const msg = $('#layout-msg'); if (!msg) return;
  const outs = state.layout.roles.map(r => r.out);
  const dup = outs.filter((o, i) => o >= 0 && outs.indexOf(o) !== i);
  const ch6 = outs.includes(6);
  const valid = layoutValid();
  if (valid) {
    msg.innerHTML = '<b>✓ 校验通过</b><span style="color:var(--muted)">7/7 角色唯一映射 · ch6 保留空</span>';
  } else if (dup.length) {
    msg.innerHTML = '<span style="color:var(--red)">✗ 输出通道重复：' + dup.map(o => 'out' + (o + 1)).join(', ') + '</span>';
  } else if (ch6) {
    msg.innerHTML = '<span style="color:var(--red)">✗ ch6 被占用（板端约定 ch6 保留空，SUB1 走 ch7/Line Out 8）</span>';
  } else {
    msg.innerHTML = '<span style="color:var(--amber)">! 存在静音角色，请为 7 个角色分配唯一输出</span>';
  }
  const btn = $('#btn-check-layout');
  if (btn) btn.classList.toggle('active', state.layout.checked && valid);
}

/* ---------------- STEP 3 ---------------- */
const LEVEL_DBFS = { FL: -21.4, FR: -20.8, SL: -23.2, SR: -22.6, HFL: -26.1, HFR: -25.3, SUB1: -18.6 };
function polInfo(ch) {
  const s = PILOT.summary[ch];
  const v = s.votes || [];
  const allSame = v.every(x => x === v[0]);
  return { votes: v, median: s.polarity, allSame, text: v.map(x => x > 0 ? '+' : '−').join(' ') };
}
function renderStep3Pilot() {
  const body = $('#step-body');
  const lowSnrCh = CH_ALL.filter(ch => PILOT.summary[ch].snr_db < 15);
  body.innerHTML =
    '<div style="display:flex;flex-direction:column;gap:11px;height:100%">' +
      '<div class="mic-cards">' +
        '<div class="mic-card"><div><div class="lbl">测量麦克风</div><div class="big">UMIK-1</div><div class="sub">序列号 718-7696</div></div></div>' +
        '<div class="mic-card"><div><div class="lbl">校准 / 朝向</div><div class="big">90°</div><div class="sub">校准文件已加载</div></div></div>' +
        '<div class="mic-card"><div><div class="lbl">本底噪声</div><div class="big">-62 <small style="font-size:10px">dBFS</small></div><div class="sub">50–200 Hz 取样</div></div></div>' +
        '<div class="mic-card"><div><div class="lbl">采样率 / 增益</div><div class="big">48k</div><div class="sub">Mic gain +18 dB</div></div></div>' +
      '</div>' +
      '<div class="check-actions">' +
        '<button class="btn primary" id="btn-check3" data-test="btn-check3">开始逐通道检测</button>' +
        '<span class="hint">SNR 为 pilot 实测（summary.csv）；电平表为演示动画，电平值为示意。</span>' +
      '</div>' +
      '<div class="panel" style="flex:1;min-height:0"><div class="panel-h">逐通道电平 / 极性 <span class="hint">目标电平 -30 ~ -12 dBFS</span></div><div class="panel-b scroll" style="padding:0 14px">' +
        '<table class="lvl-table"><thead><tr><th>通道</th><th>位置</th><th>电平（示意）</th><th style="width:130px">电平表</th><th>SNR（实测）</th><th>极性投票</th></tr></thead><tbody>' +
        CH_ALL.map(ch => {
          const snr = PILOT.summary[ch].snr_db;
          const pi = polInfo(ch);
          const bad = !pi.allSame;
          return '<tr id="lvl-' + ch + '"><td class="ch">' + ch + '</td><td style="color:var(--muted);font-size:10.5px">' + CH_LABEL[ch] + '</td>' +
            '<td class="db" id="db-' + ch + '">—</td>' +
            '<td><div class="meter"><div class="fill" id="mt-' + ch + '"></div><div class="target"></div></div></td>' +
            '<td class="snr ' + (snr < 15 ? 'low' : '') + '">' + snr.toFixed(1) + ' dB</td>' +
            '<td><span class="pol ' + (bad ? 'warn' : 'ok') + '" id="pol-' + ch + '">' + (bad ? '⚠ ' : '') + pi.text + (bad ? ' 2:1 待确认' : (pi.median < 0 ? ' 反相一致' : ' 正相一致')) + '</span></td></tr>';
        }).join('') +
        '</tbody></table>' +
        '<div id="check3-extra" style="margin-top:10px"></div>' +
      '</div></div>' +
    '</div>';
  $('#btn-check3').addEventListener('click', runCheck3);
  renderCheck3State();
}
function polBadge(ch, fixed) {
  const pi = polInfo(ch);
  const el = $('#pol-' + ch); if (!el) return;
  if (pi.allSame) {
    el.className = 'pol ok';
    el.textContent = (pi.median < 0 ? '反相一致' : '正相一致') + ' · ±' + Math.abs(pi.median);
  } else if (fixed) {
    el.className = 'pol ok';
    el.textContent = '✓ 已按多数确认（' + pi.median + '）';
  } else {
    el.className = 'pol warn';
    el.textContent = '⚠ 投票 ' + pi.votes.filter(v => v > 0).length + ':' + pi.votes.filter(v => v < 0).length + ' · 待确认';
  }
}
function renderCheck3State() {
  for (const ch of CH_ALL) {
    const mt = $('#mt-' + ch), db = $('#db-' + ch);
    if (!mt) continue;
    if (state.check3.levels[ch] != null) {
      const lv = state.check3.levels[ch];
      mt.style.width = clamp((lv + 60) / 60 * 100, 2, 100) + '%';
      db.textContent = lv.toFixed(1) + ' dBFS';
    }
    polBadge(ch, state.check3.fixed);
  }
  const extra = $('#check3-extra'); if (!extra) return;
  if (!state.check3.done) { extra.innerHTML = ''; return; }
  const bad = CH_ALL.filter(ch => !polInfo(ch).allSame);
  let html = '';
  if (bad.length && !state.check3.fixed) {
    html += '<div class="warn-card" id="pol-warn" style="margin-bottom:8px"><div class="warn-ico">!</div><div class="warn-body"><b>极性投票不一致 · ' + bad.join(' / ') + '</b><p>同一通道的三次采集极性投票为 2:1，建议按多数判定确认后再继续（声音可能出现反相/低频抵消）。</p><button class="btn ghost mini" id="btn-fix-pol" data-test="btn-fix-pol" style="margin-top:7px">全部采用多数判定（' + bad.length + ' 通道）</button></div></div>';
  }
  const lowSnr = CH_ALL.filter(ch => PILOT.summary[ch].snr_db < 15);
  if (lowSnr.length) {
    html += '<div class="warn-card info" style="margin-bottom:8px"><div class="warn-ico">i</div><div class="warn-body"><b>SNR 偏低（实测）：' + lowSnr.map(ch => ch + ' ' + PILOT.summary[ch].snr_db.toFixed(1) + ' dB').join(' · ') + '</b><p>低于 15 dB 的通道已标记，分析页会限制其高频补偿量；正式测量建议提高音量或降低噪底后重测。</p></div></div>';
  }
  if (state.check3.fixed) html += '<div class="role-ok"><b>✓</b><span style="color:var(--teal)">多数判定已确认：' + bad.join(' / ') + '</span></div>';
  extra.innerHTML = html;
  const fb = $('#btn-fix-pol');
  if (fb) fb.addEventListener('click', () => {
    state.check3.fixed = true;
    renderCheck3State(); renderHeader();
  });
}
async function runCheck3() {
  if (state.check3.busy) return;
  state.check3.busy = true; state.check3.done = false; state.check3.fixed = false;
  state.check3.levels = {};
  const btn = $('#btn-check3'); if (btn) { btn.textContent = '检测中…'; btn.disabled = true; }
  renderCheck3State();
  for (const ch of CH_ALL) {
    state.check3.levels[ch] = LEVEL_DBFS[ch];
    renderCheck3State();
    await sleep(520);
  }
  await sleep(320);
  state.check3.busy = false; state.check3.done = true;
  if (btn) { btn.textContent = '重新检测'; btn.disabled = false; }
  renderCheck3State(); renderHeader();
}

/* ---------------- STEP 4 ---------------- */
function pointsRoomBody() {
  return '<div class="two-col">' +
    '<div class="col-main"><div class="panel" style="flex:1"><div class="panel-h">测点布置 <span class="hint">俯视示意 · 点击图中测点可增删</span></div><div class="panel-b" style="padding:0"><canvas id="points-canvas" class="room-canvas"></canvas></div></div></div>' +
    '<div class="col-side">' +
      '<div class="panel"><div class="panel-h">测点列表 <span class="hint" id="pt-count"></span></div><div class="panel-b scroll">' +
        '<div class="pt-toolbar"><button class="btn ghost mini" id="btn-clear-points" data-test="btn-clear-points">清空</button><button class="btn primary mini" id="btn-auto-points" data-test="btn-auto-points">自动布置 9 点</button></div>' +
        '<div class="points-list" id="points-list"></div>' +
        '<div class="role-ok" id="points-msg" style="margin-top:9px"></div>' +
      '</div></div>' +
      '<div class="panel" style="flex:0 0 auto"><div class="panel-h">提示</div><div class="panel-b"><div style="font-size:10px;color:var(--muted);line-height:1.6">Dirac Live 按成像风格提供 9 / 13 / 17 个测点；本次演示用 9 点（1 听音位 + 8 环绕）。ART 至少需要 3 个有效测点（含甜点）。</div></div></div>' +
    '</div></div>';
}
function renderStep4() {
  $('#step-body').innerHTML = pointsRoomBody();
  $('#btn-clear-points').addEventListener('click', () => {
    state.points.forEach(p => p.on = false);
    refreshPointsUI();
  });
  $('#btn-auto-points').addEventListener('click', () => {
    state.points = initialPoints();
    refreshPointsUI();
  });
  refreshPointsUI();
  requestAnimationFrame(drawPointsCanvas);
}
function pointScreen(p, cv, w, h) {
  const t = roomTransform(w, h);
  return { x: t.x(p.x), y: t.y(p.y) };
}
function drawPointsCanvas() {
  const cv = $('#points-canvas'); if (!cv) return;
  const { ctx, w, h } = setupCanvas(cv);
  const t = drawRoom(ctx, w, h);
  const pos = { FL: [-2.6, -3.2], FR: [2.6, -3.2], SL: [-3.5, 0.1], SR: [3.5, 0.1], HFL: [-2.0, -2.3], HFR: [2.0, -2.3], SUB1: [-0.9, -3.7] };
  for (const [id, p] of Object.entries(pos)) {
    const color = id === 'SUB1' ? 'rgba(247,171,74,0.7)' : /^H/.test(id) ? 'rgba(195,155,255,0.7)' : 'rgba(67,177,255,0.7)';
    ctx.fillStyle = color; ctx.beginPath(); ctx.arc(t.x(p[0]), t.y(p[1]), 4, 0, Math.PI * 2); ctx.fill();
    ctx.fillStyle = 'rgba(160,180,205,0.55)'; ctx.font = '8px "Cascadia Mono", monospace'; ctx.textAlign = 'center'; ctx.textBaseline = 'top';
    ctx.fillText(id, t.x(p[0]), t.y(p[1]) + 6);
  }
  state.points.forEach((p, i) => {
    const x = t.x(p.x), y = t.y(p.y);
    if (!p.on) {
      ctx.strokeStyle = 'rgba(120,140,160,0.28)'; ctx.lineWidth = 1.2; ctx.setLineDash([2, 3]);
      ctx.beginPath(); ctx.arc(x, y, 9, 0, Math.PI * 2); ctx.stroke(); ctx.setLineDash([]);
      return;
    }
    const col = p.sweet ? '#f7ab4a' : '#43b1ff';
    ctx.save(); ctx.shadowColor = col; ctx.shadowBlur = 10;
    ctx.fillStyle = 'rgba(9,15,24,0.9)';
    ctx.beginPath(); ctx.arc(x, y, 11, 0, Math.PI * 2); ctx.fill();
    ctx.strokeStyle = col; ctx.lineWidth = 1.8; ctx.stroke();
    ctx.restore();
    ctx.fillStyle = col; ctx.font = '600 9px "Cascadia Mono", monospace'; ctx.textAlign = 'center'; ctx.textBaseline = 'middle';
    ctx.fillText(String(i + 1), x, y);
    if (p.sweet) { ctx.fillStyle = 'rgba(247,171,74,0.8)'; ctx.font = '8px "Microsoft YaHei", sans-serif'; ctx.fillText('甜点', x, y + 17); }
  });
  cv.onclick = e => {
    const r = cv.getBoundingClientRect(); const mx = e.clientX - r.left, my = e.clientY - r.top;
    let best = null, bd = 1e9;
    state.points.forEach(p => { const s = pointScreen(p, cv, r.width, r.height); const d = Math.hypot(s.x - mx, s.y - my); if (d < bd) { bd = d; best = p; } });
    if (best && bd < 18) { best.on = !best.on; refreshPointsUI(); }
  };
}
function refreshPointsUI() {
  const list = $('#points-list'); if (!list) return;
  list.innerHTML = state.points.map((p, i) => '<div class="pt-row' + (p.on ? '' : ' off') + (p.sweet ? ' sweet' : '') + '" data-test="pt-row-' + p.id + '"><span class="n">' + (i + 1) + '</span><span>' + p.id + '</span><span class="xy">x ' + p.x.toFixed(1) + 'm · y ' + p.y.toFixed(1) + 'm</span>' + (p.sweet ? '<span class="tagx">听音位</span>' : '') + '</div>').join('');
  $$('#points-list .pt-row').forEach((row, i) => row.addEventListener('click', () => { state.points[i].on = !state.points[i].on; refreshPointsUI(); }));
  const n = state.points.filter(p => p.on).length;
  const cnt = $('#pt-count'); if (cnt) cnt.textContent = n + ' / 9 已选';
  const msg = $('#points-msg');
  if (msg) {
    const sweet = state.points.some(p => p.on && p.sweet);
    if (typeof TSPI_LIVE !== 'undefined' && TSPI_LIVE && sweet) msg.innerHTML = '<b>✓</b><span style="color:var(--teal)">皇帝位已点亮。多点和 ART 的 3 点要求留在页面上，这次不测</span>';
    else if (n >= 3 && sweet) msg.innerHTML = '<b>✓</b><span style="color:var(--teal)">' + n + ' 点（含听音位）· 满足多点校正与 ART ≥3 要求</span>';
    else if (n < 3) msg.innerHTML = '<span style="color:var(--red)">✗ 仅 ' + n + ' 点：ART 至少需要 3 个有效测点</span>';
    else msg.innerHTML = '<span style="color:var(--red)">✗ 必须包含听音位（P1 甜点）</span>';
  }
  drawPointsCanvas(); renderHeader();
}
