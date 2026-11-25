# WebUI 对接说明（2.0 上混）

结论先行：**WebUI 零改代码即可用（方案 A）**。上混以普通 channelmap v2 profile 交付，现有
「多用户 profile 加载 + 校验 + 应用」流程直接支持；参数也能用现有「每角色校准」UI 微调。
本文件同时给出可选专用卡片（方案 B）的最小接口契约，供 web-manager 决策。

已验证（板端 OEM v5，未改任何 /etc active）：

- `GET /api/channelmap` 返回 12 个 `upmix-*` profile（output 布局 4.1.2/5.3.2 正确）。
- 沙箱内 `tspi-channelmap load-profile upmix-4.1.2-music --mode off` +
  `camilladsp --check` 通过（"Config is valid"）。
- 全程未启动/重启音频服务、未打开声卡；模式 off 下 apply 只渲染+校验+持久化。

## 1. 前置条件

1. 板端 `/usr/local/bin/tspi-channelmap` 含 upmix 修复（sha256 91e85e18…；OEM v5 已含）。
   旧渲染器会丢 `sources[].inverted`、错误合并 Filter step（**bug A**，所有 per-role 滤波都受影响）、
   BM 主声道静音，**不要用旧镜像测试完整版**。`*-safe` 变体只去掉 `sources[].inverted`，
   **不能绕过 bug A**（仍会错误合并滤波）；旧镜像只能用独立 CamillaDSP 配置路径
   （`tspi-upmix apply`，README 方案 B）或先打 `upmix/patches/` 的渲染器补丁。
2. 部署 `upmix-*.yml` 到 `/etc/tspi-audio/profiles/`（deploy/install-upmix.sh 完成）。
3. `/etc/tspi-audio/layout-presets.yml` 含 4.1.2/5.3.2 布局（OEM v5 已含；4.1.2 的 MOTU 映射：
   HFL/HFR=5-6、SUB1=8（1-based））。

## 2. 方案 A：零改代码（推荐）

WebUI 现有页面与接口：

| 操作 | 现有接口 | 说明 |
| --- | --- | --- |
| 列出 profile | GET /api/channelmap（profiles 字段） | 安装后自动出现 upmix-* |
| 加载 profile | POST /api/channelmap/profiles/load {name} | 覆盖 active（旧 active 备份），不自动 apply |
| 校验 | GET /api/channelmap / POST apply 前置 | 现有页面按钮 |
| 应用 | POST /api/channelmap/apply | 模式 off：只渲染；模式 dante/aes67：重启 CamillaDSP |
| 回滚 | POST /api/channelmap/rollback | last-good |
| 关闭上混 | 同上（加载 upmix-off-restore 或用户原 profile） | 首次安装脚本会快照 active |

用户操作：声道分配页 → profile 列表选 `upmix-4.1.2-music` → 加载 → 校验 → 应用。

### 参数 ↔ UI 字段（现有「每角色校准」可直接改）

| 想调什么 | 改哪里 |
| --- | --- |
| 环绕/高度更响/更轻 | roles[SL/SR].gain_db、roles[HFL/HFR].gain_db |
| 环绕延迟 | roles[SL/SR].delay_ms（profile 内已是 15/18 ms）；高度不再有延迟（用户裁决 2026-11-16） |
| 左右扩散（去相关） | roles[SR].delay_ms 与 SR 的差值、roles[HFR].delay_ms 同理 |
| 中置提取量（5.3.2） | roles[C].gain_db；带限改 highpass/lowpass |
| 差信号提取量 | roles[SL/SR].sources[].gain_db（FL/FR 各 -6.02） |
| 高度带限 | 默认不做（全频）；如后续要求再显式加 roles[HFL/HFR].highpass/lowpass |
| 低音分频/电平 | roles[FL/FR].highpass、roles[SUBn].lowpass、roles[SUBn].gain_db |
| 多低音对齐 | roles[SUB2/SUB3].delay_ms、gain_db |

注意：低音管理当前是**简化版**，不是完整矩阵式 BM：SUB 的 sources 固定为 FL/FR 的 mid，
只把 FL/FR 的中低频搬进 SUB；C 的 80–200 Hz、SL/SR 的 80–100 Hz 在 profile 里被丢弃，
`redirect_from: auto` 只做校验、**不生效**。真正的矩阵式 BM/redirect 已列为后续项（C-20）。
不要再用 `bass_management` 面板（它会把 SUB sources 清空并走渲染器 BM 路径；该路径当前有缺陷）。

### 良性警告

加载/校验时会出现 `WARN remap: 输出角色 X 无路由，将静音` —— 这是校验器没把显式
`roles[].sources` 计入 remap 统计导致的误报，不影响渲染与声音（可忽略）。

