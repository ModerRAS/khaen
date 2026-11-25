
/* =========================================================
 * Step 3（麦克风/校准设置）+ Step Level / Delay / Bass
 * 全部走 daemon /api/calibration/*（settings/status/jobs/apply/clear）。
 * host 字段未落地时 REST 返回 501 pending_host_field：页面显式显示，不伪造 applied。
 * ========================================================= */
'use strict';

function calState(kind) { return state[kind]; }
function settingsDeepCopy() { return JSON.parse(JSON.stringify(state.settings)); }
function calValidationText() {
  const v = state.settingsValidation;
  if (!v) return state.settingsSaved ? '设置已保存（未校验）' : '尚未保存';
  return v.label + (v.ok ? ' · 绝对读数可用' : ' · 相对/未校准');
}
function renderWarnings(list) {
  if (!list || !list.length) return '';
  return '<div class="cal-warnings">' + list.map(w => '<div class="cal-warn" data-code="' + w.code + '"><b>' + w.code + '</b><span>' + w.message + '</span></div>').join('') + '</div>';
}

async function khaenLoadSettings() {
  const payload = await khaenApi('GET', CAL_API.settings);
  if (payload && payload.settings) state.settings = payload.settings;
  state.settingsValidation = payload ? payload.validation : null;
  state.settingsSaved = true;
  if (state.settings && state.settings.target_mode) state.design.target = state.settings.target_mode;
}
async function khaenLoadStatus() {
  const payload = await khaenApi('GET', CAL_API.status);
  state.serverStatus = payload;
  state.calApplied = payload.applied || null;
}
async function khaenSaveSettings() {
  const payload = await khaenApi('POST', CAL_API.settings, settingsDeepCopy());
  if (payload && payload.settings) state.settings = payload.settings;
  state.settingsValidation = payload ? payload.validation : null;
  state.settingsSaved = true;
  if (state.settings && state.settings.target_mode) state.design.target = state.settings.target_mode;
  toast('校准设置已保存（' + calValidationText() + '）');
}
async function khaenBoot() {
  state.authOk = await khaenLoadSession();
  try {
    await khaenLoadSettings();
  } catch (error) {
    state.settingsSaved = false;
    state.settingsError = khaenErrorText(error);
  }
  try { await khaenLoadStatus(); } catch (error) { /* status is optional */ }
  const footerRun = document.getElementById('btn-run-all');
  if (footerRun) footerRun.addEventListener('click', runAll);
  renderStep();
}
function calFileSha(short) {
  if (!short || typeof short !== 'string') return '—';
  return short.length > 16 ? short.slice(0, 16) + '…' : short;
}
async function onCalFileSelected(event) {
  const file = event && event.target && event.target.files ? event.target.files[0] : null;
  if (!file) return;
  try {
    const text = await file.text();
    if (!/sensitivity|spl|calibration|freq|hz/i.test(text)) {
      toast('校准文件格式未识别：需要频率/灵敏度列（miniDSP .txt/.cal）');
      return;
    }
    let sha = null;
    if (window.crypto && window.crypto.subtle) {
      const digest = await window.crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
      sha = Array.from(new Uint8Array(digest)).map(b => b.toString(16).padStart(2, '0')).join('');
    }
    state.settings.cal_file = { path: null, name: file.name, sha256: sha, source: 'browser' };
    renderStep();
    toast('已选择 ' + file.name + (sha ? '（SHA-256 已计算）' : '（SHA-256 不可用）'));
  } catch (error) {
    toast('读取校准文件失败：' + khaenErrorText(error));
  }
}

