//! matrix 段：跨 lane 满矩阵，全速率直接卷积（T21）。
//!
//! 只有 `decimation == interpolation == 1` 时进入本段：每个格子 `(row, col)` 是一条 `taps`
//! 抽头 FIR，行输出是各列 FIR 之和（列序固定，历史按块推进）。抽取 / 插值与 3 kHz 低速率域
//! 留给 T22（编译期返回 `super::CompileError::UnsupportedRates`）。
//!
//! identity 矩阵（`rows == cols`、`taps == 1`、对角 1.0 其余 0）在编译期折叠为逐样本拷贝；
//! 全零格子跳过，保证 identity 判据下逐位相等。

use khaen_core::plan::MatrixSection;

use super::CompileError;

/// 单个矩阵格子的 FIR 状态。
#[derive(Debug)]
struct MatrixCell {
    /// 该格子的 taps 抽头系数。
    coeffs: Vec<f32>,
    /// 卷积历史：`hist[t] = x[block_start - 1 - t]`，长度 `taps - 1`。
    hist: Vec<f32>,
    /// 系数全为 0：该格子跳过。
    silent: bool,
}

/// 矩阵段的编译形态。
#[derive(Debug)]
enum MatrixMode {
    /// identity 判据成立：逐样本拷贝。
    Identity,
    /// 全速率满矩阵直接卷积。
    Full,
}

/// matrix 段状态。
#[derive(Debug)]
pub(crate) struct MatrixStage {
    /// 输出 lane 数。
    rows: usize,
    /// 输入 lane 数。
    cols: usize,
    /// 每格抽头数。
    taps: usize,
    /// 恒等 / 全矩阵。
    mode: MatrixMode,
    /// 行优先排列的格子（`rows * cols`），Identity 模式下为空。
    cells: Vec<MatrixCell>,
}

impl MatrixStage {
    /// 编译 matrix 段；非 1 的抽取 / 插值在这里被拒绝（T22 边界）。
    pub(crate) fn compile(
        section: &MatrixSection,
        cols: usize,
        rows: usize,
    ) -> Result<Self, CompileError> {
        if section.decimation != 1 || section.interpolation != 1 {
            return Err(CompileError::UnsupportedRates {
                decimation: section.decimation,
                interpolation: section.interpolation,
            });
        }
        if section.cols as usize != cols || section.rows as usize != rows {
            return Err(CompileError::LaneMismatch {
                stage: "matrix",
                expected: cols,
                actual: section.cols as usize,
            });
        }
        if rows == 0 || cols == 0 {
            return Err(CompileError::InvalidSection(
                "matrix rows and cols must be positive",
            ));
        }
        let taps = section.taps as usize;
        if taps == 0 {
            return Err(CompileError::InvalidSection("matrix taps must be positive"));
        }
        let expected = rows
            .checked_mul(cols)
            .and_then(|cells| cells.checked_mul(taps))
            .ok_or(CompileError::InvalidSection(
                "matrix coefficient count overflow",
            ))?;
        if section.coeffs.len() != expected {
            return Err(CompileError::InvalidSection(
                "matrix coefficient count mismatch",
            ));
        }
        for coeff in &section.coeffs {
            if !coeff.is_finite() {
                return Err(CompileError::NonFiniteCoefficient("matrix"));
            }
        }
        // identity 判据：rows == cols、taps == 1、对角 1.0 其余 0。
        let identity = rows == cols
            && taps == 1
            && section.coeffs.iter().enumerate().all(|(index, coeff)| {
                if index % (cols + 1) == 0 {
                    *coeff == 1.0
                } else {
                    *coeff == 0.0
                }
            });
        if identity {
            return Ok(Self {
                rows,
                cols,
                taps,
                mode: MatrixMode::Identity,
                cells: Vec::new(),
            });
        }
        let mut cells = Vec::with_capacity(rows * cols);
        for row in 0..rows {
            for col in 0..cols {
                let base = (row * cols + col) * taps;
                let coeffs = section.coeffs[base..base + taps].to_vec();
                let silent = coeffs.iter().all(|coeff| *coeff == 0.0);
                let hist = if silent {
                    Vec::new()
                } else {
                    vec![0.0f32; taps - 1]
                };
                cells.push(MatrixCell {
                    coeffs,
                    hist,
                    silent,
                });
            }
        }
        Ok(Self {
            rows,
            cols,
            taps,
            mode: MatrixMode::Full,
            cells,
        })
    }

    /// 处理一个子块：`input` 为 `cols` 个平面，`output` 为 `rows` 个平面。
    pub(crate) fn process(
        &mut self,
        input: &[f32],
        output: &mut [f32],
        stride: usize,
        frames: usize,
    ) {
        match self.mode {
            MatrixMode::Identity => {
                let mut lane = 0;
                while lane < self.rows {
                    let inp = &input[lane * stride..lane * stride + frames];
                    output[lane * stride..lane * stride + frames].copy_from_slice(inp);
                    lane += 1;
                }
            }
            MatrixMode::Full => {
                let taps = self.taps;
                let cols = self.cols;
                let mut row = 0;
                while row < self.rows {
                    output[row * stride..row * stride + frames].fill(0.0);
                    let mut col = 0;
                    while col < cols {
                        let cell = &mut self.cells[row * cols + col];
                        if !cell.silent {
                            let x = &input[col * stride..col * stride + frames];
                            let out = &mut output[row * stride..row * stride + frames];
                            Self::accumulate_cell(cell, x, out, taps, frames);
                        }
                        col += 1;
                    }
                    row += 1;
                }
            }
        }
    }

    /// 把一个格子的 FIR 累加进输出平面（固定 tap 升序，历史跨块保留）。
    fn accumulate_cell(
        cell: &mut MatrixCell,
        x: &[f32],
        out: &mut [f32],
        taps: usize,
        frames: usize,
    ) {
        let mut frame = 0;
        while frame < frames {
            let mut acc = cell.coeffs[0] * x[frame];
            let mut tap = 1;
            while tap < taps {
                let value = if tap <= frame {
                    x[frame - tap]
                } else {
                    cell.hist[tap - 1 - frame]
                };
                acc += cell.coeffs[tap] * value;
                tap += 1;
            }
            out[frame] += acc;
            frame += 1;
        }
        // 历史更新：hist[t] = x[frames - 1 - t]，不足部分沿用旧历史（从后往前写）。
        let mut index = taps - 1;
        while index > 0 {
            index -= 1;
            cell.hist[index] = if frames > index {
                x[frames - 1 - index]
            } else {
                cell.hist[index - frames]
            };
        }
    }
}
