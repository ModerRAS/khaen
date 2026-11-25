//! [IO] 格式转换（core-v1 §11.9 / §9.2 / §12.3）—— WBS T22 写域。
//!
//! 设备侧 interleaved 原始字节 <-> core 侧 planar f32 的唯一转换点。本模块属 I/O 区
//! （core-v1 §5/§9.2：I/O 区允许且预算必要 syscall 的层；format 自身不做 syscall），
//! 不得进入 DSP 调用图。实现约束：无堆分配、无锁、无 syscall、无文件/网络/日志、
//! 无主动 panic 路径（全部索引在进入转换循环前按长度/stride 封闭校验）；固定切片 + 定长循环。
//!
//! # T22 冻结语义（边界参考向量 = tests/T22/format_vectors.rs，测试名 t22_*）
//!
//! ## 1. 设备格式（v1 枚举 core-v1 §11.9；F32_PLANAR/F64/未知值一律 ERR_UNSUPPORTED）
//!
//! | sample_format | 值 | 字节 | 端序与符号 | capture 归一化（code -> f32） | playback 量化（f32 -> code） |
//! |---|---:|---:|---|---|---|
//! | S24_3LE_INTERLEAVED | 1 | 3 | little-endian，24-bit 二补码 | code / 2^23（精确 2 的幂缩放） | round-ties-even，饱和到 [-2^23, 2^23-1] |
//! | S32_LE_INTERLEAVED | 2 | 4 | little-endian，32-bit 二补码（全 32-bit 量程） | code / 2^31 | round-ties-even，饱和到 [-2^31, 2^31-1] |
//! | S16_LE_INTERLEAVED | 3 | 2 | little-endian，16-bit 二补码 | code / 2^15 | round-ties-even，饱和到 [-2^15, 2^15-1] |
//! | F32_INTERLEAVED | 4 | 4 | little-endian IEEE-754 binary32 | 直接取位（不做缩放） | 直接写位（先 sanitize/clip） |
//!
//! S32_LE 语义是「32-bit 全量程」，不是「24-bit 左对齐装在 32-bit 容器」：本栈实测设备
//! （MOTU UltraLite-mk5）协商出的格式是 S24_3LE（3 字节 packed），不存在 24-in-32 兼容需求；
//! 不做隐式 24-in-32 解释（core-v1 F-42：float<->int 语义必须显式声明）。
//!
//! ## 2. 归一化与量化（F-42：语义必须声明，禁止未声明截断/取整/饱和）
//!
//! - capture：x = code / 2^(bits-1)。除数是 2 的幂，f32 缩放精确；负满量程
//!   -2^(bits-1) 映射到 -1.0，正满量程 2^(bits-1)-1 映射到 1 - 2^-(bits-1)。
//! - playback：v = x * 2^(bits-1) + dither（乘 2 的幂精确），按 IEEE round-to-nearest,
//!   ties-to-even 取整（Rust f32::round_ties_even，IEEE-754 定义，跨 ISA 一致），再饱和到
//!   该格式整数范围。x = +1.0 映射到正满码 2^(bits-1)-1（不算 clip）；x = -1.0 映射到负满码
//!   -2^(bits-1)。
//! - 整数 -> f32 的转换（S32 大码值超出 f32 24-bit 有效位时）按 IEEE round-to-nearest,
//!   ties-to-even 舍入：例如 S32 code 0x7FFF_FF01 -> 2147483392（最近的 128 倍数）。
//!
//! ## 3. finite 与 clip（core-v1 §9.2 RT_E_INPUT_NONFINITE=209 / RT_E_INPUT_CLIP=210）
//!
//! - 方向无关：输入样本为 NaN/±Inf -> 替换为 0.0 并计入 FormatStats.nonfinite_in
//!   （§9.2 209 的固定动作是 sanitize 为 0，不是 fault，不返回错误码）。
//! - 输入幅度 |x| > 1.0 -> 饱和到 ±1.0 并计入 FormatStats.clipped（§9.2 210 的格式层语义；
//!   同样计数、继续，不返回错误码）。仅 F32 设备格式/核心输出可能越界；整数设备解码不可能
//!   超过 ±1.0（除以 2^(bits-1) 后最大为 1 - 2^-(bits-1)）。
//! - 输出恒为有限值：写出前防御性检查，若仍出现非有限则写 0 并计入 nonfinite_out
//!   （设计上恒为 0；>0 即不变量破坏，由上层置 fault）。
//! - FormatConfig.hw.flags bit2（capture_sanitize）/bit3（output_clip_meter）只记录请求；
//!   sanitize 与 clip 计数恒执行（fail-safe，不因 bit 关闭而放行非有限/越界值）。
//!
//! ## 4. dither（F-43：确定性、显式种子、默认 off；v1 仅 Off/TPDF）
//!
//! - 只作用于 playback 的整数格式（S16_LE/S24_3LE/S32_LE）。capture 与 F32_INTERLEAVED
//!   输出不施加 dither；F32 设备格式 + dither 配置视为配置错误（ERR_DEVICE_PARAMS）。
//! - 幅度：标准 TPDF，d = u1 - u2，u1/u2 为独立 uniform [0,1)（24-bit 网格），故 d 是
//!   (-1, 1) LSB 上的三角分布（峰峰 2 LSB），便于守住量化噪声的一阶/二阶矩。
//! - 施加点：v = x * 2^(bits-1) + d 之后统一 round-ties-even（dither 在取整之前）。
//! - 伪随机源：PCG32（标准 LCG 乘子 6364136223846793005 / 增量 1442695040888963407，
//!   XSH-RR 输出），种子先经 SplitMix64 混合后作为内部状态；种子 = hw.reserved[0]
//!   （§11.9：非 RT 配置）。同种子 + 同输入 + 同调用序 -> 逐位一致（F-43）。
//! - 消耗顺序：设备样本写出顺序（帧主序、通道次序），每样本 2 个 uniform。
//! - 状态：DitherState 供连续流使用（跨 period 连续，新 epoch/新流时由调用方重建）。
//!   convert_playback 无状态入口每次调用以同一种子重建状态（= 每个 period 重复同一噪声
//!   序列，仅用于参考向量/离线单块工具；连续播放必须用 convert_playback_stateful）。
//!
//! ## 5. stride 与对齐（core-v1 §11.9；T17 已冻结 actual 参数口径）
//!
//! - 设备侧（interleaved）：一个 period 的交错采样连续排放，样本步进 = bytes/sample；
//!   hw.in_stride_bytes / hw.out_stride_bytes = 一个 period 的交错字节跨度
//!   （T17 actual 实测 = period × 通道 × 字节：capture 20ch/128 帧/S24 = 7680，
//!   playback 22ch/128 帧/S24 = 8448）。转换只读该 period 的前
//!   period × 通道 × 字节 字节；stride 校验为 >= 该下界 且为 bytes 整数倍，尾部 padding 不写
//!   （playback 保持调用方内容）。
//! - core 侧（planar f32）：stride_samples 必须是 16 的倍数且 >= period_frames（§11.9），
//!   通道基址 ch × stride × 4 字节；配合 64 B 对齐基址 -> 每通道 64 B 对齐。转换入口校验
//!   stride 与切片长度；基址 64 B 对齐属缓冲区注册/plan 期契约，由 validate_planar_buffer()
//!   在 T26 分配后校验一次（转换入口不重复检查指针，避免同一缓冲区每 period 重复判定；
//!   T22 决策 D-2，见 evidence/T22/MANIFEST.md）。stride > period 的尾通道填充不读不写。
//!
//! ## 6. 拒绝码（core-v1 §9.2，非 RT 返回）
//!
//! ERR_ABI_VERSION(101) / ERR_RATE(104) / ERR_BLOCK_SIZE(105) / ERR_CHANNELS(106) /
//! ERR_STRIDE_ALIGN(109) / ERR_DEVICE_PARAMS(118) / ERR_UNSUPPORTED(122)。
//! 209/210 是「计数并继续」的 RT 级事件，不通过 Result 返回。
//!
//! ## 7. 明确非目标（不属于 T22）
//!
//! 重采样/ASRC、声道映射与 role scatter（T25/T26）、通道路由/静音、S24-in-32、大端格式、
//! 平面设备格式（ALSA v1 固定 RW_INTERLEAVED，T17）、噪声整形（v1 只有 TPDF）、
//! 设备打开/恢复（T17）、SPSC 发布（T16）、文件 WAV 解析（T14）。
//!
//! 源码级自证：无 allocation/syscall/锁 token（测试
//! t22_module_has_no_allocation_or_syscall_tokens）。