/* ---------------- STEP 3：麦克风与校准设置 ---------------- */
function renderStep3() {
  const s = state.settings;
  const cal = s.cal_file;
  const mic = s.mic;
  const orientation = s.orientation_deg;
  const bass = s.bass;
  const authNote = state.authOk === false
    ? '<div class="cal-warn"><b>AUTH</b><span>未登录：请先打开 <a href="/">控制台</a> 登录，再回到本页（会话 Cookie 同源共享）。</span></div>'
    : '';
  $('#step-body').innerHTML =
    '<div class="cal-settings-grid">' +
      '<div class="panel"><div class="panel-h">麦克风输入 <span class="badge3 model">settings</span></div><div class="panel-b">' +
        authNote +
        '<div class="cal-form">' +
          '<label>输入类型 <select id="mic-kind" data-test="mic-kind"><option value="usb"' + (mic.kind === 'usb' ? ' selected' : '') + '>USB 数字麦（UMIK-1）</option><option value="analog_48v"' + (mic.kind === 'analog_48v' ? ' selected' : '') + '>声卡模拟 + 48V（MOTU Mic/Line）</option></select></label>' +
          '<label>ALSA device <input id="mic-device" data-test="mic-device" type="text" spellcheck="false" value="' + mic.device + '"></label>' +
          '<label>通道 <input id="mic-channel" data-test="mic-channel" type="number" min="0" max="31" step="1" value="' + mic.channel + '"></label>' +
          '<label>增益 dB <input id="mic-gain" data-test="mic-gain" type="number" min="-20" max="60" step="0.5" value="' + mic.gain_db + '"></label>' +
          '<label class="cal-check"><input id="mic-phantom" data-test="mic-phantom" type="checkbox"' + (mic.phantom_48v ? ' checked' : '') + '> 48V 幻象电源（仅模拟输入）</label>' +
          '<label>麦克风朝向 <select id="mic-orientation" data-test="mic-orientation"><option value="90"' + (orientation === 90 ? ' selected' : '') + '>90°（校准文件标称）</option><option value="0"' + (orientation === 0 ? ' selected' : '') + '>0°（指向声源）</option></select></label>' +
          '<label>目标模式 <select id="target-mode" data-test="target-mode"><option value="flat"' + (s.target_mode === 'flat' ? ' selected' : '') + '>flat</option><option value="tilt10"' + (s.target_mode === 'tilt10' ? ' selected' : '') + '>tilt10（20 Hz 0 → 20 kHz −10 dB）</option></select></label>' +
        '</div>' +
        '<div class="cal-actions"><button class="btn primary" id="btn-settings-save" data-test="btn-settings-save">保存设置（POST /api/calibration/settings）</button><button class="btn ghost" id="btn-settings-reload" data-test="btn-settings-reload">从 daemon 重新读取</button><button class="btn ghost" id="btn-settings-status" data-test="btn-settings-status">刷新状态</button></div>' +
        '<div class="cal-valid" id="settings-validation" data-test="settings-validation">' + calValidationText() + '</div>' +
      '</div></div>' +
      '<div class="panel"><div class="panel-h">校准文件（可选） <span class="badge3 sim">绝对/相对</span></div><div class="panel-b">' +
        '<div class="cal-form">' +
          '<label>host 可读路径 <input id="cal-file-path" data-test="cal-file-path" type="text" spellcheck="false" placeholder="/var/lib/khaen-rt-stack/cal/umik1-90deg.txt" value="' + (cal && cal.path ? cal.path : '') + '"></label>' +
          '<label>或选择文件（浏览器计算 SHA-256；上传落盘 pending host） <input id="cal-file-input" data-test="cal-file-input" type="file" accept=".txt,.cal,.csv"></label>' +
        '</div>' +
        '<div class="cal-file-state" data-test="cal-file-state">' + (cal
          ? '文件：' + (cal.name || cal.path) + ' · sha256=' + calFileSha(cal.sha256) + (cal.sha256 ? '（绝对读数可用）' : '（缺 SHA-256 → 相对/UNCALIBRATED）')
          : '未配置校准文件 → 相对 / UNCALIBRATED（电平按相对值处理）') + '</div>' +
        '<div class="cal-actions"><button class="btn ghost" id="btn-cal-file-clear" data-test="btn-cal-file-clear">清除校准文件</button></div>' +
        '<div class="hint" style="margin-top:8px">功能说明：有校准文件 + SHA-256 时页面标 ABSOLUTE；否则标 RELATIVE / UNCALIBRATED。浏览器选择的文件不会自动写到 host（pending host field），可在路径栏填 host 路径。</div>' +
      '</div></div>' +
      '<div class="panel" style="grid-column:1/-1"><div class="panel-h">Bass management（设置将随校准 job 下发） <span class="badge3 model">bass</span></div><div class="panel-b">' +
        '<div class="cal-form cal-form-wide">' +
          '<label class="cal-check"><input id="bass-enabled" data-test="bass-enabled" type="checkbox"' + (bass.enabled ? ' checked' : '') + '> 启用低音炮分频</label>' +
          '<label>crossover Hz <input id="bass-crossover" data-test="bass-crossover" type="number" min="40" max="150" step="1" value="' + bass.crossover_hz + '"></label>' +
          '<label>主箱斜率 <select id="bass-main-slope" data-test="bass-main-slope">' + ['LR2', 'LR4', 'LR6', 'LR8', 'BW2', 'BW4', 'BW6', 'BW8'].map(v => '<option value="' + v + '"' + (bass.main_slope === v ? ' selected' : '') + '>' + v + '</option>').join('') + '</select></label>' +
          '<label>低音炮斜率 <select id="bass-sub-slope" data-test="bass-sub-slope">' + ['LR2', 'LR4', 'LR6', 'LR8', 'BW2', 'BW4', 'BW6', 'BW8'].map(v => '<option value="' + v + '"' + (bass.sub_slope === v ? ' selected' : '') + '>' + v + '</option>').join('') + '</select></label>' +
          '<label class="cal-check"><input id="bass-linear-phase" data-test="bass-linear-phase" type="checkbox"' + (bass.linear_phase ? ' checked' : '') + '> 线性相位（注意 pre-ringing）</label>' +
          '<label>低音炮增益 dB <input id="bass-sub-gain" data-test="bass-sub-gain" type="number" min="-12" max="6" step="0.5" value="' + bass.sub_gain_db + '"></label>' +
        '</div>' +
        '<div class="hint" style="margin-top:8px">线性相位分频会引入群延迟/pre-ringing；低频瞬态优先时可关掉 linear_phase 用最小相位 LR4。护栏在 Bass 步骤结果里显示。</div>' +
      '</div></div>' +
      renderWarnings(state.settingsValidation && state.settingsValidation.warnings) +
    '</div>' +
    renderRunnerPanel();
  function readSettingsInputs() {
    state.settings.mic.kind = $('#mic-kind').value;
    state.settings.mic.device = $('#mic-device').value.trim();
    state.settings.mic.channel = Number($('#mic-channel').value);
    state.settings.mic.gain_db = Number($('#mic-gain').value);
    state.settings.mic.phantom_48v = $('#mic-phantom').checked;
    state.settings.orientation_deg = Number($('#mic-orientation').value);
    state.settings.target_mode = $('#target-mode').value;
    const path = $('#cal-file-path').value.trim();
    if (path) {
      if (!state.settings.cal_file) state.settings.cal_file = { path: path, name: path.split(/[\\/]/).pop(), sha256: null, source: 'path' };
      else { state.settings.cal_file.path = path; if (!state.settings.cal_file.name) state.settings.cal_file.name = path.split(/[\\/]/).pop(); state.settings.cal_file.source = state.settings.cal_file.name && !state.settings.cal_file.path ? 'browser' : 'path'; }
    } else if (state.settings.cal_file && state.settings.cal_file.source === 'path') {
      state.settings.cal_file = null;
    }
    state.settings.bass = Object.assign({}, state.settings.bass, {
      enabled: $('#bass-enabled').checked,
      crossover_hz: Number($('#bass-crossover').value),
      main_slope: $('#bass-main-slope').value,
      sub_slope: $('#bass-sub-slope').value,
      linear_phase: $('#bass-linear-phase').checked,
      sub_gain_db: Number($('#bass-sub-gain').value),
    });
    if (state.settings.target_mode) state.design.target = state.settings.target_mode;
  }
  $('#btn-settings-save').addEventListener('click', function () {
    readSettingsInputs();
    khaenSaveSettings().then(function () { renderStep(); }).catch(function (error) {
      state.settingsError = khaenErrorText(error);
      toast('保存设置失败：' + state.settingsError);
      renderStep();
    });
  });
  $('#btn-settings-reload').addEventListener('click', function () {
    khaenLoadSettings().then(function () { renderStep(); }).catch(function (error) { toast('读取设置失败：' + khaenErrorText(error)); });
  });
  $('#btn-settings-status').addEventListener('click', function () {
    khaenLoadStatus().then(function () { renderStep(); toast('状态已刷新'); }).catch(function (error) { toast('状态刷新失败：' + khaenErrorText(error)); });
  });
  $('#cal-file-input').addEventListener('change', onCalFileSelected);
  $('#btn-cal-file-clear').addEventListener('click', function () {
    state.settings.cal_file = null;
    renderStep();
  });
  if (state.settingsInputsBound !== true) {
    const ids = ['mic-kind', 'mic-device', 'mic-channel', 'mic-gain', 'mic-phantom', 'mic-orientation', 'target-mode', 'bass-enabled', 'bass-crossover', 'bass-main-slope', 'bass-sub-slope', 'bass-linear-phase', 'bass-sub-gain', 'cal-file-path'];
    ids.forEach(function (id) { const node = document.getElementById(id); if (node) node.addEventListener('change', readSettingsInputs); });
    state.settingsInputsBound = true;
  }
  bindRunnerPanel();
  renderHeader();
}

