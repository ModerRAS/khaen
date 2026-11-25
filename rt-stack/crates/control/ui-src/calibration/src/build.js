#!/usr/bin/env node
/* 构建两套产物（源在 rt-stack/crates/control/ui-src/calibration/，bin 目录见 web.rs CALIBRATION_*）：
 * 1) ../index.html —— 单文件演示页（内嵌 CSS / JS / pilot 数据），双击可看流程（不提交）。
 * 2) ../assets/calibration/{index.html,style.css,app.js}（= crates/control/assets/calibration/）
 *    —— daemon 在 /calibration 提供的正式页面（同源外部资源，满足 CSP script-src 'self'）。
 *
 * 用法：node src/build.js（在本目录）。
 */
const fs = require('fs');
const path = require('path');
const root = __dirname;
const data = fs.readFileSync(path.join(root, '..', 'data', 'pilot.json'), 'utf8');
const css = fs.readFileSync(path.join(root, 'wizard.css'), 'utf8');
const parts = ['wizard.part1.js', 'wizard.part2.js', 'wizard.part3.js', 'wizard.part4.js', 'wizard.part5.js']
  .map(f => fs.readFileSync(path.join(root, f), 'utf8'));
const pilotLiteral = 'const PILOT = ' + JSON.stringify(JSON.parse(data)) + ';';
const js = parts.join('\n').replace('const PILOT = /*__PILOT__*/ null;', () => pilotLiteral);
if (js.includes('/*__PILOT__*/')) throw new Error('PILOT placeholder not replaced');
const template = fs.readFileSync(path.join(root, 'index.template.html'), 'utf8');

// 1) standalone demo page
let html = template.replace('/*__CSS__*/', () => css).replace('/*__JS__*/', () => js);
if (html.includes('__CSS__') || html.includes('__JS__') || html.includes('__PILOT__')) throw new Error('placeholder not replaced');
const out = path.join(root, '..', 'index.html');
fs.writeFileSync(out, html);
console.log('wrote', out, html.length, 'bytes');

// 2) daemon-served assets（CSP 同源外部资源；无 inline script/style）
const servedDir = path.join(root, '..', '..', '..', 'assets', 'calibration');
fs.mkdirSync(servedDir, { recursive: true });
let served = template
  .replace('<style>\n/*__CSS__*/\n</style>', '<link rel="stylesheet" href="/calibration/style.css">')
  .replace('<script>\n/*__JS__*/\n</script>', '<script src="/calibration/app.js" defer></script>')
  .replace('<title>KHAEN Audio · Dirac 式校准流程向导（演示）</title>', '<title>KHAEN 校准向导 · Calibration</title>');
if (served.includes('__CSS__') || served.includes('__JS__') || served.includes('__PILOT__')) throw new Error('served placeholder not replaced');
if (/<style[\s>]/i.test(served) || !served.includes('src="/calibration/app.js"') || !served.includes('href="/calibration/style.css"')) {
  throw new Error('served page must use external calibration assets only');
}
fs.writeFileSync(path.join(servedDir, 'index.html'), served);
fs.writeFileSync(path.join(servedDir, 'style.css'), css);
fs.writeFileSync(path.join(servedDir, 'app.js'), js);
console.log('wrote', servedDir, served.length, '+', css.length, '+', js.length, 'bytes');