pub use khaen_core::abi::TspiHwParamsV1;
pub use khaen_core::rt::RtStatus;

use khaen_core::abi::{sample_format, ABI_VERSION};
use khaen_core::rt::{BLOCK_SIZES, MAX_INPUT_CHANNELS, MAX_OUTPUT_CHANNELS, SAMPLE_RATES_HZ};

// ---------------------------------------------------------------------------
// 常量：hw.flags（core-v1 §11.9 bit0..bit3）与对齐/步长契约
// ---------------------------------------------------------------------------

/// hw.flags bit0：dither 开启。
pub const HW_FLAG_DITHER_ENABLED: u32 = 1 << 0;
/// hw.flags bit1：dither 类型 = TPDF（bit0 必须同时置位；v1 只定义 TPDF）。
pub const HW_FLAG_DITHER_TPDF: u32 = 1 << 1;
/// hw.flags bit2：请求 capture sanitize（语义恒执行，见模块文档 §3）。
pub const HW_FLAG_CAPTURE_SANITIZE: u32 = 1 << 2;
/// hw.flags bit3：请求输出 clip 计数（语义恒执行，见模块文档 §3）。
pub const HW_FLAG_OUTPUT_CLIP_METER: u32 = 1 << 3;
/// v1 已知 flags 位集（其余位必须为 0，否则 ERR_DEVICE_PARAMS）。
pub const HW_FLAGS_KNOWN: u32 = HW_FLAG_DITHER_ENABLED
    | HW_FLAG_DITHER_TPDF
    | HW_FLAG_CAPTURE_SANITIZE
    | HW_FLAG_OUTPUT_CLIP_METER;