/* ---------------- Level / Delay / Bass 公共渲染 ---------------- */
const STAGE_META = {
  level: {
    title: 'Level 对齐',
    hint: '逐箱从静音闭环爬坡到参考电平；SNR 门 + 锁存增益；相对读数（有校准文件才标绝对）。',
    test: 'level',
  },
  delay: {
    title: 'Delay 对齐',
    hint: '逐箱 onset 双门限；LFE 用 40–150 Hz 与 FL 互相关；显示 onset/相对/施加样本。',
    test: 'delay',
  },
  bass: {
    title: 'Bass management',
    hint: '线性相位低音炮分频：crossover/主箱与低音炮斜率/低音炮增益；结果显示群延迟、pre-ringing 与护栏。',
    test: 'bass',
  },
  fir: {
    title: '全频 FIR 校正',
    hint: '每 lane 全频 FIR：目标 flat/tilt10/custom、taps、mixed/linear phase；结果显示期望幅频误差/群延迟/峰值。',
    test: 'fir',
  },
  modal: {
    title: 'Modal EQ',
    hint: '每 lane 检测到的模态频率/Q/削减；结果表 + 每 lane 开关 + 上限校验。',
    test: 'modal',
  },
};
function stageState(kind) { return kind === 'bass' ? state.bass : state[kind]; }
function laneBadge(lane) {
  if (!lane) return '<span class="state-pending">—</span>';
  if (lane.state === 'locked') return '<span class="state-locked">✓ locked</span>';
  if (lane.state === 'failed') return '<span class="state-failed">✗ failed</span>';
  if (lane.state === 'ramping') return '<span class="state-ramping">● ramping</span>';
  return '<span class="state-pending">· ' + (lane.state || 'pending') + '</span>';
}
function levelLaneTable(lanes) {
  if (!lanes || !lanes.length) return '<div class="hint">尚无 per-lane 结果（host 字段未就绪时 REST 明确 501 pending_host_field）。</div>';
  return '<table class="lane-table" data-test="level-lanes"><thead><tr><th>lane</th><th>位置</th><th>achieved dBFS</th><th>SNR dB</th><th>locked gain dB</th><th>ramp</th><th>state</th><th>failure</th></tr></thead><tbody>' +
    lanes.map(l => '<tr data-test="lane-' + l.lane + '"><td class="ch">' + l.lane + '</td><td class="muted">' + (l.position || '') + '</td><td>' + (l.achieved_dbfs == null ? '—' : l.achieved_dbfs.toFixed(1)) + '</td><td>' + (l.snr_db == null ? '—' : l.snr_db.toFixed(1)) + '</td><td>' + (l.locked_gain_db == null ? '—' : (l.locked_gain_db > 0 ? '+' : '') + l.locked_gain_db.toFixed(1)) + '</td><td>' + (l.ramp_steps == null ? '—' : l.ramp_steps) + '</td><td>' + laneBadge(l) + '</td><td class="muted">' + (l.failure || '—') + '</td></tr>').join('') +
    '</tbody></table>';
}
function delayLaneTable(lanes) {
  if (!lanes || !lanes.length) return '<div class="hint">尚无 per-lane 结果（host 字段未就绪时 REST 明确 501 pending_host_field）。</div>';
  return '<table class="lane-table" data-test="delay-lanes"><thead><tr><th>lane</th><th>位置</th><th>onset 样本</th><th>相对样本</th><th>施加 delay 样本</th><th>方法</th><th>置信度</th><th>state</th><th>failure</th></tr></thead><tbody>' +
    lanes.map(l => '<tr data-test="lane-' + l.lane + '"><td class="ch">' + l.lane + '</td><td class="muted">' + (l.position || '') + '</td><td>' + (l.onset_samples == null ? '—' : l.onset_samples) + '</td><td>' + (l.relative_samples == null ? '—' : l.relative_samples) + '</td><td>' + (l.delay_samples == null ? '—' : l.delay_samples) + '</td><td>' + (l.method || '—') + '</td><td>' + (l.confidence == null ? '—' : (l.confidence * 100).toFixed(0) + '%') + '</td><td>' + laneBadge(l) + '</td><td class="muted">' + (l.failure || '—') + '</td></tr>').join('') +
    '</tbody></table>';
}
function bassResultBlock(result) {
  if (!result) return '<div class="hint">尚无 bass 结果（host 字段未就绪时 REST 明确 501 pending_host_field）。</div>';
  const passed = result.guardrail_passed;
  return '<div class="bass-result" data-test="bass-result">' +
    '<div class="cal-valid ' + (passed ? 'ok' : 'bad') + '">护栏：' + (passed ? 'PASS' : 'FAIL') + ' · ' + result.crossover_hz + ' Hz · 主箱 ' + result.main_slope + ' / 低音炮 ' + result.sub_slope + ' · 线性相位=' + (result.linear_phase ? '是' : '否') + '</div>' +
    '<div class="kv-grid">' +
      '<div class="kv"><span>群延迟</span><b>' + (result.group_delay_ms == null ? '—' : result.group_delay_ms.toFixed(1) + ' ms') + '</b></div>' +
      '<div class="kv"><span>pre-ringing</span><b>' + (result.pre_ringing_ms == null ? '—' : result.pre_ringing_ms.toFixed(1) + ' ms') + '</b></div>' +
      '<div class="kv"><span>sub gain</span><b>' + (result.sub_gain_db > 0 ? '+' : '') + result.sub_gain_db.toFixed(1) + ' dB</b></div>' +
    '</div>' +
    '<div class="lane-checks">' + (result.checks || []).map(c => '<div class="' + (c.passed ? 'check-ok' : 'check-bad') + '"><b>' + c.code + '</b> ' + c.detail + '</div>').join('') + '</div>' +
    '<div class="freq-mini">' + (result.freq_response || []).map(p => p.hz + ' Hz: ' + (p.db >= 0 ? '+' : '') + p.db.toFixed(1) + ' dB').join(' · ') + '</div>' +
  '</div>';
}
function renderStage(kind) {
  const meta = STAGE_META[kind];
  const slot = stageState(kind);
  const lanes = slot.lanes || [];
  const skipped = slot.skipped;
  const job = slot.jobId ? slot.jobId : '—';
  const stateHtml = slot.running ? '<span class="chip model">job 运行中</span>' : slot.applied ? '<span class="chip real">已应用（daemon）</span>' : skipped ? '<span class="chip sim">已跳过（pending host）</span>' : '<span class="chip">未开始</span>';
  const result = kind === 'bass' ? bassResultBlock(slot.result && slot.result.bass ? slot.result.bass : null)
    : kind === 'fir' ? firLaneTable(lanes)
    : kind === 'modal' ? modalLaneTable(lanes)
    : (kind === 'level' ? levelLaneTable(lanes) : delayLaneTable(lanes));
  $('#step-body').innerHTML =
    '<div class="stage-wrap">' +
      '<div class="panel"><div class="panel-h">' + meta.title + ' <span class="hint">' + meta.hint + '</span></div><div class="panel-b">' +
        '<div class="stage-top">' + stateHtml + '<span class="hint" data-test="' + meta.test + '-job">job=' + job + '</span></div>' +
        '<div class="prog-wrap"><div class="prog-status" data-test="' + meta.test + '-progress">' + (slot.progressText || '点击开始，daemon 会创建 kind=emperor stage=' + kind + ' 的任务并回报逐 lane 进度') + '</div><div class="bar"><div class="fill" style="width:' + (slot.running ? '55%' : slot.applied || skipped ? '100%' : '0%') + '"></div></div></div>' +
        '<div class="cal-actions">' +
          '<button class="btn primary" data-test="btn-' + meta.test + '-start" data-stage="' + kind + '">开始 ' + meta.title + '</button>' +
          '<button class="btn ghost" data-test="btn-' + meta.test + '-cancel" data-stage="' + kind + '">取消</button>' +
          '<button class="btn ghost" data-test="btn-' + meta.test + '-apply" data-stage="' + kind + '">应用结果</button>' +
          '<button class="btn ghost" data-test="btn-' + meta.test + '-clear" data-stage="' + kind + '">清除</button>' +
          '<button class="btn ghost" data-test="btn-' + meta.test + '-skip" data-stage="' + kind + '">跳过（host 未就绪）</button>' +
        '</div>' +
        (slot.error ? '<div class="cal-warn bad" data-test="' + meta.test + '-error"><b>FAILURE</b><span>' + slot.error + '</span></div>' : '') +
        (skipped ? '<div class="cal-warn"><b>SKIPPED</b><span>本步跳过：没有 applied 回执，结果不标 applied；可随时重新开始。</span></div>' : '') +
        result +
      '</div></div>' +
      (kind === 'bass' ? renderBassSettingsPanel() : kind === 'fir' ? renderFirSettingsPanel() : kind === 'modal' ? renderModalSettingsPanel() : '') +
      renderRunnerPanel() +
    '</div>';
  void stageState;
  if (kind === 'bass') bindBassSettings();
  if (kind === 'fir') bindFirSettings();
  if (kind === 'modal') bindModalSettings();
  bindRunnerPanel();
  document.querySelectorAll('[data-stage]').forEach(function (button) {
    button.addEventListener('click', function () {
      const action = button.getAttribute('data-test');
      const stage = button.getAttribute('data-stage');
      if (action.indexOf('-start') >= 0) startStage(stage);
      if (action.indexOf('-cancel') >= 0) cancelStage(stage);
      if (action.indexOf('-apply') >= 0) applyStage(stage);
      if (action.indexOf('-clear') >= 0) clearStage(stage);
      if (action.indexOf('-skip') >= 0) { stageState(stage).skipped = true; renderStep(); renderRail(); renderHeader(); }
    });
  });
  renderHeader();
}

