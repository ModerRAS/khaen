//! [NONRT] T67 补齐：T66 plan -> rt-host plan-arena 真实应用 seam。
//!
//! 背景（T67 pilot 阻塞）：T66 `compile_apply` 的 Apply 段此前只有
//! `OfflineSyntheticAck`（T66 自己标注的 NONRT 离线模型），"host plan apply" 完全
//! 未接线（pilot REPORT §9）。本模块把 **真实 T66 plan 字节** 交给真实的
//! `tspi-rt-host run --config <plan-arena>`：
//!
//! 1. `write_plan_bundle`：把 `CompiledPlan.plan_bytes` 写为 `plan.bin`，把
//!    `compile-report.json` 写为 manifest（host 用 `plan_sha256` + `host_graph_sha256`
//!    交叉校验），可选写 `plan_last_good.bin`（来自 T66 apply session 的 last-good）；
//! 2. `host_config_text`：生成 `[host] graph="plan-arena"` + `[paths] plan/plan_manifest`
//!    + `[run] max_blocks` 的 rt-host 配置（backend=fake 离线 / backend=alsa 板端）；
//! 3. `run_host_apply`：启动 host，解析其 stdout JSON（status/plan/swap 段），要求
//!    `status=ok`、`plan.source=primary`、`plan.parse=t20-ok`、
//!    `plan.header_crc32c=ok`、`swap.retire_ready=true`、`swap.synthetic=false`、
//!    `swap.acks>=expected_workers`，再把 host 的真实 block all-ack 结果转换为
//!    T66 `ApplySession::note_block_ack` 需要的 `TspiBlockAckV1`（字段来自 host 报告，
//!    **不是** T66 的 synthetic ack helper）；
//! 4. policy：host 启动/校验/ack 任一失败 -> 返回稳定错误码，由 FitWiring 走
//!    T66 rollback（last-good / safety mute），绝不写 applied version。
//!
//! # 诚实边界
//!
//! - 离线（Windows/fake backend）时 host 只跑 fake ALSA：T20 parse/admit/swap/block
//!   ack 与 4 worker 图执行是真实代码路径，但 **没有真实声卡/声音**；证据标
//!   `backend="fake"`、`real_audio=false`、`board_run=NOT_RUN`。
//! - 板端窗口同一命令把 backend 换 `alsa` 即真实 MOTU 路径（本模块不打开设备，设备
//!   owner 由 host 进程持有；应用后复测仍需窗口，本模块不产生 T3 结论）。
//! - 不启动/停止 systemd 服务、不写 CueMix、不伪造 applied_version。

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tspi_dsp_compat::ws::{json_encode, json_object, json_parse, JsonValue};
use tspi_dsp_core::abi::TspiBlockAckV1;

use crate::compile_apply::CompiledPlan;
use crate::job::sha256_hex;

/// host apply 证据 schema。
pub const HOST_APPLY_SCHEMA: &str = "tspi-cal-host-apply/v1";
/// host plan 文件名。
pub const HOST_PLAN_FILE: &str = "plan.bin";
/// host manifest 文件名（T66 compile-report.json 原字节）。
pub const HOST_MANIFEST_FILE: &str = "compile-report.json";
/// last-good plan 文件名。
pub const HOST_LAST_GOOD_FILE: &str = "plan_last_good.bin";
/// rt-host 配置文件名。
pub const HOST_CONFIG_FILE: &str = "rt-host.toml";
/// host 原始报告文件名（非确定性，作为 evidence 原样保留）。
pub const HOST_REPORT_FILE: &str = "host-report.json";

/// host 配置/路径非法。
pub const E_HOST_CONFIG: &str = "E_WIRING_HOST_CONFIG";
/// host 进程启动/退出失败。
pub const E_HOST_RUN: &str = "E_WIRING_HOST_RUN";
/// host 报告缺字段/自校验不符。
pub const E_HOST_REPORT: &str = "E_WIRING_HOST_REPORT";
/// host 报告版本与 T66 pending 版本不符。
pub const E_HOST_ACK: &str = "E_WIRING_HOST_ACK";