/// hw.reserved 中 dither 种子的下标（core-v1 §11.9：reserved[0]=dither_seed）。
pub const DITHER_SEED_RESERVED_INDEX: usize = 0;
/// hw.reserved[1..4] 必须为 0（本模块校验）。
pub const HW_RESERVED_ZERO_FROM: usize = 1;

/// core 侧 planar f32 的通道 stride 必须是该样本数的整数倍（core-v1 §11.9）。
pub const PLANAR_STRIDE_MULTIPLE: usize = 16;
/// core 侧每通道基址 64 B 对齐（core-v1 §11.9）。
pub const PLANAR_ALIGN_BYTES: usize = 64;

/// PCG32 LCG 乘子（标准 PCG）。
const PCG32_MULTIPLIER: u64 = 6_364_136_223_846_793_005;
/// PCG32 LCG 增量（标准 PCG；固定主流程）。
const PCG32_INCREMENT: u64 = 1_442_695_040_888_963_407;
/// uniform [0,1) 的 2^-24 缩放（24-bit 网格精确可表示）。
const UNIFORM_SCALE: f32 = 1.0 / 16_777_216.0;

// ---------------------------------------------------------------------------
// 配置/统计（T06 scaffold 冻结签名；本任务实现语义）
// ---------------------------------------------------------------------------

/// dither 配置（repr(u8) POD；默认 off；种子在 HwParams.reserved[0]）。
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DitherCfg {
    /// 关闭（默认）。
    #[default]
    Off = 0,
    /// TPDF dither（v1 唯一定义的抖动类型）。
    Tpdf = 1,
}

/// 格式转换配置（I/O 区持有；不得进入 DSP 调用图）。
///
/// hw 必须是 T17 校验通过的 actual hwparams；dither 必须与 hw.flags bit0/bit1 一致
/// （不一致 = ERR_DEVICE_PARAMS，避免两处配置漂移）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FormatConfig {
    /// 设备实际 hwparams（negotiated actual）。
    pub hw: TspiHwParamsV1,
    /// dither 配置。
    pub dither: DitherCfg,
}

/// 格式转换统计（非 RT 导出；RT 内只累加固定计数）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FormatStats {
    /// 输入 nonfinite 样本数（已 sanitize）。
    pub nonfinite_in: u64,
    /// 输出 clip 样本数（输入幅度 |x| > 1.0 的样本数）。
    pub clipped: u64,
    /// 输出 nonfinite 样本数（应为 0；>0 即不变量破坏）。
    pub nonfinite_out: u64,
}

impl FormatStats {
    /// 累加另一组统计（跨 period/跨方向汇总；饱和不溢出）。
    pub fn merge(&mut self, other: FormatStats) {
        self.nonfinite_in = self.nonfinite_in.saturating_add(other.nonfinite_in);
        self.clipped = self.clipped.saturating_add(other.clipped);
        self.nonfinite_out = self.nonfinite_out.saturating_add(other.nonfinite_out);
    }

    /// 是否无 sanitize/clip/nonfinite 事件（便于状态上报）。
    pub const fn is_clean(&self) -> bool {
        self.nonfinite_in == 0 && self.clipped == 0 && self.nonfinite_out == 0
    }
}

/// 单 period 的设备布局（validate() 的结果；[IO] 内可持有，Copy 无 Drop）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeriodLayout {
    /// 设备 sample_format（core-v1 §11.9 枚举值）。
    pub sample_format: u16,
    /// 每样本设备字节数（S24=3 / S32=4 / S16=2 / F32=4）。
    pub bytes_per_sample: usize,
    /// period 帧数（= core B；64/128）。
    pub period_frames: usize,
    /// 设备 capture 通道数。
    pub n_in: usize,
    /// 设备 playback 通道数。
    pub n_out: usize,
    /// 一个 capture period 的最少交错字节数 = period_frames × n_in × bytes。
    pub in_min_bytes: usize,
    /// 一个 playback period 的最少交错字节数 = period_frames × n_out × bytes。
    pub out_min_bytes: usize,
    /// hw.in_stride_bytes（校验后；>= in_min_bytes）。
    pub in_span_bytes: usize,
    /// hw.out_stride_bytes（校验后；>= out_min_bytes）。
    pub out_span_bytes: usize,
}

