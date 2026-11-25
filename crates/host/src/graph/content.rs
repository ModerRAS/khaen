//! content 段：容器通道到 lane 的静态映射与折叠增益。
//!
//! 运行期不做动态混音：每 lane 的输入通道列表与线性增益在编译期固定。单源、增益 1.0 的
//! 映射在编译期折叠为逐样本拷贝（identity 判据），保证与输入逐位相等；没有任何折叠项的
//! lane 输出 +0.0（静音）。

use khaen_core::plan::ContentSection;

use super::CompileError;

/// 单 lane 的映射方式（编译期固定）。
#[derive(Debug)]
enum LaneMap {
    /// 逐样本拷贝源通道（单源、增益 1.0）。
    Copy(usize),
    /// 源通道按增益求和，列表顺序即累加顺序。
    Fold(Vec<(usize, f32)>),
    /// 没有折叠项：静音。
    Silent,
}

/// content 段状态：每 lane 的映射表。
#[derive(Debug)]
pub(crate) struct ContentStage {
    /// 每 lane 的映射方式。
    lanes: Vec<LaneMap>,
}

impl ContentStage {
    /// 编译 content 段；缺少区段时按 `lane == channel` 恒等映射处理。
    pub(crate) fn compile(
        section: Option<&ContentSection>,
        in_channels: usize,
        lanes: usize,
    ) -> Result<Self, CompileError> {
        if lanes == 0 || in_channels == 0 {
            return Err(CompileError::InvalidSection(
                "content needs positive in_channels and lanes",
            ));
        }
        let mut folds: Vec<Vec<(usize, f32)>> = (0..lanes).map(|_| Vec::new()).collect();
        match section {
            Some(section) => {
                for fold in &section.folds {
                    let src = fold.src as usize;
                    let dst = fold.dst as usize;
                    if src >= in_channels {
                        return Err(CompileError::InvalidSection(
                            "content fold source out of range",
                        ));
                    }
                    if dst >= lanes {
                        return Err(CompileError::InvalidSection(
                            "content fold lane out of range",
                        ));
                    }
                    if !fold.gain.is_finite() {
                        return Err(CompileError::NonFiniteCoefficient("content fold gain"));
                    }
                    folds[dst].push((src, fold.gain));
                }
            }
            None => {
                // 缺省 content：通道 i 直接进 lane i（恒等映射）。
                for (lane, list) in folds.iter_mut().enumerate() {
                    if lane < in_channels {
                        list.push((lane, 1.0));
                    }
                }
            }
        }
        let lanes = folds
            .into_iter()
            .map(|list| {
                if list.is_empty() {
                    LaneMap::Silent
                } else if list.len() == 1 && list[0].1 == 1.0 {
                    LaneMap::Copy(list[0].0)
                } else {
                    LaneMap::Fold(list)
                }
            })
            .collect();
        Ok(Self { lanes })
    }

    /// 处理一个子块：`input` 为 `in_channels` 个平面，`output` 为 `lanes` 个平面。
    pub(crate) fn process(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        stride: usize,
        frames: usize,
    ) {
        for (lane, map) in self.lanes.iter().enumerate() {
            let out = &mut output[lane * stride..lane * stride + frames];
            match map {
                LaneMap::Copy(src) => {
                    let inp = &input[src * stride..src * stride + frames];
                    out.copy_from_slice(inp);
                }
                LaneMap::Silent => out.fill(0.0),
                LaneMap::Fold(folds) => {
                    out.fill(0.0);
                    for &(src, gain) in folds {
                        let inp = &input[src * stride..src * stride + frames];
                        for (target, sample) in out.iter_mut().zip(inp.iter()) {
                            *target += gain * *sample;
                        }
                    }
                }
            }
        }
    }
}
