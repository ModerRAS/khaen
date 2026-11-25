//! RT 可见的基础类型与契约常量（v2 唯一来源）。
//!
//! 这里是**跨 crate 共享的冻结布局**：块描述符 64 字节、align 64，字段偏移由下方
//! 编译期断言冻结。calibration / modal-design / host 都从这里引用，不允许各自镜像一份。
//!
//! v1 的 plan 头、ring、worker 统计等结构不迁入：v2 用方案文件 ABI v3 + 控制协议替代。

/// 块描述符的 ABI 版本；不兼容变化必须 +1。
pub const BLOCK_ABI_VERSION: u16 = 1;

/// MOTU UltraLite-mk5 采集通道上限。
pub const MAX_INPUT_CHANNELS: usize = 20;
/// MOTU UltraLite-mk5 播放通道上限。
pub const MAX_OUTPUT_CHANNELS: usize = 22;
/// 输入布局 role 数上限。
pub const MAX_INPUT_ROLES: usize = 16;
/// 输出容器/布局 role 数上限。
pub const MAX_OUTPUT_ROLES: usize = 16;
/// 低带 MIMO 显式参与者上限。
pub const MAX_LOW_BAND_PARTICIPANTS: usize = 8;
/// 支持的采样率。
pub const SAMPLE_RATES_HZ: [u32; 2] = [48_000, 96_000];
/// 支持的块大小。
pub const BLOCK_SIZES: [u16; 2] = [64, 128];
/// 默认块大小。
pub const DEFAULT_BLOCK_SIZE: u16 = 128;
/// 普通 profile 的 taps 上限。
pub const TAPS_NORMAL_MAX: u32 = 4096;

/// 块身份描述（64 字节，align 64）。
///
/// RT 侧只通过本结构判断时间轴、块边界与 plan 版本；字段偏移被编译期断言冻结，
/// 因此它可以在 RT 边界两侧按同一布局传递。
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockDesc {
    /// = `BLOCK_ABI_VERSION`。
    pub abi_version: u16,
    /// 块大小（帧）。
    pub block_size: u16,
    /// 本块输入通道数。
    pub n_in: u16,
    /// 本块输出通道数。
    pub n_out: u16,
    /// 时间轴 ID（单调递增、永不复用）。
    pub epoch_id: u64,
    /// epoch 内块序号（从 0 连续 +1）。
    pub block_seq: u64,
    /// 输入绝对帧号（= block_seq * block_size）。
    pub frame_index: u64,
    /// 产生本块的 plan 版本。
    pub plan_version: u32,
    /// 状态位；位定义归运行时（host），core 不解释。
    pub flags: u32,
    /// 有效帧数（0..=block_size）。
    pub valid_frames: u32,
    /// 缓冲槽号。
    pub slot_index: u32,
    /// 采集发布时刻（CLOCK_MONOTONIC，ns）。
    pub mono_publish_ns: u64,
    /// 本块必须被播放消费的绝对时刻（ns）。
    pub play_deadline_ns: u64,
}

impl BlockDesc {
    /// 全零占位（仅测试/scaffold 用，不得当作有效块发布）。
    pub const ZERO: Self = Self {
        abi_version: BLOCK_ABI_VERSION,
        block_size: 0,
        n_in: 0,
        n_out: 0,
        epoch_id: 0,
        block_seq: 0,
        frame_index: 0,
        plan_version: 0,
        flags: 0,
        valid_frames: 0,
        slot_index: 0,
        mono_publish_ns: 0,
        play_deadline_ns: 0,
    };

    /// 本块是否携带有效音频。
    pub fn is_valid(&self) -> bool {
        self.valid_frames > 0 && self.valid_frames <= u32::from(self.block_size)
    }
}

// 布局冻结：v1 的偏移一字不差地保留，跨 RT 边界按字节兼容。
const _: () = {
    assert!(core::mem::size_of::<BlockDesc>() == 64);
    assert!(core::mem::align_of::<BlockDesc>() == 64);
    assert!(core::mem::offset_of!(BlockDesc, epoch_id) == 8);
    assert!(core::mem::offset_of!(BlockDesc, block_seq) == 16);
    assert!(core::mem::offset_of!(BlockDesc, frame_index) == 24);
    assert!(core::mem::offset_of!(BlockDesc, plan_version) == 32);
    assert!(core::mem::offset_of!(BlockDesc, flags) == 36);
    assert!(core::mem::offset_of!(BlockDesc, valid_frames) == 40);
    assert!(core::mem::offset_of!(BlockDesc, slot_index) == 44);
    assert!(core::mem::offset_of!(BlockDesc, mono_publish_ns) == 48);
    assert!(core::mem::offset_of!(BlockDesc, play_deadline_ns) == 56);
    assert!(!core::mem::needs_drop::<BlockDesc>());
};

#[cfg(test)]
mod tests {
    use super::BlockDesc;

    #[test]
    fn zero_descriptor_is_invalid() {
        let zero = BlockDesc::ZERO;
        assert_eq!(zero.abi_version, super::BLOCK_ABI_VERSION);
        assert!(!zero.is_valid());
        let mut desc = zero;
        desc.block_size = 128;
        desc.valid_frames = 128;
        assert!(desc.is_valid());
        desc.valid_frames = 129;
        assert!(!desc.is_valid());
    }

    #[test]
    fn limits_match_the_hardware_contract() {
        assert_eq!(super::MAX_INPUT_CHANNELS, 20);
        assert_eq!(super::MAX_OUTPUT_CHANNELS, 22);
        assert_eq!(super::SAMPLE_RATES_HZ, [48_000, 96_000]);
        assert_eq!(super::BLOCK_SIZES, [64, 128]);
        assert!(super::BLOCK_SIZES.contains(&super::DEFAULT_BLOCK_SIZE));
    }
}