// ---------------------------------------------------------------------------
// 校验
// ---------------------------------------------------------------------------

/// 设备格式 -> 每样本字节数；非设备格式（F32_PLANAR/F64/未知）返回 None。
pub const fn device_sample_bytes(format: u16) -> Option<usize> {
    match format {
        sample_format::S24_3LE_INTERLEAVED => Some(3),
        sample_format::S32_LE_INTERLEAVED => Some(4),
        sample_format::S16_LE_INTERLEAVED => Some(2),
        sample_format::F32_INTERLEAVED => Some(4),
        _ => None,
    }
}

/// 是否 core-v1 §3.2 runtime 允许的 period（64/128）。
pub fn is_runtime_period(period_frames: u32) -> bool {
    BLOCK_SIZES
        .iter()
        .any(|block| *block as u32 == period_frames)
}

/// 是否 core-v1 §3.1 runtime 允许的采样率（48k/96k）。
pub fn is_runtime_rate(sample_rate: u32) -> bool {
    SAMPLE_RATES_HZ.iter().any(|rate| *rate == sample_rate)
}

/// 校验冻结 hwparams 并解出 period 布局（非 RT；每 period 调一次即可）。
///
/// 校验顺序与错误码：abi_version(101) -> sample_format(122) -> 未知 flags/reserved(118) ->
/// rate(104) -> period(105) -> buffer(118) -> 通道(106) -> stride(109)。
pub fn validate(cfg: &FormatConfig) -> Result<PeriodLayout, RtStatus> {
    let hw = &cfg.hw;
    if hw.abi_version != ABI_VERSION {
        return Err(RtStatus::ERR_ABI_VERSION);
    }
    let bytes = match device_sample_bytes(hw.sample_format) {
        Some(bytes) => bytes,
        None => return Err(RtStatus::ERR_UNSUPPORTED),
    };
    if hw.flags & !HW_FLAGS_KNOWN != 0 {
        return Err(RtStatus::ERR_DEVICE_PARAMS);
    }
    if hw.reserved[HW_RESERVED_ZERO_FROM] != 0
        || hw.reserved[HW_RESERVED_ZERO_FROM + 1] != 0
        || hw.reserved[HW_RESERVED_ZERO_FROM + 2] != 0
    {
        return Err(RtStatus::ERR_DEVICE_PARAMS);
    }
    if !is_runtime_rate(hw.sample_rate) {
        return Err(RtStatus::ERR_RATE);
    }
    if !is_runtime_period(hw.period_frames) {
        return Err(RtStatus::ERR_BLOCK_SIZE);
    }
    if hw.buffer_frames < hw.period_frames || hw.buffer_frames % hw.period_frames != 0 {
        return Err(RtStatus::ERR_DEVICE_PARAMS);
    }
    let n_in = hw.n_in as usize;
    let n_out = hw.n_out as usize;
    if n_in == 0 || n_in > MAX_INPUT_CHANNELS || n_out == 0 || n_out > MAX_OUTPUT_CHANNELS {
        return Err(RtStatus::ERR_CHANNELS);
    }
    let period = hw.period_frames as usize;
    let in_min = period
        .checked_mul(n_in)
        .and_then(|value| value.checked_mul(bytes))
        .ok_or(RtStatus::ERR_STRIDE_ALIGN)?;
    let out_min = period
        .checked_mul(n_out)
        .and_then(|value| value.checked_mul(bytes))
        .ok_or(RtStatus::ERR_STRIDE_ALIGN)?;
    let in_span = hw.in_stride_bytes as usize;
    let out_span = hw.out_stride_bytes as usize;
    if in_span < in_min || in_span % bytes != 0 {
        return Err(RtStatus::ERR_STRIDE_ALIGN);
    }
    if out_span < out_min || out_span % bytes != 0 {
        return Err(RtStatus::ERR_STRIDE_ALIGN);
    }
    Ok(PeriodLayout {
        sample_format: hw.sample_format,
        bytes_per_sample: bytes,
        period_frames: period,
        n_in,
        n_out,
        in_min_bytes: in_min,
        out_min_bytes: out_min,
        in_span_bytes: in_span,
        out_span_bytes: out_span,
    })
}

