//! 准入：把方案交给实时图之前先算预算，超了就拒绝（fail-closed）。
//!
//! 拒绝不是"警告后继续"：任何一项超预算或内部不自洽，都要在 `rejections` 里点名，
//! 上层据此回 `admission_rejected`，不得带着超预算的图跑实时线程。
//!
//! 这里所有数字都是**估算下界**（由 taps/延迟/系数直接算），不是实测；实测延迟归 T22/T26。

use khaen_core::Plan;

/// 方案文件字节上限（默认 4 MiB）。
pub const DEFAULT_MAX_PLAN_BYTES: u64 = 4 * 1024 * 1024;
/// 系数总数上限。
pub const DEFAULT_MAX_COEFFICIENTS: u64 = 1_000_000;
/// 矩阵格数上限（行 × 列）。
pub const DEFAULT_MAX_MATRIX_CELLS: u64 = 65_536;
/// 每格矩阵抽头上限。
pub const DEFAULT_MAX_MATRIX_TAPS_PER_CELL: u32 = 4096;
/// 端到端延迟估算上限（全采样率样本数）。
pub const DEFAULT_MAX_LATENCY_SAMPLES: u64 = 8192;
/// lane 数上限。
pub const DEFAULT_MAX_LANES: u16 = 32;
/// DAC 通道数上限。
pub const DEFAULT_MAX_DAC_CHANNELS: u16 = 64;

/// 准入预算上限。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionLimits {
    /// 方案文件字节上限。
    pub max_plan_bytes: u64,
    /// 系数总数上限。
    pub max_coefficients: u64,
    /// 矩阵格数上限。
    pub max_matrix_cells: u64,
    /// 每格矩阵抽头上限。
    pub max_matrix_taps_per_cell: u32,
    /// 延迟估算上限（全采样率样本）。
    pub max_latency_samples: u64,
    /// lane 数上限。
    pub max_lanes: u16,
    /// DAC 通道上限。
    pub max_dac_channels: u16,
}

impl Default for AdmissionLimits {
    fn default() -> Self {
        Self {
            max_plan_bytes: DEFAULT_MAX_PLAN_BYTES,
            max_coefficients: DEFAULT_MAX_COEFFICIENTS,
            max_matrix_cells: DEFAULT_MAX_MATRIX_CELLS,
            max_matrix_taps_per_cell: DEFAULT_MAX_MATRIX_TAPS_PER_CELL,
            max_latency_samples: DEFAULT_MAX_LATENCY_SAMPLES,
            max_lanes: DEFAULT_MAX_LANES,
            max_dac_channels: DEFAULT_MAX_DAC_CHANNELS,
        }
    }
}

/// 一条拒绝理由；code 稳定，detail 给人看。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    /// 稳定错误码。
    pub code: &'static str,
    /// 具体说明（含实测值与上限）。
    pub detail: String,
}

impl Rejection {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

/// 预算估算结果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Budget {
    /// 方案文件字节数。
    pub plan_bytes: u64,
    /// 系数总数。
    pub coefficients: u64,
    /// 矩阵格数（行 × 列）。
    pub matrix_cells: u64,
    /// 每格抽头数。
    pub matrix_taps_per_cell: u32,
    /// 卷积抽头总数（分频 + 矩阵）。
    pub convolution_taps: u64,
    /// 估算延迟（全采样率样本）。
    pub latency_samples: u64,
    /// lane 数。
    pub lanes: u16,
    /// DAC 通道数。
    pub dac_channels: u16,
    /// 需要常驻的滤波器状态样本数。
    pub state_samples: u64,
    /// 常驻内存估算（字节）。
    pub memory_bytes: u64,
}

/// 准入结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    /// 是否通过。
    pub accepted: bool,
    /// 拒绝理由（accepted=true 时为空）。
    pub rejections: Vec<Rejection>,
    /// 估算预算。
    pub budget: Budget,
}

