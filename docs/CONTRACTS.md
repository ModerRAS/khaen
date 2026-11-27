# khaen v2 契约

三份契约：方案文件 ABI v3、主机控制协议、校准产物 manifest。字段名一旦发布不再改动，只能追加。

## A. 方案文件 ABI v3

**用途**：host 启动或切换时加载的一次性产物；解析后全部常驻内存，运行期不改。

### A.1 文件布局

| 区段 | 内容 |
| --- | --- |
| Header | magic `KHAENPLN`、abi_version=3、block_size、sample_rate、stage_count、section_table_offset、total_bytes |
| Section Table | 每项：kind(u16)、offset(u64)、bytes(u64)、crc32c(u32) |
| Sections | 按 kind 排列，见下 |
| Trailer | 全文件 crc32c、graph_id(32 字节) |

### A.2 区段（kind）

| kind | 名称 | 内容 |
| --- | --- | --- |
| 1 | content | 内容布局映射：输入通道数、输出 lane 数、折叠增益表 |
| 2 | crossover | 每 lane 的滤波器组（类型、taps、系数） |
| 3 | matrix | 满矩阵：行列数、抽取/插值因子、系数（按行优先） |
| 4 | protection | 每 lane：延迟（样本）、极性、增益、PEQ 段、保护滤波器 |
| 5 | routing | lane → DAC 通道映射，空通道填静音 |

区段在文件中按 kind 升序（1 content → 2 crossover → 3 matrix → 4 protection → 5 routing）排列，
每个 kind 至多一次；这是规范形式，见 A.3。

### A.3 规则

- 解析必须校验每段 crc32c 与总长；任一不符拒绝加载（fail-closed）。
- `graph_id = sha256(domain="khaen-plan/v3" || 规范化序列化)`，同内容必同 id。
- 未知 kind：拒绝加载，不做跳过。
- 系数一律 f32 小端；延迟一律整数样本。
- **编码同样 fail-closed**：`Plan::encode_checked()` 对 ABI 字段域放不下的方案返回 `PlanError`
  （folds / filters / entries / PEQ / routes 计数超 u16、分频抽头或矩阵系数超解码器上限、
  编码后总长超 u32 `total_bytes`），绝不静默截断成"encode 得出来、decode 不回去"的文件；
  `Plan::encode()` 是它的 panic 包装（已由 `Plan::decode` 或 plan-builder 校验过的方案不会触发）。
  字段域之外的区段语义（矩阵系数个数、lane/DAC 引用）仍归准入与组装器校验，本层不重复。
- **规范形式（canonical form）**：`Plan::encode_checked()` 的输出是唯一合法字节串，`Plan::decode()`
  只接受它。规范形式 = ① 区段表按 kind 严格升序（1→5）、每个 kind 至多一次；② payload 从 header
  之后连续排布：第一个 payload 的 offset = 32，后续每个 payload 紧跟上一个（无重叠、无间隙、
  无重复引用），区段表紧跟最后一个 payload（`table_offset = 32 + Σ payload bytes`）。
  违反任一条都 fail-closed 拒绝，错误码依次为 `DuplicateSection` / `SectionOrder` /
  `SectionLayout` / `SectionTableOffset`，不静默重排或去重。
  **推论**：任何被接受的字节串都满足 `encode(decode(data)) == data`，同一语义的方案只有一种
  字节串，graph_id 才真正是内容寻址。
- **无保留字段 / 填充字节**：v3 的 header（32 B）、表项（22 B）、trailer（36 B）每个字节都有定义，
  没有保留位、保留字节或对齐填充；因此不存在"同一语义两串字节"的填充歧义，decode 也不做保留
  字节校验（没有可校验的保留字节）。将来若新增保留字段，规则是 encoder 写 0、decode 校验为 0。
- **解码不 panic**：任何恶意字节都必须走 `Result` / `PlanError` 返回，不得 panic（fail-closed 的
  "拒绝"是错误码，不是控制面线程崩溃）。长度 / 维度乘法一律用 checked 运算：例如 matrix 的
  `rows * cols * taps` 溢出返回 `BadPayload("matrix coefficient count overflow")`，
  偏移加长度用 checked 加法，表项长度先做上界检查再分配。

### A.4 采样率：plan ABI 接受集合与运行时能力

