//! 输出级（OutputStage）：DSP 之后、PCM 设备之前唯一的输出真值源。
//!
//! 背景：日常 mute 必须是**应用软件静音**——在送往声卡的输出链路上把节目清零，
//! 不依赖 MOTU/CueMix 或任何 ALSA 音量控制，因此任意声卡都成立。校准扫频时则需要在
//! 用户静音仍保持（cal_leave 要恢复原状态）的前提下临时放行输出，这个放行不能靠引擎
//! 再改一次静音状态，否则“用户意图”和“校准旁路”会互相覆盖。
//!
//! 设计：
//! - `program_mute` 是用户意图；请求在块边界生效（与块引擎的 MuteReceipt 语义一致）。
//! - `calibration_bypass` 是校准期间的临时放行；只在 program_mute 生效时才有意义。
//! - 安全故障/输入欠载的 `hard_mute` 优先级最高，立即清零，不淡出（fail-closed）。
//! - program mute 立即清零（块边界生效，不淡出）；解除静音时约 5 ms 线性淡入，避免咔哒声。
//!
//! 实时约束：`apply` 只做定长乘加/fill，无分配、无锁、无 IO、无日志、不 panic。

use crate::engine::MuteReceipt;

/// 默认淡入时长（毫秒）；静音方向立即生效。
pub const DEFAULT_FADE_MS: u32 = 5;

/// 输出级状态快照（控制面读取；字段都是块边界后的值）。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OutputStageStatus {
    /// 用户最近一次请求的静音状态（请求后、块边界前就可能为 true）。
    pub program_mute_requested: bool,
    /// 已生效的用户静音状态。
    pub program_mute_applied: bool,
    /// 校准旁路是否开启。
    pub calibration_bypass: bool,
    /// 当前生效的静音状态（program_mute && !bypass；不含安全故障）。
    pub muffled: bool,
    /// 最近一块是否因安全故障/输入欠载被硬清零。
    pub hard_muted: bool,
    /// 最近一次静音生效的块号（从 1 开始；未生效过为 0）。
    pub applied_block: u64,
    /// 已应用的块数。
    pub blocks: u64,
    /// 因 program mute（含淡出过程）而静音的块数。
    pub muted_blocks: u64,
    /// 因 hard_mute 而清零的块数。
    pub hard_muted_blocks: u64,
    /// 正常放行的块数。
    pub audible_blocks: u64,
    /// 淡入淡出帧数（每声道）。
    pub fade_frames: usize,
    /// 上一块结束时的增益（0..=1）。
    pub gain: f32,
}

/// 输出级：软件静音/校准旁路/安全静音的唯一输出真值源。
#[derive(Debug, Clone, PartialEq)]
pub struct OutputStage {
    channels: usize,
    fade_frames: usize,
    program_mute_requested: bool,
    program_mute_applied: bool,
    calibration_bypass: bool,
    pending_mute: Option<bool>,
    gain: f32,
    applied_block: u64,
    blocks: u64,
    muted_blocks: u64,
    hard_muted_blocks: u64,
    audible_blocks: u64,
    hard_muted: bool,
}

impl OutputStage {
    /// 新建输出级。`fade_ms == 0` 表示解除静音也立即切换（测试/特殊用途）。
    pub fn new(sample_rate: u32, channels: usize, fade_ms: u32) -> Self {
        let fade_frames = if fade_ms == 0 {
            0
        } else {
            ((u64::from(sample_rate) * u64::from(fade_ms)) / 1000) as usize
        };
        Self {
            channels: channels.max(1),
            fade_frames,
            program_mute_requested: false,
            program_mute_applied: false,
            calibration_bypass: false,
            pending_mute: None,
            gain: 1.0,
            applied_block: 0,
            blocks: 0,
            muted_blocks: 0,
            hard_muted_blocks: 0,
            audible_blocks: 0,
            hard_muted: false,
        }
    }

    /// 请求用户静音；下一块生效。回执的 applied_block 是即将开始的那一块。
    pub fn request_program_mute(&mut self, mute: bool) -> MuteReceipt {
        let effective = self.pending_mute.unwrap_or(self.program_mute_applied);
        self.program_mute_requested = mute;
        if effective == mute {
            return MuteReceipt {
                mute,
                applied_block: if self.pending_mute.is_some() {
                    self.blocks.saturating_add(1)
                } else {
                    self.applied_block
                },
                idempotent: true,
            };
        }
        self.pending_mute = Some(mute);
        MuteReceipt {
            mute,
            applied_block: self.blocks.saturating_add(1),
            idempotent: false,
        }
    }