/// 解析 playback 生效的 dither 配置（要求 cfg.dither 与 hw.flags bit0/bit1 一致）。
///
/// - flags bit0=0,bit1=0 -> Off；bit0=1,bit1=1 -> Tpdf；其余组合 = ERR_DEVICE_PARAMS；
/// - cfg.dither 必须等于 flags 推导值，否则 ERR_DEVICE_PARAMS；
/// - F32_INTERLEAVED + dither = ERR_DEVICE_PARAMS（f32 无量化，dither 无定义）。
pub fn effective_dither(cfg: &FormatConfig) -> Result<DitherCfg, RtStatus> {
    let enabled = cfg.hw.flags & HW_FLAG_DITHER_ENABLED != 0;
    let tpdf = cfg.hw.flags & HW_FLAG_DITHER_TPDF != 0;
    let from_flags = match (enabled, tpdf) {
        (false, false) => DitherCfg::Off,
        (true, true) => DitherCfg::Tpdf,
        _ => return Err(RtStatus::ERR_DEVICE_PARAMS),
    };
    if from_flags != cfg.dither {
        return Err(RtStatus::ERR_DEVICE_PARAMS);
    }
    if from_flags == DitherCfg::Tpdf && cfg.hw.sample_format == sample_format::F32_INTERLEAVED {
        return Err(RtStatus::ERR_DEVICE_PARAMS);
    }
    Ok(from_flags)
}

/// 缓冲区注册/plan 期的 planar f32 缓冲契约校验（T26 在分配后调用一次）。
///
/// 校验：stride_samples 是 16 的倍数且 >= period_frames；基址 64 B 对齐；
/// len 足以覆盖最后一个通道的 period。错误码统一 ERR_STRIDE_ALIGN。
pub fn validate_planar_buffer(
    base_addr: usize,
    len: usize,
    channels: usize,
    period_frames: usize,
    stride_samples: usize,
) -> Result<(), RtStatus> {
    check_planar_stride(stride_samples, period_frames)?;
    let required = planar_required_len(channels, period_frames, stride_samples)?;
    if len < required {
        return Err(RtStatus::ERR_STRIDE_ALIGN);
    }
    if base_addr % PLANAR_ALIGN_BYTES != 0 {
        return Err(RtStatus::ERR_STRIDE_ALIGN);
    }
    Ok(())
}

/// planar 缓冲的通道基址是否满足 64 B 对齐（stride 16 样本倍数 + 基址 64 B 对齐）。
pub const fn planar_channels_aligned(base_addr: usize, stride_samples: usize) -> bool {
    stride_samples % PLANAR_STRIDE_MULTIPLE == 0 && base_addr % PLANAR_ALIGN_BYTES == 0
}

// ---------------------------------------------------------------------------
// 转换：capture（设备 interleaved 字节 -> planar f32）
// ---------------------------------------------------------------------------

/// capture 方向：设备格式 -> planar f32（[IO]；dense planar，stride = period）。
///
/// dst.len() 必须 >= period_frames × n_in；src.len() 必须 >= 一个 capture period
/// 的交错字节数。非有限/越界按模块文档 §3 sanitize/clip 并计数，不返回错误。
pub fn convert_capture(
    cfg: &FormatConfig,
    src: &[u8],
    dst: &mut [f32],
) -> Result<FormatStats, RtStatus> {
    convert_capture_strided(cfg, src, dst, cfg.hw.period_frames as usize)
}

/// capture（显式 planar stride）：dst[ch × stride + frame]。
///
/// stride_samples 必须是 16 的倍数且 >= period_frames（core-v1 §11.9），否则
/// ERR_STRIDE_ALIGN。stride > period 的尾通道填充不写。
pub fn convert_capture_strided(
    cfg: &FormatConfig,
    src: &[u8],
    dst: &mut [f32],
    stride_samples: usize,
) -> Result<FormatStats, RtStatus> {
    let layout = validate(cfg)?;
    check_planar_stride(stride_samples, layout.period_frames)?;
    if dst.len() < planar_required_len(layout.n_in, layout.period_frames, stride_samples)? {
        return Err(RtStatus::ERR_STRIDE_ALIGN);
    }
    if src.len() < layout.in_min_bytes {
        return Err(RtStatus::ERR_STRIDE_ALIGN);
    }
    Ok(match layout.sample_format {
        sample_format::S24_3LE_INTERLEAVED => {
            capture_samples::<S24>(&layout, src, dst, stride_samples)
        }
        sample_format::S32_LE_INTERLEAVED => {
            capture_samples::<S32>(&layout, src, dst, stride_samples)
        }
        sample_format::S16_LE_INTERLEAVED => {
            capture_samples::<S16>(&layout, src, dst, stride_samples)
        }
        sample_format::F32_INTERLEAVED => {
            capture_samples::<F32I>(&layout, src, dst, stride_samples)
        }
        // validate() 已拒绝其余格式；此处不可达。
        _ => FormatStats::default(),
    })
}