impl Admission {
    /// 通过与否，带理由，便于直接回给控制面。
    pub fn summary(&self) -> String {
        if self.accepted {
            "admitted".to_string()
        } else {
            self.rejections
                .iter()
                .map(|rejection| format!("{}: {}", rejection.code, rejection.detail))
                .collect::<Vec<_>>()
                .join("; ")
        }
    }
}

/// 计算预算并判定准入。
pub fn admit(plan: &Plan, limits: &AdmissionLimits) -> Admission {
    let mut rejections = Vec::new();
    let mut budget = Budget::default();

    if plan.block_size == 0 {
        rejections.push(Rejection::new("block_size", "plan declares block_size = 0"));
    }
    if plan.sample_rate == 0 {
        rejections.push(Rejection::new(
            "sample_rate",
            "plan declares sample_rate = 0",
        ));
    }

    // 编码同样 fail-closed：手工构造的超限方案必须在这里变成一条 rejection，
    // 而不是让 panic 版 encode 把控制面线程带崩（decode/plan-builder 产物不可达此分支）。
    let plan_bytes = match plan.encode_checked() {
        Ok(bytes) => bytes.len() as u64,
        Err(err) => {
            rejections.push(Rejection::new(
                "plan_encoding",
                format!("plan cannot be encoded within ABI limits: {err}"),
            ));
            0
        }
    };
    budget.plan_bytes = plan_bytes;
    if plan_bytes > limits.max_plan_bytes {
        rejections.push(Rejection::new(
            "plan_bytes",
            format!(
                "plan is {plan_bytes} bytes, limit {}",
                limits.max_plan_bytes
            ),
        ));
    }

    // lane / DAC 规模取各区段声明的最大值，同时检查引用越界。
    let mut lanes = 0u16;
    let mut dac_channels = 0u16;
    if let Some(content) = &plan.content {
        lanes = lanes.max(content.out_lanes);
        if content.in_channels == 0 {
            rejections.push(Rejection::new("content", "in_channels = 0"));
        }
        for fold in &content.folds {
            if fold.src >= content.in_channels || fold.dst >= content.out_lanes {
                rejections.push(Rejection::new(
                    "content_fold_range",
                    format!(
                        "fold {} -> {} out of range (in {}, lanes {})",
                        fold.src, fold.dst, content.in_channels, content.out_lanes
                    ),
                ));
            }
        }
    }

    let mut coefficients = 0u64;
    let mut convolution_taps = 0u64;
    let mut crossover_latency = 0u64;
    let mut crossover_state = 0u64;
    if let Some(crossover) = &plan.crossover {
        lanes = lanes.max(crossover.lanes);
        for filter in &crossover.filters {
            if filter.lane >= crossover.lanes {
                rejections.push(Rejection::new(
                    "crossover_lane_range",
                    format!("filter lane {} >= lanes {}", filter.lane, crossover.lanes),
                ));
            }
            if filter.coeffs.is_empty() {
                rejections.push(Rejection::new(
                    "crossover_taps",
                    format!("lane {} has an empty filter", filter.lane),
                ));
                continue;
            }
            let taps = filter.coeffs.len() as u64;
            coefficients += taps;
            convolution_taps += taps;
            // 假定线性相位：群延迟 = (taps - 1) / 2。
            crossover_latency = crossover_latency.max((taps - 1) / 2);
            crossover_state += taps - 1;
        }
    }

    let mut matrix_latency = 0u64;
    let mut matrix_state = 0u64;
    if let Some(matrix) = &plan.matrix {
        lanes = lanes.max(matrix.rows);
        budget.matrix_cells = u64::from(matrix.rows) * u64::from(matrix.cols);
        budget.matrix_taps_per_cell = matrix.taps;
        let expected = budget.matrix_cells * u64::from(matrix.taps);
        if matrix.coeffs.len() as u64 != expected {
            rejections.push(Rejection::new(
                "matrix_coefficients",
                format!(
                    "{} coefficients, expected rows*cols*taps = {expected}",
                    matrix.coeffs.len()
                ),
            ));
        }
        coefficients += matrix.coeffs.len() as u64;
        convolution_taps += matrix.coeffs.len() as u64;
        if matrix.decimation == 0 || matrix.interpolation == 0 {
            rejections.push(Rejection::new(
                "matrix_rates",
                format!(
                    "decimation {} / interpolation {} must be non-zero",
                    matrix.decimation, matrix.interpolation
                ),
            ));
        }
        // 矩阵在低采样率域跑：群延迟换算回全采样率要乘抽取因子。
        let decimation = u64::from(matrix.decimation.max(1));
        matrix_latency = (u64::from(matrix.taps) / 2) * decimation;
        matrix_state = (u64::from(matrix.taps.saturating_sub(1))) * budget.matrix_cells;
        if budget.matrix_cells > limits.max_matrix_cells {
            rejections.push(Rejection::new(
                "matrix_cells",
                format!(
                    "{} cells, limit {}",
                    budget.matrix_cells, limits.max_matrix_cells
                ),
            ));
        }
        if matrix.taps > limits.max_matrix_taps_per_cell {
            rejections.push(Rejection::new(
                "matrix_taps",
                format!(
                    "{} taps per cell, limit {}",
                    matrix.taps, limits.max_matrix_taps_per_cell
                ),
            ));
        }
    } else {
        rejections.push(Rejection::new(
            "missing_matrix",
            "plan has no matrix section; the runtime graph needs one",
        ));
    }

    let mut protection_latency = 0u64;
    let mut protection_state = 0u64;
    if let Some(protection) = &plan.protection {
        lanes = lanes.max(protection.lanes);
        for entry in &protection.entries {
            if entry.lane >= protection.lanes {
                rejections.push(Rejection::new(
                    "protection_lane_range",
                    format!("entry lane {} >= lanes {}", entry.lane, protection.lanes),
                ));
            }
            if entry.polarity != 1 && entry.polarity != -1 {
                rejections.push(Rejection::new(
                    "protection_polarity",
                    format!(
                        "lane {} polarity {} must be +1 or -1",
                        entry.lane, entry.polarity
                    ),
                ));
            }
            protection_latency = protection_latency.max(u64::from(entry.delay));
            protection_state += u64::from(entry.delay);
            coefficients += (entry.peq.len() as u64) * 5;
        }
    }

    if let Some(routing) = &plan.routing {
        dac_channels = routing.dac_channels;
        for (lane, dac) in &routing.routes {
            if *lane >= lanes.max(1) {
                rejections.push(Rejection::new(
                    "routing_lane_range",
                    format!("route lane {lane} >= lanes {lanes}"),
                ));
            }
            if *dac >= routing.dac_channels {
                rejections.push(Rejection::new(
                    "routing_dac_range",
                    format!("route dac {dac} >= dac_channels {}", routing.dac_channels),
                ));
            }
        }
    }

    budget.lanes = lanes;
    budget.dac_channels = dac_channels;
    budget.coefficients = coefficients;
    budget.convolution_taps = convolution_taps;
    budget.state_samples = crossover_state + matrix_state + protection_state;
    let latency = crossover_latency + matrix_latency + protection_latency;
    budget.latency_samples = latency;
    budget.memory_bytes = (coefficients + budget.state_samples) * 4 + plan_bytes;

    if lanes > limits.max_lanes {
        rejections.push(Rejection::new(
            "lanes",
            format!("{lanes} lanes, limit {}", limits.max_lanes),
        ));
    }
    if dac_channels > limits.max_dac_channels {
        rejections.push(Rejection::new(
            "dac_channels",
            format!("{dac_channels} channels, limit {}", limits.max_dac_channels),
        ));
    }
    if coefficients > limits.max_coefficients {
        rejections.push(Rejection::new(
            "coefficients",
            format!(
                "{coefficients} coefficients, limit {}",
                limits.max_coefficients
            ),
        ));
    }
    if latency > limits.max_latency_samples {
        rejections.push(Rejection::new(
            "latency",
            format!(
                "estimated {latency} samples, limit {}",
                limits.max_latency_samples
            ),
        ));
    }

    Admission {
        accepted: rejections.is_empty(),
        rejections,
        budget,
    }
}