/// rt-host plan-arena 应用配置（本 crate 只写文件/起进程，不打开设备）。
#[derive(Clone, Debug, PartialEq)]
pub struct HostApplyConfig {
    /// `tspi-rt-host` 可执行文件路径（默认 env TSPI_RT_HOST_BIN 或 "tspi-rt-host"）。
    pub host_bin: PathBuf,
    /// ALSA backend："fake"（离线）或 "alsa"（板端真实设备；由 host 打开）。
    pub backend: String,
    /// 采样率（必须与 plan 一致；T66 admission 已校验）。
    pub sample_rate: u32,
    /// block 帧数。
    pub block_size: u16,
    /// worker 数（plan expected_workers 同源）。
    pub workers: u8,
    /// host 运行 block 数上限（离线冒烟 8–64；板端窗口取更大值）。
    pub max_blocks: u64,
    /// playback 通道数。
    pub playback_channels: u16,
    /// capture 通道数。
    pub capture_channels: u16,
    /// ALSA period 帧数。
    pub period_frames: u32,
    /// ALSA buffer 帧数。
    pub buffer_frames: u32,
    /// ALSA 设备名字符串（host 传给 backend；fake 不打开）。
    pub device: String,
    /// host RT CPU 集合串（仅 host 计划用；fake 不写内核）。
    pub dsp_cpus: String,
    /// host 运行期安全模式（normal|mute；测试期强制 mute，避免 plan-arena 跑 block 出声）。
    pub safe_mode: String,
    /// host DSP 引擎（auto|direct|partitioned；auto 在 L_F>=2048 时选 partitioned）。
    ///
    /// task-284 起 auto 可直接用于大 L_P plan：partitioned 单执行器真实完成整块后产生
    /// **single-executor all-ack**（retire_ready=true；host 报告 `swap.single_executor=true` /
    /// `swap.executor="partitioned-fft-upols"` 明示口径，不是 4 线程 ack、不是 synthetic）。
    /// direct 走 4-worker all-ack，二者都经同一 T20 `note_block_all_ack` 入口。
    pub engine: String,
    /// plan/配置/报告落盘目录（CLI 传 evidence 输出目录下 host-apply/）。
    pub work_dir: PathBuf,
}

impl Default for HostApplyConfig {
    fn default() -> Self {
        let host_bin = std::env::var("TSPI_RT_HOST_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("tspi-rt-host"));
        HostApplyConfig {
            host_bin,
            backend: "fake".to_string(),
            sample_rate: 48_000,
            block_size: 128,
            workers: 4,
            max_blocks: 8,
            playback_channels: 22,
            capture_channels: 20,
            period_frames: 128,
            buffer_frames: 1024,
            device: "hw:UltraLitemk5,0".to_string(),
            // 默认 A72 全核 4-7（RK3576 CPU4-7）；2-5 混用 A53/A72 实测慢 3.1×（planrt-20261119）。
            dsp_cpus: "4-7".to_string(),
            engine: "auto".to_string(),
            safe_mode: "normal".to_string(),
            work_dir: PathBuf::from("host-apply"),
        }
    }
}

impl HostApplyConfig {
    /// 结构校验（不触碰设备/文件）。
    pub fn validate(&self) -> Result<(), String> {
        if self.host_bin.as_os_str().is_empty() {
            return Err(format!("{E_HOST_CONFIG}: host_bin 为空"));
        }
        if self.backend != "fake" && self.backend != "alsa" {
            return Err(format!(
                "{E_HOST_CONFIG}: backend 只接受 fake|alsa（得到 {:?}）",
                self.backend
            ));
        }
        if self.safe_mode != "normal" && self.safe_mode != "mute" {
            return Err(format!(
                "{E_HOST_CONFIG}: safe_mode 只接受 normal|mute（得到 {:?}）",
                self.safe_mode
            ));
        }
        if self.engine != "auto" && self.engine != "direct" && self.engine != "partitioned" {
            return Err(format!(
                "{E_HOST_CONFIG}: engine 只接受 auto|direct|partitioned（得到 {:?}）",
                self.engine
            ));
        }
        if self.block_size != 64 && self.block_size != 128 {
            return Err(format!(
                "{E_HOST_CONFIG}: block_size 只接受 64/128（得到 {}）",
                self.block_size
            ));
        }
        if !matches!(self.workers, 2 | 4) {
            return Err(format!(
                "{E_HOST_CONFIG}: workers 只接受 2/4（得到 {}）",
                self.workers
            ));
        }
        if self.max_blocks == 0 || self.max_blocks > 10_000_000 {
            return Err(format!(
                "{E_HOST_CONFIG}: max_blocks 越界（{}）",
                self.max_blocks
            ));
        }
        if self.work_dir.as_os_str().is_empty() {
            return Err(format!("{E_HOST_CONFIG}: work_dir 为空"));
        }
        Ok(())
    }