/* ---------------- 多炮 B 方案（bass step 内的 subs[] + 路由矩阵） ---------------- */
function bassPreview(bass) {
  const rows = [bass.routing.lfe_weights, bass.routing.left_weights, bass.routing.right_weights];
  let peak = 0;
  rows.forEach(function (row) {
    let sum = 0;
    (bass.subs || []).forEach(function (sub, i) { sum += Math.abs(row[i] || 0) * Math.pow(10, sub.gain_db / 20); });
    if (sum > 0) peak = Math.max(peak, 20 * Math.log10(sum));
  });
  return { peak: peak, preGain: peak > 0 ? -Math.min(20, peak) : 0 };
}
function subRowHtml(sub, index) {
  return '<tr data-test="sub-row-' + index + '">' +
    '<td><input class="cal-mini" data-sub="id" data-index="' + index + '" value="' + sub.id + '"></td>' +
    '<td><input class="cal-mini" data-sub="output" data-index="' + index + '" value="' + sub.output + '"></td>' +
    '<td><input class="cal-mini" data-sub="hp_hz" data-index="' + index + '" type="number" min="16" max="60" step="1" value="' + sub.hp_hz + '"></td>' +
    '<td><input class="cal-mini" data-sub="lp_hz" data-index="' + index + '" type="number" min="40" max="150" step="1" value="' + sub.lp_hz + '"></td>' +
    '<td><input class="cal-mini" data-sub="gain_db" data-index="' + index + '" type="number" min="-12" max="6" step="0.5" value="' + sub.gain_db + '"></td>' +
    '<td><input class="cal-mini" data-sub="delay_samples" data-index="' + index + '" type="number" min="0" max="96000" step="1" value="' + sub.delay_samples + '"></td>' +
    '<td><select class="cal-mini" data-sub="polarity" data-index="' + index + '"><option value="1"' + (sub.polarity === 1 ? ' selected' : '') + '>+1</option><option value="-1"' + (sub.polarity === -1 ? ' selected' : '') + '>-1</option></select></td>' +
    '<td><input class="cal-mini" data-sub="notch" data-index="' + index + '" placeholder="80:-6:4" value="' + (sub.modal_notch || []).map(n => n.freq_hz + ':' + n.gain_db + ':' + n.q).join(',') + '"></td>' +
    '</tr>';
}
function renderBassSettingsPanel() {
  const bass = state.settings.bass;
  const preview = bassPreview(bass);
  const subs = bass.subs || [];
  return '<div class="panel"><div class="panel-h">Bass management / 多炮 B 方案 <span class="hint">SUB1 + SUB-L/SUB-R 都参与 LFE 与全局低音；设置随 job 下发</span></div><div class="panel-b">' +
    '<div class="cal-form">' +
      '<label class="cal-check"><input id="bass-enabled" data-test="bass-enabled" type="checkbox"' + (bass.enabled ? ' checked' : '') + '> 启用低音炮分频</label>' +
      '<label>策略 <select id="bass-strategy" data-test="bass-strategy"><option value="multi_b"' + (bass.strategy === 'multi_b' ? ' selected' : '') + '>multi_b（三炮）</option><option value="single"' + (bass.strategy === 'single' ? ' selected' : '') + '>single</option></select></label>' +
      '<label>crossover Hz <input id="bass-crossover" data-test="bass-crossover" type="number" min="40" max="150" step="1" value="' + bass.crossover_hz + '"></label>' +
      '<label>主箱斜率 <select id="bass-main-slope" data-test="bass-main-slope">' + ['LR2', 'LR4', 'LR6', 'LR8', 'BW2', 'BW4'].map(v => '<option value="' + v + '"' + (bass.main_slope === v ? ' selected' : '') + '>' + v + '</option>').join('') + '</select></label>' +
      '<label>低音炮斜率 <select id="bass-sub-slope" data-test="bass-sub-slope">' + ['LR2', 'LR4', 'LR6', 'LR8', 'BW2', 'BW4'].map(v => '<option value="' + v + '"' + (bass.sub_slope === v ? ' selected' : '') + '>' + v + '</option>').join('') + '</select></label>' +
      '<label class="cal-check"><input id="bass-linear-phase" data-test="bass-linear-phase" type="checkbox"' + (bass.linear_phase ? ' checked' : '') + '> 线性相位（注意 pre-ringing）</label>' +
    '</div>' +
    '<div class="subs-head"><b>subs[]（' + subs.length + ' 只）</b><button class="btn ghost mini" id="btn-sub-add" data-test="btn-sub-add">+ 添加炮</button><button class="btn ghost mini" id="btn-sub-remove" data-test="btn-sub-remove">− 删除末只</button></div>' +
    '<table class="lane-table sub-table" data-test="subs-table"><thead><tr><th>id</th><th>output</th><th>HP Hz</th><th>LP Hz</th><th>gain dB</th><th>delay 样本</th><th>极性</th><th>modal notch (f:g:q)</th></tr></thead><tbody>' +
      subs.map(subRowHtml).join('') + '</tbody></table>' +
    '<div class="subs-head" style="margin-top:10px"><b>低频路由矩阵</b><span class="hint">LFE / L 低切 / R 低切 → 每炮权重；默认都分给全部炮</span></div>' +
    '<table class="lane-table route-table" data-test="routing-table"><thead><tr><th>输入</th>' + subs.map(s => '<th>' + s.id + '</th>').join('') + '</tr></thead><tbody>' +
      ['lfe', 'left', 'right'].map(function (key) {
        const label = key === 'lfe' ? 'LFE' : key === 'left' ? 'L 低切' : 'R 低切';
        const row = bass.routing[key + '_weights'] || [];
        return '<tr><td>' + label + '</td>' + subs.map(function (_, i) {
          return '<td><input class="cal-mini" type="number" min="0" max="1.5" step="0.05" data-route="' + key + '_weights" data-index="' + i + '" value="' + (row[i] == null ? 0 : row[i]) + '"></td>';
        }).join('') + '</tr>';
      }).join('') +
    '</tbody></table>' +
    '<div class="cal-form" style="margin-top:8px"><label>stereo bias <input id="bass-stereo-bias" data-test="bass-stereo-bias" type="number" min="-1" max="1" step="0.05" value="' + bass.routing.stereo_bias + '"></label>' +
      '<label>sub gain dB（单炮兼容） <input id="bass-sub-gain" data-test="bass-sub-gain" type="number" min="-12" max="6" step="0.5" value="' + bass.sub_gain_db + '"></label></div>' +
    '<div class="cal-valid" data-test="bass-pre-gain">预测峰值 ' + preview.peak.toFixed(2) + ' dB · pre_gain ' + preview.preGain.toFixed(2) + ' dB（重算；applied 以 host 回执为准）</div>' +
    '<div class="cal-actions"><button class="btn ghost" id="btn-bass-save" data-test="btn-bass-save">保存 Bass/多炮设置</button></div>' +
  '</div></div>';
}
function readBassForm() {
  const bass = state.settings.bass;
  bass.enabled = $('#bass-enabled').checked;
  bass.strategy = $('#bass-strategy').value;
  bass.crossover_hz = Number($('#bass-crossover').value);
  bass.main_slope = $('#bass-main-slope').value;
  bass.sub_slope = $('#bass-sub-slope').value;
  bass.linear_phase = $('#bass-linear-phase').checked;
  bass.sub_gain_db = Number($('#bass-sub-gain').value);
  bass.subs = Array.from(document.querySelectorAll('[data-sub="id"]')).map(function (idNode) {
    const index = idNode.getAttribute('data-index');
    const pick = function (name) { return document.querySelector('[data-sub="' + name + '"][data-index="' + index + '"]'); };
    const notchText = pick('notch') ? pick('notch').value.trim() : '';
    const modalNotch = notchText ? notchText.split(',').map(function (part) {
      const bits = part.split(':').map(Number);
      return { freq_hz: bits[0] || 80, gain_db: bits[1] == null ? -6 : bits[1], q: bits[2] || 4 };
    }) : [];
    return {
      id: idNode.value.trim(),
      output: pick('output').value.trim(),
      hp_hz: Number(pick('hp_hz').value),
      lp_hz: Number(pick('lp_hz').value),
      gain_db: Number(pick('gain_db').value),
      delay_samples: Math.round(Number(pick('delay_samples').value)),
      polarity: Number(pick('polarity').value),
      modal_notch: modalNotch,
    };
  });
  ['lfe_weights', 'left_weights', 'right_weights'].forEach(function (key) {
    bass.routing[key] = bass.subs.map(function (_, i) {
      const node = document.querySelector('[data-route="' + key + '"][data-index="' + i + '"]');
      return node ? Number(node.value) : 0;
    });
  });
  bass.routing.stereo_bias = Number($('#bass-stereo-bias').value);
  const preview = bassPreview(bass);
  bass.predicted_peak_db = preview.peak;
  bass.pre_gain_db = preview.preGain;
  const node = document.querySelector('[data-test="bass-pre-gain"]');
  if (node) node.textContent = '预测峰值 ' + preview.peak.toFixed(2) + ' dB · pre_gain ' + preview.preGain.toFixed(2) + ' dB（重算；applied 以 host 回执为准）';
}
function bindBassSettings() {
  document.querySelectorAll('#step-body input, #step-body select').forEach(function (node) {
    if (node.getAttribute('data-sub') || node.getAttribute('data-route') || /^bass-/.test(node.id || '')) {
      node.addEventListener('change', readBassForm);
      node.addEventListener('input', readBassForm);
    }
  });
  const add = $('#btn-sub-add');
  if (add) add.addEventListener('click', function () {
    readBassForm();
    const bass = state.settings.bass;
    const n = bass.subs.length;
    bass.subs.push({ id: 'SUB_' + (n + 1), output: String(7 + n), hp_hz: 20, lp_hz: bass.crossover_hz, gain_db: 0, delay_samples: 0, polarity: 1, modal_notch: [] });
    bass.routing.lfe_weights.push(0); bass.routing.left_weights.push(0); bass.routing.right_weights.push(0);
    renderStep();
  });
  const remove = $('#btn-sub-remove');
  if (remove) remove.addEventListener('click', function () {
    readBassForm();
    const bass = state.settings.bass;
    if (bass.subs.length <= 1) { toast('至少保留一只炮'); return; }
    bass.subs.pop(); bass.routing.lfe_weights.pop(); bass.routing.left_weights.pop(); bass.routing.right_weights.pop();
    renderStep();
  });
  const save = $('#btn-bass-save');
  if (save) save.addEventListener('click', function () {
    readBassForm();
    khaenSaveSettings().then(function () { renderStep(); toast('Bass/多炮设置已保存'); }).catch(function (error) { toast('保存 Bass 设置失败：' + khaenErrorText(error)); });
  });
}