/// capture 转换核心（单态化到具体设备格式；内层循环无枚举分支）。
fn capture_samples<D: DeviceSample>(
    layout: &PeriodLayout,
    src: &[u8],
    dst: &mut [f32],
    stride: usize,
) -> FormatStats {
    let mut stats = FormatStats::default();
    let channels = layout.n_in;
    let frames = layout.period_frames;
    for frame in 0..frames {
        let base = frame * channels * D::BYTES;
        for channel in 0..channels {
            let offset = base + channel * D::BYTES;
            let mut sample = D::decode(src, offset);
            if !sample.is_finite() {
                stats.nonfinite_in = stats.nonfinite_in.saturating_add(1);
                sample = 0.0;
            }
            if sample > 1.0 {
                sample = 1.0;
                stats.clipped = stats.clipped.saturating_add(1);
            } else if sample < -1.0 {
                sample = -1.0;
                stats.clipped = stats.clipped.saturating_add(1);
            }
            if !sample.is_finite() {
                // 防御：sanitize 后不应出现；出现则 fail-safe 写 0 并计数。
                stats.nonfinite_out = stats.nonfinite_out.saturating_add(1);
                sample = 0.0;
            }
            dst[channel * stride + frame] = sample;
        }
    }
    stats
}

// ---------------------------------------------------------------------------
// 转换：playback（planar f32 -> 设备 interleaved 字节）
// ---------------------------------------------------------------------------

/// playback 方向：planar f32 -> 设备格式（[IO]；dense planar，stride = period）。
///
/// 无状态入口：若启用 dither，每次调用以同一种子重建噪声序列（每个 period 重复同一
/// 噪声序列）。连续播放必须改用 convert_playback_stateful（模块文档 §4）。
pub fn convert_playback(
    cfg: &FormatConfig,
    src: &[f32],
    dst: &mut [u8],
) -> Result<FormatStats, RtStatus> {
    convert_playback_strided(cfg, src, dst, cfg.hw.period_frames as usize)
}

/// playback（显式 planar stride）：src[ch × stride + frame]。
///
/// stride_samples 必须是 16 的倍数且 >= period_frames，否则 ERR_STRIDE_ALIGN。
pub fn convert_playback_strided(
    cfg: &FormatConfig,
    src: &[f32],
    dst: &mut [u8],
    stride_samples: usize,
) -> Result<FormatStats, RtStatus> {
    let layout = validate(cfg)?;
    let dither = effective_dither(cfg)?;
    let mut state = DitherState::for_config(cfg);
    convert_playback_inner(&layout, dither, &mut state, src, dst, stride_samples)
}

/// playback（连续流 dither 状态；dense planar）。
pub fn convert_playback_stateful(
    cfg: &FormatConfig,
    state: &mut DitherState,
    src: &[f32],
    dst: &mut [u8],
) -> Result<FormatStats, RtStatus> {
    convert_playback_stateful_strided(cfg, state, src, dst, cfg.hw.period_frames as usize)
}

/// playback（连续流 dither 状态 + 显式 planar stride）。
///
/// state 由调用方跨 period 持有（新 epoch/新流用 DitherState::for_config 重建）；
/// dither 关闭时不推进状态。
pub fn convert_playback_stateful_strided(
    cfg: &FormatConfig,
    state: &mut DitherState,
    src: &[f32],
    dst: &mut [u8],
    stride_samples: usize,
) -> Result<FormatStats, RtStatus> {
    let layout = validate(cfg)?;
    let dither = effective_dither(cfg)?;
    convert_playback_inner(&layout, dither, state, src, dst, stride_samples)
}

/// playback 转换核心（长度/stride 校验 + 设备格式分派）。
fn convert_playback_inner(
    layout: &PeriodLayout,
    dither: DitherCfg,
    state: &mut DitherState,
    src: &[f32],
    dst: &mut [u8],
    stride_samples: usize,
) -> Result<FormatStats, RtStatus> {
    check_planar_stride(stride_samples, layout.period_frames)?;
    if src.len() < planar_required_len(layout.n_out, layout.period_frames, stride_samples)? {
        return Err(RtStatus::ERR_STRIDE_ALIGN);
    }
    if dst.len() < layout.out_min_bytes {
        return Err(RtStatus::ERR_STRIDE_ALIGN);
    }
    Ok(match layout.sample_format {
        sample_format::S24_3LE_INTERLEAVED => {
            playback_samples::<S24>(layout, dither, state, src, dst, stride_samples)
        }
        sample_format::S32_LE_INTERLEAVED => {
            playback_samples::<S32>(layout, dither, state, src, dst, stride_samples)
        }
        sample_format::S16_LE_INTERLEAVED => {
            playback_samples::<S16>(layout, dither, state, src, dst, stride_samples)
        }
        sample_format::F32_INTERLEAVED => {
            playback_samples::<F32I>(layout, dither, state, src, dst, stride_samples)
        }
        // validate() 已拒绝其余格式；此处不可达。
        _ => FormatStats::default(),
    })
}