    /// 校准旁路：为 true 时 program mute 不再清零输出（供扫频播放）。
    ///
    /// 只应在校准控制路径、已确认安全状态后调用；本方法不检查安全故障，
    /// 安全故障在 `apply` 里始终优先。
    pub fn set_calibration_bypass(&mut self, on: bool) {
        if on && !self.calibration_bypass {
            // 扫频必须从满幅开始，不能把校准信号也淡入。
            self.gain = 1.0;
        }
        self.calibration_bypass = on;
    }

    /// 校准旁路是否开启。
    pub fn calibration_bypass(&self) -> bool {
        self.calibration_bypass
    }

    /// 用户最近一次请求的静音状态。
    pub fn program_mute_requested(&self) -> bool {
        self.program_mute_requested
    }

    /// 已生效的用户静音状态。
    pub fn program_mute_applied(&self) -> bool {
        self.program_mute_applied
    }

    /// 当前是否处在“节目被软件静音”的状态（不含安全硬静音；校准旁路时返回 false）。
    pub fn muffled(&self) -> bool {
        self.program_mute_applied && !self.calibration_bypass
    }

    /// 淡入淡出帧数。
    pub fn fade_frames(&self) -> usize {
        self.fade_frames
    }

    /// 应用一块输出（原地处理）。`hard_mute` 为安全故障/输入欠载，必须立即清零。
    ///
    /// 块长度不是 channels 的整数倍时按 fail-closed 整块清零并计数（不 panic）。
    pub fn apply(&mut self, block: &mut [f32], hard_mute: bool) {
        self.blocks = self.blocks.saturating_add(1);
        if let Some(mute) = self.pending_mute.take() {
            self.program_mute_applied = mute;
            self.applied_block = self.blocks;
        }
        if let Some(hard) = self.apply_inner(block, hard_mute) {
            self.hard_muted = hard;
        }
    }

    /// 返回 Some(hard_muted) 表示正常应用；None 表示长度不合法、已清空。
    fn apply_inner(&mut self, block: &mut [f32], hard_mute: bool) -> Option<bool> {
        if self.channels == 0 || !block.len().is_multiple_of(self.channels) {
            block.fill(0.0);
            self.gain = 0.0;
            self.hard_muted = true;
            self.hard_muted_blocks = self.hard_muted_blocks.saturating_add(1);
            return None;
        }
        if hard_mute {
            block.fill(0.0);
            // 硬静音只清零本块，不重置 gain：输入欠载/安全故障恢复后应直接回到原有电平，
            // 而不是让第一块有效音频再淡入一次（会破坏逐位直通，也会吞掉恢复后的开头）。
            self.hard_muted = true;
            self.hard_muted_blocks = self.hard_muted_blocks.saturating_add(1);
            return Some(true);
        }
        self.hard_muted = false;
        let target = if self.program_mute_applied && !self.calibration_bypass {
            0.0
        } else {
            1.0
        };
        // 静音方向立即清零：mute 是用户/校准控制动作，不允许“半个块还有声”。
        if target == 0.0 {
            block.fill(0.0);
            self.gain = 0.0;
            self.muted_blocks = self.muted_blocks.saturating_add(1);
            return Some(false);
        }
        if target == 1.0 && self.gain == 1.0 {
            self.audible_blocks = self.audible_blocks.saturating_add(1);
            return Some(false);
        }
        let step = if self.fade_frames == 0 {
            1.0
        } else {
            1.0 / self.fade_frames as f32
        };
        let mut gain = self.gain;
        for frame in block.chunks_exact_mut(self.channels) {
            if gain < target {
                gain = (gain + step).min(target);
            } else if gain > target {
                gain = (gain - step).max(target);
            }
            if gain != 1.0 {
                for sample in frame {
                    *sample *= gain;
                }
            }
        }
        self.gain = gain;
        self.audible_blocks = self.audible_blocks.saturating_add(1);
        Some(false)
    }