/* ---------------- 全频 FIR ---------------- */
function firLaneTable(lanes) {
  if (!lanes || !lanes.length) return '<div class="hint">尚无 per-lane FIR 结果（host 字段未就绪时 REST 明确 501 pending_host_field）。</div>';
  return '<table class="lane-table" data-test="fir-lanes"><thead><tr><th>lane</th><th>目标</th><th>taps</th><th>phase</th><th>期望幅频误差 dB</th><th>群延迟 ms</th><th>峰值 dBFS</th><th>state</th></tr></thead><tbody>' +
    lanes.map(l => '<tr data-test="lane-' + l.lane + '"><td class="ch">' + l.lane + '</td><td>' + (l.target_curve || '—') + '</td><td>' + (l.taps == null ? '—' : l.taps) + '</td><td>' + (l.phase || '—') + '</td><td>' + (l.expected_mag_error_db == null ? '—' : l.expected_mag_error_db.toFixed(2)) + '</td><td>' + (l.group_delay_ms == null ? '—' : l.group_delay_ms.toFixed(2)) + '</td><td>' + (l.peak_dbfs == null ? '—' : l.peak_dbfs.toFixed(2)) + '</td><td>' + laneBadge(l) + '</td></tr>').join('') +
    '</tbody></table>';
}
function renderFirSettingsPanel() {
  const fir = state.settings.fir;
  return '<div class="panel"><div class="panel-h">全频 FIR 设置 <span class="hint">每 lane 同一目标/抽头；host 字段未落地时 start 返回 501 pending</span></div><div class="panel-b"><div class="cal-form">' +
    '<label class="cal-check"><input id="fir-enabled" data-test="fir-enabled" type="checkbox"' + (fir.enabled ? ' checked' : '') + '> 启用全频 FIR</label>' +
    '<label>目标曲线 <select id="fir-target" data-test="fir-target">' + ['flat', 'tilt10', 'custom'].map(v => '<option value="' + v + '"' + (fir.target_curve === v ? ' selected' : '') + '>' + v + '</option>').join('') + '</select></label>' +
    '<label>taps <input id="fir-taps" data-test="fir-taps" type="number" min="512" max="24000" step="1" value="' + fir.taps + '"></label>' +
    '<label>相位 <select id="fir-phase" data-test="fir-phase"><option value="mixed"' + (fir.phase === 'mixed' ? ' selected' : '') + '>mixed</option><option value="linear"' + (fir.phase === 'linear' ? ' selected' : '') + '>linear</option></select></label>' +
    '<label style="grid-column:1/-1">custom 曲线（hz:db, 例如 20:0,1000:0,20000:-10） <input id="fir-custom" data-test="fir-custom" type="text" value="' + (fir.custom_curve || []).map(p => p.hz + ':' + p.db).join(',') + '"></label>' +
  '</div><div class="cal-actions"><button class="btn ghost" id="btn-fir-save" data-test="btn-fir-save">保存 FIR 设置</button></div>' +
  '<div class="hint" style="margin-top:6px">taps 上限 24000；linear phase 会给群延迟/pre-ringing 警告（host 未落地不 applied）。</div></div></div>';
}
function readFirForm() {
  const fir = state.settings.fir;
  fir.enabled = $('#fir-enabled').checked;
  fir.target_curve = $('#fir-target').value;
  fir.taps = Math.round(Number($('#fir-taps').value));
  fir.phase = $('#fir-phase').value;
  const text = $('#fir-custom').value.trim();
  fir.custom_curve = text ? text.split(',').map(function (pair) {
    const bits = pair.split(':').map(Number);
    return { hz: bits[0] || 0, db: bits[1] || 0 };
  }) : [];
}
function bindFirSettings() {
  ['fir-enabled', 'fir-target', 'fir-taps', 'fir-phase', 'fir-custom'].forEach(function (id) {
    const node = document.getElementById(id);
    if (node) node.addEventListener('change', readFirForm);
  });
  const save = $('#btn-fir-save');
  if (save) save.addEventListener('click', function () {
    readFirForm();
    khaenSaveSettings().then(function () { renderStep(); toast('全频 FIR 设置已保存'); }).catch(function (error) { toast('保存 FIR 失败：' + khaenErrorText(error)); });
  });
}