- **容器层不限制速率**：`sample_rate` 是 u32 字段。`Plan::decode` / `Plan::encode_checked` 只校验
  格式（magic / 版本 / CRC / 总长 / 字段域），不按采样率拒绝——`crates/core/tests/plan_v3.rs` 的
  `empty_plan_roundtrips` 用 44100 Hz 做往返，证明 ABI 容器能表达任意 u32 速率。
- **运行时接受集合（唯一可执行集合）**：`khaen_core::rt::SAMPLE_RATES_HZ = [48_000, 96_000]`。
  "plan ABI v3 接受的采样率集合"指的就是这个可执行集合：host 引擎、ALSA 输出后端、AES67 流，
  以及校准的 sweep / positions / phase-fit 共用该常量。
- **host 的 fail-closed 行为（三层，均以 `SAMPLE_RATES_HZ` 为准）**：① `EngineConfig::validate`
  拒绝不在集合里的速率（`EngineError::BadConfig`）；daemon / rt-host 用 `KHAEN_SAMPLE_RATE` 构造
  `EngineConfig` 后都经该收口；② 准入 `admit()` 对 `plan.sample_rate` 不在集合里给拒绝码
  `sample_rate`；③ `PlanGraph::compile` 要求 `plan.sample_rate == EngineConfig.sample_rate`，
  不一致返回 `CompileError::SampleRateMismatch`（不静默重采样）。注意
  `CompileError::UnsupportedRates` 指的是 **matrix 抽取/插值因子**（变比 ASRC 或超过因子上限），
  **不是**采样率拒绝码。
- **组装器与校准入口的 fail-closed**：`khaen-plan-builder` 的 `PlanConfig::validate` 用
  `SAMPLE_RATES_HZ` 校验，44.1 kHz 返回 `UnsupportedSampleRate`；校准 sweep / positions /
  phase-fit 也用该常量拒绝非 48k/96k。
- **设计域候选 ≠ 可执行集合**：`khaen-modal-design::target::SAMPLE_RATES_HZ =
  [44100, 48000, 88200, 96000]` 与 `khaen-designer::modal_peq::contract_sample_rates()`（同 4 个
  速率）是**设计目标候选集合**（设计域口径），不是 plan 可执行集合；这些速率上的设计产物进入
  本运行时前必须转换或拒绝。`khaen-calibration::job` 的 `44100 | 48000 | 88200 | 96000` 同样
  是设计/校准域口径，不是运行时集合。
- **单一事实来源**：运行时速率集合只由 `khaen_core::rt::SAMPLE_RATES_HZ` 决定。要把
  44.1 kHz / 88.2 kHz 纳入可执行集合，必须先改这一处并完成设备实测，不能在各 crate 自行放宽
  （modal-design / designer / calibration job 的设计域常量不参与运行时判定）。

## B. 主机控制协议

**传输**：Unix socket，一行 JSON 请求 / 一行 JSON 响应；UTF-8、无 BOM、单行。

### B.1 op 列表

| op | 请求字段 | 响应要点 |
| --- | --- | --- |
| status | — | 快照：runtime_state、graph_id、epoch、blocks、xruns、underruns、faults、silent、mute、safe_mode、actual_version、mode、readback{}、format{}、surfaces{}、surface_reasons{}、wired[]、not_wired[] |
| mute | mute(bool) | 回执：applied_block、mute（生效后 actual）、idempotent |
| prepare | plan_path、plan_sha256 | 准入结果：engine_token、planned_version、admission{} |
| commit | engine_token | 块边界生效：applied_block、version、all_ack |
| abort | engine_token | 丢弃草稿，无副作用 |
| coeff | — | 当前系数版本与 last-good |
| inspect | plan_path、可选 plan_sha256 | 只读试运行：读文件→校验 sha→解码→准入→图编译；返回 sha256/graph_id/plan 摘要/admission（accepted+rejections+budget）/compile（compiled+latency 或 error_code）；`staged` 恒 false，不产生草稿、不改变正在跑的图 |
| surface | surface、data | 语义界面两阶段：准备补丁草稿（engine_token、planned_version、graph_id、admission）；生效仍走 commit |
| rollback | 可选 target(版本号) | 回滚第一阶段：把历史里的旧版本重新准备成草稿（engine_token、planned_version、**rolled_back_to**、graph_id、admission）；版本号**递增**、历史不改写；生效仍走 commit；无历史返回 no_history(409) |
| format | 可选 sample_rate(u32)、chunksize(u16) | 格式变更第一阶段：校验目标格式（须在 `SAMPLE_RATES_HZ` / `BLOCK_SIZES` 内）→ 草稿（engine_token、planned_version、format{}、**changed**、applies_to="control-plane engine"、device_restart_required=true）；commit 重建执行器并**丢弃正在跑的图**（方案嵌格式），生效仍走 commit、abort 可丢弃 |