    /// 状态快照（控制路径）。
    pub fn status(&self) -> OutputStageStatus {
        OutputStageStatus {
            program_mute_requested: self.program_mute_requested,
            program_mute_applied: self.program_mute_applied,
            calibration_bypass: self.calibration_bypass,
            muffled: self.muffled(),
            hard_muted: self.hard_muted,
            applied_block: self.applied_block,
            blocks: self.blocks,
            muted_blocks: self.muted_blocks,
            hard_muted_blocks: self.hard_muted_blocks,
            audible_blocks: self.audible_blocks,
            fade_frames: self.fade_frames,
            gain: self.gain,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mute_applies_at_next_block_immediately() {
        let mut stage = OutputStage::new(1000, 2, 5);
        let mut block = vec![1.0f32; 10];
        stage.apply(&mut block, false);
        assert_eq!(block, vec![1.0f32; 10]);
        assert!(!stage.muffled());

        let receipt = stage.request_program_mute(true);
        assert!(!receipt.idempotent);
        assert_eq!(receipt.applied_block, 2);
        // 请求后、块边界前仍是已生效的未静音值。
        assert!(!stage.muffled());

        let mut block = vec![1.0f32; 10];
        stage.apply(&mut block, false);
        assert!(stage.muffled());
        assert!(
            block.iter().all(|s| *s == 0.0),
            "mute must zero the applied block"
        );
        assert_eq!(stage.status().applied_block, 2);
    }

    #[test]
    fn unmute_fades_in_across_blocks() {
        let mut stage = OutputStage::new(1000, 1, 10);
        stage.request_program_mute(true);
        let mut block = vec![1.0f32; 4];
        stage.apply(&mut block, false);
        assert!(block.iter().all(|s| *s == 0.0));
        stage.request_program_mute(false);
        let mut block = vec![1.0f32; 4];
        stage.apply(&mut block, false);
        assert!(
            block[0] > 0.0 && block[0] < 1.0,
            "first sample must be attenuated"
        );
        assert!(block[3] > block[0], "gain increases over the block");
        let mut block2 = vec![1.0f32; 4];
        stage.apply(&mut block2, false);
        let mut block3 = vec![1.0f32; 4];
        stage.apply(&mut block3, false);
        let mut block4 = vec![1.0f32; 4];
        stage.apply(&mut block4, false);
        assert_eq!(block4, vec![1.0f32; 4]);
        assert!(!stage.muffled());
    }

    #[test]
    fn calibration_bypass_passes_program_mute() {
        let mut stage = OutputStage::new(48000, 2, 5);
        stage.request_program_mute(true);
        let mut muted = vec![0.5f32; 10];
        stage.apply(&mut muted, false);
        assert!(muted.iter().all(|s| *s == 0.0));
        stage.set_calibration_bypass(true);
        let mut block = vec![0.25f32; 256];
        stage.apply(&mut block, false);
        assert!(block.iter().all(|s| *s == 0.25));
        assert!(stage.program_mute_applied());
        assert!(!stage.muffled(), "bypass means audible output");
    }

    #[test]
    fn hard_mute_wins_and_preserves_gain() {
        let mut stage = OutputStage::new(48000, 1, 5);
        stage.set_calibration_bypass(true);
        let mut block = vec![1.0f32; 128];
        stage.apply(&mut block, true);
        assert!(block.iter().all(|s| *s == 0.0));
        assert!(stage.status().hard_muted);
        assert_eq!(stage.status().hard_muted_blocks, 1);
        // 解除 hard mute 后直接回到原电平（不额外淡入）。
        let mut block = vec![1.0f32; 128];
        stage.apply(&mut block, false);
        assert!(block.iter().all(|s| *s == 1.0));
    }

    #[test]
    fn odd_block_fails_closed() {
        let mut stage = OutputStage::new(48000, 2, 5);
        let mut block = vec![1.0f32; 7];
        stage.apply(&mut block, false);
        assert!(block.iter().all(|s| *s == 0.0));
        assert!(stage.status().hard_muted);
    }

    #[test]
    fn idempotent_request_reports_current_state() {
        let mut stage = OutputStage::new(1000, 1, 0);
        let first = stage.request_program_mute(true);
        assert!(!first.idempotent);
        let again = stage.request_program_mute(true);
        assert!(again.idempotent);
        assert_eq!(again.applied_block, first.applied_block);
        stage.request_program_mute(false);
        let mut block = vec![1.0f32; 1];
        stage.apply(&mut block, false);
        assert!(!stage.muffled());
    }
}
