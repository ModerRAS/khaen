//! 方案文件测试夹具：apply/rollback 需要真实、可准入的方案文件。
//!
//! 与 tests/rest.rs 里的夹具同形（identity 全直通方案，图执行测试的判据见
//! crates/host/tests/graph_executor.rs）；T36 的 apply/rollback 端到端测试复用它，
//! 保证 daemon 的 prepare→commit 走的是真实控制面而不是 mock。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use khaen_host::plan::{
    ContentSection, CrossoverFilter, CrossoverSection, Fold, MatrixSection, Plan, ProtectionEntry,
    ProtectionSection, RoutingSection,
};
use khaen_host::sha256_hex;

/// 测试方案采样率。
pub const SAMPLE_RATE: u32 = 96_000;
/// 测试方案块长。
pub const BLOCK_FRAMES: u32 = 128;
/// 测试方案声道数。
pub const CHANNELS: u16 = 2;

/// 全恒等方案（逐样本直通）。
pub fn identity_plan() -> Plan {
    let lanes = CHANNELS;
    let count = CHANNELS as usize;
    let mut matrix_coeffs = vec![0.0f32; count * count];
    for lane in 0..count {
        matrix_coeffs[lane * count + lane] = 1.0;
    }
    Plan {
        block_size: BLOCK_FRAMES,
        sample_rate: SAMPLE_RATE,
        content: Some(ContentSection {
            in_channels: CHANNELS,
            out_lanes: lanes,
            folds: (0..lanes)
                .map(|lane| Fold {
                    src: lane,
                    dst: lane,
                    gain: 1.0,
                })
                .collect(),
        }),
        crossover: Some(CrossoverSection {
            lanes,
            filters: (0..lanes)
                .map(|lane| CrossoverFilter {
                    lane,
                    coeffs: vec![1.0],
                })
                .collect(),
        }),
        matrix: Some(MatrixSection {
            rows: lanes,
            cols: lanes,
            decimation: 1,
            interpolation: 1,
            taps: 1,
            coeffs: matrix_coeffs,
        }),
        protection: Some(ProtectionSection {
            lanes,
            entries: (0..lanes)
                .map(|lane| ProtectionEntry {
                    lane,
                    delay: 0,
                    polarity: 1,
                    gain_db: 0.0,
                    peq: Vec::new(),
                })
                .collect(),
        }),
        routing: Some(RoutingSection {
            dac_channels: CHANNELS,
            routes: (0..lanes).map(|lane| (lane, lane)).collect(),
        }),
    }
}

/// identity 方案 + protection 每 lane 加 `gain_db` dB（用来造两份内容不同的方案）。
pub fn gain_plan(gain_db: f32) -> Plan {
    let mut plan = identity_plan();
    if let Some(protection) = &mut plan.protection {
        for entry in &mut protection.entries {
            entry.gain_db = gain_db;
        }
    }
    plan
}

/// 临时方案文件（Drop 时删除）。
pub struct TempPlan {
    path: PathBuf,
    sha256: String,
}

impl TempPlan {
    /// 写入一份方案文件并算出 sha256。
    pub fn new(tag: &str, bytes: &[u8]) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "khaen-t36-plan-{}-{}-{}.plan",
            std::process::id(),
            tag,
            unique
        ));
        std::fs::write(&path, bytes).expect("write temp plan");
        Self {
            path,
            sha256: sha256_hex(bytes),
        }
    }

    /// 路径字符串（REST 载荷/服务引用用）。
    pub fn path(&self) -> String {
        self.path.display().to_string()
    }

    /// 方案文件 sha256。
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// 方案文件路径。
    pub fn as_path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempPlan {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