/* ---------------- Modal EQ ---------------- */
function modalLaneTable(lanes) {
  if (!lanes || !lanes.length) return '<div class="hint">尚无 modal 结果（host 字段未就绪时 REST 明确 501 pending_host_field）。</div>';
  const disabled = state.settings.modal.disabled_lanes || [];
  return '<table class="lane-table" data-test="modal-lanes"><thead><tr><th>lane</th><th>检测模态（Hz / Q / 削减 dB）</th><th>state</th><th>本 lane 启用</th></tr></thead><tbody>' +
    lanes.map(l => '<tr data-test="lane-' + l.lane + '"><td class="ch">' + l.lane + '</td><td>' + ((l.notches || []).map(n => n.freq_hz.toFixed(1) + ' / Q' + n.q.toFixed(1) + ' / ' + n.gain_db.toFixed(1)).join(' · ') || '—') + '</td><td>' + laneBadge(l) + '</td><td><input type="checkbox" class="modal-lane-toggle" data-modal-lane="' + l.lane + '"' + (disabled.indexOf(l.lane) >= 0 ? '' : ' checked') + '></td></tr>').join('') +
    '</tbody></table>';
}
function renderModalSettingsPanel() {
  const modal = state.settings.modal;
  return '<div class="panel"><div class="panel-h">Modal EQ 设置 <span class="hint">每 lane 检测模态并做 PEQ 削减；上限校验</span></div><div class="panel-b"><div class="cal-form">' +
    '<label class="cal-check"><input id="modal-enabled" data-test="modal-enabled" type="checkbox"' + (modal.enabled ? ' checked' : '') + '> 启用 modal EQ</label>' +
    '<label class="cal-check"><input id="modal-auto" data-test="modal-auto" type="checkbox"' + (modal.auto_detect ? ' checked' : '') + '> 自动检测模态</label>' +
    '<label>每 lane 最大 notch 数（1..8） <input id="modal-max" data-test="modal-max" type="number" min="1" max="8" step="1" value="' + modal.max_notches + '"></label>' +
  '</div><div class="cal-actions"><button class="btn ghost" id="btn-modal-save" data-test="btn-modal-save">保存 Modal 设置</button></div>' +
  '<div class="hint" style="margin-top:6px">关闭某 lane 会写入 modal.disabled_lanes；超过 max_notches 由契约 400 拒绝。</div></div></div>';
}
function readModalForm() {
  const modal = state.settings.modal;
  modal.enabled = $('#modal-enabled').checked;
  modal.auto_detect = $('#modal-auto').checked;
  const max = Math.round(Number($('#modal-max').value));
  modal.max_notches = max;
}
function bindModalSettings() {
  ['modal-enabled', 'modal-auto', 'modal-max'].forEach(function (id) {
    const node = document.getElementById(id);
    if (node) node.addEventListener('change', readModalForm);
  });
  const disabled = [];
  document.querySelectorAll('.modal-lane-toggle').forEach(function (node) {
    if (!node.checked) disabled.push(node.getAttribute('data-modal-lane'));
    node.addEventListener('change', function () {
      state.settings.modal.disabled_lanes = Array.from(document.querySelectorAll('.modal-lane-toggle')).filter(n => !n.checked).map(n => n.getAttribute('data-modal-lane'));
    });
  });
  state.settings.modal.disabled_lanes = disabled;
  const save = $('#btn-modal-save');
  if (save) save.addEventListener('click', function () {
    readModalForm();
    khaenSaveSettings().then(function () { renderStep(); toast('Modal 设置已保存'); }).catch(function (error) { toast('保存 Modal 失败：' + khaenErrorText(error)); });
  });
}