### B.1.1 字段形状

- `wired`：已实现 op 的名字数组，例如 `["status","mute"]`。
- `not_wired`：对象数组 `{"op": "...", "reason": "..."}`，reason 不得为空。
- `reasons`：`status` 的附加字段；凡是当前没有数据来源的字段（值为 null）必须在这里给出原因，禁止编造数值。
- `readback`：`status` 的加法字段，控制面 `base_plan`（启动时加载或最近一次 commit）的只读回读（`available/source:"base_plan"/sample_rate/block_size/content{in_channels,out_lanes,folds}/crossover{lanes,filters[{lane,taps}]}/matrix{rows,cols,decimation,interpolation,taps}/protection{lanes,entries[{lane,delay_samples,polarity,gain_db,peq_bands}]}/routing{dac_channels,routes[{lane,dac}]}`）。只给控制面语义，不复制 FIR/矩阵系数；没有已加载方案时 `available:false` + `reason`，不返回默认布局。

### B.1.2 语义界面（surface）

`surface` 是五个界面的两阶段入口：先准备一份“在上一次 commit 生效方案上打补丁”的草稿，返回
`engine_token`；`commit` 仍按块边界原子生效，`abort` 丢弃。没有已生效方案时返回
`no_base_plan`（409），不允许对着空基线打补丁（防止界面把“未见过的方案”当成当前值去改）。

| surface | data 字段 | 说明 |
| --- | --- | --- |
| volume | `gains_db: [{lane, gain_db}]` | 逐 lane 保护增益（dB）。lane 必须已存在于方案的 protection 段；非有限值拒绝。 |
| routing | `routes: [[lane, dac], ...]`，可选 `dac_channels` | 替换 lane→DAC 路由；dac 必须 < dac_channels（缺省沿用方案值）。 |
| layout | `in_channels`、`out_lanes`、`folds: [[src, dst, gain], ...]` | 替换内容布局折叠表；越界/空表拒绝。 |
| format | `sample_rate`、`chunksize`（至少一项） | **已接通**（委托给 B.1.4 的执行器配置两阶段）：不要求已生效方案；`commit` 重建控制面引擎并丢弃正在跑的图，`device_restart_required=true` 如实上报（真实声卡必须重开 PCM 流）。 |
| mode | `mode: "off" \| "on" \| "aes67"` | **已接通**（见 B.1.5）：两阶段输入使能；`off` = 块边界静音整链，`on`/`aes67` = 解除静音（当前唯一实现的输入源是 aes67）；`toslink`/`dante` 等未实现的源显式 `bad_request`，不假装可选。 |

`status` 里 `surfaces` 逐项上报 `true/false`，未接通的在 `surface_reasons` 给原因；禁止把未接通
界面报成可用。补丁只搬运字段，ABI 字段域由 `encode_checked`、语义由 host 准入与图编译各自校验，
不在 surface 层重复实现契约。

### B.1.3 回滚（rollback）

控制面保留**有限历史**（最近 16 个已生效方案，按版本升序；超出丢弃最旧）。`rollback` 是两阶段
第一阶段，只准备草稿：

- `target` 缺省 = 上一个已生效版本（历史倒数第二项）；给出版本号则必须存在于历史里。
- 回滚是**前进式提交**：版本号取"下一个生效版本"，历史不被改写、不被删除；响应里
  `rolled_back_to` 指明草稿内容来自哪个旧版本。`coeff` 的 `available_versions` 列出可回滚版本。
- 目标等于当前运行版本 → `bad_request`（不允许"成功但什么也没做"）；只有一个版本时缺省回滚 →
  `bad_request`（没有更旧版本）；目标不在历史 → `bad_request` 并列出可用版本。
- 没有任何已生效方案 → `no_history`(409)。
- 与 prepare/surface 相同：读文件之外全部在控制线程完成，草稿只由 `commit` 在下一块边界生效，
  `abort` 丢弃；回滚方案会重新过准入与图编译，不复用旧处理器对象。

### B.1.4 格式变更（format）

格式（采样率 / 块长）是**执行器配置**，不是方案补丁；方案 ABI v3 里也嵌了 `block_size` / `sample_rate`，因此格式变化会让当前图不再合法。