    /// 是否真实声卡 backend（证据字段）。
    pub fn is_real_audio(&self) -> bool {
        self.backend == "alsa"
    }
}

/// 已写盘的 host plan bundle。
#[derive(Clone, Debug, PartialEq)]
pub struct PlanBundle {
    /// 落盘目录。
    pub work_dir: PathBuf,
    /// plan.bin 路径。
    pub plan_path: PathBuf,
    /// compile-report.json 路径（host manifest）。
    pub manifest_path: PathBuf,
    /// last-good plan 路径（无则 None）。
    pub last_good_path: Option<PathBuf>,
    /// rt-host.toml 路径。
    pub config_path: PathBuf,
    /// plan 字节 sha256。
    pub plan_sha256: String,
    /// manifest 字节 sha256。
    pub manifest_sha256: String,
    /// graph_id hex。
    pub graph_id_hex: String,
    /// plan 字节数。
    pub plan_bytes: u64,
}

/// host 一次 plan-arena run 的解析结果（全部字段来自 host stdout JSON）。
#[derive(Clone, Debug, PartialEq)]
pub struct HostApplyOutcome {
    /// host 原始报告 JSON。
    pub report_json: String,
    /// host 报告 sha256（确定性证据绑定）。
    pub report_sha256: String,
    /// plan 来源（primary/last_good）。
    pub plan_source: String,
    /// T20 parse 结果。
    pub plan_parse: String,
    /// header CRC-32C 结果。
    pub header_crc32c: String,
    /// manifest 状态。
    pub manifest_state: String,
    /// 图形态（host adapter 只支持 factorized）。
    pub form: String,
    /// T20 分配/生效版本。
    pub plan_version: u32,
    /// 执行的 block 数。
    pub blocks: u64,
    /// 已 ack 的 worker 数（host swap.acks；host 报告为计数，不是位图）。
    pub acks: u32,
    /// all-ack block 数（host swap.all_ack_blocks）。
    pub all_ack_blocks: u64,
    /// host retire_ready（真实 block ack 汇总）。
    pub retire_ready: bool,
    /// host 报告 synthetic 标记（必须 false）。
    pub synthetic: bool,
    /// retire_ready 来源（必须 note_block_all_ack）。
    pub retire_ready_source: String,
    /// host 报告里的 mode/status。
    pub status: String,
}

impl HostApplyOutcome {
    /// 构造 T66 `ApplySession::note_block_ack` 输入：字段全部取 host 真实 block ack 结果。
    ///
    /// 说明：host 的 swap 段由每个真实完成 block 的 `completed_block_ack` 汇总；
    /// 本函数只做 ABI 结构映射（三 phase 位图 = host acks 位图），不生成/放大任何 ack。
    pub fn all_ack_block(&self, expected_workers: u8) -> Result<TspiBlockAckV1, String> {
        if !self.retire_ready || self.synthetic {
            return Err(format!(
                "{E_HOST_ACK}: host 未给出真实 all-ack（retire_ready={} synthetic={}）",
                self.retire_ready, self.synthetic
            ));
        }
        let mask = if expected_workers >= 4 {
            0xFu32
        } else {
            (1u32 << expected_workers) - 1
        };
        // host swap.acks 是"已 ack 的 worker 数"（不是位图）：必须覆盖 expected_workers。
        if self.acks < u32::from(expected_workers) {
            return Err(format!(
                "{E_HOST_ACK}: host acks={} < expected_workers={expected_workers}（worker 未全部用上新 plan）",
                self.acks
            ));
        }
        let block =
            TspiBlockAckV1::new(self.all_ack_blocks, 0, self.plan_version, expected_workers);
        for phase in 0..block.phase_done.len() {
            block.phase_done[phase].store(mask, std::sync::atomic::Ordering::Release);
        }
        Ok(block)
    }