/* ---------------- 一键运行（链序 crossover → FIR → modal → level → delay → emperor） ---------------- */
const RUN_ORDER = ['bass', 'fir', 'modal', 'level', 'delay', 'measure'];
const RUN_LABELS = { bass: '① crossover/bass', fir: '② 全频 FIR', modal: '③ modal EQ', level: '④ level', delay: '⑤ delay', measure: '⑥ emperor/A-B' };
function runnerStageLabel(stage) { return RUN_LABELS[stage] || stage; }
function renderRunnerPanel() {
  const r = state.runner;
  const rows = RUN_ORDER.map(function (stage) {
    const res = (r.results || []).find(x => x.stage === stage);
    const stateText = res ? (res.state === 'complete' ? '✓ complete' : res.state === 'failed' ? '✗ failed' : res.state) : (r.current === stage ? '运行中…' : '等待');
    return '<div class="run-stage ' + (res ? res.state : '') + '" data-test="runner-stage-' + stage + '"><b>' + runnerStageLabel(stage) + '</b><span>' + stateText + '</span></div>';
  }).join('');
  return '<div class="panel" data-test="runner-panel"><div class="panel-h">一键运行（一条按钮拉起全流程） <span class="hint">失败即停；可一键重试；dry-run 无音频自检</span></div><div class="panel-b">' +
    '<div class="cal-form"><label class="cal-check"><input id="run-dry-run" data-test="run-dry-run" type="checkbox"' + (r.dryRun ? ' checked' : '') + '> dry-run（无音频自检，不创建 job）</label></div>' +
    '<div class="prog-status" data-test="runner-progress">' + (r.progressText || '点击“一键运行”按链序执行；dry-run 会明确标 pending 而不是假装通过') + '</div>' +
    '<div class="bar"><div class="fill" style="width:' + Math.round(((r.done || 0) / (r.total || RUN_ORDER.length)) * 100) + '%"></div></div>' +
    '<div class="run-stages">' + rows + '</div>' +
    (r.failedStage ? '<div class="cal-warn bad" data-test="runner-error"><b>FAILED</b><span>' + runnerStageLabel(r.failedStage) + '：' + (r.error || '') + '</span></div>' : '') +
    '<div class="cal-actions"><button class="btn primary" id="btn-run-all-panel" data-test="btn-run-all-panel"' + (r.running ? ' disabled' : '') + '>' + (r.running ? '运行中…' : '▶ 一键运行（' + (r.dryRun ? 'dry-run' : '真实下发') + '）') + '</button>' +
    '<button class="btn ghost" id="btn-run-retry" data-test="btn-run-retry"' + (r.failedStage && !r.running ? '' : ' disabled style="opacity:.45"') + '>重试失败阶段</button></div>' +
  '</div></div>';
}
function bindRunnerPanel() {
  const dry = document.getElementById('run-dry-run');
  if (dry) dry.addEventListener('change', function () { state.runner.dryRun = dry.checked; renderStep(); });
  const all = document.getElementById('btn-run-all-panel');
  if (all && !all.disabled) all.addEventListener('click', runAll);
  const retry = document.getElementById('btn-run-retry');
  if (retry && !retry.disabled) retry.addEventListener('click', retryRun);
}
async function runAll() {
  if (state.runner.running) return;
  const dryRun = state.runner.dryRun !== false;
  state.runner = { running: true, dryRun: dryRun, stages: RUN_ORDER.slice(), results: [], current: null, failedStage: null, error: null, done: 0, total: RUN_ORDER.length, progressText: '准备中…' };
  renderStep();
  if (dryRun) {
    try {
      const payload = await khaenApi('POST', CAL_API.run, { dry_run: true, stages: RUN_ORDER });
      const stages = payload.stages || [];
      state.runner.stages = stages;
      state.runner.total = stages.length || RUN_ORDER.length;
      state.runner.results = stages.map(function (stage) {
        return { stage: stage.stage, state: stage.available ? 'planned' : 'pending', error: stage.available ? '' : 'pending_host_field' };
      });
      state.runner.done = state.runner.results.length;
      const pendingCount = stages.filter(s => !s.available).length;
      state.runner.progressText = 'dry-run 完成：' + stages.length + ' 阶段计划，' + pendingCount + ' 阶段 pending host 字段（未假装通过）；未创建 job、未出声';
      state.runner.running = false;
      renderStep(); toast('dry-run 完成（无音频自检）');
    } catch (error) {
      state.runner.running = false;
      state.runner.failedStage = 'dry-run';
      state.runner.error = khaenErrorText(error);
      state.runner.progressText = 'dry-run 失败：' + state.runner.error;
      renderStep();
    }
    return;
  }
  for (const stage of RUN_ORDER) {
    state.runner.current = stage;
    state.runner.progressText = '执行 ' + runnerStageLabel(stage) + ' …';
    renderStep();
    const result = await startStage(stage);
    state.runner.results.push({ stage: stage, state: result.ok ? 'complete' : 'failed', error: result.error });
    state.runner.done = state.runner.results.length;
    if (!result.ok) {
      state.runner.failedStage = stage;
      state.runner.error = result.error;
      state.runner.progressText = '失败于 ' + runnerStageLabel(stage) + '：' + (result.error || 'unknown');
      state.runner.running = false;
      renderStep();
      return;
    }
  }
  state.runner.running = false;
  state.runner.current = null;
  state.runner.progressText = '全部阶段完成（回执 complete）';
  renderStep();
}
async function retryRun() {
  const failed = state.runner.failedStage;
  if (!failed || state.runner.running) return;
  const order = failed === 'measure' ? ['measure'] : [failed, 'measure'];
  state.runner.failedStage = null;
  state.runner.error = null;
  state.runner.running = true;
  renderStep();
  for (const stage of order) {
    const result = await startStage(stage);
    state.runner.results = state.runner.results.filter(x => x.stage !== stage).concat([{ stage: stage, state: result.ok ? 'complete' : 'failed', error: result.error }]);
    state.runner.done = state.runner.results.filter(x => x.state === 'complete').length;
    state.runner.current = stage;
    if (!result.ok) {
      state.runner.failedStage = stage;
      state.runner.error = result.error;
      state.runner.running = false;
      state.runner.progressText = '重试失败于 ' + runnerStageLabel(stage) + '：' + (result.error || 'unknown');
      renderStep();
      return;
    }
  }
  state.runner.running = false;
  state.runner.current = null;
  state.runner.progressText = '重试完成';
  renderStep();
}

