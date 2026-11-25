#!/usr/bin/env node
/* 构建单文件演示页：内嵌 CSS / JS / 实测 FR 数据，生成可双击打开的 index.html */
const fs = require('fs');
const path = require('path');
const root = __dirname;
const data = JSON.parse(fs.readFileSync(path.join(root, '..', 'data', 'fr-20-200.json'), 'utf8'));
const css = fs.readFileSync(path.join(root, 'demo.css'), 'utf8');
const js = fs.readFileSync(path.join(root, 'demo.part1.js'), 'utf8') + '\n' + fs.readFileSync(path.join(root, 'demo.part2.js'), 'utf8');
let html = fs.readFileSync(path.join(root, 'index.template.html'), 'utf8');
html = html.replace('/*__CSS__*/', () => css);
html = html.replace('/*__JS__*/', () => js);
const json = JSON.stringify(data);
html = html.replace('const FR_DATA = /*__FR_DATA__*/ null;', () => 'const FR_DATA = ' + json + ';');
if (html.includes('__CSS__') || html.includes('__JS__') || html.includes('__FR_DATA__')) throw new Error('placeholder not replaced');
const out = path.join(root, '..', 'index.html');
fs.writeFileSync(out, html);
console.log('wrote', out, html.length, 'bytes');