- 请求字段：`sample_rate`（必须在 `khaen_core::rt::SAMPLE_RATES_HZ` 内）、`chunksize`（必须在 `BLOCK_SIZES` 内），至少给一项；两项都可以给。通道数不能通过此 op 改变。
- 第一阶段（`format`）：只校验并准备草稿，返回 `engine_token` / `planned_version` / `format{}` / `changed`；**不改变正在跑的引擎**。同 token 重复请求幂等。若当前已有方案草稿，会被丢弃（草稿按旧格式编译，禁止混用）。
- 第二阶段（`commit`）：若目标格式与当前一致 → `changed=false`、`applied_block=null`，不重建；若不同 → 用新配置重建 `SlotEngine`（保留用户静音状态），**丢弃正在跑的图**（`graph_dropped=true`）、清空可回滚历史（`history_cleared`），`applied_block=1` 表示新执行器从第 1 块开始。之后必须 prepare 一份与新格式一致的方案才能恢复图。
- `abort` 可丢弃格式草稿（回执 `aborted_surface="format"`）；错 token 不销毁草稿。
- **设备边界**：本控制面不碰设备；响应里的 `device_restart_required=true` 是给上层的明确信号（真实播放进程需要重开 PCM 流）。不把控制面重建声称为设备已换格式。

### B.1.5 模式（mode）

模式是**输入使能**，不是方案补丁；真实采集源切换属于数据面/设备监督层（跨进程控制通道已由 T70f-b/T70f-c 以 UNIX socket 落地；采集源切换 T70g 未实现）。

- 支持值：`off`（整链静音）、`on` / `aes67`（解除静音；当前唯一实现的输入源固定为 aes67）。其它值 `bad_request`，消息里写明 "not wired"，不返回假成功。
- 两阶段与其它界面一致：`surface=mode` 只准备草稿（`engine_token` / `planned_version` / `mode` / `changed` / `applies_to="control-plane engine (output enable)"`）；`commit` 才在下一块边界调用引擎静音（`off`）或解除静音（`aes67`），响应带 `applied_block`。同值请求 `changed=false`，commit 为空操作。
- `abort` 可丢弃模式草稿（`aborted_surface="mode"`）；错 token 不销毁草稿；与方案/格式草稿互斥。
- `status.mode` 是**rt-host 控制协议 status 的顶层字段**（运行时模式，如 `playback`/`aes67`）；daemon `GET /api/status` 现已**加法投影**该字段（仅 socket 主路径有真值；file/process/链路不可用 → null + `reasons.mode`，不编造）。`host_link.mode` 仍是控制通道传输模式（socket/file/none），不要混用。`status.surfaces.mode=true` 表示该入口可用，不代表采集源切换已实现。

### B.2 语义

- `status` 为原子快照，不阻塞音频线程。
- `mute` 块边界生效；同值重复调用返回同一 `applied_block` 且 `idempotent=true`。
- `prepare` 只做校验，不改音频；失败返回稳定错误码。
- `commit` 在下一块边界原子切换；超时未完成返回 `not_applied`，不得假报成功。
- 未知 op 返回 `unknown_op`；字段缺失返回 `bad_request`。
- 能力上报必须真实：未实现的 op 必须出现在 `not_wired` 里。

### B.3 错误码

`bad_request` / `unknown_op` / `unsupported` / `not_applied` / `runtime_not_running` / `admission_rejected` / `engine_token_invalid` / `no_base_plan` / `no_history`。

完整能力列表随 `status.wired` 上报：`status / mute / prepare / commit / abort / coeff / surface / inspect / rollback`。

## C. 校准产物 manifest

**用途**：一次校准运行的完整可复现快照；同输入同参数必同 sha。

### C.1 目录

    run-<params_sha256 前16位>/
      params.json     输入与参数快照（键排序、2 空格缩进）
      report.json     指标与三态结果（FULL/PARTIAL/NONE + 残差）
      plan.bin        方案文件（如该路径产出）
      manifest.json   索引与校验

### C.2 manifest 字段

| 字段 | 含义 |
| --- | --- |
| schema | `khaen-cal-artifact/v1` |
| params_sha256 | params.json 的 sha256 |
| files[] | 每个文件：path（相对）、bytes、sha256、role |
| result | 自洽描述：params_sha256 + 产物内容散列 |
| result_sha256 | `sha256(canonical_json(result))` |
| plan | present、format、runtime_abi、sha256（缺失时给原因） |

### C.3 规则