    /// 确定性证据 JSON（job artifact 用；不含 host 报告全文，只含其 sha256）。
    pub fn evidence_json(&self) -> String {
        let document = json_object(vec![
            ("schema", JsonValue::String(HOST_APPLY_SCHEMA.to_string())),
            ("host_status", JsonValue::String(self.status.clone())),
            (
                "plan_source",
                JsonValue::String(self.plan_source.clone()),
            ),
            ("plan_parse", JsonValue::String(self.plan_parse.clone())),
            (
                "header_crc32c",
                JsonValue::String(self.header_crc32c.clone()),
            ),
            (
                "manifest_state",
                JsonValue::String(self.manifest_state.clone()),
            ),
            ("form", JsonValue::String(self.form.clone())),
            ("plan_version", JsonValue::UInt(self.plan_version as u64)),
            ("blocks", JsonValue::UInt(self.blocks)),
            ("acks", JsonValue::UInt(self.acks as u64)),
            ("all_ack_blocks", JsonValue::UInt(self.all_ack_blocks)),
            ("retire_ready", JsonValue::Bool(self.retire_ready)),
            ("synthetic", JsonValue::Bool(self.synthetic)),
            (
                "retire_ready_source",
                JsonValue::String(self.retire_ready_source.clone()),
            ),
            ("host_report_sha256", JsonValue::String(self.report_sha256.clone())),
            (
                "note",
                JsonValue::String(
                    "block all-ack 来自真实 tspi-rt-host plan-arena 运行（per-block completed_block_ack）；
abi 映射为 TspiBlockAckV1 后喂 T66 ApplySession。backend=fake 时无真实声卡/声音；backend=alsa 属板端窗口。"
                        .to_string(),
                ),
            ),
        ]);
        match json_encode(&document) {
            Ok(text) => text,
            Err(error) => {
                format!("{{\"schema\":\"{HOST_APPLY_SCHEMA}\",\"error\":\"json encode: {error}\"}}")
            }
        }
    }
}

/// 写 host plan bundle（plan.bin + compile-report.json manifest + 可选 last-good + rt-host.toml）。
pub fn write_plan_bundle(
    plan: &CompiledPlan,
    config: &HostApplyConfig,
    last_good_plan_bytes: Option<&[u8]>,
) -> Result<PlanBundle, String> {
    config.validate()?;
    fs::create_dir_all(&config.work_dir).map_err(|error| {
        format!(
            "{E_HOST_CONFIG}: create work_dir {}: {error}",
            config.work_dir.display()
        )
    })?;
    let plan_path = config.work_dir.join(HOST_PLAN_FILE);
    let manifest_path = config.work_dir.join(HOST_MANIFEST_FILE);
    let config_path = config.work_dir.join(HOST_CONFIG_FILE);
    let report_bytes = plan.report_json().into_bytes();
    fs::write(&plan_path, &plan.plan_bytes)
        .map_err(|error| format!("write plan {}: {error}", plan_path.display()))?;
    fs::write(&manifest_path, &report_bytes)
        .map_err(|error| format!("write manifest {}: {error}", manifest_path.display()))?;
    let last_good_path = match last_good_plan_bytes {
        Some(bytes) => {
            let path = config.work_dir.join(HOST_LAST_GOOD_FILE);
            fs::write(&path, bytes)
                .map_err(|error| format!("write last-good {}: {error}", path.display()))?;
            Some(path)
        }
        None => None,
    };
    let bundle = PlanBundle {
        work_dir: config.work_dir.clone(),
        plan_path,
        manifest_path,
        last_good_path,
        config_path,
        plan_sha256: sha256_hex(&plan.plan_bytes),
        manifest_sha256: sha256_hex(&report_bytes),
        graph_id_hex: plan.graph_id_hex(),
        plan_bytes: plan.plan_bytes.len() as u64,
    };
    let config_text = host_config_text(&bundle, config);
    fs::write(&bundle.config_path, config_text).map_err(|error| {
        format!(
            "write host config {}: {error}",
            bundle.config_path.display()
        )
    })?;
    Ok(bundle)
}

/// 生成 rt-host plan-arena 配置文本（自定义 TOML 子集解析；路径按原样写）。
pub fn host_config_text(bundle: &PlanBundle, config: &HostApplyConfig) -> String {
    let path_text = |path: &Path| path.to_string_lossy().into_owned();
    let mut text = String::with_capacity(1024);
    text.push_str("[host]\n");
    let _ = writeln!(text, "block_size = {}", config.block_size);
    let _ = writeln!(text, "sample_rate = {}", config.sample_rate);
    let _ = writeln!(text, "workers = {}", config.workers);
    text.push_str("graph = \"plan-arena\"\n");
    let _ = writeln!(text, "safe_mode = \"{}\"", config.safe_mode);
    text.push_str("[alsa]\n");
    let _ = writeln!(text, "backend = \"{}\"", config.backend);
    let _ = writeln!(text, "device = \"{}\"", config.device);
    let _ = writeln!(text, "capture_channels = {}", config.capture_channels);
    let _ = writeln!(text, "playback_channels = {}", config.playback_channels);
    let _ = writeln!(text, "period_frames = {}", config.period_frames);
    let _ = writeln!(text, "buffer_frames = {}", config.buffer_frames);
    if config.engine != "auto" {
        text.push_str("[dsp]\n");
        let _ = writeln!(text, "engine = \"{}\"", config.engine);
    }
    text.push_str("[rt]\n");
    let _ = writeln!(text, "dsp_cpus = \"{}\"", config.dsp_cpus);
    text.push_str("tick_freq_hz = 1000000000\n");
    text.push_str("[web]\n");
    text.push_str("bind = \"127.0.0.1:1\"\n");
    text.push_str("[paths]\n");
    let _ = writeln!(text, "plan = \"{}\"", path_text(&bundle.plan_path));
    let _ = writeln!(
        text,
        "plan_manifest = \"{}\"",
        path_text(&bundle.manifest_path)
    );
    if let Some(last_good) = bundle.last_good_path.as_ref() {
        let _ = writeln!(text, "plan_last_good = \"{}\"", path_text(last_good));
    }
    text.push_str("[run]\n");
    let _ = writeln!(text, "max_blocks = {}", config.max_blocks);
    text
}

/// 运行 host plan-arena 并解析/校验其报告。
pub fn run_host_apply(
    bundle: &PlanBundle,
    config: &HostApplyConfig,
) -> Result<HostApplyOutcome, String> {
    let output = Command::new(&config.host_bin)
        .arg("run")
        .arg("--config")
        .arg(&bundle.config_path)
        .output()
        .map_err(|error| {
            format!(
                "{E_HOST_RUN}: 启动 {} 失败：{error}（离线需 --host-bin 指向已构建的 tspi-rt-host；板端为 /opt/tspi-rt-stack/current/bin/tspi-rt-host）",
                config.host_bin.display()
            )
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!(
            "{E_HOST_RUN}: host 退出码 {:?}；stderr 尾部：{}",
            output.status.code(),
            tail(&stderr, 800)
        ));
    }
    fs::write(config.work_dir.join(HOST_REPORT_FILE), stdout.as_bytes())
        .map_err(|error| format!("write host-report.json: {error}"))?;
    parse_host_report(&stdout, bundle, config)
}

/// 解析并校验 host plan-arena JSON 报告（纯函数；单测覆盖）。
pub fn parse_host_report(
    stdout: &str,
    bundle: &PlanBundle,
    config: &HostApplyConfig,
) -> Result<HostApplyOutcome, String> {
    let line = stdout
        .lines()
        .filter(|line| line.trim_start().starts_with('{'))
        .last()
        .ok_or_else(|| format!("{E_HOST_REPORT}: host stdout 无 JSON 报告"))?;
    let document = json_parse(line).map_err(|error| {
        format!(
            "{E_HOST_REPORT}: JSON 解析失败（offset {}）：{error}",
            error.offset
        )
    })?;
    let text_field = |value: Option<&JsonValue>, what: &str| -> Result<String, String> {
        value
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .ok_or_else(|| format!("{E_HOST_REPORT}: 缺字段 {what}"))
    };
    let uint_field = |value: Option<&JsonValue>, what: &str| -> Result<u64, String> {
        value
            .and_then(|value| value.as_u64())
            .ok_or_else(|| format!("{E_HOST_REPORT}: 缺字段 {what}"))
    };
    let status = text_field(document.get("status"), "status")?;
    if status != "ok" {
        return Err(format!("{E_HOST_REPORT}: host status={status:?} 非 ok"));
    }
    let graph = text_field(document.get("graph"), "graph")?;
    if graph != "plan-arena" {
        return Err(format!(
            "{E_HOST_REPORT}: host graph={graph:?} 非 plan-arena（应用路径未走 plan-arena）"
        ));
    }
    let plan = document
        .get("plan")
        .ok_or_else(|| format!("{E_HOST_REPORT}: 缺 plan 段"))?;
    let swap = document
        .get("swap")
        .ok_or_else(|| format!("{E_HOST_REPORT}: 缺 swap 段"))?;
    let plan_source = text_field(plan.get("source"), "plan.source")?;
    if plan_source != "primary" {
        return Err(format!(
            "{E_HOST_REPORT}: host 加载的是 {plan_source:?} 而不是本次 primary plan（last-good 回退不得当 applied）"
        ));
    }
    let plan_parse = text_field(plan.get("parse"), "plan.parse")?;
    let header_crc32c = text_field(plan.get("header_crc32c"), "plan.header_crc32c")?;
    let manifest_state = text_field(plan.get("manifest"), "plan.manifest")?;
    let form = text_field(plan.get("form"), "plan.form")?;
    let plan_version = uint_field(plan.get("version"), "plan.version")? as u32;
    let plan_sha = text_field(plan.get("sha256"), "plan.sha256")?;
    if !plan_sha.eq_ignore_ascii_case(&bundle.plan_sha256) {
        return Err(format!(
            "{E_HOST_REPORT}: host 加载 plan sha256={plan_sha} 与写出字节 {} 不一致",
            bundle.plan_sha256
        ));
    }
    let graph_id = text_field(plan.get("graph_id"), "plan.graph_id")?;
    if !graph_id.eq_ignore_ascii_case(&bundle.graph_id_hex) {
        return Err(format!(
            "{E_HOST_REPORT}: host graph_id={graph_id} 与 T66 graph_id {} 不一致",
            bundle.graph_id_hex
        ));
    }
    let blocks = uint_field(document.get("blocks"), "blocks")?;
    let acks = uint_field(swap.get("acks"), "swap.acks")? as u32;
    let all_ack_blocks = uint_field(swap.get("all_ack_blocks"), "swap.all_ack_blocks")?;
    let retire_ready = swap
        .get("retire_ready")
        .and_then(|value| value.as_bool())
        .ok_or_else(|| format!("{E_HOST_REPORT}: 缺字段 swap.retire_ready"))?;
    let synthetic = swap
        .get("synthetic")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let retire_ready_source = swap
        .get("retire_ready_source")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    if !retire_ready {
        return Err(format!(
            "{E_HOST_REPORT}: host retire_ready=false（plan 未在真实 block 上完成 all-ack）"
        ));
    }
    if synthetic {
        return Err(format!(
            "{E_HOST_REPORT}: host swap.synthetic=true（合成 ack 不得作为 apply 证据）"
        ));
    }
    if retire_ready_source != "note_block_all_ack" {
        return Err(format!(
            "{E_HOST_REPORT}: retire_ready_source={retire_ready_source:?} 非 note_block_all_ack"
        ));
    }
    if header_crc32c != "ok" || plan_parse != "t20-ok" {
        return Err(format!(
            "{E_HOST_REPORT}: plan 校验未过（parse={plan_parse} crc32c={header_crc32c}）"
        ));
    }
    if form != "factorized" {
        return Err(format!(
            "{E_HOST_REPORT}: host adapter form={form:?}（当前只接受 factorized）"
        ));
    }
    let outcome = HostApplyOutcome {
        report_json: stdout.to_string(),
        report_sha256: sha256_hex(stdout.as_bytes()),
        plan_source,
        plan_parse,
        header_crc32c,
        manifest_state,
        form,
        plan_version,
        blocks,
        acks,
        all_ack_blocks,
        retire_ready,
        synthetic,
        retire_ready_source,
        status,
    };
    // 端到端自校验：host 版本必须与 T66 registry 预期一致（由调用方再核对 pending）。
    if outcome.plan_version == 0 {
        return Err(format!("{E_HOST_REPORT}: host plan.version=0 非法"));
    }
    let _ = config;
    Ok(outcome)
}

fn tail(text: &str, limit: usize) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    if chars.len() > limit {
        chars = chars[chars.len() - limit..].to_vec();
    }
    chars.into_iter().collect()
}