function renderStepLevel() { renderStage('level'); }
function renderStepDelay() { renderStage('delay'); }
function renderStepBass() { renderStage('bass'); }
function renderStepFir() { renderStage('fir'); }
function renderStepModal() { renderStage('modal'); }

async function startStage(kind) {
  const slot = stageState(kind);
  if (slot.running) return;
  slot.running = true; slot.error = null; slot.skipped = false; slot.progressText = '提交 stage=' + kind + ' …';
  slot.lanes = []; if (kind === 'bass') slot.result = null;
  renderStep();
  try {
    const payload = await khaenPostJob(kind);
    slot.jobId = payload.job_id;
    slot.progressText = payload.progress_text || ('job ' + payload.job_id + ' 已创建');
    const firstLanes = payload.lanes || (payload.job && payload.job.lanes) || null;
    if (firstLanes && firstLanes.lanes) slot.lanes = firstLanes.lanes;
    if (firstLanes && firstLanes.bass) slot.result = firstLanes;
    const job = await khaenPollJob(payload.job_id, function (update) {
      slot.progressText = update.progress_text || slot.progressText;
      const lanes = update.lanes || (update.job && update.job.lanes) || null;
      if (lanes && lanes.lanes) slot.lanes = lanes.lanes;
      if (lanes && lanes.bass) slot.result = lanes;
      renderStep();
    });
    const finalLanes = job.lanes || (job.job && job.job.lanes) || null;
    if (finalLanes && finalLanes.lanes) slot.lanes = finalLanes.lanes;
    if (finalLanes && finalLanes.bass) slot.result = finalLanes;
    slot.progressText = job.progress_text || slot.progressText;
    if (job.state !== 'complete') slot.error = job.failure || ('job 状态 ' + job.state);
  } catch (error) {
    slot.error = khaenErrorText(error) + (error && error.code === 'pending_host_field'
      ? '（host 字段未就绪；可点“跳过”继续，但绝不会标 applied）'
      : '');
  }
  slot.running = false;
  renderStep(); renderRail(); renderHeader();
  return { ok: !slot.error, error: slot.error, job: slot.jobId };
}
async function cancelStage(kind) {
  const slot = stageState(kind);
  if (!slot.jobId) return;
  try {
    await khaenApi('POST', CAL_API.jobs + '/' + encodeURIComponent(slot.jobId) + '/cancel', {});
    slot.progressText = '已取消 ' + slot.jobId;
  } catch (error) {
    slot.error = '取消失败：' + khaenErrorText(error);
  }
  renderStep();
}
async function applyStage(kind) {
  const slot = stageState(kind);
  const body = { targets: [kind], job_id: slot.jobId || undefined };
  if (kind === 'level') body.level_trims = (slot.lanes || []).filter(l => l.state === 'locked' && l.locked_gain_db != null).map(l => ({ lane: l.lane, gain_db: l.locked_gain_db }));
  if (kind === 'delay') body.delays = (slot.lanes || []).filter(l => l.state === 'locked' && l.delay_samples != null).map(l => ({ lane: l.lane, delay_samples: l.delay_samples }));
  if (kind === 'bass') body.bass = state.settings.bass;
  if (kind === 'fir') body.fir = state.settings.fir;
  if (kind === 'modal') body.modal = state.settings.modal;
  try {
    const payload = await khaenApi('POST', CAL_API.apply, body);
    slot.applied = true; slot.skipped = false;
    state.calApplied = payload.applied || state.calApplied;
    toast(kind + ' 已应用（daemon 回执；applied_to_audio=false）');
  } catch (error) {
    slot.error = '应用失败：' + khaenErrorText(error) + (error && error.code === 'pending_host_field' ? '（host 字段未就绪，不标 applied）' : '');
    slot.applied = false;
  }
  renderStep(); renderRail(); renderHeader();
}
async function clearStage(kind) {
  const slot = stageState(kind);
  try {
    const payload = await khaenApi('POST', CAL_API.clear, { targets: [kind] });
    slot.applied = false; slot.skipped = false;
    state.calApplied = payload.applied || null;
    toast(kind + ' 已清除（daemon 回执）');
  } catch (error) {
    slot.error = '清除失败：' + khaenErrorText(error);
  }
  renderStep(); renderRail(); renderHeader();
}