- 禁止写入时间戳、主机名、绝对路径、PID、随机数。
- `verify` 重算并逐文件比对，失败点名文件与期望/实际值。
- 三态必须出现在 report.json；PARTIAL 必须带残差，不允许以“顶满”冒充成功。
- **浮点规范化**：canonical JSON 里的浮点数先量化到 10 位有效数字再输出（见 `khaen_mimo_design::report::CANONICAL_SIG_DIGITS`）。理由：Windows 与 Linux 的 libm 末位可差 1 ulp，直接输出最短表示会让同一输入产出不同 sha；量化后中文文本与普通数值跨平台稳定，10 位远超 dB/Hz/延迟的物理精度需求。
- **跨平台边界（如实声明）**：迭代求解器（MIMO/模态拟合）的收敛步数依赖平台 libm 的末位比较，Windows 与 Linux 的解可能在第 7~10 位有效数字上不同。因此"同输入同参数必同 sha"**在同一平台内成立**；跨平台比较不承诺 byte-identical。交付/CI 平台是 Linux，冻结摘要按平台记录（测试里 `LINUX_REFERENCE` / `WINDOWS_REFERENCE` 两张表）。

## D. 单点闭环复测产物

**用途**：把「方案 + 基线录音 + 施加方案后的录音」变成可审计、可复现的判定证据包。三个 schema 都是确定性 JSON（固定键序、10 位有效数字科学计数、末尾换行）；清单里禁止时间戳/主机名/绝对路径。

### D.1 电平集合（`khaen-closed-loop-levels/v1`）

| 字段 | 含义 |
| --- | --- |
| schema | 固定 `khaen-closed-loop-levels/v1` |
| sample_rate_hz | 采样率（Hz），同一份对账的两边必须一致 |
| points[] | name（测点名，集合内唯一）+ bands[] |
| bands[] | center_hz（1/3 oct 中心，升序）+ level_db（可空 = 缺数据） |

### D.2 判定报告（`khaen-closed-loop-report/v1`）

| 字段 | 含义 |
| --- | --- |
| verdict | `PASS` / `FAIL` / `INCOMPLETE` |
| tolerance_db / min_compared_bands | 判定配置（默认 2.0 dB / 3 带） |
| compared_bands / missing_bands / violations | 合计可比较 / 缺数据 / 超容差带数 |
| max_abs_delta_db | 最大 |实测−预测|，无数据时 null |
| points[] | 逐测点：compared/missing/max_abs_delta + bands[] |
| bands[] | center_hz、predicted_db、observed_db、delta_db、within_tolerance（可空） |
| notes[] | 固定说明文本（缺数据不算通过、INCOMPLETE 不是通过等） |

### D.3 运行包清单（`khaen-closed-loop-run-manifest/v1`）

| 字段 | 含义 |
| --- | --- |
| tool / schema | `khaen-closed-loop-run` / 固定 schema |
| verdict / point / lane | 判定、测点名、方案 lane |
| sample_rate_hz / tolerance_db / min_compared_bands / window_len | 分析配置 |
| compared_bands / missing_bands / violations / max_abs_delta_db | 判定摘要 |
| centers_hz[] | 本次使用的 1/3 oct 中心 |
| inputs | plan_sha256、baseline_wav_sha256、applied_wav_sha256 |
| outputs | predicted_json_sha256、observed_json_sha256、report_json_sha256 |

### D.4 规则

- 输出目录必须为空或不存在；四份文件固定命名 `predicted.json` / `observed.json` / `report.json` / `manifest.json`。
- 同输入重跑四份文件逐字节相同（清单无时间戳）；清单 sha256 可作一次复测的身份。
- 任一侧带功率为 0 记 null（missing）；`min_compared_bands` 不足判 INCOMPLETE，绝不把缺数据当通过。
- 预测覆盖方案保护段（PEQ + 每 lane 增益）与 crossover 的每 lane FIR（幅度，不含群延迟）；方案含非恒等的内容/矩阵/路由段时预测阶段直接报 `E_PLAN_LEVELS_PLAN`。
- 方案来源两条：`khaen-scene-plan`（mimo 报告选定变体的 FIR → crossover FIR 方案；`khaen-fir-plan` 为手动兜底）或既有 `plan_compile`（PEQ 设计 → 保护段方案）；`khaen-plan-levels` 对两者用同一套预测口径。
- 命令行退出码：0 PASS / 3 FAIL / 4 INCOMPLETE / 2 输入或方案错误 / 1 写输出失败。
