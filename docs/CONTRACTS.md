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