/// playback 转换核心（单态化到具体设备格式）。
fn playback_samples<D: DeviceSample>(
    layout: &PeriodLayout,
    dither: DitherCfg,
    state: &mut DitherState,
    src: &[f32],
    dst: &mut [u8],
    stride: usize,
) -> FormatStats {
    let mut stats = FormatStats::default();
    let channels = layout.n_out;
    let frames = layout.period_frames;
    for frame in 0..frames {
        let base = frame * channels * D::BYTES;
        for channel in 0..channels {
            let mut sample = src[channel * stride + frame];
            if !sample.is_finite() {
                stats.nonfinite_in = stats.nonfinite_in.saturating_add(1);
                sample = 0.0;
            }
            if sample > 1.0 {
                sample = 1.0;
                stats.clipped = stats.clipped.saturating_add(1);
            } else if sample < -1.0 {
                sample = -1.0;
                stats.clipped = stats.clipped.saturating_add(1);
            }
            let noise = match dither {
                DitherCfg::Off => 0.0,
                DitherCfg::Tpdf => state.next_tpdf(),
            };
            let offset = base + channel * D::BYTES;
            if D::encode(dst, offset, sample, noise) {
                stats.nonfinite_out = stats.nonfinite_out.saturating_add(1);
            }
        }
    }
    stats
}

// ---------------------------------------------------------------------------
// DitherState：PCG32 + TPDF（确定性，无分配）
// ---------------------------------------------------------------------------

/// TPDF dither 状态（POD；无 Drop/无分配）。
///
/// 种子 = hw.reserved[0]（经 SplitMix64 混合进入 PCG32 状态）；同一状态 + 同一输入顺序
/// 产生逐位一致的输出（F-43）。连续播放由 I/O owner 跨 period 持有；新 epoch/新流重建。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DitherState {
    /// PCG32 LCG 内部状态。
    state: u64,
    /// 原始种子（诊断/重放用）。
    seed: u64,
}

impl DitherState {
    /// 由种子构造（SplitMix64 混合后作为 PCG32 状态）。
    pub const fn new(seed: u64) -> Self {
        // SplitMix64 finalizer：避免低熵种子导致的短周期/相关性。
        let mut mixed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^= mixed >> 31;
        Self { state: mixed, seed }
    }

    /// 由 hwparams 的 dither 种子字段构造（reserved[0]）。
    pub const fn for_config(cfg: &FormatConfig) -> Self {
        Self::new(cfg.hw.reserved[DITHER_SEED_RESERVED_INDEX])
    }

    /// 原始种子。
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// 重新播种（新 epoch/新流；语义等价于 DitherState::new）。
    pub const fn reseed(&mut self, seed: u64) {
        *self = Self::new(seed);
    }

    /// 下一个 PCG32 输出（XSH-RR）。
    #[inline]
    fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old
            .wrapping_mul(PCG32_MULTIPLIER)
            .wrapping_add(PCG32_INCREMENT);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// uniform [0,1)（24-bit 网格，精确可表示）。
    #[inline]
    fn next_uniform(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * UNIFORM_SCALE
    }

    /// 下一个 TPDF 噪声（LSB 单位，(-1,1) 三角分布，峰峰 2 LSB）。
    #[inline]
    pub fn next_tpdf(&mut self) -> f32 {
        self.next_uniform() - self.next_uniform()
    }
}

// ---------------------------------------------------------------------------
// 设备样本格式：端序/符号扩展/归一化/量化（单态化实现）
// ---------------------------------------------------------------------------

/// 设备样本格式的编译期分派（每 format 一个零尺寸类型，内层循环无枚举分支）。
trait DeviceSample {
    /// 每样本字节数。
    const BYTES: usize;
    /// 解码一个样本到归一化 f32（端序 + 符号扩展 + 2 的幂缩放）。
    fn decode(src: &[u8], offset: usize) -> f32;
    /// 编码一个样本（sample 已 sanitize 且已夹取到 [-1,1]）；dither 为 LSB 单位的 TPDF
    /// 噪声；返回写出值是否为非有限（仅 F32 路径可能）。
    fn encode(dst: &mut [u8], offset: usize, sample: f32, dither: f32) -> bool;
}

/// S24_3LE：3 字节 little-endian packed 24-bit 二补码。
struct S24;
/// S32_LE：4 字节 little-endian 32-bit 二补码（全量程）。
struct S32;
/// S16_LE：2 字节 little-endian 16-bit 二补码。
struct S16;
/// F32_INTERLEAVED：4 字节 little-endian IEEE-754 binary32。
struct F32I;

impl DeviceSample for S16 {
    const BYTES: usize = 2;

    #[inline]
    fn decode(src: &[u8], offset: usize) -> f32 {
        (i16::from_le_bytes([src[offset], src[offset + 1]]) as f32) / 32_768.0
    }

    #[inline]
    fn encode(dst: &mut [u8], offset: usize, sample: f32, dither: f32) -> bool {
        let code = quantize::<16>(sample, dither);
        let bytes = (code as i16).to_le_bytes();
        dst[offset] = bytes[0];
        dst[offset + 1] = bytes[1];
        false
    }
}

