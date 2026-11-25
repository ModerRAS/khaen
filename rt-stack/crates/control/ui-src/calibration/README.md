# Dirac 式完整校准流程 · 8 步向导演示页（v2 修正版）

> 仿 Dirac Live 完整测量/校准流程的独立演示页（本地 mock，不连接设备）。
> 交互参考 Dirac Live 公开文档；不使用其商标与素材。

## 打开

双击 **index.html**（单文件自包含；推荐 Chrome/Edge，≥1440×900）。

## 8 步

1. 设备与房间检查（MOTU/UMIK/线序/静音，只读模拟检测）
2. 扬声器布局与分配（4.1.2 七角色 → MOTU 输出，板端真相；ch6 保留空）
3. 麦克风与电平（UMIK 90°、噪底、逐通道电平/极性；SNR 为 pilot 实测）
4. 测点选择（听音位 + 8 多点，可增删；ART 至少 3 点）
5. 逐点测量（实时进度、SNR 提示、低 SNR 重测）
6. 分析（FR 全带低频高亮 / IR 重建示意 / **3D CSD 瀑布 20–200 Hz×0–600 ms** / **低频 RT60 40–200 Hz** / **实测模态 46–185 Hz + PEQ 建议**）
7. 设计（**目标曲线全带 20–20k，tilt10 = −10 dB@20k**；低切 **主箱 55 / 环绕 60**；**ART/MIMO 衰减 dB 0…−24（示意）**；预测与警告）
8. 回顾/应用/回滚（**实测 / 示意 / 目标分列**；before/after；应用确认、导出 JSON/YAML、回滚二次确认）

## 数据

- 实测：FR/SNR/极性/全带 RT60（2026-11-18 board pilot）；CSD/低频 RT60/模态（2026-11-19 mimo-kit）；滚降表由 FR 计算（方法见页面）。
- 示意：测点布局、逐点测量/SNR 动画、ART/MIMO 衰减、After 预测曲线、IR 重建；页面均标注。
- 抽取数据：data/pilot.json；构建：node src/build.js。

## 文档/证据

- 修正记录：rt-stack/evidence/dirac-style-flow-fix/FIX-NOTES.md
- 流程调研：rt-stack/evidence/dirac-style-flow/RESEARCH-DIRAC-FLOW.md
- BC/ART 调研：rt-stack/evidence/crossover-ui-demo/RESEARCH-DIRAC-BC-ART.md

## 限制

不写设备/不参与真实 ART/MIMO 计算；ART/MIMO/After/IR 为示意；仅验证 Chrome 1600×900。