## 3. 方案 B（可选）：专用「2.0 上混」卡片

若 web-manager 想加一个更直观的入口，**不需要新 schema、不需要新渲染器**，最小方案：

前端：

1. 从 GET /api/channelmap 的 `profiles` 里筛 `name.startswith("upmix-")`，按 output_layout 分组。
2. 「启用」按钮：POST /api/channelmap/profiles/load {name} → POST /api/channelmap/apply。
3. 「关闭」按钮：加载 `upmix-off-restore`（存在时）或用户上一个 profile。
4. 参数表单（可选）：读 profile YAML 中 roles 的 gain_db/delay_ms/highpass/lowpass，
   编辑后走现有 profile 保存/应用接口。

建议后端（可选，纯转发，无状态）：

| 接口 | 请求 | 行为 |
| --- | --- | --- |
| GET /api/upmix | - | 返回 upmix-* profile 列表 + 当前 active 是否 upmix（读 profile.name 前缀）+ 预设参数视图 |
| POST /api/upmix/apply | {preset} | 等价 load-profile(preset) + apply（复用现有备份/回滚语义） |
| POST /api/upmix/off | - | load-profile(upmix-off-restore) + apply（不存在则 409 + 提示） |

参数视图可直接来自 `tspi-upmix params <preset>`（JSON），或从 profile 文件读取。
「预设 → profile 名」映射固定：`upmix-<layout>-<style>`，例如 `upmix-4.1.2-music`；
容器输入的 stereo 变体 profile.name 带 `-h16` 后缀（如 `upmix-4.1.2-music-h16`），避免与 2ch 同名冲突；
跨布局直通 profile.name 为 `passthrough-<输入布局>-to-<输出布局>`，不是同布局直通。
**不要**让 WebUI 自己生成 DSP 配置；生成器是唯一来源，避免两套逻辑漂移。

## 4. 与现有功能的边界

- 不改 channelmap v2 schema（全部字段已存在），不改 `tspi-channelmap` 校验规则——**唯一例外**是
  2026-11-16 裁决新增的 `peq[].type`（peaking/low_shelf/high_shelf，未知 type rc=2），详见
  `docs/PEQ-TYPE.md`；Web 本地回退必须与 Python 规则一致，CLI 不可用时禁止用本地规则替代。
- 不改 CAMILLA 模板 `/etc/camilladsp/dante.yml|aes67.yml`；上混 profile 走 channelmap 的
  渲染→apply 流程（与现有布局 profile 相同）。
- apply 时的服务重启语义完全复用现有点击行为（模式 off 不启动；模式 active 才重启 CamillaDSP）。
- 若后续要 2ch 输入源切换（AES67 2ch / Dante 2ch / USB），只需在 profile 的 input.channels=2 与
  设备选择上做（现有 probe/设备逻辑），本工具不新增设备管理。

## 5. 若暂时不能升级渲染器

`*-safe` 只去掉 `sources[].inverted`，**不能绕过渲染器 bug A**（Filter step 错误合并，影响所有
per-role 滤波），所以旧镜像上它也不是“可用”的完整替代。旧镜像的正确路径是**独立 CamillaDSP
配置**（README 方案 B，`tspi-upmix apply`），该路径不依赖 channelmap 渲染器；或先打 `upmix/patches/`
的渲染器补丁再装 profile。


## 6. 输入内容模式（task-19；Lead 2026-11-15 裁决）

接口（web-manager 实现，纯转发/读写文件，不改 channelmap v2 schema）：

| 接口 | 行为 |
| --- | --- |
| GET /api/channelmap/input-mode | 读 `/run/tspi/input-content.json`（daemon 未跑返回 detected=unknown） |
| PUT /api/channelmap/input-mode {"mode": ...} | 写 `/etc/tspi-audio/input-mode.json`（接受 force-stereo/force-multichannel 别名，返回新状态） |

运行态字段（schema `tspi-input-content/v1`，字段名已裁决，不再改）：
`content_mode`（effective auto|stereo|multichannel）、`detected`（unknown|stereo|multichannel）、
`override`（null|auto|stereo|multichannel）、`confidence`、`active_channels`、`rms_db`、
`threshold_db`、`hold_ms`、`reason`、`h16_internal_upmix`（bool|null）、`container_layout`、
`detector`、`updated_unix`、`schema`。

页面建议：badge 显示“自动/上混/直通”+ 检测到的内容；下拉覆盖“自动 / 强制 2.0 / 强制多声道”；
`h16_internal_upmix=true`（或 detected=multichannel）时提示“H16 已内部上混，板端直通，未二次上混”。
CLI 等价：`tspi-upmix input-mode get|set`；离线测试：`tspi-upmix detect --wav ...`。