impl DeviceSample for S24 {
    const BYTES: usize = 3;

    #[inline]
    fn decode(src: &[u8], offset: usize) -> f32 {
        let raw = read_u24_le(src, offset);
        // 24-bit 二补码符号扩展（bit23 为符号位）。
        let signed = ((raw << 8) as i32) >> 8;
        (signed as f32) / 8_388_608.0
    }

    #[inline]
    fn encode(dst: &mut [u8], offset: usize, sample: f32, dither: f32) -> bool {
        let code = quantize::<24>(sample, dither);
        let bytes = code.to_le_bytes();
        dst[offset] = bytes[0];
        dst[offset + 1] = bytes[1];
        dst[offset + 2] = bytes[2];
        false
    }
}

impl DeviceSample for S32 {
    const BYTES: usize = 4;

    #[inline]
    fn decode(src: &[u8], offset: usize) -> f32 {
        // 整数 -> f32 按 IEEE round-to-nearest/ties-to-even（f32 只有 24-bit 有效位）。
        (read_u32_le(src, offset) as i32 as f32) / 2_147_483_648.0
    }

    #[inline]
    fn encode(dst: &mut [u8], offset: usize, sample: f32, dither: f32) -> bool {
        let code = quantize::<32>(sample, dither);
        write_u32_le(dst, offset, code as u32);
        false
    }
}

impl DeviceSample for F32I {
    const BYTES: usize = 4;

    #[inline]
    fn decode(src: &[u8], offset: usize) -> f32 {
        f32::from_bits(read_u32_le(src, offset))
    }

    #[inline]
    fn encode(dst: &mut [u8], offset: usize, sample: f32, dither: f32) -> bool {
        let _ = dither; // f32 输出无量化，dither 已在校验层禁止
        if !sample.is_finite() {
            write_u32_le(dst, offset, 0);
            return true;
        }
        write_u32_le(dst, offset, sample.to_bits());
        false
    }
}

/// 量化：x 属于 [-1,1]（已夹取、已 sanitize）-> 有符号整数码，round-ties-even + 饱和。
#[inline]
fn quantize<const BITS: u32>(sample: f32, dither: f32) -> i32 {
    debug_assert!(sample.is_finite() && sample >= -1.0 && sample <= 1.0);
    let scale = ((1u64 << (BITS - 1)) as u32) as f32; // 2^(BITS-1)，2 的幂精确
    let max_code = ((1i64 << (BITS - 1)) - 1) as i32;
    let scaled = sample * scale + dither;
    let rounded = scaled.round_ties_even();
    if rounded >= scale {
        max_code
    } else if rounded <= -scale {
        -max_code - 1
    } else {
        rounded as i32
    }
}

/// 读 3 字节 little-endian（低 24 位有效）。
#[inline]
fn read_u24_le(src: &[u8], offset: usize) -> u32 {
    (src[offset] as u32) | ((src[offset + 1] as u32) << 8) | ((src[offset + 2] as u32) << 16)
}

/// 读 4 字节 little-endian。
#[inline]
fn read_u32_le(src: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        src[offset],
        src[offset + 1],
        src[offset + 2],
        src[offset + 3],
    ])
}

/// 写 4 字节 little-endian。
#[inline]
fn write_u32_le(dst: &mut [u8], offset: usize, value: u32) {
    let bytes = value.to_le_bytes();
    dst[offset] = bytes[0];
    dst[offset + 1] = bytes[1];
    dst[offset + 2] = bytes[2];
    dst[offset + 3] = bytes[3];
}

// ---------------------------------------------------------------------------
// planar 布局校验辅助
// ---------------------------------------------------------------------------

/// 校验 planar stride：16 样本倍数且 >= period_frames。
fn check_planar_stride(stride_samples: usize, period_frames: usize) -> Result<(), RtStatus> {
    if stride_samples < period_frames || stride_samples % PLANAR_STRIDE_MULTIPLE != 0 {
        return Err(RtStatus::ERR_STRIDE_ALIGN);
    }
    Ok(())
}

/// planar 切片最少样本数：(channels-1) × stride + period。
fn planar_required_len(
    channels: usize,
    period_frames: usize,
    stride_samples: usize,
) -> Result<usize, RtStatus> {
    channels
        .checked_sub(1)
        .and_then(|value| value.checked_mul(stride_samples))
        .and_then(|value| value.checked_add(period_frames))
        .ok_or(RtStatus::ERR_STRIDE_ALIGN)
}

// ---------------------------------------------------------------------------
// 测试注册（leaf 测试自注册，不经 public_registration.rs；WBS T06 纪律 / README §5）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod format_tests {
    include!("../tests/T22/format_vectors.rs");
}