#[cfg(test)]
mod host_apply_tests {
    use super::*;

    fn bundle() -> PlanBundle {
        PlanBundle {
            work_dir: PathBuf::from("work"),
            plan_path: PathBuf::from("work/plan.bin"),
            manifest_path: PathBuf::from("work/compile-report.json"),
            last_good_path: None,
            config_path: PathBuf::from("work/rt-host.toml"),
            plan_sha256: "aa".repeat(32),
            manifest_sha256: "bb".repeat(32),
            graph_id_hex: "cc".repeat(32),
            plan_bytes: 716,
        }
    }

    const GOOD_REPORT: &str = concat!(
        r#"{"status":"ok","mode":"tspi-rt-host-run","graph":"plan-arena","blocks":8,"#,
        r#""plan":{"source":"primary","path":"work/plan.bin","bytes":716,"sha256":"AAAA","#,
        r#""graph_id":"CCCC","version":1,"parse":"t20-ok","header_crc32c":"ok","#,
        r#""manifest":"verified","form":"factorized","admission":"mock"},"#,
        r#""swap":{"init_status":0,"acks":4,"switches":1,"retire_ready":true,"#,
        r#""valid":true,"synthetic":false,"retire_ready_source":"note_block_all_ack","#,
        r#""all_ack_blocks":8,"plan_version":1}}"#
    );

