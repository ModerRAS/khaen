//! crossover 段：每 lane 一条全采样率 FIR，复用 direct 卷积内核。
//!
//! 每 lane 用 1 源 / 1 角色 / 1 容器的 `DirectTopology` 表达一条 FIR：`a_fir` 即该 lane 的
//! 抽头，`g = [0.0]`、`p = [1.0]`，内核对每个样本做 `y = a * x`（`x * 1.0` 逐位不扰动）。
//! `taps == [1.0]` 的 identity 滤波器在编译期折叠为直通；没有滤波器的 lane 同样直通。

use khaen_core::kernels::direct::{
    direct_block_f32, DirectCoeffsF32, DirectLayout, DirectTopology, DIRECT_MAX_TAPS,
};
use khaen_core::plan::CrossoverSection;

use super::CompileError;

/// 单 lane 的 direct 内核状态与系数。
#[derive(Debug)]
struct LaneFir {
    /// 1 源 / 1 角色 / 1 容器的 direct 拓扑。
    topo: DirectTopology,
    /// 固定缓冲布局（含 block 中间量）。
    layout: DirectLayout,
    /// 输入侧 FIR = 该 lane 的抽头。
    a: Vec<f32>,
    /// 低带矩阵系数（固定为 0）。
    g: Vec<f32>,
    /// 输出对角 FIR（固定为 delta）。
    p: Vec<f32>,
    /// 跨块保留的卷积状态（编译期零初始化）。
    state: Vec<f32>,
}

/// crossover 段状态：每 lane 可选的一条 FIR。
#[derive(Debug)]
pub(crate) struct CrossoverStage {
    /// `None` 表示该 lane 直通。
    filters: Vec<Option<LaneFir>>,
}

impl CrossoverStage {
    /// 编译 crossover 段；缺少区段时整段直通。
    pub(crate) fn compile(
        section: Option<&CrossoverSection>,
        lanes: usize,
    ) -> Result<Self, CompileError> {
        let mut filters: Vec<Option<LaneFir>> = (0..lanes).map(|_| None).collect();
        if let Some(section) = section {
            for filter in &section.filters {
                let lane = filter.lane as usize;
                if lane >= lanes {
                    return Err(CompileError::InvalidSection(
                        "crossover filter lane out of range",
                    ));
                }
                if filters[lane].is_some() {
                    return Err(CompileError::InvalidSection("duplicate crossover lane"));
                }
                if filter.coeffs.is_empty() {
                    return Err(CompileError::InvalidSection("crossover filter has no taps"));
                }
                let taps = filter.coeffs.len();
                if taps > DIRECT_MAX_TAPS {
                    return Err(CompileError::TapsTooLong {
                        stage: "crossover",
                        taps,
                        limit: DIRECT_MAX_TAPS,
                    });
                }
                for coeff in &filter.coeffs {
                    if !coeff.is_finite() {
                        return Err(CompileError::NonFiniteCoefficient("crossover"));
                    }
                }
                // identity 判据：taps == [1.0] 在编译期折叠为直通。
                if taps == 1 && filter.coeffs[0] == 1.0 {
                    continue;
                }
                let mut topo = DirectTopology::identity();
                topo.l_a = taps as u16;
                let layout = DirectLayout::build(&topo).map_err(|_| {
                    CompileError::InvalidSection("crossover filter rejected by direct kernel")
                })?;
                let state = vec![0.0f32; layout.state_samples()];
                filters[lane] = Some(LaneFir {
                    topo,
                    layout,
                    a: filter.coeffs.clone(),
                    g: vec![0.0],
                    p: vec![1.0],
                    state,
                });
            }
        }
        Ok(Self { filters })
    }

    /// 处理一个子块：`input` / `output` 均为 `lanes` 个平面。
    pub(crate) fn process(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        stride: usize,
        frames: usize,
    ) {
        for (lane, filter) in self.filters.iter_mut().enumerate() {
            let inp = &input[lane * stride..lane * stride + frames];
            let out = &mut output[lane * stride..lane * stride + frames];
            match filter {
                None => out.copy_from_slice(inp),
                Some(fir) => {
                    let coeffs = DirectCoeffsF32 {
                        a_fir: &fir.a,
                        g: &fir.g,
                        p: &fir.p,
                    };
                    let status = direct_block_f32(
                        &fir.topo,
                        &fir.layout,
                        &coeffs,
                        &mut fir.state,
                        inp,
                        frames,
                        out,
                        frames,
                        frames,
                        frames,
                    );
                    // 编译期已保证参数合法；若内核仍报错则本 lane 静音（fail-safe，不 panic）。
                    if !status.is_ok() {
                        out.fill(0.0);
                    }
                }
            }
        }
    }
}
