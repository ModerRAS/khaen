# 低切 / 分频 + ART 支持 · 交互演示页

> 独立演示页（mock，不连板）：参考 Dirac Live Bass Control / Active Room Treatment 的**公开操作逻辑**做的交互原型，用于给用户看“低切怎么选、ART 支持怎么调”。
> 不使用 Dirac 商标与素材；所有图形由 Canvas/DOM 绘制。

## 打开方式

直接双击本目录的 **index.html**（单文件自包含，无需 dev server、无需联网）。

- 数据已内嵌（20–200 Hz 实测 FR），file:// 下可正常运行。
- 推荐 Chrome / Edge，窗口 ≥ 1440×900（录制分辨率为 1600×900）。

## 页面一：低切 / 分频（Bass Control 式）

- 实测 FR：2026-11-18 板端窗口 座位 B / UMIK-1，FL、FR、SL、SR、SUB1 的 20–200 Hz 曲线。
- 主箱 L/R、环绕 L/R 两个组，**每组独立低切点 + 斜率**（12/24/48 dB/oct，LR2/LR4/LR8）。
- 图上橙色竖线可**拖动**（也可双击图内或直接输入数字）调整低切；预测曲线实时刷新。
- 同时显示：实测 FR（细灰）、主箱高通（蓝）、SUB1 互补低通（橙虚线）、预测合成（白粗线）。
- 预测模型：实测幅度 × LR 分频复数响应后相加，假设相位已对齐、未含目标曲线 EQ；属近似模型，页面有说明。
- **有效范围提示**：用实测数据自动算出每组“有效下潜”（主箱 43.6 Hz / 环绕 61.7 Hz），低于它时红色风险区 + 警告卡 + 底部说明，防止把低切拖到音箱下潜以下。
- 预设：主箱 55 Hz、环绕 60 Hz（环绕预设会触发警告，演示“低于实测下潜”的保护逻辑）。
- 低音炮：SUB1 电平滑杆、SUB2 未连接占位；导出 bass_management 配置。
- **保存 / 导出**：弹出 JSON 配置 + channelmap YAML 片段，可下载 JSON（不写设备）。

## 页面二：ART 支持（Active Room Treatment 式，简化）

- 5×5 **支持矩阵**：行 = 主组（FL/FR/SL/SR/LFE），列 = 支持组（FL/FR/SL/SR/SUB1）；点选单元格后可在下方开关“是否支持”。
- **支持量**滑杆 -24 ~ -1 dB（-24 最强、-1 最弱）；**F-support Low/High** 与 **Fsiso**（50–150 Hz）限制支持频率范围。
- 左侧**衰减预览**：10 个倍频程 20–160 Hz 的 RT60 前/后柱状对比；随矩阵/支持量实时变化。
- **注意**：衰减预览是演示模型（示意），不是实测、不是 Dirac 算法；页面和 README 都有标注。
- **防误操作**：环绕箱支持 LFE 且 F-support Low 低于其实测下潜时，矩阵单元格标红、侧栏警告；点“计算 ART 滤波器”弹出确认清单（连接数 / Fsiso / 风险），确认后才应用。
- 导出 ART 支持配置（JSON / YAML 片段，演示字段形状）。

## 数据来源

- 实测 FR 原始 CSV：
  rt-stack/evidence/board-window-20261118-calib/analysis/board-pilot/B/fr/UMIK/{FL,FR,SL,SR,SUB1}.csv
- 本目录 data/fr-20-200.json 为抽取结果（20–200 Hz、nan 插值、1/6 oct 平滑、含有效下潜检测值）。
- 抽取/构建脚本：src/（index.template.html + demo.css + demo.part1.js + demo.part2.js + build.js）。

## 重新构建

在项目任意位置执行：

    node web/demo/crossover-ui-demo/src/build.js

会读取 ../data/fr-20-200.json，把 CSS/JS/数据内嵌，生成自包含的 index.html。

## 调研依据

交互要点、官方文档引用、视频/评测线索、映射表见：
rt-stack/evidence/crossover-ui-demo/RESEARCH-DIRAC-BC-ART.md

## 已知限制

- 演示页不连设备、不写 channelmap、不参与真实 CamillaDSP/ART 计算。
- 预测曲线/衰减数据为近似或示意，用于交互演示，不作为声学结论。
- 仅验证 Chrome 1600×900。
