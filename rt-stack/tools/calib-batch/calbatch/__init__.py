"""KHAEN 校准批量闭环工具包（task-201）。

模块：
  config   冻结配置加载/fail-closed 校验
  dsp      扫频/反卷积/FR/EDC/RT/瀑布/Biquad/SVG（numpy）
  measure  capture meta 布局与 IR 提取、质量门
  model    离线合成 plant 与 fake apply（仅自测，不是真实算法）
  plan     窗口前冻结计划（命令/时间预算/RUN-PLAN.md）
  runner   计划执行器（fake/cmd transport；板端只跑采集/应用）
  analysis 前后对比/瀑布/RT/MIMO/holdout/问题定位/报告（观察量，不是验收门）
  selftest 离线端到端自测
"""

__version__ = "1.0.0"
TOOL_NAME = "khaen-cal-batch"
