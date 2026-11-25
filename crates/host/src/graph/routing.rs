//! routing 段：lane 到 DAC 通道的静态路由。
//!
//! 未映射的 DAC 通道逐样本输出 +0.0。lane i -> dac i 且 `dac_channels == lanes` 的 identity
//! 路由就是逐样本拷贝。同一 DAC 不允许被多个 lane 映射（编译期拒绝），同一 lane 可以广播到
//! 多个 DAC。

use khaen_core::plan::RoutingSection;

use super::CompileError;

/// routing 段状态：每个 DAC 通道的来源 lane。
#[derive(Debug)]
pub(crate) struct RoutingStage {
    /// 长度 = DAC 通道数；`None` 表示该 DAC 输出静音。
    source: Vec<Option<usize>>,
}

impl RoutingStage {
    /// 编译 routing 段；缺少区段时按 `lane i -> dac i` 恒等路由处理。
    pub(crate) fn compile(
        section: Option<&RoutingSection>,
        lanes: usize,
        channels: usize,
    ) -> Result<Self, CompileError> {
        // 缺少区段时按 lane i -> dac i 恒等路由；有区段时未列出的 DAC 一律静音。
        let mut source: Vec<Option<usize>> = if section.is_some() {
            (0..channels).map(|_| None).collect()
        } else {
            (0..channels)
                .map(|dac| if dac < lanes { Some(dac) } else { None })
                .collect()
        };
        if let Some(section) = section {
            let mut assigned = vec![false; channels];
            for &(lane, dac) in &section.routes {
                let lane = lane as usize;
                let dac = dac as usize;
                if lane >= lanes {
                    return Err(CompileError::InvalidSection("routing lane out of range"));
                }
                if dac >= channels {
                    return Err(CompileError::InvalidSection("routing dac out of range"));
                }
                if assigned[dac] {
                    return Err(CompileError::InvalidSection("duplicate routing target dac"));
                }
                assigned[dac] = true;
                source[dac] = Some(lane);
            }
        }
        Ok(Self { source })
    }

    /// 处理一个子块：把 lane 平面写回帧交错输出块；未映射 DAC 写 +0.0。
    pub(crate) fn process(
        &self,
        lanes: &[f32],
        output: &mut [f32],
        channels: usize,
        stride: usize,
        frames: usize,
    ) {
        let mut dac = 0;
        while dac < self.source.len() {
            match self.source[dac] {
                Some(lane) => {
                    let plane = &lanes[lane * stride..lane * stride + frames];
                    let mut frame = 0;
                    while frame < frames {
                        output[frame * channels + dac] = plane[frame];
                        frame += 1;
                    }
                }
                None => {
                    let mut frame = 0;
                    while frame < frames {
                        output[frame * channels + dac] = 0.0;
                        frame += 1;
                    }
                }
            }
            dac += 1;
        }
    }
}
