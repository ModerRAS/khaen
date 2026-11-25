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

### A.4 采样率：plan ABI 接受集合与运行时能力

- **容器层不限制速率**：`sample_rate` 是 u32 字段。`Plan::decode` / `Plan::encode_checked` 只校验
  格式（magic / 版本 / CRC / 总长 / 字段域），不按采样率拒绝——`crates/core/tests/plan_v3.rs` 的
  `empty_plan_roundtrips` 用 44100 Hz 做往返，证明 ABI 容器能表达任意 u32 速率。
- **运行时接受集合（唯一可执行集合）**：`khaen_core::rt::SAMPLE_RATES_HZ = [48_000, 96_000]`。
  "plan ABI v3 接受的采样率集合"指的就是这个可执行集合：host 引擎、ALSA 输出后端、AES67 流，
  以及校准的 sweep / positions / phase-fit 共用该常量。
- **host 的 fail-closed 行为**：`PlanGraph::compile` 要求 `plan.sample_rate ==
  EngineConfig.sample_rate`，不一致返回 `CompileError::SampleRateMismatch`（不静默重采样）；
  注意 `CompileError::UnsupportedRates` 指的是 **matrix 抽取/插值因子**（变比 ASRC 或超过因子上限），
  **不是**采样率拒绝码。`EngineConfig::validate` 与 host 准入只要求 `sample_rate > 0`。
- **组装器与校准入口的 fail-closed**：`khaen-plan-builder` 的 `PlanConfig::validate` 用
  `SAMPLE_RATES_HZ` 校验，44.1 kHz 返回 `UnsupportedSampleRate`；校准 sweep / positions /
  phase-fit 也用该常量拒绝非 48k/96k。
- **设计域候选 ≠ 可执行集合**：`khaen-modal-design::target::SAMPLE_RATES_HZ =
  [44100, 48000, 88200, 96000]` 与 `khaen-designer::modal_peq::contract_sample_rates()`（同 4 个
  速率）是**设计目标候选集合**（设计域口径），不是 plan 可执行集合；这些速率上的设计产物进入
  本运行时前必须转换或拒绝。`khaen-calibration::job` 的 `44100 | 48000 | 88200 | 96000` 同样
  是设计/校准域口径，不是运行时集合。
- **已知缺口（如实声明，未修）**：host 的准入与 `PlanGraph::compile` 目前没有直接引用
  `SAMPLE_RATES_HZ` 做成员校验（只校验非零 + 方案与运行配置一致）。把 `EngineConfig.sample_rate`
  配成 44.1 kHz 时，host 层不会以"速率不在运行时集合"为由拒绝；当前兜底在 plan-builder 与校准
  入口。后续二选一：host 编译期补成员校验，或设备实测确认后把 `rt::SAMPLE_RATES_HZ` 扩到 4 个
  速率；两者长期并存就是口径漂移。

## B. 主机控制协议

**传输**：Unix socket，一行 JSON 请求 / 一行 JSON 响应；UTF-8、无 BOM、单行。

### B.1 op 列表

| op | 请求字段 | 响应要点 |
| --- | --- | --- |
| status | — | 快照：runtime_state、graph_id、epoch、blocks、xruns、underruns、faults、silent、mute、safe_mode、actual_version、wired[]、not_wired[] |
| mute | mute(bool) | 回执：applied_block、mute（生效后 actual）、idempotent |
| prepare | plan_path、plan_sha256 | 准入结果：engine_token、planned_version、admission{} |
| commit | engine_token | 块边界生效：applied_block、version、all_ack |
| abort | engine_token | 丢弃草稿，无副作用 |
| coeff | — | 当前系数版本与 last-good |

### B.1.1 字段形状

- `wired`：已实现 op 的名字数组，例如 `["status","mute"]`。
- `not_wired`：对象数组 `{"op": "...", "reason": "..."}`，reason 不得为空。
- `reasons`：`status` 的附加字段；凡是当前没有数据来源的字段（值为 null）必须在这里给出原因，禁止编造数值。

### B.2 语义

- `status` 为原子快照，不阻塞音频线程。
- `mute` 块边界生效；同值重复调用返回同一 `applied_block` 且 `idempotent=true`。
- `prepare` 只做校验，不改音频；失败返回稳定错误码。
- `commit` 在下一块边界原子切换；超时未完成返回 `not_applied`，不得假报成功。
- 未知 op 返回 `unknown_op`；字段缺失返回 `bad_request`。
- 能力上报必须真实：未实现的 op 必须出现在 `not_wired` 里。

### B.3 错误码

`bad_request` / `unknown_op` / `unsupported` / `not_applied` / `runtime_not_running` / `admission_rejected` / `engine_token_invalid`。

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