    fn good_report() -> String {
        GOOD_REPORT
            .replace("AAAA", &"aa".repeat(32))
            .replace("CCCC", &"cc".repeat(32))
    }

    #[test]
    fn t224_host_config_text_is_plan_arena_with_manifest() {
        let config = HostApplyConfig {
            host_bin: PathBuf::from("tspi-rt-host"),
            backend: "fake".to_string(),
            work_dir: PathBuf::from("work"),
            ..HostApplyConfig::default()
        };
        let text = host_config_text(&bundle(), &config);
        for needle in [
            r#"graph = "plan-arena""#,
            "[paths]",
            r#"plan = "work/plan.bin""#,
            r#"plan_manifest = "work/compile-report.json""#,
            "[run]",
            r#"backend = "fake""#,
        ] {
            assert!(text.contains(needle), "missing {needle:?} in {text}");
        }
        assert!(!text.contains("plan_last_good"), "{text}");
    }

    #[test]
    fn t224_host_config_rejects_bad_backend_and_block() {
        let mut config = HostApplyConfig::default();
        config.backend = "jack".to_string();
        assert!(config.validate().unwrap_err().contains(E_HOST_CONFIG));
        config.backend = "fake".to_string();
        config.block_size = 256;
        assert!(config.validate().unwrap_err().contains(E_HOST_CONFIG));
        config.block_size = 128;
        config.workers = 3;
        assert!(config.validate().unwrap_err().contains(E_HOST_CONFIG));
        config.workers = 4;
        config.max_blocks = 0;
        assert!(config.validate().unwrap_err().contains(E_HOST_CONFIG));
        config.max_blocks = 8;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn t224_parse_host_report_requires_real_all_ack() {
        let config = HostApplyConfig {
            backend: "fake".to_string(),
            ..HostApplyConfig::default()
        };
        let good = good_report();
        let outcome = parse_host_report(&good, &bundle(), &config).expect("good host report");
        assert_eq!(outcome.plan_version, 1);
        assert_eq!(outcome.blocks, 8);
        assert_eq!(outcome.acks, 4);
        assert!(outcome.retire_ready);
        assert!(!outcome.synthetic);
        let block = outcome.all_ack_block(4).expect("block");
        assert_eq!(block.plan_version, 1);
        assert_eq!(block.expected_workers, 4);
        for phase in 0..block.phase_done.len() {
            assert_eq!(
                block.phase_done[phase].load(std::sync::atomic::Ordering::Acquire),
                0xF
            );
        }
        assert_eq!(block.block_seq, 8);
    }

    #[test]
    fn t224_parse_host_report_rejects_synthetic_or_last_good_or_version0() {
        let config = HostApplyConfig::default();
        let good = good_report();
        let synthetic = good.replace(r#""synthetic":false"#, r#""synthetic":true"#);
        assert!(parse_host_report(&synthetic, &bundle(), &config)
            .unwrap_err()
            .contains("synthetic"));
        let fallback = good.replace(r#""source":"primary""#, r#""source":"last_good""#);
        assert!(parse_host_report(&fallback, &bundle(), &config)
            .unwrap_err()
            .contains("last_good"));
        let no_ready = good.replace(r#""retire_ready":true"#, r#""retire_ready":false"#);
        assert!(parse_host_report(&no_ready, &bundle(), &config)
            .unwrap_err()
            .contains("retire_ready"));
        let wrong_sha = good.replace(&"aa".repeat(32), &"dd".repeat(32));
        assert!(parse_host_report(&wrong_sha, &bundle(), &config)
            .unwrap_err()
            .contains("sha256"));
        let version0 = good.replace(r#""version":1"#, r#""version":0"#);
        assert!(parse_host_report(&version0, &bundle(), &config)
            .unwrap_err()
            .contains("version=0"));
    }

    #[test]
    fn t224_parse_host_report_rejects_parse_failure() {
        assert!(
            parse_host_report("no json here", &bundle(), &HostApplyConfig::default())
                .unwrap_err()
                .contains(E_HOST_REPORT)
        );
        assert!(parse_host_report(
            r#"{"status":"error","graph":"plan-arena"}"#,
            &bundle(),
            &HostApplyConfig::default()
        )
        .unwrap_err()
        .contains("status"));
    }

    #[test]
    fn t224_outcome_evidence_json_is_deterministic_and_honest() {
        let config = HostApplyConfig::default();
        let outcome = parse_host_report(&good_report(), &bundle(), &config).unwrap();
        let text = outcome.evidence_json();
        assert!(text.contains(HOST_APPLY_SCHEMA), "{text}");
        assert!(text.contains(r#""retire_ready":true"#), "{text}");
        assert!(text.contains(r#""synthetic":false"#), "{text}");
        assert!(text.contains("note_block_all_ack"), "{text}");
        assert_eq!(text, outcome.evidence_json());
    }
}
