//! TSPI RT-first DSP 自研 host 入口（WBS **T26**；crate `khaen-rt-host` bin）。
//!
//! 本文件只做**串行入口集成**：把已完成的 leaf 模块接到一个 host 进程里，不代替它们、
//! 不重写 kernel/调度/协议。默认 `backend=fake` 不打开真实 ALSA 设备；`backend=alsa`
//! 在 Linux target 打开真实 MOTU duplex 播放（T71 前置；真实设备/板端验证 NOT_RUN）。
//! `capture.source=file`（T74 前置）把预渲染 WAV/raw 多声道文件按 block 喂进同一
//! graph/plan-arena/ALSA 链路（2ch=2.0 strict / 8ch=4.1.2 strict，48k，S24_3LE|f32）。
//! `capture.source=motu|inferno|aes67`（T74 前置）把 T43 AoIP 端点接进 capture 路径：
//! motu = MOTU Toslink ch12/13（既有 loopback 升格），inferno = pcm.inferno（96k/S32_LE/
//! 16ch/PTPv1），aes67 = RAVENNA LKM（48k/S32_LE/L24/PTPv2）；mode（off|toslink|aes67|dante）
//! 与 source 一一对应并互斥校验；SDP/角色映射/失流（停滞静音、持续失流 fail closed）复用 T43。
//! 真实 daemon/PTP/网络与板端出声 NOT_RUN（evidence/cursor-fix-capturesrc/）。
//!
//! **T239 用户态 AES67 传输**：`[capture] aes67_transport=userspace` 时 source=aes67 改走
//! 自研用户态接收端（crates/aoip-aes67：SDP/组播/单播 RTP L24/抖动缓冲 → 128 帧块 →
//! H16 容器/角色显式映射 → planar f32），不依赖 aes67-daemon/RAVENNA LKM；与 ravenna
//! 传输互斥（同一 source/mode 只有一个 capture owner），失流/停滞语义复用 T43
//! StreamMonitor（停滞整块静音、持续失流 RT_E_DEVICE_LOST fail closed）；backend=fake 时
//! 由 userspace capture 自带块节拍（48k/128=2.667ms，无设备时钟的静音链路测试）。
//! 真实板端网卡/IGMP/PTP、电平/听音 NOT_RUN（evidence/aes67-host-wire/）。
//!
//! **T203/T210 中置下混内容适配**：graph=plan-arena 播放链在 capture 帧进图之前接入
//! `khaen_rt_host::content_map`（**host 自持**；task-210 把该适配从 compat 依赖移入 rt-host，
//! rt-host 不再依赖/调用 compat 的 S1 兼容层）——[content] 声明 T57 输入/输出布局后，
//! 缺中置输出布局（如 4.1.2，无 C 角色）把 5.1 内容的 C 以 −3 dB 预混进 FL/FR 并把 C 源
//! 置零（D7 headroom 超门时全局 trim）；有中置（布局含 C）或无中置内容时逐位 passthrough；
//! plan 的 A_map/graph/RT 语义不变。旧 2ch 图不做内容适配（显式拒绝 [content]）。
//! 真实板端出声 NOT_RUN（evidence/cursor-fix-v1-hostmap/、evidence/center-downmix-host/）。
//!
//! - `fake-io`：fake 设备闭环（合成输入 → 4 worker 静态 DAG → S32_3LE 设备字节），
//!   覆盖 T16 SPSC 有界 desc 环、T19 tick 预算、T22 format、T09 worker phase DAG；
//! - `selftest`：同一 fake 管线与 T07 `direct_block_f32` 逐位对拍（4 worker DAG
//!   必须与直算 bit-exact），admission/format 前置校验全绿；
//! - `admission-check`：T18 admission 容量/预算 + T22 format 契约 + T45 scheduler
//!   参数自校验（只读，不写内核）。
//!
//! file 端到端（WAV → plan → WAV）由 `rt-check host-file` 驱动 T14 file-runner 的
//! `BlockSource`/`runner::run`（rt-host crate 不能反向依赖 file-runner，见 T14
//! Cargo 依赖方向；host 与 runner 共用同一输入切分，见 T14 source.rs）。本 bin 的
//! `fake-io` 负责“唯一 I/O owner + 4 线程 DAG”这一段真实执行。
//! T74 前置：`run` 的 `[capture] source=file` 让生产入口直接读预渲染 WAV/raw（多声道
//! 48k）驱动同一 graph/plan-arena → ALSA/fake 链路（本文件内含独立小 WAV 解码器，无第三方
//! 依赖；真实板端出声/听音仍 NOT_RUN）。
//!
//! **F-12（P1）块循环零分配/不新建线程**：`fake-io`/`selftest`/`run` 均在启动时一次性
//! 创建 `BlockWorkers`（常驻 B2 worker + 预分配 partials/merged），块循环只借用/复用
//! 缓冲并经纯原子闸门同步；回归断言见 `tests::t26_block_loop_zero_alloc_and_no_new_threads`。
//!
//! **F-26（P2）swap 证据真实性**：`run` 的 swap 段不再用循环 `ack_worker` 合成 ack——`retire_ready`
//! 只由**真实 block all-ack** 置位：`run_block_4thread` 只有 B1 每 worker OK、B2 全 done 且
//! status OK、B3 每 worker OK 才返回 Ok，据此生成 `TspiBlockAckV1`（三 phase 位图 =
//! ACK_MASK(expected)、cutoff=0）并交唯一 RT 入口 `TspiSwapAckV1::note_block_all_ack`。
//! 后半程 plan 切换改为**真实** T20 路径：同图 re-plan v2 -> control 侧 arm -> master block
//! 边界 `commit_swap_at_block_boundary` -> 旧图 v1 入 `GraphQuarantine` -> v2 首块 all-ack
//! 经 `note_block_switched` 清 quarantine 位 -> 退出路径 `try_release`。JSON 以
//! `swap.synthetic=false` / `retire_ready_source=note_block_all_ack` 显式标注来源；不存在
//! synthetic/demo ack 冒充真实 ack（回归 `tests::t26_real_all_ack_*`）。
//! task-284：partitioned（单执行器、无 worker 线程）在真实完成整块后同样走唯一 RT 入口
//! `note_block_all_ack`（三 phase 位图 = ACK_MASK(expected)），报告 `swap.single_executor=true`
//! + `swap.executor="partitioned-fft-upols"` 明示"单执行器覆盖 plan 全部 worker lane"的口径，
//! 不合成 synthetic ack、不写 4 线程语义。
//!
//! **T71 前置接线**：`backend=alsa` 走 T17 `LinuxAlsaBackend`（MOTU hw:4,0 / S24_3LE /
//! 22ch / 48k，禁 192k；loopback=Toslink ch12/13 或 synth 合成输入，safe_mode=mute 写
//! 全零 period），xrun 计数与 T21 事件环真实上报（§8.2：underrun/overrun severity=3 +
//! consecutive/capacity；safety mute severity=4 + 原因码/epoch_id）。恢复耗尽进入 Muted
//! 后按 SafetyMute「不处理新块」：不读 capture、不跑 graph，只写预分配静音并计数，
//! 不把 ERR_SWITCH_BUSY 当不可恢复；仅 Faulted 才 fail closed（exit 1）。
//!
//! **F6/task-216 RT 内核应用**：`[rt] apply_affinity=true` 时启动期真实写 mlockall/
//! SCHED_FIFO（I/O 优先级高于 worker）/worker 亲和（T45 计划 CPU），写后读回校验、失败
//! fail closed；sched JSON 用 `kernel_applied` + `kernel_apply` 真实上报。worker 块间等待
//! 改为有界自旋 + park（不再 4 核无条件忙等）。真实 Linux syscall 本机未执行（NOT_RUN，
//! aarch64/x86_64 Linux target cargo check + Mock 单测）；证据 evidence/cursor-fix-t45-apply/。
//!
//! **明确未做（NOT_RUN）**：真实设备/板端读写与听音、RT 时限/soak、IRQ 线程亲和/优先级、
//! Swap 交叉淡入、控制面 REST/WS、file 播放的真实 MOTU 时轴/听音。证据见 evidence/T26/、
//! evidence/cursor-fix-alsa-backend/、evidence/cursor-fix-fileinput/。

use std::env;
use std::marker::PhantomData;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use khaen_core::abi::{
    plan_abi_supported, sample_format, TspiBlockAckV1, TspiBlockDescV1, TspiHwParamsV1,
    TspiPlanHeaderV1, TspiRoleIirV2, ABI_VERSION, PLAN_IIR_ENTRY_BYTES, PLAN_IIR_HEADER_BYTES,
    PLAN_IIR_SECTION_COEFFS, TSPI_PLAN_ABI_V1, TSPI_PLAN_ABI_V2, TSPI_PLAN_MAGIC,
};
use khaen_core::direct::{
    direct_block_f32, DirectCoeffsF32, DirectLayout, DirectTopology, DIRECT_MAX_PARTICIPANTS,
    DIRECT_MAX_ROLES, DIRECT_NO_SOURCE,
};
use khaen_core::partitioned::{
    partitioned_block_f32, PartitionedLayout, PartitionedPlan, PARTITIONED_MAX_TAPS_EXTENDED,
};
use khaen_core::rt::{PlanView, RtStatus};
use khaen_core::workers::{
    ack_mask, worker_phase_b1, worker_phase_b2, worker_phase_b3, WorkerDagPlan, WorkerPartials,
    WORKER_REDUCE_LANES,
};
use khaen_rt_host::admission::AdmissionEnv;
use khaen_rt_host::aoip::{self, H16Role, RoleSelection};
use khaen_rt_host::clock::{self, LockState, TickBudget};
use khaen_rt_host::content_map::{
    CenterDownmixConfig, CenterDownmixPlan, CenterDownmixSource, CenterDownmixTarget,
    CenterSpeakerState, LayoutPresets, CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS,
};
use khaen_rt_host::format::{self, DitherCfg, FormatConfig};
use khaen_rt_host::scheduler::{SchedPlan, CPU_UNUSED, DSP_CPU_MAX};
use khaen_rt_host::spsc::{
    validate_block_desc, Consumer, DescSlot, Producer, FLAG_EPOCH_FIRST, FLAG_VALID, SPSC_CAPACITY,
    SPSC_ENTRY_BYTES,
};
// task-320：AES67/组播监听面（接口列表/组白名单/来源白名单；启动期解析与 fail-closed 校验）。
use khaen_aoip_aes67::iface::IfaceLookupError;
use khaen_aoip_aes67::listen::{GroupAllow, Ipv4Cidr, ReceiverInterfaces, SourceAllow};

// F6/task-216：启动期真实 RT 内核应用（mlockall/SCHED_FIFO/亲和；NONRT，块循环内不再调用）。
mod rt_apply;

/// host 常量：生产块长/采样率/worker 数/plan 版本/epoch（fake 路径固定值）。
const BLOCK: u16 = 128;
const SAMPLE_RATE: u32 = 48_000;
const WORKERS: u8 = 4;
const PLAN_VERSION: u32 = 1;
const EPOCH_ID: u64 = 1;
/// master 每 block 预算内 tick 读取上限（core-v1 I04 = 3）。
const TICK_BUDGET: u32 = 3;

/// graceful stop 请求（SIGINT/SIGTERM 置位；run 循环与测试共用）。
static STOP_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
}

#[cfg(unix)]
extern "C" fn on_stop_signal(_signum: i32) {
    STOP_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// 安装 SIGINT/SIGTERM -> STOP_REQUESTED（仅 Unix；Windows 用 --blocks/--duration-ms）。
fn install_stop_signals() {
    #[cfg(unix)]
    unsafe {
        let _ = signal(2, on_stop_signal as usize);
        let _ = signal(15, on_stop_signal as usize);
    }
    STOP_REQUESTED.store(false, std::sync::atomic::Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// 图：factorized 拓扑 + f32 系数（T12/T26 适配的 host 侧最小图；不是设计器）
// ---------------------------------------------------------------------------

/// 单段 biquad（plan ABI v2 role IIR；a0 已归一化为 1）。
///
/// `y[n] = b0 x[n] + b1 x[n-1] + b2 x[n-2] - a1 y[n-1] - a2 y[n-2]`
#[derive(Clone, Copy, Debug, PartialEq)]
struct RoleBiquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

/// plan ABI v2 per-role IIR 级联（GAP-T66-3；离线解码结果，启动期一次构建）。
///
/// 每容器 0..=32 段，sections 连续存放（容器 c 的段为 `[starts[c], starts[c]+counts[c])`）。
/// RT 块循环只读本结构 + 固定状态 `Vec<f32>`，零分配。
#[derive(Clone, Debug, Default)]
struct RoleIirCascade {
    sections: Vec<RoleBiquad>,
    starts: [u32; DIRECT_MAX_CONTAINERS],
    counts: [u16; DIRECT_MAX_CONTAINERS],
}

impl RoleIirCascade {
    fn empty() -> Self {
        Self {
            sections: Vec::new(),
            starts: [0; DIRECT_MAX_CONTAINERS],
            counts: [0; DIRECT_MAX_CONTAINERS],
        }
    }

    fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    fn section_count(&self) -> usize {
        self.sections.len()
    }

    fn role_count(&self) -> usize {
        self.counts.iter().filter(|count| **count != 0).count()
    }

    /// 每段 Transposed-DF2 需要 2 个状态（s1/s2）。
    fn state_len(&self) -> usize {
        self.sections.len() * 2
    }
}

/// [RT] 对一段 biquad 跑一个 block（Transposed Direct Form II，2 state/段）。
#[inline]
fn role_biquad_block(section: &RoleBiquad, state: &mut [f32], samples: &mut [f32]) {
    let mut s1 = state[0];
    let mut s2 = state[1];
    for value in samples.iter_mut() {
        let x = *value;
        let y = section.b0 * x + s1;
        s1 = section.b1 * x - section.a1 * y + s2;
        s2 = section.b2 * x - section.a2 * y;
        *value = y;
    }
    state[0] = s1;
    state[1] = s2;
}

/// [RT] 每容器按段顺序跑 role IIR 级联（plan ABI v2；块内零分配/零锁）。
///
/// 调用点：引擎（direct/partitioned）产出 `output` 后、写 sink 前；SafetyMute/gate 块不调用
/// （不消费新块，也不推进 IIR 状态，与引擎状态口径一致）。
fn apply_role_iir_block(
    iir: &RoleIirCascade,
    state: &mut [f32],
    output: &mut [f32],
    stride: usize,
    containers: usize,
    block: usize,
) {
    if iir.is_empty() || block == 0 || containers > DIRECT_MAX_CONTAINERS {
        return;
    }
    let mut state_pos = 0usize;
    let mut container = 0usize;
    while container < containers {
        let count = iir.counts[container] as usize;
        let start = iir.starts[container] as usize;
        let mut index = 0usize;
        while index < count {
            let section = iir.sections[start + index];
            let base = container * stride;
            let end = base + block;
            if end > output.len() || state_pos + 2 > state.len() {
                return;
            }
            role_biquad_block(&section, &mut state[state_pos..state_pos + 2], &mut output[base..end]);
            state_pos += 2;
            index += 1;
        }
        container += 1;
    }
}

/// 最小 factorized 图（拓扑 + A/G/P f32 系数 + plan ABI v2 role IIR）。
struct Graph {
    topo: DirectTopology,
    a: Vec<f32>,
    g: Vec<f32>,
    p: Vec<f32>,
    iir: RoleIirCascade,
}

impl Graph {
    fn coeffs(&self) -> DirectCoeffsF32<'_> {
        DirectCoeffsF32 {
            a_fir: &self.a,
            g: &self.g,
            p: &self.p,
        }
    }

    fn layout(&self) -> Result<DirectLayout, RtStatus> {
        DirectLayout::build(&self.topo)
    }

    /// 4 worker 静态 DAG（T09 `WorkerDagPlan::build`）。
    fn plan(&self) -> Result<WorkerDagPlan, RtStatus> {
        WorkerDagPlan::build(&self.topo, BLOCK, WORKERS, PLAN_VERSION)
    }

    /// L_F = L_A + L_G + L_P - 2（T03 SPEC §3.3），flush 零 = L_F - 1。
    fn total_taps(&self) -> usize {
        self.topo.l_a as usize + self.topo.l_g as usize + self.topo.l_p as usize - 2
    }

    fn flush_zeros(&self) -> usize {
        self.total_taps() - 1
    }
}

fn blank_topo(
    n_src: u16,
    n_role: u16,
    container: u16,
    d: u32,
    l_a: u16,
    l_g: u16,
    l_p: u16,
    m: u16,
) -> DirectTopology {
    DirectTopology {
        n_src,
        n_role,
        container_channels: container,
        d_samples: d,
        l_a,
        l_g,
        l_p,
        m,
        src_of_role: [DIRECT_NO_SOURCE; DIRECT_MAX_ROLES],
        participants: [DIRECT_NO_SOURCE; DIRECT_MAX_PARTICIPANTS],
        scatter: [0u16; DIRECT_MAX_ROLES],
    }
}

/// stereo 直通图（G=I 由 B2 的 delta_D baseline 提供，g=0、A=P=delta；D=0）。
fn graph_stereo_identity() -> Graph {
    let m = 2usize;
    let mut topo = blank_topo(2, 2, 2, 0, 1, 1, 1, m as u16);
    topo.src_of_role[0] = 0;
    topo.src_of_role[1] = 1;
    topo.participants[0] = 0;
    topo.participants[1] = 1;
    topo.scatter[0] = 0;
    topo.scatter[1] = 1;
    Graph {
        topo,
        a: vec![1.0; 2],
        g: vec![0.0; m * m],
        p: vec![1.0; 2],
        iir: RoleIirCascade::empty(),
    }
}

/// 2x2 off-diagonal + D=1 + 尾部（T09 `graph_2x2_offdiag` 同构图，用于 selftest 对拍）。
fn graph_2x2_offdiag() -> Graph {
    let m = 2usize;
    let l_g = 2usize;
    let mut topo = blank_topo(2, 2, 2, 1, 1, l_g as u16, 2, m as u16);
    topo.src_of_role[0] = 0;
    topo.src_of_role[1] = 1;
    topo.participants[0] = 0;
    topo.participants[1] = 1;
    topo.scatter[0] = 0;
    topo.scatter[1] = 1;
    let mut g = vec![0.0f32; m * m * l_g];
    // g[(row*m+col)*l_g + tap]
    g[(0 * m + 1) * l_g + 0] = 0.5;
    g[(1 * m + 0) * l_g + 0] = 0.25;
    g[(1 * m + 1) * l_g + 1] = 0.5;
    Graph {
        topo,
        a: vec![1.0, 1.0],
        g,
        p: vec![1.0, -1.0, 1.0, 0.0],
        iir: RoleIirCascade::empty(),
    }
}

// ---------------------------------------------------------------------------
// 4 worker DAG：B1/master 串行 → B2 四线程并行 → B3/master 串行（T09 phase API）
// ---------------------------------------------------------------------------

/// 一次 block 的执行统计。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BlockStats {
    /// B2 常驻并行 worker 数（4 worker profile 为 4；2 核降级 profile 为 2）。
    pub b2_threads: u8,
    /// 各 worker B2 都返回 OK。
    pub all_ok: bool,
}

/// B2 master 与常驻 worker 之间的 RT 闸门（纯原子；无锁 / 无堆分配 / 无 syscall）。
struct B2Gate {
    /// master 每发布一个 block +1（Release）；worker 以 Acquire 等待新 generation。
    generation: AtomicU32,
    /// 本 block 已完成 B2 的 worker 数（worker Release 累加，master Acquire 等待）。
    done: AtomicU32,
    /// 各 worker 本 block 的 B2 状态码（0 = OK；worker Release 写，master done 之后读）。
    statuses: [AtomicU32; WORKERS as usize],
    /// 任一 worker panic（RT 不变式破坏）后置位：master 不再死等（等价旧实现 join 失败）。
    worker_dead: AtomicBool,
    /// master 请求停机（Drop 时置位并唤醒 worker 退出）。
    shutdown: AtomicBool,
    /// 本 block B2 只读的 state 视图（B1 完成后发布，B2 结束前 master 不再写 state）。
    state_ptr: AtomicPtr<f32>,
    state_len: AtomicUsize,
    valid_frames: AtomicUsize,
    /// 累计创建的 worker 线程数（启动后不得再增长；单测核查断言）。
    spawned_threads: AtomicU64,
    /// 本 block B2 只读的 plan（F-26：master 在两块之间可切到新 plan_version；
    /// Release 发布，与 generation 构成同一发布序）。
    plan_ptr: AtomicPtr<WorkerDagPlan>,
    /// F6/task-216：worker 启动期 RT 配置完成数（SCHED_FIFO+亲和；块循环内不再变化）。
    rt_ready: AtomicU32,
    /// F6/task-216：任一 worker RT 配置失败（启动期 fail-closed 依据）。
    rt_failed: AtomicBool,
    /// F6/task-216：每 worker RT 失败编码（rt_apply::ApplyError::encode；0 = 无失败）。
    rt_errors: [AtomicU32; WORKERS as usize],
    /// F6/task-216：块间 gate 等待进入 park 的次数（自旋策略回归证据；热路径一次 Relaxed）。
    gate_parks: AtomicU64,
}

impl B2Gate {
    fn new() -> Self {
        Self {
            generation: AtomicU32::new(0),
            done: AtomicU32::new(0),
            statuses: std::array::from_fn(|_| AtomicU32::new(0)),
            worker_dead: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            state_ptr: AtomicPtr::new(std::ptr::null_mut()),
            state_len: AtomicUsize::new(0),
            valid_frames: AtomicUsize::new(0),
            spawned_threads: AtomicU64::new(0),
            plan_ptr: AtomicPtr::new(std::ptr::null_mut()),
            rt_ready: AtomicU32::new(0),
            rt_failed: AtomicBool::new(false),
            rt_errors: std::array::from_fn(|_| AtomicU32::new(0)),
            gate_parks: AtomicU64::new(0),
        }
    }
}

/// 常驻 B2 worker 的只读共享视图（系数/partials arena 在 start 时固定；plan 经
/// `B2Gate.plan_ptr` 发布，支持 F-26/T20 的 block 边界 plan 切换）。
struct B2Shared<'a> {
    coeffs: DirectCoeffsF32<'a>,
    /// 启动时一次分配的 n_workers 份 WorkerPartials arena；worker i 只写第 i 槽。
    partials: *mut WorkerPartials,
    /// F6/task-216：worker 启动期 RT 目标（None = 不应用；spawn 后只读）。
    worker_rt: [Option<rt_apply::ThreadRtTarget>; WORKERS as usize],
    gate: B2Gate,
}

// SAFETY：系数在 start 之后只读；plan 切换只写 `B2Gate.plan_ptr` 原子（Release）且切换
// 发生在 master 等到全部 worker done 之后（无 worker 正在读旧 plan 的 block 内窗口）；
// partials arena 每槽只被唯一 worker 写、master 只在 done 屏障（Acquire）之后读；跨线程
// 同步全部走 B2Gate 原子（generation/plan_ptr/done 的 Release-Acquire 配对）。
unsafe impl Sync for B2Shared<'_> {}

/// [启动路径] worker 线程入口（每 worker 只 spawn 一次；块循环内不再创建线程）。
fn b2_worker_entry(shared_ptr: usize, index: usize) {
    // SAFETY：shared_ptr 指向 BlockWorkers 持有的 Box<B2Shared>；BlockWorkers::drop
    // 先 shutdown + join 全部 worker，之后才释放该 Box，故引用在 worker 生命周期内有效。
    let shared = unsafe { &*(shared_ptr as *const B2Shared<'_>) };
    shared.gate.spawned_threads.fetch_add(1, Ordering::SeqCst);
    // F6/task-216：启动期一次写入本 worker 的 RT（SCHED_FIFO + 计划 CPU 亲和，读回校验）；
    // 失败不静默：把 stage/errno 带回父线程，由 BlockWorkers::start_rt fail closed。
    if let Some(target) = shared.worker_rt[index] {
        if let Err(error) = rt_apply::apply_worker_thread(target) {
            shared.gate.rt_errors[index].store(error.encode(), Ordering::Release);
            shared.gate.rt_failed.store(true, Ordering::Release);
            return;
        }
    }
    shared.gate.rt_ready.fetch_add(1, Ordering::Release);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        b2_worker_body(shared, index);
    }))
    .is_err();
    if panicked {
        // 等价旧 std::thread::scope join 失败 → RT_E_INVARIANT：发布状态并计 done，
        // 避免 master 在块循环里死等（本次 block 返回错误后不在错误状态继续跑）。
        shared.gate.statuses[index].store(RtStatus::RT_E_INVARIANT.0, Ordering::Release);
        shared.gate.worker_dead.store(true, Ordering::Release);
        shared.gate.done.fetch_add(1, Ordering::Release);
    }
}

/// 块间 gate 等待：先有界自旋，再进入有上界的 park_timeout。
///
/// F6 根因是"4 个 worker 无条件纯自旋占满 4 核"：块间等待期没有任何有效工作，纯 FIFO 自旋
/// 会 100% 占用 RT 预算（板端 sched_rt_runtime_us=950000 → 95% 触发 RT throttling）且让
/// I/O 主循环拿不到 CPU。改为：只在 gate 活跃期（generation 刚发布/即将发布）自旋，
/// 非活跃期 park 让出 CPU；master 每块发布 generation 后 unpark 唤醒（token 语义不丢唤醒）。
const WORKER_GATE_SPIN_ITERS: u32 = 256;
/// park 等待上界（防 master 异常时 worker 永久睡眠；1 ms 远小于 2.67 ms period 的 1/2）。
const WORKER_GATE_PARK_TIMEOUT_MS: u64 = 1;

/// [RT] 常驻 worker 主循环：等 generation → 读发布的 state 视图 → B2 → 回报。
fn b2_worker_body(shared: &B2Shared<'_>, index: usize) {
    let coeffs = &shared.coeffs;
    // SAFETY：index < n_workers（start 校验），arena 由 BlockWorkers 持有且 join 前不释放。
    let partials = unsafe { &mut *shared.partials.add(index) };
    let mut seen_generation = 0u32;
    let mut spins = 0u32;
    loop {
        // [RT] 块间等待：有界自旋 + 有上界 park（不取锁、不堆分配；park 只在空闲期发生）。
        let generation = loop {
            let current = shared.gate.generation.load(Ordering::Acquire);
            if shared.gate.shutdown.load(Ordering::Acquire) {
                return;
            }
            if current != seen_generation {
                break current;
            }
            if spins < WORKER_GATE_SPIN_ITERS {
                spins += 1;
                std::hint::spin_loop();
            } else {
                spins = 0;
                shared.gate.gate_parks.fetch_add(1, Ordering::Relaxed);
                std::thread::park_timeout(core::time::Duration::from_millis(
                    WORKER_GATE_PARK_TIMEOUT_MS,
                ));
            }
        };
        seen_generation = generation;
        spins = 0;
        // F-26：plan 与 generation 同一发布序——master 先 Release 存 plan_ptr，下一 block
        // 再 Release 递增 generation；worker Acquire generation 后 Acquire 读 plan_ptr，
        // 因此 plan v1->v2 切换时 worker 无需重启（切换在 block 边界、全部 worker done 之后）。
        // SAFETY：plan_ptr 指向 run_command/测试持有的 `WorkerDagPlan`，其生命周期覆盖
        // BlockWorkers（Drop 先 shutdown+join，之后调用方才释放 plan）。
        let plan = unsafe { &*shared.gate.plan_ptr.load(Ordering::Acquire) };
        // 先 Acquire generation，再读其前 Release 发布的 state 视图（Relaxed 即可见）。
        let state_ptr = shared.gate.state_ptr.load(Ordering::Relaxed);
        let state_len = shared.gate.state_len.load(Ordering::Relaxed);
        let valid_frames = shared.gate.valid_frames.load(Ordering::Relaxed);
        // SAFETY：B2 窗口内 master 只在 done 屏障等 worker、不写 state；state 由调用方
        // 保持存活（run_block_4thread 的借用参数）到 done 屏障之后。
        let state = unsafe { std::slice::from_raw_parts(state_ptr, state_len) };
        let status = worker_phase_b2(plan, index as u8, coeffs, state, partials, valid_frames);
        shared.gate.statuses[index].store(status.0, Ordering::Release);
        shared.gate.done.fetch_add(1, Ordering::Release);
    }
}

/// 常驻 B2 worker 执行器（F-12）：start 时一次分配 partials/merged 并创建 worker 线程，
/// 之后块循环只借用/复用同一批缓冲；Drop 置 shutdown 并 join。
struct BlockWorkers<'a> {
    shared: Box<B2Shared<'a>>,
    handles: Vec<std::thread::JoinHandle<()>>,
    partials: Box<[WorkerPartials]>,
    merged: WorkerPartials,
    n_workers: u8,
    /// 当前生效 plan（块循环内只读；`switch_plan` 只在 block 边界替换，F-26/T20）。
    current_plan: &'a WorkerDagPlan,
    // 生命周期约束：start 保存的 plan/系数引用必须活过执行器（借用检查器强制）。
    _plan_lifetime: PhantomData<&'a WorkerDagPlan>,
    _coeff_lifetime: PhantomData<&'a [f32]>,
}

/// BlockWorkers 启动失败（带 RtStatus 与可读细节；RT 应用失败必须能说出 stage/errno）。
struct BlockWorkersStartError {
    status: RtStatus,
    message: String,
}

impl<'a> BlockWorkers<'a> {
    /// [启动/准入，非块循环] 一次分配 n_workers 份 partial（每份 45 KB）并创建
    /// n_workers 个常驻 worker；块循环内不再分配、不再建线程。
    fn start(plan: &'a WorkerDagPlan, coeffs: &'a DirectCoeffsF32<'a>) -> Result<Self, RtStatus> {
        Self::start_inner(plan, coeffs, None).map_err(|error| error.status)
    }

    /// [启动/准入] 同上，且对每个 worker 真实写 RT（SCHED_FIFO + 计划 CPU 亲和）；
    /// 任一 worker 写入/读回校验失败 → fail closed（返回含 stage/errno 的错误，不静默降级）。
    fn start_rt(
        plan: &'a WorkerDagPlan,
        coeffs: &'a DirectCoeffsF32<'a>,
        worker_rt: [Option<rt_apply::ThreadRtTarget>; WORKERS as usize],
    ) -> Result<Self, String> {
        Self::start_inner(plan, coeffs, Some(worker_rt)).map_err(|error| error.message)
    }

    fn start_inner(
        plan: &'a WorkerDagPlan,
        coeffs: &'a DirectCoeffsF32<'a>,
        worker_rt: Option<[Option<rt_apply::ThreadRtTarget>; WORKERS as usize]>,
    ) -> Result<Self, BlockWorkersStartError> {
        let n_workers = plan.expected_workers;
        if n_workers == 0 || n_workers as usize > WORKERS as usize {
            return Err(BlockWorkersStartError {
                status: RtStatus::ERR_ADMISSION_SCHED,
                message: "BlockWorkers: expected_workers 必须 1..=4".to_string(),
            });
        }
        // 非 RT 路径（start）允许全 None；start_rt 必须每个 worker 都有 RT 目标。
        let worker_rt = match worker_rt {
            Some(targets) => {
                if targets[..n_workers as usize]
                    .iter()
                    .any(|target| target.is_none())
                {
                    return Err(BlockWorkersStartError {
                        status: RtStatus::ERR_ADMISSION_SCHED,
                        message: "BlockWorkers::start_rt: 每个 worker 都必须有 RT 目标".to_string(),
                    });
                }
                targets
            }
            None => [None; WORKERS as usize],
        };
        // 启动时一次性预分配：每 worker 一份固定容量私有 partial。
        let mut partials: Box<[WorkerPartials]> = (0..n_workers as usize)
            .map(|_| WorkerPartials::new())
            .collect();
        let shared = Box::new(B2Shared {
            coeffs: *coeffs,
            partials: partials.as_mut_ptr(),
            worker_rt,
            gate: B2Gate::new(),
        });
        // 初始 plan 在 worker spawn 前 Release 发布（F-26：后续切换只替换该原子指针）。
        shared.gate.plan_ptr.store(
            plan as *const WorkerDagPlan as *mut WorkerDagPlan,
            Ordering::Release,
        );
        let shared_ptr = (&*shared) as *const B2Shared<'a> as usize;
        let mut handles: Vec<std::thread::JoinHandle<()>> = Vec::with_capacity(n_workers as usize);
        let mut index = 0usize;
        while index < n_workers as usize {
            let spawned = std::thread::Builder::new()
                .name(format!("tspi-b2-worker-{index}"))
                .spawn(move || b2_worker_entry(shared_ptr, index));
            match spawned {
                Ok(handle) => handles.push(handle),
                Err(_) => {
                    // 启动失败（资源不足）：置 shutdown 并唤醒/回收，返回错误。
                    shared.gate.shutdown.store(true, Ordering::Release);
                    for handle in &handles {
                        handle.thread().unpark();
                    }
                    for handle in handles.drain(..) {
                        let _ = handle.join();
                    }
                    return Err(BlockWorkersStartError {
                        status: RtStatus::RT_E_INVARIANT,
                        message: "BlockWorkers: worker 线程创建失败".to_string(),
                    });
                }
            }
            index += 1;
        }
        // 启动路径自旋等全部 worker 完成启动期 RT 配置（块循环之外；之后不再变化）。
        loop {
            if shared.gate.rt_failed.load(Ordering::Acquire) {
                let mut slot = 0usize;
                let mut code = 0u32;
                while slot < n_workers as usize {
                    code = shared.gate.rt_errors[slot].load(Ordering::Acquire);
                    if code != 0 {
                        break;
                    }
                    slot += 1;
                }
                let error = rt_apply::ApplyError::decode(code);
                let status = if error.stage == rt_apply::ApplyStage::Unsupported {
                    RtStatus::ERR_UNSUPPORTED
                } else {
                    RtStatus::ERR_ADMISSION_SCHED
                };
                shared.gate.shutdown.store(true, Ordering::Release);
                for handle in &handles {
                    handle.thread().unpark();
                }
                for handle in handles.drain(..) {
                    let _ = handle.join();
                }
                return Err(BlockWorkersStartError {
                    status,
                    message: format!("BlockWorkers::start_rt: worker[{slot}] RT 应用失败：{error}"),
                });
            }
            if shared.gate.rt_ready.load(Ordering::Acquire) >= n_workers as u32 {
                break;
            }
            std::hint::spin_loop();
        }
        Ok(Self {
            shared,
            handles,
            partials,
            merged: WorkerPartials::new(),
            n_workers,
            current_plan: plan,
            _plan_lifetime: PhantomData,
            _coeff_lifetime: PhantomData,
        })
    }

    /// 把执行器切到新 plan（只在 block 边界、全部 worker done 之后调用）。
    ///
    /// F-26：让"plan v1→v2 切换"成为真实执行（下一块 B1/B2/B3 实际使用新 plan），而不是只改
    /// 控制面标签；plan 切换只写 `B2Gate.plan_ptr` 原子，不分配、不新建线程。
    fn switch_plan(&mut self, plan: &'a WorkerDagPlan) -> Result<(), RtStatus> {
        if plan.expected_workers != self.n_workers {
            return Err(RtStatus::ERR_ADMISSION_SCHED);
        }
        // 先发布指针（下一块 generation Release 之前的写），再更新 master 侧当前 plan。
        self.shared.gate.plan_ptr.store(
            plan as *const WorkerDagPlan as *mut WorkerDagPlan,
            Ordering::Release,
        );
        self.current_plan = plan;
        Ok(())
    }

    /// 当前生效 plan 版本（run --config 的 block ack 标签来源；F-26）。
    fn plan_version(&self) -> u32 {
        self.current_plan.plan_version
    }

    /// [RT] 执行一个 block：B1 串行（写 state）→ 常驻 worker B2 并行（只读 state、
    /// 各写自己的预分配 partial）→ master 合并 lane → B3 串行。
    ///
    /// 本函数**不得堆分配、不得创建线程**（F-12；回归断言见
    /// tests::t26_block_loop_zero_alloc_and_no_new_threads）。
    fn run_block_4thread(
        &mut self,
        state: &mut [f32],
        input: &[f32],
        in_stride: usize,
        output: &mut [f32],
        out_stride: usize,
        valid_frames: usize,
    ) -> Result<BlockStats, RtStatus> {
        let plan = self.current_plan;
        let coeffs = &self.shared.coeffs;
        let n_workers = self.n_workers;
        // B1：每个 worker 只写自己的 u_cur/x_past；串行（历史推进顺序确定）。
        let mut worker = 0u8;
        while worker < n_workers {
            let status =
                worker_phase_b1(plan, worker, coeffs, state, input, in_stride, valid_frames);
            if !status.is_ok() {
                return Err(status);
            }
            worker += 1;
        }
        // B2 发布（无分配）：清零本 block 私有 partial，Release 发布 state 视图 + generation。
        let mut index = 0usize;
        while index < n_workers as usize {
            self.partials[index].zero();
            index += 1;
        }
        self.shared
            .gate
            .state_ptr
            .store(state.as_mut_ptr(), Ordering::Relaxed);
        self.shared
            .gate
            .state_len
            .store(state.len(), Ordering::Relaxed);
        self.shared
            .gate
            .valid_frames
            .store(valid_frames, Ordering::Relaxed);
        self.shared.gate.done.store(0, Ordering::Relaxed);
        self.shared.gate.generation.fetch_add(1, Ordering::Release);
        // F6/task-216：发布 generation 后唤醒处于 park 的 worker（token 语义保证不丢唤醒；
        // worker 已自旋则只置 token，无 syscall）。
        let mut wake = 0usize;
        while wake < n_workers as usize {
            self.handles[wake].thread().unpark();
            wake += 1;
        }
        // master 等 n_workers 个 B2 all-done（纯自旋；worker 数固定，无阻塞 syscall）。
        loop {
            if self.shared.gate.done.load(Ordering::Acquire) >= n_workers as u32 {
                break;
            }
            if self.shared.gate.worker_dead.load(Ordering::Acquire) {
                return Err(RtStatus::RT_E_INVARIANT);
            }
            std::hint::spin_loop();
        }
        worker = 0;
        while worker < n_workers {
            let code = self.shared.gate.statuses[worker as usize].load(Ordering::Relaxed);
            if code != RtStatus::OK.0 {
                return Err(RtStatus::from_code(code));
            }
            worker += 1;
        }
        // master 合并 active lane（inactive lane 不参与 reduce，与 W 无关）。
        self.merged.zero();
        let n_role = plan.topology.n_role as usize;
        let mut lane = 0usize;
        while lane < WORKER_REDUCE_LANES {
            if plan.lane_active[lane] != 0 {
                let owner = plan.lane_worker[lane] as usize;
                let mut role = 0usize;
                while role < n_role {
                    let src = match self.partials[owner].lane_role(lane, role) {
                        Some(slice) => slice,
                        None => return Err(RtStatus::ERR_BAD_LAYOUT),
                    };
                    let dst = match self.merged.lane_role_mut(lane, role) {
                        Some(slice) => slice,
                        None => return Err(RtStatus::ERR_BAD_LAYOUT),
                    };
                    dst.copy_from_slice(src);
                    role += 1;
                }
            }
            lane += 1;
        }
        // B3：reduce + P FIR + scatter（写 state 历史与 output 行；串行）。
        let bad_output = AtomicU32::new(0);
        worker = 0;
        while worker < n_workers {
            let status = worker_phase_b3(
                plan,
                worker,
                coeffs,
                state,
                &self.merged,
                output,
                out_stride,
                valid_frames,
                &bad_output,
            );
            if !status.is_ok() {
                return Err(status);
            }
            worker += 1;
        }
        if bad_output.load(Ordering::Acquire) != 0 {
            return Err(RtStatus::RT_E_OUTPUT_NONFINITE);
        }
        Ok(BlockStats {
            b2_threads: n_workers,
            all_ok: true,
        })
    }

    /// 常驻 worker 数（start 后不变；测试/证据用）。
    #[cfg(test)]
    fn worker_count(&self) -> usize {
        self.n_workers as usize
    }

    /// 累计创建的 worker 线程数（启动后块循环不得再增长）。
    #[cfg(test)]
    fn spawned_threads(&self) -> u64 {
        self.shared.gate.spawned_threads.load(Ordering::SeqCst)
    }

    /// 启动时一次预分配的 partials arena 字节数（测试/证据用）。
    #[cfg(test)]
    fn partials_bytes(&self) -> usize {
        self.partials.len() * std::mem::size_of::<WorkerPartials>()
    }

    /// 块间 park 次数（F6 自旋策略回归证据；只增不减）。
    #[cfg(test)]
    fn gate_parks(&self) -> u64 {
        self.shared.gate.gate_parks.load(Ordering::Relaxed)
    }
}

impl Drop for BlockWorkers<'_> {
    fn drop(&mut self) {
        // 唤醒常驻 worker 退出并 join（只在停机路径；块循环内从不进入）。
        // F6/task-216：worker 空闲期在 park 中等待，置 shutdown 后必须 unpark 才能 join；
        // 停机时绝不发布新 generation（避免在调用方缓冲生命周期结束后再触碰 state 视图）。
        self.shared.gate.shutdown.store(true, Ordering::Release);
        for handle in &self.handles {
            handle.thread().unpark();
        }
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// F-26：真实 block all-ack 证据（retire_ready 的唯一来源）
// ---------------------------------------------------------------------------

/// 把"master 已真实观测到整块 4 worker 三 phase 全部完成"翻译成 `TspiBlockAckV1`。
///
/// 调用前提（缺一不可；否则就是 F-26 指出的合成 ack）：
/// - `run_block_4thread` 已返回 Ok：B1 每 worker OK、B2 全部 done 且 status OK、B3 每 worker OK；
/// - `plan_version` 取本块**实际执行**的 plan（`BlockWorkers::plan_version`，不是控制面预告值）；
/// - `expected_workers` = 该 plan 的 worker 数（与 swap 的 expected/ACK_MASK 一致）。
///
/// 本函数只填**已经发生的事实**（三 phase 位图 = `ack_mask(expected)`、cutoff=0），不接受任何
/// synthetic/demo 输入；产物只能交 T20 唯一 RT 入口 `TspiSwapAckV1::note_block_all_ack`，
/// 由它校验标签并置 retire_ready。
fn completed_block_ack(block_seq: u64, plan_version: u32, expected_workers: u8) -> TspiBlockAckV1 {
    let mask = ack_mask(expected_workers);
    let ack = TspiBlockAckV1::new(block_seq, EPOCH_ID, plan_version, expected_workers);
    let mut phase = 0usize;
    while phase < 3 {
        ack.phase_done[phase].store(mask, Ordering::Release);
        phase += 1;
    }
    ack.cutoff.store(0, Ordering::Release);
    ack
}

/// 把"partitioned 单执行器已真实完成整块"翻译成 T20 block all-ack（RG-2 证据）。
///
/// 背景（task-284）：partitioned 引擎（T08 共享 FFT UPOLS）由**主线程单执行器**执行，
/// 没有 worker 线程，因此不会像 direct 4-worker 路径那样逐 worker 置位。但该执行器
/// 实际完成了整个 block（覆盖 plan 全部 worker lane 的 A/G/P 相位计算）——"新 plan 已在
/// 所有执行上下文生效"这一 retire 安全条件由这一次真实执行满足。
///
/// 本函数只填**已经发生的事实**（三 phase 位图 = ACK_MASK(expected)、cutoff=0），
/// 不接受任何 synthetic/demo 输入；调用前提：partitioned run_block 已返回 Ok（engine_blocks
/// 已递增）。产物同样只能交 T20 唯一 RT 入口 `TspiSwapAckV1::note_block_all_ack`。
///
/// 口径边界：报告里 `swap.single_executor=true` + `swap.executor="partitioned-fft-upols"`
/// 明示这是单执行器覆盖，**不得读成 4 个 worker 线程各自 ack**；`synthetic=false` 保持不变。
fn completed_single_executor_block_ack(
    block_seq: u64,
    plan_version: u32,
    expected_workers: u8,
) -> TspiBlockAckV1 {
    let mask = ack_mask(expected_workers);
    let ack = TspiBlockAckV1::new(block_seq, EPOCH_ID, plan_version, expected_workers);
    let mut phase = 0usize;
    while phase < 3 {
        ack.phase_done[phase].store(mask, Ordering::Release);
        phase += 1;
    }
    ack.cutoff.store(0, Ordering::Release);
    ack
}

// ---------------------------------------------------------------------------
// T07 直算参考（selftest：4 worker DAG 必须 bit-exact）
// ---------------------------------------------------------------------------

fn direct_stream(
    graph: &Graph,
    input: &[Vec<f32>],
    blocks: usize,
) -> Result<Vec<Vec<f32>>, RtStatus> {
    let layout = graph.layout()?;
    let coeffs = graph.coeffs();
    let mut state = vec![0.0f32; layout.state_samples()];
    let block = BLOCK as usize;
    let stride = block;
    let n_src = graph.topo.n_src as usize;
    let containers = graph.topo.container_channels as usize;
    let mut planar = vec![0.0f32; n_src * stride];
    let mut output = vec![0.0f32; containers * stride];
    let mut collected = vec![Vec::new(); containers];
    let mut b = 0usize;
    while b < blocks {
        planar.iter_mut().for_each(|sample| *sample = 0.0);
        let mut channel = 0usize;
        while channel < n_src {
            planar[channel * stride..channel * stride + block]
                .copy_from_slice(&input[channel][b * block..(b + 1) * block]);
            channel += 1;
        }
        let status = direct_block_f32(
            &graph.topo,
            &layout,
            &coeffs,
            &mut state,
            &planar,
            stride,
            &mut output,
            stride,
            stride,
            block,
        );
        if !status.is_ok() {
            return Err(status);
        }
        channel = 0;
        while channel < containers {
            collected[channel]
                .extend_from_slice(&output[channel * stride..channel * stride + block]);
            channel += 1;
        }
        b += 1;
    }
    Ok(collected)
}

// ---------------------------------------------------------------------------
// fake 设备 I/O 环（T16 SPSC 有界 desc 环 + T22 format + T19 tick 预算）
// ---------------------------------------------------------------------------

/// 一个固定容量 T16 SPSC 环（header + desc 槽分开堆分配，地址稳定）。
struct FakeRing {
    header: Box<khaen_core::abi::TspiSpscHeaderV1>,
    slots: Box<[DescSlot]>,
}

impl FakeRing {
    fn new() -> Self {
        Self {
            header: Box::new(khaen_core::abi::TspiSpscHeaderV1::new(
                SPSC_CAPACITY as u32,
                SPSC_ENTRY_BYTES,
            )),
            slots: (0..SPSC_CAPACITY).map(|_| DescSlot::empty()).collect(),
        }
    }
}

/// fake-io 运行结果（写进 JSON）。
struct FakeIoStats {
    pub blocks: u64,
    pub frames: u64,
    pub spsc_capacity: usize,
    pub spsc_pushed: u64,
    pub spsc_popped: u64,
    pub device_bytes: u64,
    pub tick_max_compute: u64,
    pub tick_entry_reads: u32,
    pub format_nonfinite: u64,
    pub format_clipped: u64,
    pub b2_threads: u8,
    pub output_sha_prefix: String,
}

fn device_config() -> FormatConfig {
    FormatConfig {
        hw: TspiHwParamsV1 {
            abi_version: ABI_VERSION,
            sample_format: sample_format::F32_INTERLEAVED,
            n_in: 2,
            n_out: 2,
            sample_rate: SAMPLE_RATE,
            period_frames: BLOCK as u32,
            buffer_frames: (BLOCK as u32) * 8,
            in_stride_bytes: (BLOCK as u32) * 2 * 4,
            out_stride_bytes: (BLOCK as u32) * 2 * 4,
            flags: 0,
            reserved: [0; 4],
        },
        dither: DitherCfg::Off,
    }
}

fn synth_input(block: usize, samples: usize) -> Vec<Vec<f32>> {
    let block_size = BLOCK as usize;
    let mut planar = vec![vec![0.0f32; samples]; 2];
    let mut n = 0usize;
    while n < samples {
        let t = (block * block_size + n) as f64 / SAMPLE_RATE as f64;
        planar[0][n] = (2.0 * std::f64::consts::PI * 440.0 * t).sin() as f32 * 0.25;
        planar[1][n] = (2.0 * std::f64::consts::PI * 660.0 * t).sin() as f32 * 0.25;
        n += 1;
    }
    planar
}
/// 非分配版本：把合成输入直接写进**已分配**的 planar 缓冲（ch0 在
/// dest[..samples]、ch1 在 dest[stride..stride + samples]）。F-12：块循环只
/// 借用/复用缓冲，不再每 block 构造 Vec<Vec<f32>>（旧 synth_input 仅 selftest
/// 参考路径使用）。
fn synth_input_into(block: usize, dest: &mut [f32], stride: usize, samples: usize) {
    let block_size = BLOCK as usize;
    let mut n = 0usize;
    while n < samples {
        let t = (block * block_size + n) as f64 / SAMPLE_RATE as f64;
        dest[n] = (2.0 * std::f64::consts::PI * 440.0 * t).sin() as f32 * 0.25;
        dest[stride + n] = (2.0 * std::f64::consts::PI * 660.0 * t).sin() as f32 * 0.25;
        n += 1;
    }
}

fn fake_io(blocks: u64) -> Result<FakeIoStats, RtStatus> {
    let graph = graph_stereo_identity();
    let plan = graph.plan()?;
    let coeffs = graph.coeffs();
    let layout = graph.layout()?;
    let block = BLOCK as usize;
    let stride = block;
    let containers = graph.topo.container_channels as usize;
    // F-12：启动时一次创建常驻 worker + 预分配 partials；块循环只借用/复用。
    let mut workers = BlockWorkers::start(&plan, &coeffs)?;

    let ring_capture = FakeRing::new();
    let ring_playback = FakeRing::new();
    // SAFETY：两个环的 header/desc 槽各自 Box 分离分配、地址稳定到函数结束；每环只有一个
    // producer 与一个 consumer（fake harness 在单线程里按生产/消费顺序驱动，desc 只经
    // T16 API 发布/消费，不绕过 §11.7 校验）。
    let mut cap_prod = unsafe { Producer::new(&ring_capture.header, &ring_capture.slots[..]) }?;
    let mut cap_cons = unsafe { Consumer::new(&ring_capture.header, &ring_capture.slots[..]) }?;
    let mut pb_prod = unsafe { Producer::new(&ring_playback.header, &ring_playback.slots[..]) }?;
    let mut pb_cons = unsafe { Consumer::new(&ring_playback.header, &ring_playback.slots[..]) }?;

    let mut ctl = khaen_core::workers::TspiRtControlV1::new();
    ctl.tick_freq_hz = 1_000_000_000;
    ctl.tick_read_budget = TICK_BUDGET;
    let mut budget = TickBudget::new(TICK_BUDGET);
    let cfg = device_config();
    format::validate(&cfg)?;
    let mut dither_state = format::DitherState::for_config(&cfg);
    let mut state = vec![0.0f32; layout.state_samples()];
    let mut input = vec![0.0f32; 2 * stride];
    let mut output = vec![0.0f32; containers * stride];
    let mut device = vec![0u8; block * 2 * 4];

    let mut stats = FakeIoStats {
        blocks: 0,
        frames: 0,
        spsc_capacity: SPSC_CAPACITY,
        spsc_pushed: 0,
        spsc_popped: 0,
        device_bytes: 0,
        tick_max_compute: 0,
        tick_entry_reads: 0,
        format_nonfinite: 0,
        format_clipped: 0,
        b2_threads: 0,
        output_sha_prefix: String::new(),
    };
    let mut hasher = khaen_rt_host_sha();
    let mut index = 0u64;
    while index < blocks {
        synth_input_into(index as usize, &mut input, stride, block);
        // capture -> DSP：T16 有界 desc 环（FLAG_VALID；首块 FLAG_EPOCH_FIRST）。
        budget.reset();
        let tick_entry = budget
            .read(&ctl)
            .map_err(|_| RtStatus::ERR_ADMISSION_CLOCK)?;
        stats.tick_entry_reads = budget.reads();
        let flags = FLAG_VALID | if index == 0 { FLAG_EPOCH_FIRST } else { 0 };
        let desc = TspiBlockDescV1 {
            abi_version: ABI_VERSION,
            block_size: BLOCK,
            n_in: 2,
            n_out: 2,
            epoch_id: EPOCH_ID,
            block_seq: index,
            frame_index: index * BLOCK as u64,
            plan_version: PLAN_VERSION,
            flags,
            valid_frames: BLOCK as u32,
            slot_index: (index % SPSC_CAPACITY as u64) as u32,
            mono_publish_ns: clock::mono_now_ns(),
            play_deadline_ns: 0,
        };
        cap_prod.try_push(desc)?;
        stats.spsc_pushed += 1;
        let popped = match cap_cons.try_pop() {
            Some(value) => value,
            None => return Err(RtStatus::ERR_CAPACITY),
        };
        stats.spsc_popped += 1;
        let slot = (index % SPSC_CAPACITY as u64) as u32;
        let status = validate_block_desc(&popped, slot, EPOCH_ID);
        if !status.is_ok() {
            return Err(status);
        }

        output.iter_mut().for_each(|sample| *sample = 0.0);
        let block_stats =
            workers.run_block_4thread(&mut state, &input, stride, &mut output, stride, block)?;
        stats.b2_threads = stats.b2_threads.max(block_stats.b2_threads);
        let tick_exit = budget
            .read(&ctl)
            .map_err(|_| RtStatus::ERR_ADMISSION_CLOCK)?;
        let compute = tick_exit.wrapping_sub(tick_entry);
        stats.tick_max_compute = stats.tick_max_compute.max(compute);

        // DSP -> playback：第二个 SPSC 环（输出 desc）。
        let mut out_desc = desc;
        out_desc.slot_index = slot;
        out_desc.frame_index = index * BLOCK as u64;
        pb_prod.try_push(out_desc)?;
        let out_popped = match pb_cons.try_pop() {
            Some(value) => value,
            None => return Err(RtStatus::ERR_CAPACITY),
        };
        let status = validate_block_desc(&out_popped, slot, EPOCH_ID);
        if !status.is_ok() {
            return Err(status);
        }

        // T22：planar f32 -> 设备字节（fake 设备写；连续流用 stateful dither 状态）。
        let fmt_stats =
            format::convert_playback_stateful(&cfg, &mut dither_state, &output, &mut device)?;
        stats.format_nonfinite = stats
            .format_nonfinite
            .saturating_add(fmt_stats.nonfinite_out);
        stats.format_clipped = stats.format_clipped.saturating_add(fmt_stats.clipped);
        let mut offset = 0usize;
        while offset < device.len() {
            hasher.update(&device[offset..offset + 1]);
            offset += 1;
        }
        stats.device_bytes += device.len() as u64;
        stats.frames += BLOCK as u64;
        stats.blocks += 1;
        index += 1;
    }
    stats.output_sha_prefix = hasher.finish_hex_prefix();
    Ok(stats)
}

// ---------------------------------------------------------------------------
// 极简 SHA-256（只用于 fake 设备字节指纹；无第三方依赖）
// ---------------------------------------------------------------------------

struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    used: usize,
    length: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0u8; 64],
            used: 0,
            length: 0,
        }
    }

    fn update(&mut self, byte: &[u8]) {
        self.length = self.length.wrapping_add(byte.len() as u64);
        for value in byte {
            self.buffer[self.used] = *value;
            self.used += 1;
            if self.used == 64 {
                self.compress();
                self.used = 0;
            }
        }
    }

    fn finish_hex_prefix(mut self) -> String {
        let bits = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        while self.used != 56 {
            self.update(&[0x00]);
        }
        self.update(&bits.to_be_bytes());
        let mut out = String::with_capacity(16);
        for word in &self.state[..2] {
            out.push_str(&format!("{:08x}", word));
        }
        out
    }

    /// 完整 32 B 摘要（T185：plan sha256 / graph_id hex 交叉校验用；与 finish_hex_prefix 同口径）。
    fn finish_bytes(mut self) -> [u8; 32] {
        let bits = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        while self.used != 56 {
            self.update(&[0x00]);
        }
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 32];
        let mut index = 0usize;
        while index < 8 {
            out[index * 4..(index + 1) * 4].copy_from_slice(&self.state[index].to_be_bytes());
            index += 1;
        }
        out
    }

    fn compress(&mut self) {
        const K: [u32; 64] = [
            0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
            0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
            0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
            0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
            0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
            0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
            0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
            0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
            0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
            0xc67178f2,
        ];
        let mut w = [0u32; 64];
        let mut i = 0usize;
        while i < 16 {
            w[i] = u32::from_be_bytes([
                self.buffer[i * 4],
                self.buffer[i * 4 + 1],
                self.buffer[i * 4 + 2],
                self.buffer[i * 4 + 3],
            ]);
            i += 1;
        }
        while i < 64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
            i += 1;
        }
        let mut a = self.state[0];
        let mut b = self.state[1];
        let mut c = self.state[2];
        let mut d = self.state[3];
        let mut e = self.state[4];
        let mut f = self.state[5];
        let mut g = self.state[6];
        let mut h = self.state[7];
        let mut i = 0usize;
        while i < 64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
            i += 1;
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

fn khaen_rt_host_sha() -> Sha256 {
    Sha256::new()
}

// ---------------------------------------------------------------------------
// admission-check：T18 容量/预算 + T22 format + T45 scheduler 参数自校验
// ---------------------------------------------------------------------------

fn plan_header() -> TspiPlanHeaderV1 {
    TspiPlanHeaderV1 {
        magic: TSPI_PLAN_MAGIC,
        abi_version: ABI_VERSION,
        header_size: 128,
        plan_version: PLAN_VERSION,
        sample_rate: SAMPLE_RATE,
        block_size: BLOCK,
        n_in: 2,
        n_out: 2,
        low_band_count: 2,
        path_count: 0,
        diag_fir_count: 0,
        max_taps: 4096,
        workers: WORKERS,
        flags: 0,
        precision: 0,
        reserved0: 0,
        graph_id: [0x11; 32],
        total_coeff_bytes: 0,
        path_table_offset: 128,
        diag_table_offset: 128,
        in_stride: 128,
        out_stride: 128,
        role_table_offset: 128,
        role_table_count: 0,
        reserved1: 0,
        affinity_table_offset: 0,
        affinity_count: 0,
        reserved2: 0,
        ctl_meta_offset: 0,
        ctl_meta_len: 0,
        reserved3: [0; 3],
        header_crc32c: 0,
    }
}

fn admission_check() -> Result<(), RtStatus> {
    let header = plan_header();
    let view = PlanView {
        header: &header,
        paths: &[],
        diags: &[],
        roles: &[],
        coeffs: &[],
    };
    let mut env = AdmissionEnv::new();
    env.sample_rate_hz = SAMPLE_RATE;
    env.block_size = BLOCK;
    env.rt_workers = WORKERS;
    env.rt_privilege_ok = true;
    env.clock_source_ok = true;
    env.cpu_set_ok = true;
    env.cpu_set_mask = 0x0f;
    env.allowed_cpu = [0, 1, 2, 3];
    env.actual_cpu = [0, 1, 2, 3];
    env.expected_workers = WORKERS;
    env.sched_priority = [70, 70, 70, 70];
    let _memory = khaen_rt_host::admission::plan_memory(&view, &env)?;
    let cfg = device_config();
    format::validate(&cfg)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// selftest：fake 管线 vs T07 直算 bit-exact + admission/format/SPSC 契约
// ---------------------------------------------------------------------------

fn selftest() -> Result<String, RtStatus> {
    admission_check()?;
    // 1) stereo identity：4 worker DAG 必须与 direct bit-exact（含 4 个 block 的流式历史）。
    let graph = graph_stereo_identity();
    let plan = graph.plan()?;
    let coeffs = graph.coeffs();
    let layout = graph.layout()?;
    let block = BLOCK as usize;
    let stride = block;
    let containers = graph.topo.container_channels as usize;
    // F-12：启动时一次创建常驻 worker + 预分配 partials；块循环只借用/复用。
    let mut workers = BlockWorkers::start(&plan, &coeffs)?;
    let mut state = vec![0.0f32; layout.state_samples()];
    let mut input = vec![0.0f32; 2 * stride];
    let mut output = vec![0.0f32; containers * stride];
    let blocks = 4usize;
    let mut pipeline: Vec<Vec<f32>> = vec![Vec::new(); containers];
    let mut b = 0usize;
    while b < blocks {
        let source = synth_input(b, block);
        let mut channel = 0usize;
        while channel < 2 {
            input[channel * stride..channel * stride + block].copy_from_slice(&source[channel]);
            channel += 1;
        }
        output.iter_mut().for_each(|sample| *sample = 0.0);
        let stats =
            workers.run_block_4thread(&mut state, &input, stride, &mut output, stride, block)?;
        if stats.b2_threads != WORKERS {
            return Err(RtStatus::RT_E_INVARIANT);
        }
        channel = 0;
        while channel < containers {
            pipeline[channel]
                .extend_from_slice(&output[channel * stride..channel * stride + block]);
            channel += 1;
        }
        b += 1;
    }
    let source: Vec<Vec<f32>> = (0..2)
        .map(|channel| {
            let mut all = Vec::with_capacity(blocks * block);
            let mut b = 0usize;
            while b < blocks {
                all.extend_from_slice(&synth_input(b, block)[channel]);
                b += 1;
            }
            all
        })
        .collect();
    let reference = direct_stream(&graph, &source, blocks)?;
    let mut channel = 0usize;
    while channel < containers {
        if pipeline[channel].len() != reference[channel].len() {
            return Err(RtStatus::RT_E_INVARIANT);
        }
        let mut n = 0usize;
        while n < pipeline[channel].len() {
            if pipeline[channel][n].to_bits() != reference[channel][n].to_bits() {
                return Err(RtStatus::RT_E_INVARIANT);
            }
            n += 1;
        }
        channel += 1;
    }
    // 2) 2x2 offdiag（D=1 + 尾部）也与直算 bit-exact（覆盖 G 非对角 + P 负号）。
    let graph2 = graph_2x2_offdiag();
    let plan2 = graph2.plan()?;
    let coeffs2 = graph2.coeffs();
    let layout2 = graph2.layout()?;
    let mut workers2 = BlockWorkers::start(&plan2, &coeffs2)?;
    let mut state2 = vec![0.0f32; layout2.state_samples()];
    let mut input2 = vec![0.0f32; 2 * stride];
    let mut output2 = vec![0.0f32; containers * stride];
    let mut pipeline2: Vec<Vec<f32>> = vec![Vec::new(); containers];
    b = 0;
    while b < blocks {
        let source = synth_input(b + 4, block);
        let mut channel = 0usize;
        while channel < 2 {
            input2[channel * stride..channel * stride + block].copy_from_slice(&source[channel]);
            channel += 1;
        }
        output2.iter_mut().for_each(|sample| *sample = 0.0);
        workers2.run_block_4thread(&mut state2, &input2, stride, &mut output2, stride, block)?;
        channel = 0;
        while channel < containers {
            pipeline2[channel]
                .extend_from_slice(&output2[channel * stride..channel * stride + block]);
            channel += 1;
        }
        b += 1;
    }
    let source2: Vec<Vec<f32>> = (0..2)
        .map(|channel| {
            let mut all = Vec::with_capacity(blocks * block);
            let mut b = 0usize;
            while b < blocks {
                all.extend_from_slice(&synth_input(b + 4, block)[channel]);
                b += 1;
            }
            all
        })
        .collect();
    let reference2 = direct_stream(&graph2, &source2, blocks)?;
    channel = 0;
    while channel < containers {
        let mut n = 0usize;
        while n < pipeline2[channel].len() {
            if pipeline2[channel][n].to_bits() != reference2[channel][n].to_bits() {
                return Err(RtStatus::RT_E_INVARIANT);
            }
            n += 1;
        }
        channel += 1;
    }
    // 3) fake 设备闭环（SPSC/T22/T19）。
    let stats = fake_io(24)?;
    Ok(format!(
        "{{\"status\":\"ok\",\"selftest\":true,\"workers\":{},\"b2_threads\":{},\"bit_exact_graphs\":2,\"fake_blocks\":{},\"device_bytes\":{},\"tick_max_compute\":{},\"spsc_pushed\":{},\"format_nonfinite\":{}}}",
        WORKERS, stats.b2_threads, stats.blocks, stats.device_bytes, stats.tick_max_compute, stats.spsc_pushed, stats.format_nonfinite
    ))
}

fn fake_io_json(blocks: u64) -> Result<String, RtStatus> {
    let stats = fake_io(blocks)?;
    Ok(format!(
        "{{\"status\":\"ok\",\"mode\":\"fake-io\",\"workers\":{},\"b2_threads\":{},\"blocks\":{},\"frames\":{},\"block_size\":{},\"sample_rate\":{},\"spsc_capacity\":{},\"spsc_pushed\":{},\"spsc_popped\":{},\"device_bytes\":{},\"tick_max_compute\":{},\"tick_entry_reads\":{},\"format_nonfinite\":{},\"format_clipped\":{},\"device_sha_prefix\":\"{}\"}}",
        WORKERS, stats.b2_threads, stats.blocks, stats.frames, BLOCK, SAMPLE_RATE, stats.spsc_capacity, stats.spsc_pushed, stats.spsc_popped, stats.device_bytes, stats.tick_max_compute, stats.tick_entry_reads, stats.format_nonfinite, stats.format_clipped, stats.output_sha_prefix
    ))
}

fn admission_json() -> Result<String, RtStatus> {
    admission_check()?;
    Ok(format!(
        "{{\"status\":\"ok\",\"mode\":\"admission-check\",\"workers\":{},\"block_size\":{},\"sample_rate\":{},\"format\":\"f32-interleaved\",\"admission\":\"plan_memory-ok\",\"scheduler\":\"t45-plan-apply-wired(task-216)\"}}",
        WORKERS, BLOCK, SAMPLE_RATE
    ))
}

// ---------------------------------------------------------------------------
// T155 / T26 follow-up：生产入口 run --config（建图 / ALSA owner / worker RT /
// swap(T20) / fault(T21) / scheduler affinity(T45) / Web 控制面 / 优雅退出）
// ---------------------------------------------------------------------------

/// host 运行时配置 schema（rt-host.toml；T155 冻结为 khaen-rt-host/v1）。
const HOST_CONFIG_SCHEMA: &str = "khaen-rt-host/v1";

/// host 配置（rt-host.toml + TSPI_HOST_* 环境变量覆盖）。
#[derive(Clone, Debug)]
struct HostRunConfig {
    block_size: u16,
    sample_rate: u32,
    workers: u8,
    graph: String,
    /// G-4096 性能（task-246）：plan-arena DSP 引擎选择。
    /// "direct" = T09 4-worker B2 直算（f32 逐位口径，原默认行为）；
    /// "partitioned" = T08 共享 FFT UPOLS 单线程内核（f32 误差界见 T08：rel L2 <= 1e-5）；
    /// "auto" = plan 融合支撑 L_F=L_A+L_G+L_P-2 >= 2048 时用 partitioned，否则 direct。
    dsp_engine: String,
    safe_mode: String,
    backend: String,
    /// backend=alsa 的输入模式：synth（合成 440/660Hz）| loopback（Toslink ch12/13）。
    alsa_io_mode: String,
    device: String,
    capture_channels: u16,
    playback_channels: u16,
    period_frames: u32,
    buffer_frames: u32,
    dsp_cpus: String,
    dsp_priority: u8,
    io_priority: u8,
    irq_priority: u8,
    tick_freq_hz: u32,
    web_bind: String,
    control_url: String,
    daemon_config: String,
    last_good: String,
    /// T185：graph="plan-arena" 时加载的 T66 plan.bin 路径（空 = 未配置，默认 graph 不受影响）。
    plan: String,
    /// T185：可选 compile-report.json（或 artifact 目录）；校验 plan_sha256 + host_graph_sha256。
    plan_manifest: String,
    /// T185：last-good plan.bin（primary 坏时回退；空 = 不启用回退，直接拒绝启动）。
    plan_last_good: String,
    /// T74：[capture] source = "synth"（既有合成输入）| "file"（预渲染 WAV/raw 文件输入）。
    capture_source: String,
    /// T74：[capture] path = 预渲染文件（source=file 必填；WAV 或 raw）。
    capture_path: String,
    /// T74：[capture] format = "wav"（默认）| "raw"。
    capture_format: String,
    /// T74：[capture] raw_format = "s24_3le" | "f32le"（仅 format=raw）。
    capture_raw_format: String,
    /// T74：[capture] file_channels（仅 raw 必填 2|8；WAV 从 header 推导，非 0 时校验一致）。
    capture_file_channels: u16,
    /// T74：[capture] eof = "silence"（读完补静音继续）| "exit"（读完最后一帧后优雅退出）。
    capture_eof: String,
    /// T74 前置：[capture] mode = off|toslink|aes67|dante（模式面互斥；与 source 一一对应）。
    capture_mode: String,
    /// T74 前置：[capture] ptp_lock = locked|unlocked（启动期 PTP 锁断言；真实 servo 归 T19 NOT_WIRED）。
    capture_ptp_lock: String,
    /// T74 前置：[capture] aoip_channels = 0（默认 16）|12|16（AoIP 容器通道数）。
    capture_aoip_channels: u16,
    /// T74 前置：Inferno ALSA 插件 PCM 名（默认 pcm.inferno = "inferno"；必须与 T43 常量一致）。
    capture_inferno_pcm: String,
    /// T74 前置：Inferno device id（16 hex 或 auto；auto = 由 MAC 派生 + 0000）。
    capture_inferno_device_id: String,
    /// T74 前置：Inferno PTPv1 虚拟钟导出路径（statime usrvclock）。
    capture_inferno_clock_path: String,
    /// T74 前置：Inferno 绑定网卡（source=inferno 必填；缺失 = 拒收）。
    capture_inferno_iface: String,
    /// T74 前置：AES67/RAVENNA 稳定 card id（默认 RAVENNA；禁止写死 card 号）。
    capture_aes67_card: String,
    /// T74 前置：AES67 PTPv2 domain（须与 daemon/SDP 一致）。
    capture_aes67_ptp_domain: u8,
    /// T74 前置：AES67 daemon 就绪断言（原子启动期事实；真实 daemon 探测 NOT_WIRED）。
    capture_aes67_daemon_ready: bool,
    /// T74 前置：AES67 SDP 文件（可含多条 v=0 会话；空 = 用固定 flow 配置，SDP 校验跳过）。
    capture_aes67_sdp: String,
    /// T239：[capture] aes67_transport = "ravenna"（默认；既有 aes67-daemon+RAVENNA LKM）|
    /// "userspace"（自研用户态 RTP 接收端：SDP/组播/L24/抖动缓冲/128 帧；不依赖 daemon/LKM）。
    /// 同一 source=aes67 下两条传输互斥。
    capture_aes67_transport: String,
    /// T239/task-320：userspace 收包网卡（IPv4 或网卡名；逗号分隔列表 / 单行 TOML 数组 =
    /// 多接口；空 = 0.0.0.0 旧行为）。组播 = 每个接口一条 join；单播 = bind 首个接口。
    capture_aes67_iface: String,
    /// task-320：组播组白名单（IPv4 组播地址列表；空 = 旧行为）。SDP 声明的组不在列表 →
    /// 启动 fail-closed（不 join 未授权组）。
    capture_aes67_group_allow: String,
    /// task-320：RTP 来源白名单（IPv4/CIDR 列表；空 = 旧行为）。非白名单来源的包在
    /// RTP 解析/抖动缓冲之前丢弃并计数（不污染 jitter/underrun）。
    capture_aes67_source_allow: String,
    /// T239：userspace 接收 IO 线程优先级（0 = 不设置；>0 = Linux SCHED_FIFO / Windows 映射）。
    capture_aes67_io_priority: u8,
    /// T239：userspace 失流停滞门限（block 数；0 = T43 默认 3；持续失流 = 2×N -> fail closed）。
    capture_aes67_stall_blocks: u32,
    /// T239：userspace 单 flow 的 H16 容器 slot 覆盖（如 "0,1,4,5,10,11,3"；空 = H16 TX1..N）。
    capture_aes67_rx_slots: String,
    /// T265-followup：userspace 抖动缓冲目标延迟 µs（0 = AES67 默认 4000；250..=100000）。
    capture_aes67_jitter_latency_us: u32,
    /// T265-followup：userspace 抖动缓冲 ring 容量帧（0 = 默认 8192；512..=65536 且 2 的幂）。
    capture_aes67_jitter_ring_frames: u32,
    /// T239-crackle：流结束保持（hold）。true = 流间隙/会话重启时不 fail closed，
    /// 保持静音等待恢复并走内部 reanchor（standalone `--stay-on-stream-end` 同语义）。
    capture_aes67_stay_on_stream_end: bool,
    /// T239-crackle：连续静音达到该毫秒数请求一次内部 reanchor（0 = 不请求；默认 80）。
    capture_aes67_silence_hold_ms: u32,
    /// T239-crackle：hold 静音总时长超过该毫秒数才 fail closed（0 = 永不；默认 10000）。
    capture_aes67_stall_fail_close_ms: u32,
    /// T239-crackle：stale-jitter 看门狗（datagrams 涨但 accepted 不涨 → 内部 reanchor）。
    capture_aes67_reanchor_watchdog: bool,
    /// T203：[content] T57 overlay layout 预设文件路径（空 = 默认 /etc/tspi-audio/layout-presets.yml）。
    content_layout_presets: String,
    /// T203：[content] 内容输入布局 id（T57 input_layouts，如 h16-5.1）；与 output_layout 同时配置才启用。
    content_input_layout: String,
    /// T203：[content] 输出布局 id（T57 output_layouts，如 4.1.2）；决定中置是否存在。
    content_output_layout: String,
    /// T203：[content] 中置声明 auto|present|absent（对应 rt-host content_map CenterSpeakerState）。
    content_center: String,
    /// T203：[content] D7 program peak 基准（dBFS；默认 −18.0，同 calibration peq_fit 默认）。
    content_program_peak_dbfs: f64,
    max_blocks: u64,
    duration_ms: u64,
    /// F6/task-216：[rt] apply_affinity=true 时**真实写内核**（Linux：mlockall/SCHED_FIFO/亲和）；
    /// false = 只计划（sched JSON 如实报 kernel_applied=false，不谎称 RT）。
    apply_affinity: bool,
    /// F6/task-216：[rt] mlockall（apply_affinity=true 时；默认 true）。
    rt_mlockall: bool,
    /// F6/task-216：[rt] io_cpu（主/I-O 线程绑核；CPU_UNUSED = 自动选第一个非 DSP_CPUS 的 CPU）。
    rt_io_cpu: u16,
    /// planrt：离线渲染 sink 路径（backend=fake + [sink] out=path）。非空时 graph 输出块被
    /// 真实写成 raw 文件（默认 S24_3LE 交错），用于无实时能力/无设备时把 plan 图真实跑出的
    /// 输出落盘，再用 aplay 做同路径声学 A/B；不打开任何音频设备。
    sink_out: String,
    /// planrt：[sink] out 的 raw 格式："s24_3le"（默认，可直接 aplay）| "f32le"。
    sink_out_format: String,
    /// planrt：[capture] tee = path：backend=alsa 时把每个 capture period 的设备字节原样
    /// 落盘（host 播放同时录 MOTU 20ch capture；无需 arecord，解决 duplex 独占互斥）。
    capture_tee: String,
    /// task-253：MIMO 12 kHz 低频修正支路产物目录（params.json + g12 + resamp）。
    /// 空 = 未配置（默认路径完全不变）；非空时要求 plan 是 identity-G / Δsys baseline /
    /// P=identity 的直通 plan（12k G 只跑修正量，直通支路负责 δ_Δsys）。
    mimo12k_artifact: String,
}

impl Default for HostRunConfig {
    fn default() -> Self {
        Self {
            block_size: BLOCK,
            sample_rate: SAMPLE_RATE,
            workers: WORKERS,
            graph: "stereo_identity".to_string(),
            // 默认 auto：小支撑 plan（identity/2x2/测试图）走 direct 逐位口径；G-4096 这类
            // 大支撑 plan-arena 图自动走共享 FFT 分区卷积（task-246：G-4096 真实时）。
            dsp_engine: "auto".to_string(),
            safe_mode: "normal".to_string(),
            backend: "fake".to_string(),
            alsa_io_mode: "synth".to_string(),
            device: "hw:UltraLitemk5,0".to_string(),
            capture_channels: 20,
            playback_channels: 22,
            period_frames: 128,
            buffer_frames: 1024,
            // 默认 A72 全核 4-7（RK3576 CPU4-7）；2-5 混用 A53/A72 实测慢 3.1×
            // （planrt-20261119：G-4096 96.1 -> 314.8 blocks/s）。T45 依赖 IRQ 仍钉 A53
            // （IRQ58/56/57 -> CPU1/2/3；见 deploy/config/irq-affinity.env）。
            dsp_cpus: "4-7".to_string(),
            dsp_priority: 70,
            io_priority: 80,
            irq_priority: 90,
            tick_freq_hz: 1_000_000_000,
            web_bind: "127.0.0.1:8090".to_string(),
            control_url: "http://127.0.0.1:8090".to_string(),
            daemon_config: String::new(),
            last_good: "/var/lib/tspi-rt-stack/last-good.json".to_string(),
            plan: String::new(),
            plan_manifest: String::new(),
            plan_last_good: String::new(),
            capture_source: "synth".to_string(),
            capture_path: String::new(),
            capture_format: "wav".to_string(),
            capture_raw_format: "s24_3le".to_string(),
            capture_file_channels: 0,
            capture_eof: "exit".to_string(),
            capture_mode: CAPTURE_MODE_OFF.to_string(),
            capture_ptp_lock: CAPTURE_PTP_UNLOCKED.to_string(),
            capture_aoip_channels: 0,
            capture_inferno_pcm: aoip::INFERNO_ALSA_DEVICE.to_string(),
            capture_inferno_device_id: "auto".to_string(),
            capture_inferno_clock_path: aoip::INFERNO_CLOCK_PATH_DEFAULT.to_string(),
            capture_inferno_iface: String::new(),
            capture_aes67_card: aoip::AES67_CARD_ID_DEFAULT.to_string(),
            capture_aes67_ptp_domain: aoip::AES67_PTP_DOMAIN_DEFAULT,
            capture_aes67_daemon_ready: false,
            capture_aes67_sdp: String::new(),
            capture_aes67_transport: CAPTURE_AES67_TRANSPORT_RAVENNA.to_string(),
            capture_aes67_iface: String::new(),
            capture_aes67_group_allow: String::new(),
            capture_aes67_source_allow: String::new(),
            capture_aes67_io_priority: 0,
            capture_aes67_stall_blocks: 0,
            capture_aes67_rx_slots: String::new(),
            capture_aes67_jitter_latency_us: 0,
            capture_aes67_jitter_ring_frames: 0,
            capture_aes67_stay_on_stream_end: false,
            capture_aes67_silence_hold_ms: AES67_DEFAULT_SILENCE_HOLD_MS,
            capture_aes67_stall_fail_close_ms: AES67_DEFAULT_STALL_FAIL_CLOSE_MS,
            capture_aes67_reanchor_watchdog: false,
            content_layout_presets: DEFAULT_LAYOUT_PRESETS_PATH.to_string(),
            content_input_layout: String::new(),
            content_output_layout: String::new(),
            content_center: "auto".to_string(),
            content_program_peak_dbfs: CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS,
            max_blocks: 0,
            duration_ms: 0,
            apply_affinity: false,
            rt_mlockall: true,
            rt_io_cpu: khaen_rt_host::scheduler::CPU_UNUSED,
            sink_out: String::new(),
            sink_out_format: "s24_3le".to_string(),
            capture_tee: String::new(),
            mimo12k_artifact: String::new(),
        }
    }
}

fn config_trim_comment(line: &str) -> &str {
    match line.find('#') {
        Some(index) => &line[..index],
        None => line,
    }
}

fn config_unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

/// 解析 [rt] io_cpu（整数 0..63 或 auto = CPU_UNUSED 自动选非 DSP CPU）。
fn parse_rt_io_cpu(value: &str) -> Result<u16, String> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("auto") {
        return Ok(khaen_rt_host::scheduler::CPU_UNUSED);
    }
    let cpu: u16 = value
        .parse()
        .map_err(|_| format!("bad [rt] io_cpu: {value}（整数或 auto）"))?;
    if cpu as usize >= khaen_rt_host::scheduler::MAX_CPU {
        return Err(format!("[rt] io_cpu {cpu} out of range 0..64"));
    }
    Ok(cpu)
}

fn parse_host_config(text: &str, warnings: &mut Vec<String>) -> Result<HostRunConfig, String> {
    let mut config = HostRunConfig::default();
    let mut section = String::new();
    for raw in text.lines() {
        let line = config_trim_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some(pair) => pair,
            None => return Err(format!("invalid config line: {line}")),
        };
        let key = key.trim().to_ascii_lowercase();
        let value = config_unquote(value);
        let parse_u64 = |v: &str| -> Result<u64, String> {
            v.parse::<u64>().map_err(|_| format!("bad integer: {v}"))
        };
        let parse_f64 = |v: &str| -> Result<f64, String> {
            v.parse::<f64>().map_err(|_| format!("bad float: {v}"))
        };
        match (section.as_str(), key.as_str()) {
            ("host", "block_size") | ("", "block_size") => {
                config.block_size = parse_u64(&value)? as u16
            }
            ("host", "sample_rate") | ("", "sample_rate") => {
                config.sample_rate = parse_u64(&value)? as u32
            }
            ("host", "workers") | ("", "workers") => config.workers = parse_u64(&value)? as u8,
            ("host", "graph") | ("", "graph") => config.graph = value,
            ("host", "safe_mode") | ("", "safe_mode") => config.safe_mode = value,
            ("alsa", "backend") => config.backend = value,
            ("alsa", "io_mode") => config.alsa_io_mode = value,
            ("alsa", "device") => config.device = value,
            ("alsa", "capture_channels") => config.capture_channels = parse_u64(&value)? as u16,
            ("alsa", "playback_channels") => config.playback_channels = parse_u64(&value)? as u16,
            ("alsa", "period_frames") => config.period_frames = parse_u64(&value)? as u32,
            ("alsa", "buffer_frames") => config.buffer_frames = parse_u64(&value)? as u32,
            ("dsp", "engine") | ("host", "engine") | ("rt", "dsp_engine") => {
                let value = value.to_ascii_lowercase();
                if value != "direct" && value != "partitioned" && value != "auto" {
                    return Err(format!(
                        "[dsp] engine must be direct|partitioned|auto, got {value}"
                    ));
                }
                config.dsp_engine = value
            }
            ("rt", "dsp_cpus") => config.dsp_cpus = value,
            ("rt", "dsp_priority") => config.dsp_priority = parse_u64(&value)? as u8,
            ("rt", "io_priority") => config.io_priority = parse_u64(&value)? as u8,
            ("rt", "irq_priority") => config.irq_priority = parse_u64(&value)? as u8,
            ("rt", "tick_freq_hz") => config.tick_freq_hz = parse_u64(&value)? as u32,
            ("rt", "apply_affinity") => config.apply_affinity = value == "true",
            ("rt", "mlockall") => config.rt_mlockall = value == "true" || value == "1",
            ("rt", "io_cpu") => config.rt_io_cpu = parse_rt_io_cpu(&value)?,
            ("web", "bind") | ("web", "listen") => config.web_bind = value,
            ("control", "url") => config.control_url = value,
            ("daemon", "config") => config.daemon_config = value,
            ("paths", "last_good") => config.last_good = value,
            ("paths", "plan") => config.plan = value,
            ("paths", "plan_manifest") => config.plan_manifest = value,
            ("paths", "plan_last_good") => config.plan_last_good = value,
            ("capture", "source") => config.capture_source = value,
            ("capture", "path") | ("capture", "file") => config.capture_path = value,
            ("capture", "format") => config.capture_format = value,
            ("capture", "raw_format") | ("capture", "sample_format") => {
                config.capture_raw_format = value
            }
            ("capture", "channels") | ("capture", "file_channels") => {
                config.capture_file_channels = parse_u64(&value)? as u16
            }
            ("capture", "eof") | ("capture", "eof_policy") => config.capture_eof = value,
            // planrt：离线渲染 sink（[sink] out）与 capture tee（host 播放同时落盘 capture）。
            ("sink", "out") | ("sink", "path") | ("sink", "file") => config.sink_out = value,
            ("sink", "format") => config.sink_out_format = value,
            ("capture", "tee") | ("capture", "tee_path") => config.capture_tee = value,
            // task-253：MIMO 12 kHz 低频修正支路产物目录（空 = 关闭；默认路径不受影响）。
            ("mimo12k", "artifact") | ("mimo12k", "dir") | ("mimo12k", "g") => {
                config.mimo12k_artifact = value
            }
            // T74 前置：capture source = motu|inferno|aes67（mode 互斥；端点/SDP/PTP 配置）
            ("capture", "mode") => config.capture_mode = value,
            ("capture", "ptp_lock") => config.capture_ptp_lock = value,
            ("capture", "aoip_channels") | ("capture", "container_channels") => {
                config.capture_aoip_channels = parse_u64(&value)? as u16
            }
            ("capture", "inferno_pcm") => config.capture_inferno_pcm = value,
            ("capture", "inferno_device_id") => config.capture_inferno_device_id = value,
            ("capture", "inferno_clock_path") => config.capture_inferno_clock_path = value,
            ("capture", "inferno_iface") | ("capture", "inferno_interface") => {
                config.capture_inferno_iface = value
            }
            ("capture", "aes67_card") => config.capture_aes67_card = value,
            ("capture", "aes67_ptp_domain") => {
                config.capture_aes67_ptp_domain = parse_u64(&value)? as u8
            }
            ("capture", "aes67_daemon_ready") => {
                config.capture_aes67_daemon_ready = value == "true"
            }
            ("capture", "aes67_sdp") | ("capture", "sdp") => config.capture_aes67_sdp = value,
            // T239：source=aes67 的两条传输（ravenna 默认 / userspace 自研接收端）+ 用户态参数。
            ("capture", "aes67_transport") | ("capture", "aes67_xport") => {
                config.capture_aes67_transport = value
            }
            ("capture", "aes67_iface") | ("capture", "aes67_interface") => {
                config.capture_aes67_iface = value
            }
            ("capture", "aes67_group_allow") | ("capture", "aes67_groups") => {
                config.capture_aes67_group_allow = value
            }
            ("capture", "aes67_source_allow") | ("capture", "aes67_sources") => {
                config.capture_aes67_source_allow = value
            }
            ("capture", "aes67_io_priority") => {
                config.capture_aes67_io_priority = parse_u64(&value)? as u8
            }
            ("capture", "aes67_stall_blocks") => {
                config.capture_aes67_stall_blocks = parse_u64(&value)? as u32
            }
            ("capture", "aes67_rx_slots") => config.capture_aes67_rx_slots = value,
            ("capture", "aes67_jitter_latency_us") => {
                config.capture_aes67_jitter_latency_us = parse_u64(&value)? as u32
            }
            ("capture", "aes67_jitter_ring_frames") => {
                config.capture_aes67_jitter_ring_frames = parse_u64(&value)? as u32
            }
            // T239-crackle：hold / 内部 reanchor / stale-jitter 看门狗（userspace 专用）。
            ("capture", "aes67_stay_on_stream_end") => {
                config.capture_aes67_stay_on_stream_end = value == "true"
            }
            ("capture", "aes67_silence_hold_ms") => {
                config.capture_aes67_silence_hold_ms = parse_u64(&value)? as u32
            }
            ("capture", "aes67_stall_fail_close_ms") => {
                config.capture_aes67_stall_fail_close_ms = parse_u64(&value)? as u32
            }
            ("capture", "aes67_reanchor_watchdog") => {
                config.capture_aes67_reanchor_watchdog = value == "true"
            }
            // T203/T210：中置下混内容适配（T57 overlay 布局 + rt-host 自持 content_map）。
            ("content", "layout_presets") | ("content", "presets") => {
                config.content_layout_presets = value
            }
            ("content", "input_layout") => config.content_input_layout = value,
            ("content", "output_layout") => config.content_output_layout = value,
            ("content", "center") => config.content_center = value,
            ("content", "program_peak_dbfs") | ("content", "program_peak") => {
                config.content_program_peak_dbfs = parse_f64(&value)?
            }
            ("run", "max_blocks") => config.max_blocks = parse_u64(&value)?,
            ("run", "duration_ms") => config.duration_ms = parse_u64(&value)?,
            _ => warnings.push(format!("unknown config key [{section}] {key}")),
        }
    }
    // 环境变量覆盖（systemd EnvironmentFile 契约，见 deploy/config/rt-host.env.example）。
    let env_map: [(&str, fn(&mut HostRunConfig, String)); 40] = [
        ("TSPI_HOST_BLOCK", |c, v| {
            if let Ok(n) = v.parse() {
                c.block_size = n
            }
        }),
        ("TSPI_HOST_RATE", |c, v| {
            if let Ok(n) = v.parse() {
                c.sample_rate = n
            }
        }),
        ("TSPI_HOST_WORKERS", |c, v| {
            if let Ok(n) = v.parse() {
                c.workers = n
            }
        }),
        ("TSPI_HOST_DSP_CPUS", |c, v| c.dsp_cpus = v),
        ("TSPI_HOST_MOTU_CARD_ID", |c, v| {
            c.device = format!("hw:{v},0")
        }),
        ("TSPI_HOST_DEVICE", |c, v| c.device = v),
        ("TSPI_HOST_PLAYBACK_CHANNELS", |c, v| {
            if let Ok(n) = v.parse() {
                c.playback_channels = n
            }
        }),
        ("TSPI_HOST_CAPTURE_CHANNELS", |c, v| {
            if let Ok(n) = v.parse() {
                c.capture_channels = n
            }
        }),
        ("TSPI_HOST_ALSA_IO_MODE", |c, v| c.alsa_io_mode = v),
        ("TSPI_HOST_SAFE_MODE", |c, v| c.safe_mode = v),
        ("TSPI_HOST_LAST_GOOD", |c, v| c.last_good = v),
        ("TSPI_HOST_CAPTURE_SOURCE", |c, v| c.capture_source = v),
        ("TSPI_HOST_CAPTURE_FILE", |c, v| c.capture_path = v),
        ("TSPI_HOST_CAPTURE_FORMAT", |c, v| c.capture_format = v),
        ("TSPI_HOST_CAPTURE_RAW_FORMAT", |c, v| {
            c.capture_raw_format = v
        }),
        ("TSPI_HOST_CAPTURE_FILE_CHANNELS", |c, v| {
            if let Ok(n) = v.parse() {
                c.capture_file_channels = n
            }
        }),
        // T74 前置：capture source=motu|inferno|aes67 的端点/PTP 覆盖（部署 EnvironmentFile 用）。
        ("TSPI_HOST_CAPTURE_MODE", |c, v| c.capture_mode = v),
        ("TSPI_HOST_CAPTURE_PTP_LOCK", |c, v| c.capture_ptp_lock = v),
        ("TSPI_HOST_CAPTURE_INFERNO_IFACE", |c, v| {
            c.capture_inferno_iface = v
        }),
        ("TSPI_HOST_CAPTURE_AES67_SDP", |c, v| {
            c.capture_aes67_sdp = v
        }),
        ("TSPI_HOST_CAPTURE_AES67_CARD", |c, v| {
            c.capture_aes67_card = v
        }),
        ("TSPI_HOST_CAPTURE_AES67_DAEMON_READY", |c, v| {
            c.capture_aes67_daemon_ready = v == "true"
        }),
        // T239：source=aes67 传输选择与用户态接收端参数（部署 EnvironmentFile 用）。
        ("TSPI_HOST_CAPTURE_AES67_TRANSPORT", |c, v| {
            c.capture_aes67_transport = v
        }),
        ("TSPI_HOST_CAPTURE_AES67_IFACE", |c, v| {
            c.capture_aes67_iface = v
        }),
        ("TSPI_HOST_CAPTURE_AES67_GROUP_ALLOW", |c, v| {
            c.capture_aes67_group_allow = v
        }),
        ("TSPI_HOST_CAPTURE_AES67_SOURCE_ALLOW", |c, v| {
            c.capture_aes67_source_allow = v
        }),
        ("TSPI_HOST_CAPTURE_AES67_IO_PRIORITY", |c, v| {
            if let Ok(n) = v.parse() {
                c.capture_aes67_io_priority = n
            }
        }),
        ("TSPI_HOST_CAPTURE_AES67_STALL_BLOCKS", |c, v| {
            if let Ok(n) = v.parse() {
                c.capture_aes67_stall_blocks = n
            }
        }),
        ("TSPI_HOST_CAPTURE_AES67_STAY_ON_STREAM_END", |c, v| {
            c.capture_aes67_stay_on_stream_end = v == "true" || v == "1"
        }),
        ("TSPI_HOST_CAPTURE_AES67_SILENCE_HOLD_MS", |c, v| {
            if let Ok(n) = v.parse() {
                c.capture_aes67_silence_hold_ms = n
            }
        }),
        ("TSPI_HOST_CAPTURE_AES67_STALL_FAIL_CLOSE_MS", |c, v| {
            if let Ok(n) = v.parse() {
                c.capture_aes67_stall_fail_close_ms = n
            }
        }),
        ("TSPI_HOST_CAPTURE_AES67_REANCHOR_WATCHDOG", |c, v| {
            c.capture_aes67_reanchor_watchdog = v == "true" || v == "1"
        }),
        // T203：中置下混内容适配（部署 EnvironmentFile 用；布局 id 由 T57 overlay 解析）。
        ("TSPI_HOST_CONTENT_LAYOUT_PRESETS", |c, v| {
            c.content_layout_presets = v
        }),
        ("TSPI_HOST_CONTENT_INPUT_LAYOUT", |c, v| {
            c.content_input_layout = v
        }),
        ("TSPI_HOST_CONTENT_OUTPUT_LAYOUT", |c, v| {
            c.content_output_layout = v
        }),
        ("TSPI_HOST_CONTENT_CENTER", |c, v| c.content_center = v),
        // F6/task-216：RT 内核应用开机开关/参数（EnvironmentFile 侧，无需改 toml）。
        ("TSPI_HOST_APPLY_AFFINITY", |c, v| {
            c.apply_affinity = v == "true" || v == "1"
        }),
        ("TSPI_HOST_MLOCKALL", |c, v| {
            c.rt_mlockall = v == "true" || v == "1"
        }),
        ("TSPI_HOST_IO_CPU", |c, v| {
            if let Ok(cpu) = parse_rt_io_cpu(&v) {
                c.rt_io_cpu = cpu
            }
        }),
        // task-246：DSP 引擎选择（plan-arena；systemd EnvironmentFile 可覆盖）。
        ("TSPI_HOST_DSP_ENGINE", |c, v| {
            c.dsp_engine = v.to_ascii_lowercase()
        }),
    ];
    for (name, apply) in env_map {
        if let Ok(value) = std::env::var(name) {
            apply(&mut config, value);
        }
    }
    if config.workers != WORKERS && config.workers != 2 {
        return Err(format!("workers must be 2 or 4, got {}", config.workers));
    }
    if config.safe_mode != "normal" && config.safe_mode != "mute" {
        return Err(format!(
            "safe_mode must be normal|mute, got {}",
            config.safe_mode
        ));
    }
    if config.dsp_engine != "direct"
        && config.dsp_engine != "partitioned"
        && config.dsp_engine != "auto"
    {
        return Err(format!(
            "dsp engine must be direct|partitioned|auto, got {}",
            config.dsp_engine
        ));
    }
    // task-320：aes67_iface 留空 = 0.0.0.0（旧行为）；多网卡机器上启动警告（不拒绝启动）。
    if let Some(warning) = aes67_multicast_listen_warning(&config) {
        warnings.push(warning);
    }
    Ok(config)
}

fn load_host_config(path: &str) -> Result<(HostRunConfig, Vec<String>), String> {
    let text = std::fs::read_to_string(path).map_err(|err| format!("read {path}: {err}"))?;
    let text = text.trim_start_matches('﻿');
    let mut warnings = Vec::new();
    let config = parse_host_config(text, &mut warnings)?;
    Ok((config, warnings))
}

fn graph_for(name: &str) -> Result<Graph, String> {
    match name {
        "stereo_identity" => Ok(graph_stereo_identity()),
        "2x2_offdiag" => Ok(graph_2x2_offdiag()),
        other => Err(format!("unknown graph profile: {other}")),
    }
}

fn hex32(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

fn json_escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 解析 CPU 列表文本（"2-5" / "2,3,4,5" / "2"；越界/重复拒绝）。
fn parse_cpu_list(text: &str, min_cpus: usize) -> Result<Vec<u16>, String> {
    let mut cpus: Vec<u16> = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            let start: u16 = start
                .trim()
                .parse()
                .map_err(|_| format!("bad cpu: {part}"))?;
            let end: u16 = end.trim().parse().map_err(|_| format!("bad cpu: {part}"))?;
            if end < start || end - start > 63 {
                return Err(format!("bad cpu range: {part}"));
            }
            for cpu in start..=end {
                cpus.push(cpu);
            }
        } else {
            cpus.push(part.parse().map_err(|_| format!("bad cpu: {part}"))?);
        }
    }
    // 只要够 config.workers 个就能跑（4 核机器上用 2 worker + 2 核 housekeeping 是合法配置；
    // 原来这里硬用 WORKERS=4，导致 N150 这类 4 核 x86 无法启动 RT 栈）
    if cpus.len() < min_cpus {
        return Err(format!(
            "dsp_cpus needs >= {} entries（config.workers）, got {}",
            min_cpus,
            cpus.len()
        ));
    }
    let mut sorted = cpus.clone();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.len() != cpus.len() {
        return Err("dsp_cpus contains duplicates".to_string());
    }
    if cpus.iter().any(|cpu| *cpu >= 64) {
        return Err("dsp_cpus out of range".to_string());
    }
    Ok(cpus)
}

/// T45 调度计划（本函数只计划；真实内核写入见 `RtRuntime`/task-216：apply_affinity=true 时
/// 启动期一次应用 mlockall/SCHED_FIFO/亲和，失败 fail closed）。
fn build_sched_plan(
    config: &HostRunConfig,
) -> Result<
    (
        khaen_rt_host::scheduler::SchedPlan,
        khaen_rt_host::scheduler::TopologySnapshot,
    ),
    String,
> {
    use khaen_rt_host::scheduler::{
        plan as sched_plan, SchedRequest, TopologySnapshot, CPU_UNKNOWN_CORE, MAX_CPU,
    };
    let cpus = parse_cpu_list(&config.dsp_cpus, config.workers as usize)?;
    let mut online = 0u64;
    let mut cores = [CPU_UNKNOWN_CORE; MAX_CPU];
    for cpu in 0..8u16 {
        online |= 1u64 << cpu;
        cores[cpu as usize] = cpu as i8;
    }
    let topo = TopologySnapshot::new()
        .with_online_mask(online)
        .with_possible_mask(online)
        .with_allowed_mask(online)
        .with_isolated_mask(0)
        .with_nohz_full_mask(0)
        .with_core_map(cores);
    let mut req = SchedRequest::new()
        .with_workers(config.workers)
        .with_priorities(config.dsp_priority, config.io_priority, config.irq_priority)
        .with_tick(config.tick_freq_hz, TICK_BUDGET)
        .with_calibration(
            khaen_core::workers::WORKER_DEFAULT_SPIN_MAX_ITERS,
            khaen_core::workers::WORKER_DEFAULT_CUTOFF_DRAIN_ITERS,
        );
    let mut index = 0usize;
    while index < config.workers as usize {
        req = req.with_dsp_cpu(index, cpus[index]);
        index += 1;
    }
    let plan = sched_plan(&req, &topo).map_err(|status| {
        format!(
            "scheduler plan rejected: {} (ERR_ADMISSION_SCHED=115 checks cpu/topo/priorities/tick)",
            status.code()
        )
    })?;
    Ok((plan, topo))
}

// ---------------------------------------------------------------------------
// F6/task-216：启动期真实 RT 内核应用（T45 计划 → mlockall/SCHED_FIFO/亲和）
// ---------------------------------------------------------------------------

/// 启动期 RT 应用上下文（仅 apply_affinity=true 时存在；非 Linux target 直接拒启）。
///
/// 关键约束（F6 板端实测教训）：写内核必须在**启动期一次**完成（core-v1 §10.1 F-17）；
/// 块循环内不再改亲和/优先级；任何一步失败都 fail closed（调用方返回 Err → exit 1）。
#[derive(Debug)]
struct RtRuntime {
    targets: rt_apply::RtTargets,
    backend: rt_apply::LinuxKernel,
    mlockall_done: bool,
    main_applied: bool,
    workers_applied: u8,
}

/// 把 RT 应用失败变成可读的 fail-closed 启动错误（带 stage/errno/cpu/prio）。
fn rt_apply_error(context: &str, error: &rt_apply::ApplyError) -> String {
    format!(
        "{context} 失败：{error}（rt.apply_affinity=true 要求真实写内核；按 fail-closed 拒绝启动，不静默降级。请确认 Linux target + root/CAP_SYS_NICE + rtprio/memlock 权限）"
    )
}

impl RtRuntime {
    /// [启动/准入] 读当前允许 CPU → 由 T45 计划派生目标 → mlockall；失败返回 Err。
    fn prepare(config: &HostRunConfig, sched: &SchedPlan) -> Result<Option<Self>, String> {
        if !config.apply_affinity {
            return Ok(None);
        }
        if !rt_apply::LINUX_TARGET {
            return Err(
                "rt.apply_affinity=true 但本构建不是 Linux target：无法写 mlockall/SCHED_FIFO/亲和（fail-closed 拒绝启动，不静默降级；Windows 开发请保持 apply_affinity=false）"
                    .to_string(),
            );
        }
        let mut backend = rt_apply::linux_kernel();
        let allowed = rt_apply::allowed_cpu_mask(&mut backend)
            .map_err(|error| rt_apply_error("读取 Cpus_allowed 集合", &error))?;
        let apply_config = rt_apply::RtApplyConfig {
            apply_affinity: true,
            mlockall: config.rt_mlockall,
            io_cpu: config.rt_io_cpu,
        };
        let targets = match rt_apply::prepare_targets(sched, apply_config, allowed) {
            Ok(targets) => targets,
            Err(error) => {
                return Err(format!(
                    "{} [debug: workers={} allowed=0x{:x} dsp_cpus={} io_cpu={} dsp_prio={} io_prio={} irq_prio={} aff0={} aff1={}]",
                    rt_apply_error("由 T45 计划派生 RT 目标", &error),
                    sched.workers,
                    allowed,
                    config.dsp_cpus,
                    apply_config.io_cpu,
                    sched.dsp_priority,
                    sched.io_priority,
                    sched.irq_priority,
                    sched.affinity[0].cpu_index,
                    sched.affinity[1].cpu_index
                ));
            }
        };
        let mlockall_done = if config.rt_mlockall {
            rt_apply::apply_mlockall(&mut backend, rt_apply::MLOCK_ALL_FLAGS)
                .map_err(|error| rt_apply_error("mlockall(MCL_CURRENT|MCL_FUTURE)", &error))?;
            true
        } else {
            false
        };
        Ok(Some(Self {
            targets,
            backend,
            mlockall_done,
            main_applied: false,
            workers_applied: 0,
        }))
    }

    /// [启动/准入] task-246：partitioned 单线程引擎的 RT 目标。
    ///
    /// partitioned 引擎由**主线程**执行共享 FFT UPOLS（无 worker 线程），因此把主线程绑到
    /// T45 计划的第一个 DSP CPU（RK3576 = A73 簇 CPU4；全 A73 比 A53 快 3.1×），优先级取
    /// max(io_priority, dsp_priority)：单线程同时承担 capture/playback I/O 与计算，不存在
    /// 主线程与同核 worker 的 FIFO 饿死问题。mlockall 与读回校验语义同 direct 路径。
    fn prepare_partitioned(
        config: &HostRunConfig,
        sched: &SchedPlan,
    ) -> Result<Option<Self>, String> {
        if !config.apply_affinity {
            return Ok(None);
        }
        if !rt_apply::LINUX_TARGET {
            return Err(
                "rt.apply_affinity=true 但本构建不是 Linux target：无法写 mlockall/SCHED_FIFO/亲和（fail-closed 拒绝启动，不静默降级；Windows 开发请保持 apply_affinity=false）"
                    .to_string(),
            );
        }
        let mut backend = rt_apply::linux_kernel();
        let allowed = rt_apply::allowed_cpu_mask(&mut backend)
            .map_err(|error| rt_apply_error("读取 Cpus_allowed 集合", &error))?;
        let cpu = sched.affinity[0].cpu_index;
        if cpu == CPU_UNUSED
            || cpu as usize >= khaen_rt_host::scheduler::MAX_CPU
            || allowed & (1u64 << cpu) == 0
        {
            return Err(format!(
                "partitioned 引擎：首个 DSP CPU {cpu} 未配置/越界/不在允许集合（fail-closed 拒绝启动）"
            ));
        }
        let priority = sched.io_priority.max(sched.dsp_priority);
        let targets = rt_apply::RtTargets {
            // partitioned 引擎不创建 worker 线程：worker 数如实为 0（不伪造 4-worker 应用）。
            workers: 0,
            worker: [rt_apply::ThreadRtTarget::new(CPU_UNUSED, 0); DSP_CPU_MAX],
            main: rt_apply::ThreadRtTarget::new(cpu, priority),
            irq_priority: sched.irq_priority,
        };
        let mlockall_done = if config.rt_mlockall {
            rt_apply::apply_mlockall(&mut backend, rt_apply::MLOCK_ALL_FLAGS)
                .map_err(|error| rt_apply_error("mlockall(MCL_CURRENT|MCL_FUTURE)", &error))?;
            true
        } else {
            false
        };
        Ok(Some(Self {
            targets,
            backend,
            mlockall_done,
            main_applied: false,
            workers_applied: 0,
        }))
    }

    /// 逐 worker RT 目标（供 BlockWorkers::start_rt；未用槽 None）。
    fn worker_targets(&self) -> [Option<rt_apply::ThreadRtTarget>; WORKERS as usize] {
        self.targets.worker_targets()
    }

    /// worker 已全部真实写入（BlockWorkers::start_rt 成功即全部读回校验通过）。
    fn mark_workers_applied(&mut self) {
        self.workers_applied = self.targets.workers;
    }

    /// [块循环前] 主/I-O owner 线程 SCHED_FIFO（优先级 io_priority）+ 绑非 DSP CPU。
    fn apply_main(&mut self) -> Result<(), String> {
        rt_apply::apply_thread(
            &mut self.backend,
            self.targets.main,
            rt_apply::ApplyRole::Main,
        )
        .map_err(|error| rt_apply_error("主/I-O 线程 SCHED_FIFO+亲和", &error))?;
        self.main_applied = true;
        Ok(())
    }

    /// 报告（kernel_applied 只在 mlockall+主线程+全部 worker 都真实成功时为 true）。
    fn report(&self) -> rt_apply::RtApplyReport {
        rt_apply::RtApplyReport::from_applied(
            &self.targets,
            self.mlockall_done,
            if self.mlockall_done {
                rt_apply::MLOCK_ALL_FLAGS
            } else {
                0
            },
            self.main_applied,
            self.workers_applied,
        )
    }
}

/// sched JSON（§12.3 字段 + F6/task-216 kernel_apply 真实上报）；
/// 未应用时也必须给出显式 reason，不得用固定 kernel_applied:false 假装"已支持"。
fn scheduler_report_json(sched: &SchedPlan, rt: Option<&RtRuntime>) -> String {
    let (kernel_applied, kernel_apply) = match rt {
        Some(runtime) => {
            let report = runtime.report();
            (report.kernel_applied, report.to_json())
        }
        None => (
            false,
            rt_apply::RtApplyReport::disabled(sched.irq_priority).to_json(),
        ),
    };
    format!(
        "{{\"workers\":{},\"dsp_cpus\":[{}],\"dsp_priority\":{},\"io_priority\":{},\"irq_priority\":{},\"tick_freq_hz\":{},\"tick_read_budget\":{},\"affinity_hash\":\"{}\",\"kernel_applied\":{},\"kernel_apply\":{}}}",
        sched.workers,
        sched
            .affinity
            .iter()
            .take(sched.workers as usize)
            .map(|entry| entry.cpu_index.to_string())
            .collect::<Vec<_>>()
            .join(","),
        sched.dsp_priority,
        sched.io_priority,
        sched.irq_priority,
        sched.tick_freq_hz,
        sched.tick_read_budget,
        hex32(&sched.affinity_hash),
        kernel_applied,
        kernel_apply
    )
}

/// F6：backend=alsa 但未开启 RT 应用时的显式告警（板端播放会持续 xrun，不能静默）。
fn rt_not_applied_warning(config: &HostRunConfig) -> Option<String> {
    if config.apply_affinity || config.backend != "alsa" {
        return None;
    }
    Some(
        "rt.apply_affinity=false：F6 —— 真实播放不写 mlockall/SCHED_FIFO/亲和（kernel_applied=false），I/O 主循环会被 worker 忙等拖过 period 预算 → 持续 xrun；播放验收前请设 [rt] apply_affinity=true（或 TSPI_HOST_APPLY_AFFINITY=true）"
            .to_string(),
    )
}

/// 运行期采样率白名单（core-v1 §3.2：48k/96k；192k 一律拒绝）。
const ALSA_ALLOWED_RATES: [u32; 2] = [48_000, 96_000];
/// graph 输出（container）声道映射到设备 playback 的起始 index（4.1.2：ch0..=9）。
const ALSA_PLAYBACK_CHANNEL_BASE: usize = 0;
/// T01 E1：Toslink FL/FR = capture ch12/13（0-based）；loopback 模式取这两路。
const ALSA_LOOPBACK_CAPTURE_CHANNELS: [usize; 2] = [12, 13];
/// 恢复阶段驱动的上界（3 阶段 × recovery_attempts=3 仍有余量；防状态机卡死）。
const ALSA_RECOVERY_STEP_LIMIT: u32 = 16;

/// backend=alsa 的输入模式（T71 板端链路/出声验证）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AlsaIoMode {
    /// 合成输入（与 fake-io 同源 440/660 Hz；不依赖 capture 信号）。
    Synth,
    /// 真实 capture → graph 输入（Toslink FL/FR = capture ch12/13 → 2ch）。
    Loopback,
}

impl AlsaIoMode {
    /// 稳定名称（配置/证据/JSON 用）。
    const fn name(self) -> &'static str {
        match self {
            AlsaIoMode::Synth => "synth",
            AlsaIoMode::Loopback => "loopback",
        }
    }

    /// 解析配置文本（只接受 synth|loopback；拒绝猜测）。
    fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            "synth" => Ok(AlsaIoMode::Synth),
            "loopback" => Ok(AlsaIoMode::Loopback),
            other => Err(format!("alsa.io_mode must be synth|loopback, got {other}")),
        }
    }
}

/// 设备字符串解析结果：`hw:<card>,<device>`（card = 十进制 index 或稳定 id）。
#[derive(Clone, Debug, PartialEq, Eq)]
enum AlsaCardKey {
    /// 十进制 card index（板端 MOTU 实测 4；仍必须经 /proc/asound/cards 解析出 id）。
    Index(u8),
    /// ALSA card 稳定 id（如 UltraLitemk5）。
    Id(String),
}

/// `hw:<card>,<device>` 解析结果。
#[derive(Clone, Debug, PartialEq, Eq)]
struct AlsaDeviceSpec {
    /// card 身份。
    card: AlsaCardKey,
    /// 同一 card 下的 device index。
    device: u8,
}

/// 解析 `hw:<card>,<device>`（card = 0..=31 index 或 ALSA stable id；设备号 0..=255）。
fn parse_alsa_device(text: &str) -> Result<AlsaDeviceSpec, String> {
    let text = text.trim();
    let rest = text
        .strip_prefix("hw:")
        .ok_or_else(|| format!("alsa.device 必须是 hw:<card>,<device>（got {text}）"))?;
    let (card, device) = rest
        .split_once(',')
        .ok_or_else(|| format!("alsa.device 缺少 ','（got {text}）"))?;
    let card = card.trim();
    let device = device.trim();
    if card.is_empty() || device.is_empty() {
        return Err(format!("alsa.device 的 card/device 不能为空（got {text}）"));
    }
    let device: u8 = device
        .parse()
        .map_err(|_| format!("alsa.device index 非法（got {device}）"))?;
    let card = if card.bytes().all(|byte| byte.is_ascii_digit()) {
        let index: u8 = card
            .parse()
            .map_err(|_| format!("alsa.device card index 非法（got {card}）"))?;
        if index > 31 {
            return Err(format!(
                "alsa.device card index 越界（got {index}，allowed 0..=31）"
            ));
        }
        AlsaCardKey::Index(index)
    } else {
        if !card
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err(format!("alsa.device card id 非法（got {card}）"));
        }
        AlsaCardKey::Id(card.to_string())
    };
    Ok(AlsaDeviceSpec { card, device })
}

/// backend + ALSA 运行参数校验（fake 契约：原有 rate/period/format 校验顺序与错误保持）。
///
/// 规则（T71 前置）：backend ∈ {fake, alsa}；rate ∈ {48k, 96k}（192k 显式拒绝）；
/// period ∈ {64,128}；backend=alsa 额外要求 block_size == period_frames、buffer 为
/// period 整数倍、通道/设备串/io_mode 合法。真实设备打开/板端验证 NOT_RUN。
fn validate_backend_config(config: &HostRunConfig) -> Result<(), String> {
    use khaen_core::abi::sample_format as sf;
    match config.backend.as_str() {
        "fake" | "alsa" => {}
        other => {
            return Err(format!(
                "unsupported backend: {other}（allowed: fake|alsa）"
            ))
        }
    }
    if !khaen_rt_host::alsa::is_runtime_rate(config.sample_rate) {
        if config.sample_rate == 192_000 {
            return Err(
                "bad sample_rate 192000：192k 禁用（白名单 48000/96000；MOTU/overlay 固定 48k）"
                    .to_string(),
            );
        }
        return Err(format!(
            "bad sample_rate {}（allowed: {}）",
            config.sample_rate, "48000/96000"
        ));
    }
    debug_assert!(ALSA_ALLOWED_RATES.contains(&config.sample_rate));
    if !khaen_rt_host::alsa::is_runtime_period(config.period_frames) {
        return Err(format!(
            "bad period_frames {}（allowed: 64/128）",
            config.period_frames
        ));
    }
    if !khaen_rt_host::alsa::device_format_allowed(sf::S24_3LE_INTERLEAVED, false) {
        return Err("S24_3LE not allowed for this backend".to_string());
    }
    if config.backend == "alsa" {
        if !khaen_rt_host::alsa::is_runtime_period(config.block_size as u32) {
            return Err(format!(
                "bad block_size {}（allowed: 64/128）",
                config.block_size
            ));
        }
        if config.block_size as u32 != config.period_frames {
            return Err(format!(
                "backend=alsa 要求 block_size == period_frames（{} != {}）",
                config.block_size, config.period_frames
            ));
        }
        if config.buffer_frames < config.period_frames
            || config.buffer_frames % config.period_frames != 0
        {
            return Err(format!(
                "bad buffer_frames {}（必须 >= period 且为 period 整数倍）",
                config.buffer_frames
            ));
        }
        let max_in = khaen_core::rt::MAX_INPUT_CHANNELS as u16;
        let max_out = khaen_core::rt::MAX_OUTPUT_CHANNELS as u16;
        if config.capture_channels == 0 || config.capture_channels > max_in {
            return Err(format!(
                "bad capture_channels {}（allowed 1..={max_in}）",
                config.capture_channels
            ));
        }
        if config.playback_channels == 0 || config.playback_channels > max_out {
            return Err(format!(
                "bad playback_channels {}（allowed 1..={max_out}）",
                config.playback_channels
            ));
        }
        let _ = parse_alsa_device(&config.device)?;
        let mode = AlsaIoMode::parse(&config.alsa_io_mode)?;
        if mode == AlsaIoMode::Loopback
            && config.capture_channels <= ALSA_LOOPBACK_CAPTURE_CHANNELS[1] as u16
        {
            return Err(format!(
                "alsa.io_mode=loopback 需要 capture_channels > {}（Toslink FL/FR = capture ch12/13）",
                ALSA_LOOPBACK_CAPTURE_CHANNELS[1]
            ));
        }
    }
    // T74：capture source 校验（file 固定 48k、与 loopback 输入互斥、错误码稳定）。
    validate_capture_config(config)?;
    Ok(())
}

/// fake backend 的 T17 owner 契约 JSON（只做 hwparams/格式/period 校验与 hash；不打开设备）。
///
/// 输出字段/顺序与 final-wiring 冻结证据逐字节一致（backend=fake 契约不变）。
fn fake_alsa_contract_json(config: &HostRunConfig) -> Result<String, String> {
    use khaen_core::abi::sample_format as sf;
    if !khaen_rt_host::alsa::is_runtime_rate(config.sample_rate) {
        return Err(format!("bad sample_rate {}", config.sample_rate));
    }
    if !khaen_rt_host::alsa::is_runtime_period(config.period_frames) {
        return Err(format!("bad period_frames {}", config.period_frames));
    }
    if !khaen_rt_host::alsa::device_format_allowed(sf::S24_3LE_INTERLEAVED, false) {
        return Err("S24_3LE not allowed for this backend".to_string());
    }
    let sample_bytes = khaen_rt_host::alsa::sample_bytes(sf::S24_3LE_INTERLEAVED)
        .ok_or_else(|| "sample_bytes(S24_3LE) failed".to_string())?;
    let hw = TspiHwParamsV1 {
        abi_version: ABI_VERSION,
        sample_format: sf::S24_3LE_INTERLEAVED,
        n_in: config.capture_channels,
        n_out: config.playback_channels,
        sample_rate: config.sample_rate,
        period_frames: config.period_frames,
        buffer_frames: config.buffer_frames,
        in_stride_bytes: config.period_frames * config.capture_channels as u32 * sample_bytes,
        out_stride_bytes: config.period_frames * config.playback_channels as u32 * sample_bytes,
        flags: 0,
        reserved: [0; 4],
    };
    let hash = khaen_rt_host::alsa::hwparams_hash(&hw);
    Ok(format!(
        "{{\"backend\":\"fake\",\"device\":\"{}\",\"owner\":\"host\",\"sample_format\":\"S24_3LE\",\"period_frames\":{},\"buffer_frames\":{},\"capture_channels\":{},\"playback_channels\":{},\"hwparams_hash\":{}}}",
        json_escape_text(&config.device),
        config.period_frames,
        config.buffer_frames,
        config.capture_channels,
        config.playback_channels,
        hash
    ))
}

// ---------------------------------------------------------------------------
// T74 前置：capture source = file（预渲染 WAV/raw 多声道文件 → graph → ALSA/fake）
//
// 只做 host 入口接线：启动期一次性读入/校验（非 RT），块循环只从内存按 block 解码进既有
// planar 输入缓冲（无文件 I/O、零堆分配）。布局 strict：2ch = 2.0（FL/FR）；8ch = 4.1.2
// （FL0 FR1 SL2 SR3 HFL4 HFR5 ch6 空 SUB7），不做隐式上/下混，声道数必须与图/plan 输入
// 宽度逐通道一致。真实板端文件播放/听音 NOT_RUN（evidence/cursor-fix-fileinput/）。
// ---------------------------------------------------------------------------

/// file 输入唯一采样率（T74：48k；192k/176.4k 明确拒绝，96k 也不作 file 输入）。
const FILE_CAPTURE_RATE: u32 = 48_000;
/// 2.0 strict 文件声道数（FL/FR）。
const FILE_CAPTURE_STEREO_CHANNELS: usize = 2;
/// 4.1.2 文件容器声道数（8ch；ch6 空）。
const FILE_CAPTURE_412_CHANNELS: usize = 8;
/// 5.1 文件声道数（6ch；FL FR C LFE SL SR）。
///
/// 只在 [content] 声明内容输入布局（T203 中置下混，如 h16-5.1）时启用；无内容适配时
/// 6ch 文件仍按 strict 口径拒绝（既有 2/8ch 行为不变）。
const FILE_CAPTURE_51_CHANNELS: usize = 6;
/// 4.1.2 唯一空槽（0-based：FL0 FR1 SL2 SR3 HFL4 HFR5 ch6 SUB7）。
const FILE_CAPTURE_412_EMPTY_CHANNEL: usize = 6;

/// [capture] source 合法值（synth = 既有 440/660Hz 合成输入；file = 预渲染文件）。
const CAPTURE_SOURCE_SYNTH: &str = "synth";
const CAPTURE_SOURCE_FILE: &str = "file";
/// T74 前置：AOIP/MOTU 输入源（与 [capture] mode 一一对应，互斥）。
const CAPTURE_SOURCE_MOTU: &str = "motu";
const CAPTURE_SOURCE_INFERNO: &str = "inferno";
const CAPTURE_SOURCE_AES67: &str = "aes67";
/// 模式面取值（T44 A01/K39：off/toslink/aes67/dante 互斥）。
const CAPTURE_MODE_OFF: &str = "off";
const CAPTURE_MODE_TOSLINK: &str = "toslink";
const CAPTURE_MODE_AES67: &str = "aes67";
const CAPTURE_MODE_DANTE: &str = "dante";
/// 启动期 PTP 锁断言（真实 T19 servo/板端 PTP 未接线；locked 才允许 AoIP 采集）。
const CAPTURE_PTP_LOCKED: &str = "locked";
const CAPTURE_PTP_UNLOCKED: &str = "unlocked";
/// T239：source=aes67 的两条互斥传输方式（同 source/mode，同一时刻只有一个 capture owner）。
const CAPTURE_AES67_TRANSPORT_RAVENNA: &str = "ravenna";
const CAPTURE_AES67_TRANSPORT_USERSPACE: &str = "userspace";
/// T239：userspace 传输的 capture 块长（任务卡/原型固定 128 帧 @48k = 2.667 ms）。
const AES67_USER_BLOCK_FRAMES: usize = khaen_rt_host::aes67_rx::AES67_RX_BLOCK_FRAMES;
/// T239：userspace 传输的媒体采样率（AES67 profile 48k）。
const AES67_USER_RATE_HZ: u32 = aoip::AES67_RATE_HZ;

/// T239-crackle：hold 语义默认值（毫秒）。`aes67_silence_hold_ms` = 连续静音达到该值
/// 请求一次内部 reanchor（standalone `--stream-end-ms` 默认 80ms 同口径）。
const AES67_DEFAULT_SILENCE_HOLD_MS: u32 = 80;
/// `aes67_stall_fail_close_ms`：hold 期间静音总时长超过该值才 fail closed（0 = 永不）。
/// 必须显著大于 NSP 会话启动/间隙（6–10s），避免每次会话切换整进程重启。
const AES67_DEFAULT_STALL_FAIL_CLOSE_MS: u32 = 10_000;
/// stale-jitter 看门狗检查间隔（块；48k/128 ≈ 500ms 一次，非 RT 热点）。
const AES67_WATCHDOG_INTERVAL_BLOCKS: u64 = 188;
/// 看门狗判"有包但全被 jitter 拒绝"的最小新增 datagrams（≈600pps > 阈值，NSP ~1000pps）。
const AES67_WATCHDOG_MIN_DATAGRAMS: u64 = 300;
/// 看门狗连续命中次数上限（超过 = 内部 reanchor 无效 → fail closed 交 systemd）。
const AES67_WATCHDOG_MAX_STRIKES: u32 = 3;

/// 稳定错误码（run 错误 JSON 的 error 文本以该码开头；单测逐码锁定，不随文案漂移）。
const ERR_HOST_CAPTURE_CONFIG: &str = "ERR_HOST_CAPTURE_CONFIG";
const ERR_HOST_CAPTURE_CONFLICT: &str = "ERR_HOST_CAPTURE_CONFLICT";
const ERR_HOST_FILE_READ: &str = "ERR_HOST_FILE_READ";
const ERR_HOST_FILE_FORMAT: &str = "ERR_HOST_FILE_FORMAT";
const ERR_HOST_FILE_RATE: &str = "ERR_HOST_FILE_RATE";
const ERR_HOST_FILE_CHANNELS: &str = "ERR_HOST_FILE_CHANNELS";
const ERR_HOST_FILE_LAYOUT: &str = "ERR_HOST_FILE_LAYOUT";
const ERR_HOST_FILE_EMPTY: &str = "ERR_HOST_FILE_EMPTY";

fn capture_error(code: &str, detail: impl AsRef<str>) -> String {
    format!("{code}: {}", detail.as_ref())
}

/// file 采样率错误（192k/176.4k 显式点名；其余非 48k 拒绝）。
fn file_rate_error(rate: u32) -> (&'static str, String) {
    if rate == 192_000 || rate == 176_400 {
        (
            ERR_HOST_FILE_RATE,
            format!(
                "file sample_rate {rate}：192k/176.4k 禁用（file input 固定 48000；MOTU/overlay 48k）"
            ),
        )
    } else {
        (
            ERR_HOST_FILE_RATE,
            format!("file sample_rate {rate}（file input 只允许 48000）"),
        )
    }
}

/// file 样本编码（T74 只允许 S24_3LE 或 f32；不接受 16-bit / 32-bit int / 隐式移位）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileSampleKind {
    S24Le,
    F32Le,
}

impl FileSampleKind {
    const fn name(self) -> &'static str {
        match self {
            FileSampleKind::S24Le => "S24_3LE",
            FileSampleKind::F32Le => "F32_LE",
        }
    }

    const fn bytes(self) -> usize {
        match self {
            FileSampleKind::S24Le => 3,
            FileSampleKind::F32Le => 4,
        }
    }

    fn parse(text: &str) -> Result<Self, String> {
        match text.trim().to_ascii_lowercase().as_str() {
            "s24_3le" | "s24le" | "s24" => Ok(FileSampleKind::S24Le),
            "f32le" | "f32" | "float32" => Ok(FileSampleKind::F32Le),
            other => Err(format!(
                "[capture] raw_format 必须是 s24_3le|f32le（got {other}）"
            )),
        }
    }
}

/// 文件读完后的尾部策略（T74：静音补满继续 / 最后一帧后优雅退出，均不 crash）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileEofPolicy {
    Silence,
    Exit,
}

impl FileEofPolicy {
    const fn name(self) -> &'static str {
        match self {
            FileEofPolicy::Silence => "silence",
            FileEofPolicy::Exit => "exit",
        }
    }

    fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            "silence" => Ok(FileEofPolicy::Silence),
            "exit" => Ok(FileEofPolicy::Exit),
            other => Err(format!("[capture] eof 必须是 silence|exit（got {other}）")),
        }
    }
}

/// 一个 block 的 file 输入结果（块循环零分配，只用启动期预分配的 FileCapture 缓冲）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileFill {
    /// 本块写入了文件数据；last=true 表示这是文件最后一帧所在块（尾部已补静音）。
    Data { last: bool },
    /// 文件已读完（eof=silence 策略下后续块全部静音；eof=exit 不会走到这里）。
    Ended,
}
fn le_u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset + 2)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn le_u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// fmt chunk 解析结果（只保留 host 消费的字段）。
#[derive(Clone, Copy, Debug)]
struct WavFmt {
    channels: u16,
    sample_rate: u32,
    block_align: u16,
    sample_kind: FileSampleKind,
}

/// 解析 WAV 规格（RIFF/WAVE + fmt + data；EXTENSIBLE 允许 PCM24/f32；未知 chunk 跳过）。
#[derive(Clone, Copy, Debug)]
struct WavSpec {
    channels: usize,
    sample_kind: FileSampleKind,
    data_offset: usize,
    data_len: usize,
}

fn parse_wav_fmt(bytes: &[u8], body: usize, size: usize) -> Result<WavFmt, (&'static str, String)> {
    let format_error = |detail: &str| (ERR_HOST_FILE_FORMAT, detail.to_string());
    if size < 16 {
        return Err(format_error("fmt chunk 短于 16 B"));
    }
    let audio_format = le_u16_at(bytes, body).ok_or_else(|| format_error("fmt 截断"))?;
    let channels = le_u16_at(bytes, body + 2).ok_or_else(|| format_error("fmt 截断"))?;
    let sample_rate = le_u32_at(bytes, body + 4).ok_or_else(|| format_error("fmt 截断"))?;
    let block_align = le_u16_at(bytes, body + 12).ok_or_else(|| format_error("fmt 截断"))?;
    let bits = le_u16_at(bytes, body + 14).ok_or_else(|| format_error("fmt 截断"))?;
    let sample_kind = match audio_format {
        1 => match bits {
            24 => FileSampleKind::S24Le,
            other => {
                return Err(format_error(&format!(
                    "PCM bits={other}：file input 仅支持 24-bit（S24_3LE）或 IEEE float32"
                )))
            }
        },
        3 => match bits {
            32 => FileSampleKind::F32Le,
            other => {
                return Err(format_error(&format!(
                    "IEEE_FLOAT bits={other}：file input 仅支持 32-bit float"
                )))
            }
        },
        0xFFFE => {
            if size < 40 {
                return Err(format_error(
                    "WAVE_FORMAT_EXTENSIBLE 需要 >= 40 B fmt chunk",
                ));
            }
            let valid_bits = le_u16_at(bytes, body + 18).ok_or_else(|| format_error("fmt 截断"))?;
            let sub_format = le_u16_at(bytes, body + 24).ok_or_else(|| format_error("fmt 截断"))?;
            if valid_bits != 0 && valid_bits != bits {
                return Err(format_error(
                    "EXTENSIBLE valid_bits != container bits（拒绝隐式移位）",
                ));
            }
            match (sub_format, bits) {
                (1, 24) => FileSampleKind::S24Le,
                (3, 32) => FileSampleKind::F32Le,
                (1, other) | (3, other) => {
                    return Err(format_error(&format!(
                        "EXTENSIBLE subformat PCM/FLOAT bits={other}：仅支持 PCM24 或 float32"
                    )))
                }
                (other, _) => {
                    return Err(format_error(&format!(
                        "EXTENSIBLE subformat={other}：不是 PCM/IEEE_FLOAT"
                    )))
                }
            }
        }
        other => {
            return Err(format_error(&format!(
                "audio_format={other}：仅支持 PCM(1)/IEEE_FLOAT(3)/EXTENSIBLE(0xFFFE)"
            )))
        }
    };
    Ok(WavFmt {
        channels,
        sample_rate,
        block_align,
        sample_kind,
    })
}

fn parse_wav_spec(bytes: &[u8]) -> Result<WavSpec, (&'static str, String)> {
    let format_error = |detail: &str| (ERR_HOST_FILE_FORMAT, detail.to_string());
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(format_error("不是 RIFF/WAVE 文件（需要 RIFF....WAVE 头）"));
    }
    let mut fmt: Option<WavFmt> = None;
    let mut data: Option<(usize, usize)> = None;
    let mut offset = 12usize;
    while offset + 8 <= bytes.len() {
        let id = [
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ];
        let size =
            le_u32_at(bytes, offset + 4).ok_or_else(|| format_error("chunk size 截断"))? as usize;
        let body = offset + 8;
        let end = body
            .checked_add(size)
            .filter(|end| *end <= bytes.len())
            .ok_or_else(|| format_error("chunk body 超出文件末尾"))?;
        match &id {
            b"fmt " => {
                if fmt.is_some() {
                    return Err(format_error("重复 fmt chunk"));
                }
                fmt = Some(parse_wav_fmt(bytes, body, size)?);
            }
            b"data" => {
                if data.is_some() {
                    return Err(format_error("多 data chunk 不支持"));
                }
                data = Some((body, size));
            }
            _ => {}
        }
        offset = end + (size & 1);
    }
    let fmt = fmt.ok_or_else(|| format_error("缺少 fmt chunk"))?;
    let (data_offset, data_len) = data.ok_or_else(|| format_error("缺少 data chunk"))?;
    if data_len == 0 {
        return Err((ERR_HOST_FILE_EMPTY, "data chunk 为空（0 帧）".to_string()));
    }
    if fmt.channels == 0 {
        return Err(format_error("channels=0"));
    }
    if fmt.sample_rate != FILE_CAPTURE_RATE {
        return Err(file_rate_error(fmt.sample_rate));
    }
    let channels = fmt.channels as usize;
    if channels != FILE_CAPTURE_STEREO_CHANNELS
        && channels != FILE_CAPTURE_51_CHANNELS
        && channels != FILE_CAPTURE_412_CHANNELS
    {
        return Err((
            ERR_HOST_FILE_CHANNELS,
            format!(
                "file channels={channels}：只支持 2（2.0 strict）、6（5.1，需 [content] input_layout）或 8（4.1.2 strict）；不做隐式上/下混"
            ),
        ));
    }
    let frame_bytes = channels * fmt.sample_kind.bytes();
    if fmt.block_align as usize != frame_bytes {
        return Err(format_error(&format!(
            "block_align={} != channels {} x sample_bytes {}",
            fmt.block_align,
            channels,
            fmt.sample_kind.bytes()
        )));
    }
    if data_len % frame_bytes != 0 {
        return Err(format_error("data 长度不是整帧（截断文件）"));
    }
    Ok(WavSpec {
        channels,
        sample_kind: fmt.sample_kind,
        data_offset,
        data_len,
    })
}
/// 解码一个 file 样本（S24_3LE 符号扩展 / f32 位重解释）。
fn decode_file_sample(bytes: &[u8], kind: FileSampleKind, offset: usize) -> f32 {
    match kind {
        FileSampleKind::S24Le => {
            let raw = (bytes[offset] as i32)
                | ((bytes[offset + 1] as i32) << 8)
                | ((bytes[offset + 2] as i32) << 16);
            let signed = (raw << 8) >> 8;
            signed as f32 / 8_388_608.0
        }
        FileSampleKind::F32Le => f32::from_bits(u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ])),
    }
}

/// 4.1.2 strict：ch6 必须为空（全部 ±0.0），拒绝把其他信号塞进空槽。
fn verify_412_empty_channel(
    bytes: &[u8],
    data_offset: usize,
    frames: u64,
    channels: usize,
    kind: FileSampleKind,
) -> Result<(), String> {
    let frame_bytes = channels * kind.bytes();
    let sample_bytes = kind.bytes();
    let mut frame = 0u64;
    while frame < frames {
        let base = data_offset
            + (frame as usize) * frame_bytes
            + FILE_CAPTURE_412_EMPTY_CHANNEL * sample_bytes;
        let silent = match kind {
            FileSampleKind::S24Le => {
                bytes[base] == 0 && bytes[base + 1] == 0 && bytes[base + 2] == 0
            }
            FileSampleKind::F32Le => {
                (u32::from_le_bytes([
                    bytes[base],
                    bytes[base + 1],
                    bytes[base + 2],
                    bytes[base + 3],
                ]) & 0x7fff_ffff)
                    == 0
            }
        };
        if !silent {
            return Err(format!(
                "4.1.2 strict：ch{FILE_CAPTURE_412_EMPTY_CHANNEL} 必须为空（帧 {frame} 非零）；不支持把其他信号塞进空槽"
            ));
        }
        frame += 1;
    }
    Ok(())
}

/// 启动期读入并校验的 file 输入源（块循环只读内存，无文件 I/O / 无堆分配）。
struct FileCapture {
    path: String,
    format_name: &'static str,
    sample_kind: FileSampleKind,
    channels: usize,
    layout: String,
    frames: u64,
    block_size: usize,
    data_offset: usize,
    bytes: Vec<u8>,
    next_frame: u64,
    blocks_fed: u64,
    tail_silence_blocks: u64,
    eof_reached: bool,
    eof_policy: FileEofPolicy,
    file_bytes: u64,
    sha256: String,
}

impl FileCapture {
    /// 读文件 + strict 校验（48k；channels 与图/plan 输入宽度一致；2ch=2.0 / 6ch=5.1 /
    /// 8ch=4.1.2）。6ch 5.1 只在 [content] 声明内容输入布局（T203 中置下混）时启用：
    /// 文件通道顺序 = T57 输入布局 role channel（如 h16-5.1 = FL FR C LFE SL SR）。
    fn open(
        config: &HostRunConfig,
        expected_channels: usize,
        content: Option<&ContentAdapter>,
    ) -> Result<Self, String> {
        let content_layout =
            content.map(|adapter| (adapter.input_layout_id(), adapter.source_channels()));
        if let Some((layout_id, channels)) = content_layout {
            if channels != expected_channels {
                return Err(capture_error(
                    ERR_HOST_CONTENT_LAYOUT,
                    format!(
                        "[content] 输入布局 {layout_id} 通道数 {channels} != 图/plan 输入宽度 {expected_channels}（拒绝隐式上/下混）"
                    ),
                ));
            }
        }
        let supported = match content_layout {
            // 声明内容适配：允许 2.0/5.1/4.1.2 strict（6ch 仅此路径启用）。
            Some(_) => {
                expected_channels == FILE_CAPTURE_STEREO_CHANNELS
                    || expected_channels == FILE_CAPTURE_51_CHANNELS
                    || expected_channels == FILE_CAPTURE_412_CHANNELS
            }
            None => {
                expected_channels == FILE_CAPTURE_STEREO_CHANNELS
                    || expected_channels == FILE_CAPTURE_412_CHANNELS
            }
        };
        if !supported {
            return Err(capture_error(
                ERR_HOST_FILE_LAYOUT,
                format!(
                    "图/plan 输入宽度 {expected_channels} 不是 file 支持的 strict 布局（2=2.0 / 8=4.1.2；6=5.1 仅在 [content] input_layout 声明时启用；不做隐式上/下混）"
                ),
            ));
        }
        let path = config.capture_path.trim();
        if path.is_empty() {
            return Err(capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                "source=file 需要 [capture] path = <WAV/raw 路径>",
            ));
        }
        let bytes = std::fs::read(path).map_err(|err| {
            capture_error(
                ERR_HOST_FILE_READ,
                format!("读取 file input 失败 {path}: {err}"),
            )
        })?;
        if bytes.is_empty() {
            return Err(capture_error(
                ERR_HOST_FILE_EMPTY,
                format!("file input 为空: {path}"),
            ));
        }
        let format_name = config.capture_format.trim();
        let (data_offset, data_len, channels, sample_kind) = match format_name {
            "wav" => {
                let spec =
                    parse_wav_spec(&bytes).map_err(|(code, detail)| capture_error(code, detail))?;
                (
                    spec.data_offset,
                    spec.data_len,
                    spec.channels,
                    spec.sample_kind,
                )
            }
            "raw" => {
                let channels = config.capture_file_channels as usize;
                let six_allowed = content_layout.is_some() && channels == FILE_CAPTURE_51_CHANNELS;
                if channels != FILE_CAPTURE_STEREO_CHANNELS
                    && channels != FILE_CAPTURE_412_CHANNELS
                    && !six_allowed
                {
                    return Err(capture_error(
                        ERR_HOST_FILE_CHANNELS,
                        format!(
                            "capture.format=raw 需要 [capture] channels = 2|8（声明 [content] input_layout 时另支持 6=5.1；got {channels}）"
                        ),
                    ));
                }
                let sample_kind = FileSampleKind::parse(&config.capture_raw_format)
                    .map_err(|detail| capture_error(ERR_HOST_CAPTURE_CONFIG, detail))?;
                (0usize, bytes.len(), channels, sample_kind)
            }
            other => {
                return Err(capture_error(
                    ERR_HOST_CAPTURE_CONFIG,
                    format!("[capture] format 必须是 wav|raw（got {other}）"),
                ))
            }
        };
        if channels != expected_channels {
            return Err(capture_error(
                ERR_HOST_FILE_CHANNELS,
                format!(
                    "file channels {channels} != 图/plan 输入宽度 {expected_channels}（strict：不做隐式上/下混）"
                ),
            ));
        }
        if config.capture_file_channels != 0 && config.capture_file_channels as usize != channels {
            return Err(capture_error(
                ERR_HOST_FILE_CHANNELS,
                format!(
                    "[capture] file_channels={} != 文件声道 {channels}",
                    config.capture_file_channels
                ),
            ));
        }
        let frame_bytes = channels * sample_kind.bytes();
        if data_len % frame_bytes != 0 {
            return Err(capture_error(
                ERR_HOST_FILE_FORMAT,
                format!(
                    "data 长度 {data_len} 不是整帧（{channels}ch x {}B = {frame_bytes}B/帧）",
                    sample_kind.bytes()
                ),
            ));
        }
        let frames = (data_len / frame_bytes) as u64;
        if frames == 0 {
            return Err(capture_error(ERR_HOST_FILE_EMPTY, "file input 0 帧"));
        }
        if channels == FILE_CAPTURE_412_CHANNELS && content_layout.is_none() {
            verify_412_empty_channel(&bytes, data_offset, frames, channels, sample_kind)
                .map_err(|detail| capture_error(ERR_HOST_FILE_LAYOUT, detail))?;
        }
        let layout = match content_layout {
            // 声明内容布局：布局名 = T57 输入布局 id（报告/证据用；逐通道对应已校验）。
            Some((layout_id, _)) => layout_id.to_string(),
            None if channels == FILE_CAPTURE_412_CHANNELS => "4.1.2".to_string(),
            None => "2.0-strict".to_string(),
        };
        let eof_policy = FileEofPolicy::parse(&config.capture_eof)
            .map_err(|detail| capture_error(ERR_HOST_CAPTURE_CONFIG, detail))?;
        let sha256 = sha256_hex_full(&bytes);
        Ok(Self {
            path: path.to_string(),
            format_name: if format_name == "raw" { "raw" } else { "wav" },
            sample_kind,
            channels,
            layout,
            frames,
            block_size: config.block_size as usize,
            data_offset,
            file_bytes: bytes.len() as u64,
            bytes,
            next_frame: 0,
            blocks_fed: 0,
            tail_silence_blocks: 0,
            eof_reached: false,
            eof_policy,
            sha256,
        })
    }

    const fn eof_policy(&self) -> FileEofPolicy {
        self.eof_policy
    }

    /// 把下一 block 解码进图输入 planar（ch0 起始 dest[0..stride]、ch1 在 dest[stride..]）。
    /// 文件最后一帧所在块不足 block 时尾部补静音（不 crash、不带上一块残留）。
    fn fill_block(&mut self, dest: &mut [f32], stride: usize, block: usize) -> FileFill {
        let channels = self.channels;
        let sample_bytes = self.sample_kind.bytes();
        let frame_bytes = channels * sample_bytes;
        let mut channel = 0usize;
        while channel < channels {
            let start = channel * stride;
            if start + block <= dest.len() {
                dest[start..start + block].fill(0.0);
            }
            channel += 1;
        }
        if self.next_frame >= self.frames {
            return FileFill::Ended;
        }
        let available = self.frames - self.next_frame;
        let n = (block as u64).min(available) as usize;
        let mut frame = 0usize;
        while frame < n {
            let base = self.data_offset + ((self.next_frame as usize) + frame) * frame_bytes;
            let mut channel = 0usize;
            while channel < channels {
                let dst_index = channel * stride + frame;
                if dst_index < dest.len() {
                    dest[dst_index] = decode_file_sample(
                        &self.bytes,
                        self.sample_kind,
                        base + channel * sample_bytes,
                    );
                }
                channel += 1;
            }
            frame += 1;
        }
        self.next_frame += n as u64;
        self.blocks_fed += 1;
        if self.next_frame >= self.frames {
            self.eof_reached = true;
            return FileFill::Data { last: true };
        }
        FileFill::Data { last: false }
    }

    /// eof=silence：文件读完后整块静音补满（计数供报告/证据）。
    fn note_silence_block(&mut self) {
        self.tail_silence_blocks += 1;
    }

    /// 证据/报告 JSON（sha256/声道/布局/时长/EOF 尾部统计；板端出声 NOT_RUN）。
    fn report_json(&self, exit_reason: &str) -> String {
        let duration_ms = self.frames.saturating_mul(1000) / FILE_CAPTURE_RATE as u64;
        let blocks_available = (self.frames + self.block_size as u64 - 1) / self.block_size as u64;
        let tail_padding_frames = if self.eof_reached {
            self.blocks_fed
                .saturating_mul(self.block_size as u64)
                .saturating_sub(self.frames)
        } else {
            0
        };
        format!(
            "{{\"source\":\"file\",\"path\":\"{}\",\"format\":\"{}\",\"sample_format\":\"{}\",\"channels\":{},\"layout\":\"{}\",\"sample_rate\":{},\"bytes\":{},\"sha256\":\"{}\",\"frames\":{},\"duration_ms\":{},\"blocks_available\":{},\"blocks_fed\":{},\"eof\":\"{}\",\"eof_reached\":{},\"tail_silence_blocks\":{},\"tail_padding_frames\":{},\"exit_reason\":\"{}\"}}",
            json_escape_text(&self.path),
            self.format_name,
            self.sample_kind.name(),
            self.channels,
            self.layout,
            FILE_CAPTURE_RATE,
            self.file_bytes,
            self.sha256,
            self.frames,
            duration_ms,
            blocks_available,
            self.blocks_fed,
            self.eof_policy.name(),
            self.eof_reached,
            self.tail_silence_blocks,
            tail_padding_frames,
            exit_reason
        )
    }
}

/// capture source 取值（T74：synth|file 既有；motu/inferno/aes67 = T43 AoIP/MOTU 输入）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureSource {
    /// 既有 440/660Hz 合成输入（默认，向后兼容）。
    Synth,
    /// 预渲染 WAV/raw 文件输入（task-196）。
    File,
    /// MOTU Toslink 经 capture ch12/13 回环（既有 T71 loopback 路径的显式名字）。
    Motu,
    /// 已有 inferno ALSA 插件（pcm.inferno；S32_LE/96k/16ch；statime PTPv1 虚拟钟）。
    Inferno,
    /// 已有 aes67-daemon + MergingRavennaALSA（hw:<RAVENNA>,0；S32_LE/48k/线上 L24）。
    Aes67,
}

impl CaptureSource {
    fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            CAPTURE_SOURCE_SYNTH => Ok(CaptureSource::Synth),
            CAPTURE_SOURCE_FILE => Ok(CaptureSource::File),
            CAPTURE_SOURCE_MOTU => Ok(CaptureSource::Motu),
            CAPTURE_SOURCE_INFERNO => Ok(CaptureSource::Inferno),
            CAPTURE_SOURCE_AES67 => Ok(CaptureSource::Aes67),
            other => Err(capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                format!("[capture] source 必须是 synth|file|motu|inferno|aes67（got {other}）"),
            )),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            CaptureSource::Synth => CAPTURE_SOURCE_SYNTH,
            CaptureSource::File => CAPTURE_SOURCE_FILE,
            CaptureSource::Motu => CAPTURE_SOURCE_MOTU,
            CaptureSource::Inferno => CAPTURE_SOURCE_INFERNO,
            CaptureSource::Aes67 => CAPTURE_SOURCE_AES67,
        }
    }

    /// 是否 AoIP 网络端点（inferno/aes67：由 T43 端点层驱动）。
    const fn is_aoip(self) -> bool {
        matches!(self, CaptureSource::Inferno | CaptureSource::Aes67)
    }
}

/// 模式面（T44 模式名子集；同一时刻只有一个生效模式 -> capture source 必须对应）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureMode {
    /// 无设备模式（synth/file 离线输入）。
    Off,
    /// MOTU USB/Toslink 直通（MOTU frame 时钟；缺省 P1 日常模式）。
    Toslink,
    /// AES67/RAVENNA（PTPv2；aes67-daemon + RAVENNA LKM）。
    Aes67,
    /// Dante/Inferno（PTPv1 虚拟钟；inferno ALSA 插件）。
    Dante,
}

impl CaptureMode {
    fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            CAPTURE_MODE_OFF => Ok(CaptureMode::Off),
            CAPTURE_MODE_TOSLINK => Ok(CaptureMode::Toslink),
            CAPTURE_MODE_AES67 => Ok(CaptureMode::Aes67),
            CAPTURE_MODE_DANTE => Ok(CaptureMode::Dante),
            other => Err(capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                format!("[capture] mode 必须是 off|toslink|aes67|dante（got {other}）"),
            )),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            CaptureMode::Off => CAPTURE_MODE_OFF,
            CaptureMode::Toslink => CAPTURE_MODE_TOSLINK,
            CaptureMode::Aes67 => CAPTURE_MODE_AES67,
            CaptureMode::Dante => CAPTURE_MODE_DANTE,
        }
    }
}

/// source ↔ mode 一一对应（A01/K39 互斥：一个生效模式只有一个输入 owner）。
fn capture_mode_for_source(source: CaptureSource) -> CaptureMode {
    match source {
        CaptureSource::Synth | CaptureSource::File => CaptureMode::Off,
        CaptureSource::Motu => CaptureMode::Toslink,
        CaptureSource::Inferno => CaptureMode::Dante,
        CaptureSource::Aes67 => CaptureMode::Aes67,
    }
}

/// T239：source=aes67 的两条互斥传输方式（同一 source/mode 只有一个 capture owner）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Aes67Transport {
    /// 既有 aes67-daemon + MergingRavennaALSA 内核卡（T43/T74 路径；默认，向后兼容）。
    Ravenna,
    /// 自研用户态 RTP 接收端（khaen-aoip-aes67；本任务 T239；不依赖 daemon/LKM）。
    Userspace,
}

impl Aes67Transport {
    fn parse(text: &str) -> Result<Self, String> {
        match text.trim() {
            CAPTURE_AES67_TRANSPORT_RAVENNA => Ok(Aes67Transport::Ravenna),
            CAPTURE_AES67_TRANSPORT_USERSPACE => Ok(Aes67Transport::Userspace),
            other => Err(capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                format!("[capture] aes67_transport 必须是 ravenna|userspace（got {other}）"),
            )),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Aes67Transport::Ravenna => CAPTURE_AES67_TRANSPORT_RAVENNA,
            Aes67Transport::Userspace => CAPTURE_AES67_TRANSPORT_USERSPACE,
        }
    }
}

/// task-320：解析"逗号分隔列表或单行 TOML 数组"的配置值（接口/组白名单/来源白名单共用）。
///
/// 最小 TOML 解析器是逐行的，因此只支持**单行**数组（跨行数组给出明确错误，不静默）；
/// 元素两端引号与空白会被去掉，尾随逗号容忍。
fn parse_aes67_list_items(text: &str, key: &str) -> Result<Vec<String>, String> {
    let text = text.trim();
    let inner = if text.starts_with('[') {
        if !text.ends_with(']') || text.len() < 2 {
            return Err(capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                format!(
                    "[capture] {key} 的数组必须单行闭合（最小 TOML 解析器不支持跨行数组）：{text}"
                ),
            ));
        }
        &text[1..text.len() - 1]
    } else {
        text
    };
    let mut items = Vec::new();
    for part in inner.split(',') {
        let part = part.trim();
        let part = config_unquote(part);
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        items.push(part.to_string());
    }
    if items.is_empty() {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!("[capture] {key} 列表为空（想要旧行为请整项留空：{key} = \"\"）"),
        ));
    }
    Ok(items)
}

/// task-320：单个接口条目 → IPv4（IPv4 字面量或网卡名；0.0.0.0 由调用方单独处理）。
fn parse_aes67_interface_item<F>(item: &str, resolve: &mut F) -> Result<std::net::Ipv4Addr, String>
where
    F: FnMut(&str) -> Result<std::net::Ipv4Addr, IfaceLookupError>,
{
    if item.eq_ignore_ascii_case("auto") {
        // 多网卡部署不允许猜：空 = 0.0.0.0，显式写 IPv4/网卡名；auto 明确拒收。
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            "[capture] aes67_iface 不接受 auto（多网卡部署必须显式 IPv4/网卡名；单网卡可留空 = 0.0.0.0）",
        ));
    }
    if let Ok(address) = item.parse::<std::net::Ipv4Addr>() {
        return Ok(address);
    }
    resolve(item).map_err(|error| {
        capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!("[capture] aes67_iface 网卡名 \"{item}\" 解析失败：{error}"),
        )
    })
}

/// [capture] aes67_iface 解析核心（注入网卡名解析器，便于确定性单测）。
///
/// 语法：单个 IPv4 / 网卡名、逗号分隔列表、或单行 TOML 数组；空 = 0.0.0.0 旧行为；
/// 0.0.0.0 只能单独出现（= 旧行为）。多接口用于组播（每个接口一条显式 join）。
fn parse_aes67_iface_with<F>(text: &str, mut resolve: F) -> Result<ReceiverInterfaces, String>
where
    F: FnMut(&str) -> Result<std::net::Ipv4Addr, IfaceLookupError>,
{
    let text = text.trim();
    if text.is_empty() {
        return Ok(ReceiverInterfaces::EMPTY);
    }
    let items = parse_aes67_list_items(text, "aes67_iface")?;
    let mut addresses: Vec<std::net::Ipv4Addr> = Vec::with_capacity(items.len());
    let mut unspecified_items = 0usize;
    for item in &items {
        let address = parse_aes67_interface_item(item, &mut resolve)?;
        if address.is_unspecified() {
            unspecified_items += 1;
            continue;
        }
        if address.is_multicast() || address.is_broadcast() {
            return Err(capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                format!(
                    "[capture] aes67_iface 条目 \"{item}\" 不是可用的单播接口地址（不接受组播/广播）"
                ),
            ));
        }
        addresses.push(address);
    }
    if unspecified_items > 0 {
        if items.len() > 1 {
            return Err(capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                "[capture] aes67_iface 里 0.0.0.0 只能单独使用（0.0.0.0 = 所有接口；不能与具体接口混写）",
            ));
        }
        return Ok(ReceiverInterfaces::EMPTY);
    }
    ReceiverInterfaces::try_from_addresses(&addresses).map_err(|error| {
        capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!("[capture] aes67_iface 接口列表非法：{error}"),
        )
    })
}

/// [capture] aes67_iface → 接口集合（真实系统网卡名解析；空 = 0.0.0.0 旧行为）。
fn parse_aes67_iface(text: &str) -> Result<ReceiverInterfaces, String> {
    parse_aes67_iface_with(text, khaen_aoip_aes67::iface::resolve_interface_name)
}

/// [capture] aes67_group_allow → 组播组白名单（空 = 旧行为）。
fn parse_aes67_group_allow(text: &str) -> Result<GroupAllow, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(GroupAllow::EMPTY);
    }
    let items = parse_aes67_list_items(text, "aes67_group_allow")?;
    let mut groups = Vec::with_capacity(items.len());
    for item in &items {
        let group = item.parse::<std::net::Ipv4Addr>().map_err(|_| {
            capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                format!(
                    "[capture] aes67_group_allow 条目 \"{item}\" 不是 IPv4 地址（期望组播组，如 239.69.16.146）"
                ),
            )
        })?;
        groups.push(group);
    }
    GroupAllow::try_from_groups(&groups).map_err(|error| {
        capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!("[capture] aes67_group_allow 非法：{error}"),
        )
    })
}

/// [capture] aes67_source_allow → RTP 来源白名单（IPv4/CIDR；空 = 旧行为）。
fn parse_aes67_source_allow(text: &str) -> Result<SourceAllow, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(SourceAllow::EMPTY);
    }
    let items = parse_aes67_list_items(text, "aes67_source_allow")?;
    let mut cidrs = Vec::with_capacity(items.len());
    for item in &items {
        let cidr = Ipv4Cidr::parse(item).map_err(|error| {
            capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                format!("[capture] aes67_source_allow 条目 \"{item}\" 非法：{error}"),
            )
        })?;
        cidrs.push(cidr);
    }
    SourceAllow::try_from_cidrs(&cidrs).map_err(|error| {
        capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!("[capture] aes67_source_allow 非法：{error}"),
        )
    })
}

/// task-320：aes67_iface 留空 = 0.0.0.0（所有接口）时的启动警告（注入网卡数，便于确定性单测）。
///
/// 只在 source=aes67 + transport=userspace + iface 为空时产生；非环回 IPv4 网卡 >1 才警告
/// （单网卡 0.0.0.0 是正常旧行为）。警告不阻断启动。
fn aes67_multicast_listen_warning_with(
    config: &HostRunConfig,
    non_loopback_interfaces: usize,
) -> Option<String> {
    if CaptureSource::parse(&config.capture_source).ok()? != CaptureSource::Aes67 {
        return None;
    }
    if Aes67Transport::parse(&config.capture_aes67_transport).ok()?
        != Aes67Transport::Userspace
    {
        return None;
    }
    if !config.capture_aes67_iface.trim().is_empty() {
        return None;
    }
    if non_loopback_interfaces <= 1 {
        return None;
    }
    Some(format!(
        "[capture] aes67_iface 为空 = 0.0.0.0（所有接口）：检测到 {non_loopback_interfaces} 个非环回 IPv4 网卡，\\
组播组会在所有网卡上 join、单播也可从所有接口到达；多网卡部署请显式配置 aes67_iface=<网卡名|IPv4>（多接口用逗号分隔或单行数组）"
    ))
}

/// task-320：真实网卡枚举版启动警告（枚举失败静默跳过——警告不是启动门槛）。
fn aes67_multicast_listen_warning(config: &HostRunConfig) -> Option<String> {
    if CaptureSource::parse(&config.capture_source).ok()? != CaptureSource::Aes67 {
        return None;
    }
    if Aes67Transport::parse(&config.capture_aes67_transport).ok()?
        != Aes67Transport::Userspace
    {
        return None;
    }
    if !config.capture_aes67_iface.trim().is_empty() {
        return None;
    }
    let non_loopback = khaen_aoip_aes67::iface::non_loopback_ipv4_interface_count().ok()?;
    aes67_multicast_listen_warning_with(config, non_loopback)
}

/// [capture] aes67_rx_slots -> H16 单 flow 容器 slot 列表（逗号分隔 0..15，≤8 且不重复）。
fn parse_aes67_rx_slots(text: &str) -> Result<Option<Vec<u8>>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let mut slots: Vec<u8> = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        let slot: u8 = part.parse().map_err(|_| {
            capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                format!("[capture] aes67_rx_slots 非法：\"{text}\"（逗号分隔 H16 slot 0..15）"),
            )
        })?;
        if slot as u16 >= aoip::H16_CONTAINER_CHANNELS {
            return Err(capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                format!(
                    "[capture] aes67_rx_slots slot {slot} 越界（H16 容器只有 0..{}）",
                    aoip::H16_CONTAINER_CHANNELS - 1
                ),
            ));
        }
        if slots.contains(&slot) {
            return Err(capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                format!("[capture] aes67_rx_slots slot {slot} 重复（flow 内 slot 不得重复）"),
            ));
        }
        slots.push(slot);
    }
    if slots.is_empty() || slots.len() > aoip::H16_MAX_FLOW_CHANNELS {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!(
                "[capture] aes67_rx_slots 需要 1..={} 个 slot（got {}）",
                aoip::H16_MAX_FLOW_CHANNELS,
                slots.len()
            ),
        ));
    }
    Ok(Some(slots))
}

/// userspace 传输的启动期配置校验（SDP/网卡/块长/独占字段；不打开 socket）。
fn validate_aes67_userspace_config(config: &HostRunConfig) -> Result<(), String> {
    if config.sample_rate != AES67_USER_RATE_HZ {
        return Err(capture_error(
            aoip::AoipError::Rate.name(),
            format!(
                "capture.source=aes67（transport=userspace）固定 {} Hz（AES67 profile；got {}）",
                AES67_USER_RATE_HZ, config.sample_rate
            ),
        ));
    }
    if config.block_size as usize != AES67_USER_BLOCK_FRAMES {
        return Err(capture_error(
            aoip::AoipError::BlockSize.name(),
            format!(
                "capture.source=aes67（transport=userspace）固定 {} 帧块（48k/2.667ms；got {}）；44.1/96k 或 64 帧不接线",
                AES67_USER_BLOCK_FRAMES, config.block_size
            ),
        ));
    }
    if config.backend == "alsa" && AlsaIoMode::parse(&config.alsa_io_mode)? == AlsaIoMode::Loopback
    {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFLICT,
            "capture.source=aes67（transport=userspace）与 alsa.io_mode=loopback 冲突：用户态 RTP 接收与 MOTU capture ch12/13 是互斥输入 owner",
        ));
    }
    if config.capture_aes67_daemon_ready {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFLICT,
            "capture.source=aes67（transport=userspace）与 aes67_daemon_ready=true 互斥：用户态接收端不依赖外部 aes67-daemon/RAVENNA LKM（请移除该项，避免谎报 daemon 就绪）",
        ));
    }
    let sdp = config.capture_aes67_sdp.trim();
    if sdp.is_empty() {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            "capture.source=aes67（transport=userspace）需要 [capture] aes67_sdp = <SDP 文件路径>（组播地址/端口/通道/编码/参考钟由 SDP 给出）",
        ));
    }
    if !std::path::Path::new(sdp).is_file() {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!("AES67 SDP 文件不存在或不是普通文件：{sdp}"),
        ));
    }
    parse_aes67_iface(&config.capture_aes67_iface)?;
    parse_aes67_group_allow(&config.capture_aes67_group_allow)?;
    parse_aes67_source_allow(&config.capture_aes67_source_allow)?;
    parse_aes67_rx_slots(&config.capture_aes67_rx_slots)?;
    if config.capture_aes67_io_priority > 99 {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!(
                "[capture] aes67_io_priority 必须是 0..=99（Linux SCHED_FIFO 范围；got {}）",
                config.capture_aes67_io_priority
            ),
        ));
    }
    if config.capture_aes67_stall_blocks > 1_000 {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!(
                "[capture] aes67_stall_blocks 必须是 0..=1000（0 = T43 默认 3；got {}）",
                config.capture_aes67_stall_blocks
            ),
        ));
    }
    if config.capture_aes67_silence_hold_ms > 5_000 {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!(
                "[capture] aes67_silence_hold_ms 必须是 0..=5000（0 = 不请求内部 reanchor；got {}）",
                config.capture_aes67_silence_hold_ms
            ),
        ));
    }
    let fail_close = config.capture_aes67_stall_fail_close_ms;
    if fail_close != 0 && !(100..=600_000).contains(&fail_close) {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!(
                "[capture] aes67_stall_fail_close_ms 必须是 0（永不超时）或 100..=600000 ms；got {fail_close}"
            ),
        ));
    }
    let latency = config.capture_aes67_jitter_latency_us;
    if latency != 0 && !(250..=100_000).contains(&latency) {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!(
                "[capture] aes67_jitter_latency_us 必须是 0（默认 4000）或 250..=100000 µs；got {latency}"
            ),
        ));
    }
    let ring = config.capture_aes67_jitter_ring_frames;
    if ring != 0 && !(512..=65_536).contains(&ring) {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!(
                "[capture] aes67_jitter_ring_frames 必须是 0（默认 8192）或 512..=65536 帧；got {ring}"
            ),
        ));
    }
    // PTP 锁字符串仍按既有稳定错误名校验（userspace 传输本身不用 PTP；见报告 clock=usrvclock）。
    capture_ptp_lock_state(config)?;
    Ok(())
}

/// capture source/mode 启动校验（mode 互斥、设备/profile 前置、错误码稳定）。
fn validate_capture_config(config: &HostRunConfig) -> Result<(), String> {
    let source = CaptureSource::parse(&config.capture_source)?;
    // T203：声明 [content] 内容布局时，file 输入额外允许 6ch（5.1 strict）；未声明 = 既有 2/8ch。
    let content_declared = content_config_declared(config);
    let mode = CaptureMode::parse(&config.capture_mode)?;
    let expected_mode = capture_mode_for_source(source);
    if mode != expected_mode {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFLICT,
            format!(
                "capture.source={} 需要 capture.mode={}（got mode={}）：toslink|aes67|dante|off 互斥，一个生效模式只有一个输入 owner",
                source.as_str(),
                expected_mode.as_str(),
                mode.as_str()
            ),
        ));
    }
    let format_name = config.capture_format.trim();
    if format_name != "wav" && format_name != "raw" {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!("[capture] format 必须是 wav|raw（got {format_name}）"),
        ));
    }
    FileSampleKind::parse(&config.capture_raw_format)
        .map_err(|detail| capture_error(ERR_HOST_CAPTURE_CONFIG, detail))?;
    FileEofPolicy::parse(&config.capture_eof)
        .map_err(|detail| capture_error(ERR_HOST_CAPTURE_CONFIG, detail))?;
    let file_channels = config.capture_file_channels as usize;
    let six_channels_allowed = content_declared && file_channels == FILE_CAPTURE_51_CHANNELS;
    if file_channels != 0
        && file_channels != FILE_CAPTURE_STEREO_CHANNELS
        && file_channels != FILE_CAPTURE_412_CHANNELS
        && !six_channels_allowed
    {
        return Err(capture_error(
            ERR_HOST_FILE_CHANNELS,
            format!(
                "[capture] channels 只支持 0（WAV 推导）|2|8（声明 [content] input_layout 时另支持 6=5.1；got {file_channels}）"
            ),
        ));
    }
    if source == CaptureSource::File {
        if config.capture_path.trim().is_empty() {
            return Err(capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                "source=file 需要 [capture] path = <WAV/raw 路径>",
            ));
        }
        if config.sample_rate != FILE_CAPTURE_RATE {
            if config.sample_rate == 192_000 || config.sample_rate == 176_400 {
                return Err(capture_error(
                    ERR_HOST_FILE_RATE,
                    format!(
                        "sample_rate={}：192k/176.4k 禁用（capture.source=file 固定 48000）",
                        config.sample_rate
                    ),
                ));
            }
            return Err(capture_error(
                ERR_HOST_FILE_RATE,
                format!(
                    "sample_rate={}（capture.source=file 只允许 48000）",
                    config.sample_rate
                ),
            ));
        }
        if format_name == "raw"
            && file_channels != FILE_CAPTURE_STEREO_CHANNELS
            && file_channels != FILE_CAPTURE_412_CHANNELS
            && !six_channels_allowed
        {
            return Err(capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                "capture.format=raw 需要 [capture] channels = 2|8（声明 [content] input_layout 时另支持 6=5.1）",
            ));
        }
        if config.backend == "alsa" {
            let io_mode = AlsaIoMode::parse(&config.alsa_io_mode)?;
            if io_mode == AlsaIoMode::Loopback {
                return Err(capture_error(
                    ERR_HOST_CAPTURE_CONFLICT,
                    "capture.source=file 与 alsa.io_mode=loopback 冲突：loopback 输入来自设备 capture ch12/13；file 输入不允许隐式覆盖，拒绝启动",
                ));
            }
        }
    }
    match source {
        CaptureSource::Synth | CaptureSource::File => {}
        CaptureSource::Motu => {
            // MOTU Toslink 经 capture ch12/13 = 既有 T71 loopback 路径：必须是 backend=alsa
            // + io_mode=loopback；否则等于没有真实 capture 流（fake 只有 synth）。
            if config.backend != "alsa" {
                return Err(capture_error(
                    ERR_HOST_CAPTURE_CONFLICT,
                    "capture.source=motu 需要 backend=alsa（真实 MOTU capture ch12/13；backend=fake 无设备输入）",
                ));
            }
            if AlsaIoMode::parse(&config.alsa_io_mode)? != AlsaIoMode::Loopback {
                return Err(capture_error(
                    ERR_HOST_CAPTURE_CONFLICT,
                    "capture.source=motu 需要 alsa.io_mode=loopback（Toslink FL/FR = MOTU capture ch12/13）",
                ));
            }
        }
        CaptureSource::Inferno => {
            if config.backend != "alsa" {
                return Err(capture_error(
                    ERR_HOST_CAPTURE_CONFLICT,
                    "capture.source=inferno 需要 backend=alsa（AoIP ALSA capture 端点 + MOTU 播放；fake 无设备）",
                ));
            }
            if AlsaIoMode::parse(&config.alsa_io_mode)? == AlsaIoMode::Loopback {
                return Err(capture_error(
                    ERR_HOST_CAPTURE_CONFLICT,
                    "capture.source=inferno 与 alsa.io_mode=loopback 冲突（AoIP 端点与 MOTU capture ch12/13 互斥）",
                ));
            }
            if config.sample_rate != aoip::INFERNO_RATE_HZ {
                return Err(capture_error(
                    aoip::AoipError::Rate.name(),
                    format!(
                        "capture.source=inferno 固定 {} Hz（T43 协议 profile；got {}）",
                        aoip::INFERNO_RATE_HZ,
                        config.sample_rate
                    ),
                ));
            }
            // 端点/容器/PTP 前置（纯解析，不打开设备；缺端点/格式错在此拒收）。
            validate_aoip_capture_config(config, source)?;
        }
        CaptureSource::Aes67 => {
            // T239：同一 source 的两条互斥传输（ravenna 默认 = 既有 T43/T74 路径）。
            let transport = Aes67Transport::parse(&config.capture_aes67_transport)?;
            match transport {
                Aes67Transport::Ravenna => {
                    if config.backend != "alsa" {
                        return Err(capture_error(
                            ERR_HOST_CAPTURE_CONFLICT,
                            "capture.source=aes67（transport=ravenna）需要 backend=alsa（RAVENNA ALSA capture 端点 + MOTU 播放；fake 无设备）",
                        ));
                    }
                    if AlsaIoMode::parse(&config.alsa_io_mode)? == AlsaIoMode::Loopback {
                        return Err(capture_error(
                            ERR_HOST_CAPTURE_CONFLICT,
                            "capture.source=aes67（transport=ravenna）与 alsa.io_mode=loopback 冲突（AES67 端点与 MOTU capture ch12/13 互斥）",
                        ));
                    }
                    if config.sample_rate != aoip::AES67_RATE_HZ {
                        return Err(capture_error(
                            aoip::AoipError::Rate.name(),
                            format!(
                                "capture.source=aes67 固定 {} Hz（T43 协议 profile；got {}）",
                                aoip::AES67_RATE_HZ,
                                config.sample_rate
                            ),
                        ));
                    }
                    // userspace-only 参数不得混入 ravenna 路径（避免"配了但没生效"）。
                    if !config.capture_aes67_iface.trim().is_empty()
                        || !config.capture_aes67_group_allow.trim().is_empty()
                        || !config.capture_aes67_source_allow.trim().is_empty()
                        || !config.capture_aes67_rx_slots.trim().is_empty()
                        || config.capture_aes67_io_priority != 0
                        || config.capture_aes67_stall_blocks != 0
                        || config.capture_aes67_stay_on_stream_end
                        || config.capture_aes67_silence_hold_ms != AES67_DEFAULT_SILENCE_HOLD_MS
                        || config.capture_aes67_stall_fail_close_ms
                            != AES67_DEFAULT_STALL_FAIL_CLOSE_MS
                        || config.capture_aes67_reanchor_watchdog
                    {
                        return Err(capture_error(
                            ERR_HOST_CAPTURE_CONFLICT,
                            "capture.aes67_transport=ravenna 不接受 userspace 参数（aes67_iface/aes67_group_allow/aes67_source_allow/io_priority/stall_blocks/rx_slots/stay_on_stream_end/silence_hold_ms/stall_fail_close_ms/reanchor_watchdog）；请设 aes67_transport=\"userspace\" 或移除",
                        ));
                    }
                    // 端点/容器/PTP 前置（纯解析，不打开设备；缺端点/格式错在此拒收）。
                    validate_aoip_capture_config(config, source)?;
                }
                Aes67Transport::Userspace => {
                    // 用户态接收端不打开 ALSA capture 端点：backend=fake（无播放设备，静音链路）
                    // 与 backend=alsa（播放 MOTU，设备节拍驱动）都允许；loopback 输入 owner 互斥。
                    validate_aes67_userspace_config(config)?;
                }
            }
        }
    }
    Ok(())
}

/// 创建 file 输入源（synth/motu/inferno/aes67 = None，保持既有合成/设备输入路径不变）。
///
/// content = Some 时启用 6ch 5.1 内容布局（T203 中置下混；通道顺序由 T57 输入布局给定）。
fn prepare_capture_source(
    config: &HostRunConfig,
    expected_channels: usize,
    content: Option<&ContentAdapter>,
) -> Result<Option<FileCapture>, String> {
    match CaptureSource::parse(&config.capture_source)? {
        CaptureSource::File => FileCapture::open(config, expected_channels, content).map(Some),
        _ => Ok(None),
    }
}

/// [run-loop] file 输入阶段：把下一 block 解码进图输入 planar。
///
/// 返回 true = 本块是文件最后一帧所在块且 eof=exit（调用方写完输出/ack 后优雅退出）。
/// SafetyMute/gate 拒绝的块按 core-v1 §8.2「不处理新块」不消费文件，输入保持全零。
fn file_input_phase(
    file: Option<&mut FileCapture>,
    input: &mut [f32],
    stride: usize,
    block: usize,
    mute: bool,
) -> bool {
    let file = match file {
        Some(file) => file,
        None => return false,
    };
    if mute {
        input.fill(0.0);
        return false;
    }
    match file.fill_block(input, stride, block) {
        FileFill::Data { last } => last && file.eof_policy() == FileEofPolicy::Exit,
        FileFill::Ended => {
            file.note_silence_block();
            input.fill(0.0);
            false
        }
    }
}

/// run 报告 capture 段（synth 保持旧行为的最小对象；file 给 sha256/布局/EOF 统计；
/// T74 前置：motu/inferno/aes67（ravenna）给模式/端点/PTP/角色映射/失流计数；
/// T239：aes67（userspace）给 SDP/flow/socket/jitter/失流计数 + 适配层报告）。
fn capture_report_json(
    config: &HostRunConfig,
    file: Option<&FileCapture>,
    capture: Option<&CaptureInput>,
    blocks: u64,
    exit_reason: &str,
) -> String {
    if let Some(file) = file {
        return file.report_json(exit_reason);
    }
    if let Some(capture) = capture {
        return match capture {
            CaptureInput::Aoip(aoip) => aoip.report_json(blocks, exit_reason),
            CaptureInput::Aes67User(userspace) => userspace.report_json(exit_reason),
        };
    }
    if config.capture_source.trim() == CAPTURE_SOURCE_MOTU {
        return format!(
            "{{\"source\":\"motu\",\"mode\":\"toslink\",\"input\":\"alsa-loopback\",\"device\":\"{}\",\"capture_channels\":{},\"toslink_channels\":[{},{}],\"sample_rate\":{},\"period_frames\":{},\"blocks\":{},\"exit_reason\":\"{}\",\"note\":\"MOTU capture ch12/13（T71 loopback 路径升格）；真实板端出声/听音 NOT_RUN\"}}",
            json_escape_text(&config.device),
            config.capture_channels,
            ALSA_LOOPBACK_CAPTURE_CHANNELS[0],
            ALSA_LOOPBACK_CAPTURE_CHANNELS[1],
            config.sample_rate,
            config.period_frames,
            blocks,
            exit_reason
        );
    }
    "{\"source\":\"synth\"}".to_string()
}
// ---------------------------------------------------------------------------
// T74 前置：capture source = motu / inferno / aes67（T43 AoIP 端点 -> 图输入）
//
// 三条设备输入源互斥（同一时刻只有一个，见 [capture] source/mode 校验）：
// - motu   = MOTU Toslink 经 capture ch12/13（既有 T71 alsa.io_mode=loopback 路径升格为显式
//            source；真实设备打开/读取仍由 backend=alsa 的 T17 duplex 完成）；
// - inferno= 已有 inferno ALSA 插件（pcm.inferno；S32_LE、96k、16ch 容器；statime PTPv1
//            虚拟钟 /tmp/ptp-usrvclock）；
// - aes67  = 已有 aes67-daemon + MergingRavennaALSA（hw:<RAVENNA>,0；S32_LE、48k、线上 L24）。
//
// 复用 T43 `aoip.rs` 端点层（不重写协议）：端点规格/preflight/唯一 PTP 来源、AES67 SDP
// 只读解析与 flow 交叉校验、H16 逻辑 12/容器 16 + 显式角色映射（plan_role_selection，禁止
// 静默丢弃/静默零填）、失流监视器 StreamMonitor 与端点状态机 AoipEndpoint。
//
// 本段只做 host 入口接线：
// - 启动期（非 RT）解析/校验端点 -> AoipCapturePlan（spec + 角色映射 + 证据字段）；
// - 块循环按 period 读 AoIP capture 帧 -> S32_LE -> planar f32 -> **显式角色映射** -> graph/
//   plan-arena 输入（与 file-input 同一优先级：都在 synth/loopback 之后覆盖图输入）；
// - 失流按 T43：停滞（首包未到/短停顿）-> 整块静音、数据恢复后同 epoch 继续（Resumed）；
//   持续失流/PTP 失锁/端点错误 -> 必须新 epoch+reopen；host 无 T19/T26 epoch owner，
//   fail closed（稳定错误码 RT_E_DEVICE_LOST），不拿旧 epoch 数据拼接、不半块输出。
// - 真实端点 open/read 只在 #[cfg(target_os = "linux")] 的 AoipAlsaReader（非阻塞 S32_LE
//   capture，复用 T17 errno 映射）；Windows/macOS 明确 NOT_RUN。真实 daemon/PTP/网络端到端
//   与板端出声 NOT_RUN（evidence/cursor-fix-capturesrc/）。
// ---------------------------------------------------------------------------

/// capture 图输入 strict 布局名（与 file-input 同口径；不做隐式上/下混）。
const CAPTURE_LAYOUT_STEREO: &str = "2.0-strict";
const CAPTURE_LAYOUT_412: &str = "4.1.2-strict";
/// 16ch 图输入布局名（H16 Container16 8+8；NSP 2x8ch AES67 16ch Phase 1）。
const CAPTURE_LAYOUT_16: &str = "16ch-h16";
/// AoIP 容器通道数默认值（T43 H16 容器 16；12 = 7.1.4 逻辑 12 容器）。
const CAPTURE_AOIP_CONTAINER_DEFAULT: u16 = 16;

/// capture 图输入通道 -> H16 逻辑角色（strict 2.0：FL/FR）。
const CAPTURE_ROLES_2CH: [Option<H16Role>; 2] =
    [Some(H16Role::FrontLeft), Some(H16Role::FrontRight)];
/// capture 图输入通道 -> H16 逻辑角色（strict 4.1.2：FL FR SL SR TFL TFR 空 LFE）。
///
/// 与 file-input 4.1.2 严格布局一致（ch6 空）；AoIP 侧用 H16 角色名：SL=SurroundLeft、
/// SR=SurroundRight、TFL/TFR=TopFrontLeft/TopFrontRight、SUB=LFE。
const CAPTURE_ROLES_412: [Option<H16Role>; 8] = [
    Some(H16Role::FrontLeft),
    Some(H16Role::FrontRight),
    Some(H16Role::SurroundLeft),
    Some(H16Role::SurroundRight),
    Some(H16Role::TopFrontLeft),
    Some(H16Role::TopFrontRight),
    None,
    Some(H16Role::Lfe),
];

/// capture 图输入通道 -> H16 逻辑角色（strict 16ch = H16 Container16 8+8）。
///
/// 图通道 = 容器 capture 顺序 = H16 slot 0..15：slot 8/9（Lw/Rw）与 12/13（Aux12/Aux13）
/// 在 H16 逻辑上无角色（Phase 1 显式零填，plan_role_selection 显式 ignore 并计数）；
/// 12 个逻辑角色必须全部映射（禁止静默丢弃/静默零填）。
const CAPTURE_ROLES_16: [Option<H16Role>; 16] = [
    Some(H16Role::FrontLeft),
    Some(H16Role::FrontRight),
    Some(H16Role::Center),
    Some(H16Role::Lfe),
    Some(H16Role::SurroundLeft),
    Some(H16Role::SurroundRight),
    Some(H16Role::BackLeft),
    Some(H16Role::BackRight),
    None, // Lw
    None, // Rw
    Some(H16Role::TopFrontLeft),
    Some(H16Role::TopFrontRight),
    None, // Aux12
    None, // Aux13
    Some(H16Role::TopRearLeft),
    Some(H16Role::TopRearRight),
];

/// AES67 单 flow **7ch 4.1.2**（SMPTE2110.(41,2)：L,R,Ls,Rs,Ltf,Rtf,LFE）→ H16 容器 slot。
///
/// 图输入仍是 4.1.2 strict 8ch（ch6 空槽、LFE 落 ch7）；7ch flow 的通道顺序按
/// SMPTE2110.(41,2) 映射到 H16 逻辑 slot：FL=0 FR=1 SL=4 SR=5 TFL=10 TFR=11 LFE=3
/// （slot 号见 H16Role::container_slot；10/11 是 TFL/TFR 的容器位置）。LFE 是 flow 第 7
/// 通道，但落 graph ch7（不是 ch3），这是 4.1.2 strict 的空槽语义。
const AES67_SLOTS_412_7CH: [u8; 7] = [0, 1, 4, 5, 10, 11, 3];

/// userspace AES67 源 flow 布局名（report/证据；图输入布局名仍用 CAPTURE_LAYOUT_*）。
const CAPTURE_FLOW_LAYOUT_2CH: &str = "2.0-2ch";
const CAPTURE_FLOW_LAYOUT_412_7CH: &str = "4.1.2-7ch-sMPTE2110(41,2)";
const CAPTURE_FLOW_LAYOUT_412_8CH: &str = "4.1.2-8ch";
const CAPTURE_FLOW_LAYOUT_512_8CH: &str = "5.1.2-8ch-SMPTE2110(51,2)";
const CAPTURE_FLOW_LAYOUT_H16_DUAL: &str = "h16-dual-flow";
const CAPTURE_FLOW_LAYOUT_CUSTOM: &str = "custom-slots";

/// 图输入宽度 -> (strict 布局名, 角色表)；其它宽度一律拒收（不猜布局）。
fn capture_layout_roles(
    graph_input_channels: usize,
) -> Result<(&'static str, &'static [Option<H16Role>]), String> {
    match graph_input_channels {
        2 => Ok((CAPTURE_LAYOUT_STEREO, &CAPTURE_ROLES_2CH)),
        8 => Ok((CAPTURE_LAYOUT_412, &CAPTURE_ROLES_412)),
        // 16 = H16 Container16 8+8（NSP 2x8ch AES67 16ch Phase 1）：只有 12 个逻辑角色，
        // Lw/Rw/Aux12/Aux13 四个非逻辑容器通道显式 None 零填。
        16 => Ok((CAPTURE_LAYOUT_16, &CAPTURE_ROLES_16)),
        other => Err(capture_error(
            aoip::AoipError::Roles.name(),
            format!(
                "AoIP capture 只支持 strict 2ch(2.0)/8ch(4.1.2)/16ch(16ch-h16) 图输入（got {other}）；不做隐式上/下混"
            ),
        )),
    }
}

/// T43 AoIP 错误 -> host 稳定错误文本（码 = AoipError::name()；不新增码）。
fn aoip_error(error: aoip::AoipError) -> String {
    aoip_error_with("T43 AoIP 端点校验", error)
}

/// T43 AoIP 错误 + 上下文 -> host 稳定错误文本。
fn aoip_error_with(context: &str, error: aoip::AoipError) -> String {
    capture_error(
        error.name(),
        format!("{context}：status={}（T43 aoip.rs）", error.status().code()),
    )
}

/// [capture] ptp_lock -> T19 LockState（启动期断言；真实 servo 归 T19，NOT_WIRED）。
fn capture_ptp_lock_state(config: &HostRunConfig) -> Result<LockState, String> {
    match config.capture_ptp_lock.trim() {
        CAPTURE_PTP_LOCKED => Ok(LockState::Locked),
        CAPTURE_PTP_UNLOCKED => Ok(LockState::Unlocked),
        other => Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!("[capture] ptp_lock 必须是 locked|unlocked（got {other}）"),
        )),
    }
}

/// [capture] aoip_channels -> H16 容器通道数（0 = 默认 16；12 = 逻辑 12 容器）。
fn aoip_container_channels(config: &HostRunConfig) -> Result<u16, String> {
    match config.capture_aoip_channels {
        0 => Ok(CAPTURE_AOIP_CONTAINER_DEFAULT),
        CAPTURE_AOIP_CONTAINER_DEFAULT | 12 => Ok(config.capture_aoip_channels),
        other => Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!("[capture] aoip_channels 必须是 0|12|16（got {other}）"),
        )),
    }
}

/// 容器通道数 -> T43 显式 flow 配置（12 = 8+4 逻辑 12；16 = 8+8 容器 16）。
fn aoip_flow_config(container_channels: u16) -> aoip::H16FlowConfig {
    if container_channels == 12 {
        aoip::H16FlowConfig::logical12()
    } else {
        aoip::H16FlowConfig::container16()
    }
}

/// AoIP 端点规格回填 host period/buffer 并复校验（格式/速率/period/通道/唯一 PTP 来源）。
fn finish_aoip_spec(
    config: &HostRunConfig,
    spec: aoip::AoipEndpointSpec<'static>,
) -> Result<aoip::AoipEndpointSpec<'static>, String> {
    let spec = spec.with_period_buffer(config.block_size as u32, config.buffer_frames);
    spec.validate().map_err(|error| {
        aoip_error_with("AoIP 端点规格校验（格式/速率/period/通道/容器）", error)
    })?;
    Ok(spec)
}

/// ALSA 端点地址串（证据/report 用；hw:<card>,<dev> 由稳定 card id 解析，禁止写死卡号）。
fn alsa_endpoint_name(spec: &aoip::AoipEndpointSpec<'_>) -> Result<String, String> {
    match spec.alsa_device {
        Some(aoip::AlsaDevice::Named(name)) => Ok(name.to_string()),
        Some(aoip::AlsaDevice::Hw(device)) => {
            let mut buffer = [0u8; 16];
            let length = device.write_hw_name(&mut buffer).map_err(|error| {
                capture_error(
                    aoip::AoipError::from(error).name(),
                    "解析 AoIP ALSA hw:<card>,<dev> 名称失败",
                )
            })?;
            Ok(String::from_utf8_lossy(&buffer[..length]).to_string())
        }
        None => Err(capture_error(
            aoip::AoipError::DeviceParams.name(),
            "ALSA 端点规格缺设备地址（transport=alsa 必须 Some）",
        )),
    }
}

/// AoIP 启动计划（端点规格 + 角色映射 + 证据字段；不持有设备、不打开硬件）。
#[derive(Debug)]
struct AoipCapturePlan {
    source: CaptureSource,
    mode: CaptureMode,
    spec: aoip::AoipEndpointSpec<'static>,
    container: aoip::H16Container,
    role_map: AoipRoleMap,
    layout: &'static str,
    ptp_lock: LockState,
    device_name: String,
    detail: AoipPlanDetail,
}

/// 计划里的协议细节（report/证据用；与 RT 决策无关）。
#[derive(Debug)]
enum AoipPlanDetail {
    /// Inferno/Dante：已有 ALSA 插件 + statime PTPv1 虚拟钟。
    Inferno {
        pcm: String,
        device_id: String,
        clock_path: String,
        iface: String,
        rx_channels: u16,
    },
    /// AES67/RAVENNA：已有 aes67-daemon + RAVENNA LKM。
    Aes67 {
        card: String,
        ptp_domain: u8,
        flow_count: u8,
        encoding: &'static str,
        sdp: String,
        daemon_ready: bool,
    },
}

/// graph 输入通道 -> 容器 capture 通道的显式角色映射（T43 plan_role_selection 结果）。
#[derive(Debug)]
struct AoipRoleMap {
    layout: &'static str,
    roles: &'static [Option<H16Role>],
    capture_index: Vec<Option<u16>>,
    selected: u16,
    ignored: u16,
    non_logical_ignored: u32,
}

impl AoipRoleMap {
    /// 由容器 + 图输入宽度构造显式映射：未映射角色必须**显式 ignore**（T43 禁止静默丢弃）；
    /// 目标角色在容器里没订阅 -> ERR_ROLES（禁止静默零填）；16ch 非逻辑通道显式忽略并计数。
    fn build(container: &aoip::H16Container, graph_input_channels: usize) -> Result<Self, String> {
        let (layout, roles) = capture_layout_roles(graph_input_channels)?;
        let mut selected = RoleSelection::NONE;
        for role in roles.iter().flatten() {
            selected = selected.with(*role);
        }
        let ignored = RoleSelection::from_mask(container.logical_present_mask() & !selected.mask());
        let plan = aoip::plan_role_selection(
            container,
            selected,
            ignored,
            aoip::NonLogicalPolicy::ExplicitlyIgnore,
        )
        .map_err(|error| aoip_error_with("AoIP 角色映射（禁止静默丢弃/静默零填）", error))?;
        let mut capture_index = Vec::with_capacity(roles.len());
        for role in roles {
            match role {
                Some(role) => capture_index.push(Some(
                    plan.capture_index(*role)
                        .ok_or_else(|| aoip_error(aoip::AoipError::Roles))?,
                )),
                None => capture_index.push(None),
            }
        }
        Ok(Self {
            layout,
            roles,
            capture_index,
            selected: plan.selected,
            ignored: plan.ignored,
            non_logical_ignored: plan.non_logical_ignored,
        })
    }

    /// 角色映射证据 JSON（容器 capture index -> 角色；空槽单列）。
    fn report_json(&self) -> String {
        use std::fmt::Write as _;
        let mut map = String::new();
        for (index, role) in self.roles.iter().enumerate() {
            if index > 0 {
                map.push(',');
            }
            match role {
                Some(role) => {
                    let capture = self.capture_index[index].unwrap_or(0);
                    let _ = write!(map, "\"{}\":{}", role.as_str(), capture);
                }
                None => {
                    let _ = write!(map, "\"empty\":{index}");
                }
            }
        }
        format!(
            "{{\"layout\":\"{}\",\"selected_mask\":{},\"ignored_mask\":{},\"non_logical_ignored\":{},\"map\":{{{}}}}}",
            self.layout, self.selected, self.ignored, self.non_logical_ignored, map
        )
    }
}

/// [capture] 段（inferno/aes67）的纯解析前置校验（不打开设备；缺端点/非法值在此拒收）。
fn validate_aoip_capture_config(
    config: &HostRunConfig,
    source: CaptureSource,
) -> Result<(), String> {
    // 解析即校验（PTP 锁/容器/端点参数）；具体端点规格在 plan 阶段用 T43 复校验。
    capture_ptp_lock_state(config)?;
    aoip_container_channels(config)?;
    match source {
        CaptureSource::Inferno => {
            if config.capture_inferno_pcm.trim() != aoip::INFERNO_ALSA_DEVICE {
                return Err(capture_error(
                    aoip::AoipError::DeviceParams.name(),
                    format!(
                        "[capture] inferno_pcm 必须是 \"{}\"（T43 既有 inferno 插件 PCM 名；got \"{}\"）",
                        aoip::INFERNO_ALSA_DEVICE,
                        config.capture_inferno_pcm.trim()
                    ),
                ));
            }
            if config.capture_inferno_iface.trim().is_empty() {
                return Err(capture_error(
                    aoip::AoipError::DeviceParams.name(),
                    "capture.source=inferno 需要 [capture] inferno_iface = <绑定网卡>（InfernoEnv 缺绑定一律拒收）",
                ));
            }
            if !aoip::inferno_device_id_valid(config.capture_inferno_device_id.trim()) {
                return Err(capture_error(
                    aoip::AoipError::DeviceParams.name(),
                    format!(
                        "[capture] inferno_device_id 必须是 auto 或 16 个十六进制字符（got \"{}\"）",
                        config.capture_inferno_device_id.trim()
                    ),
                ));
            }
            if config.capture_inferno_clock_path.trim().is_empty() {
                return Err(capture_error(
                    aoip::AoipError::DeviceParams.name(),
                    "capture.source=inferno 需要 [capture] inferno_clock_path（statime usrvclock 导出路径）",
                ));
            }
        }
        CaptureSource::Aes67 => {
            if config.capture_aes67_card.trim().is_empty() {
                return Err(capture_error(
                    aoip::AoipError::DeviceParams.name(),
                    "capture.source=aes67 需要 [capture] aes67_card = <稳定 card id>（默认 RAVENNA；禁止猜 hw 号）",
                ));
            }
            let sdp = config.capture_aes67_sdp.trim();
            if !sdp.is_empty() && !std::path::Path::new(sdp).is_file() {
                return Err(capture_error(
                    ERR_HOST_CAPTURE_CONFIG,
                    format!("AES67 SDP 文件不存在或不是普通文件：{sdp}"),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

/// 启动计划：source=inferno/aes67 时解析 T43 端点；其它 source 返回 None（motu 走既有 sink）。
///
/// cards_text 为 `/proc/asound/cards` 文本（AES67 稳定 card id 解析用；测试传入 fixture）。
fn aoip_capture_plan(
    config: &HostRunConfig,
    graph_input_channels: usize,
    cards_text: &str,
) -> Result<Option<AoipCapturePlan>, String> {
    match CaptureSource::parse(&config.capture_source)? {
        CaptureSource::Inferno => {
            build_inferno_capture_plan(config, graph_input_channels).map(Some)
        }
        CaptureSource::Aes67 => {
            build_aes67_capture_plan(config, graph_input_channels, cards_text).map(Some)
        }
        _ => Ok(None),
    }
}

/// Inferno/Dante 端点计划（T43 inferno_alsa_endpoint + preflight；不打开插件）。
fn build_inferno_capture_plan(
    config: &HostRunConfig,
    graph_input_channels: usize,
) -> Result<AoipCapturePlan, String> {
    let container_channels = aoip_container_channels(config)?;
    let flow_config = aoip_flow_config(container_channels);
    flow_config
        .validate()
        .map_err(|error| aoip_error_with("Inferno flow 配置", error))?;
    let container = aoip::H16Container::from_flow_config(&flow_config)
        .map_err(|error| aoip_error_with("Inferno 容器映射", error))?;
    let env = aoip::InfernoEnv::new(
        config.capture_inferno_device_id.trim(),
        config.capture_inferno_clock_path.trim(),
        config.capture_inferno_iface.trim(),
        aoip::INFERNO_RATE_HZ,
        container.capture_channels(),
    );
    let spec = aoip::inferno_alsa_endpoint(&env, container).map_err(|error| {
        aoip_error_with(
            "Inferno 端点解析（pcm.inferno / 16ch 容器 / statime PTPv1 虚拟钟）",
            error,
        )
    })?;
    let spec = finish_aoip_spec(config, spec)?;
    let ptp_lock = capture_ptp_lock_state(config)?;
    aoip::preflight(&spec, ptp_lock).map_err(|error| {
        aoip_error_with(
            "AoIP 启动前预检（PTP 锁/唯一来源；PTP 未锁不得开始采集）",
            error,
        )
    })?;
    let role_map = AoipRoleMap::build(&container, graph_input_channels)?;
    let layout = role_map.layout;
    Ok(AoipCapturePlan {
        source: CaptureSource::Inferno,
        mode: CaptureMode::Dante,
        spec,
        container,
        role_map,
        layout,
        ptp_lock,
        device_name: aoip::INFERNO_ALSA_DEVICE.to_string(),
        detail: AoipPlanDetail::Inferno {
            pcm: config.capture_inferno_pcm.trim().to_string(),
            device_id: config.capture_inferno_device_id.trim().to_string(),
            clock_path: config.capture_inferno_clock_path.trim().to_string(),
            iface: config.capture_inferno_iface.trim().to_string(),
            rx_channels: container.capture_channels(),
        },
    })
}

/// AES67/RAVENNA 端点计划（SDP 只读解析/交叉校验 + daemon 就绪 + RAVENNA 稳定 card 解析）。
fn build_aes67_capture_plan(
    config: &HostRunConfig,
    graph_input_channels: usize,
    cards_text: &str,
) -> Result<AoipCapturePlan, String> {
    let container_channels = aoip_container_channels(config)?;
    let flow_config = aoip_flow_config(container_channels);
    flow_config
        .validate()
        .map_err(|error| aoip_error_with("AES67 flow 配置", error))?;
    let ptp_domain = config.capture_aes67_ptp_domain;
    let service = aoip::Aes67ServiceState::new(
        config.capture_aes67_daemon_ready,
        aoip::AES67_RATE_HZ,
        ptp_domain,
    );
    let sdp_path = config.capture_aes67_sdp.trim();
    let plan = if sdp_path.is_empty() {
        // 无 SDP：用显式 flow 配置拼容器（SDP 只给每 flow 通道数，构成必须来自配置）。
        let container = aoip::H16Container::from_flow_config(&flow_config)
            .map_err(|error| aoip_error_with("AES67 容器映射", error))?;
        aoip::Aes67CapturePlan {
            container,
            flow_count: flow_config.flow_count() as u8,
            total_channels: container.capture_channels(),
            encoding: aoip::Aes67Encoding::L24,
            sample_rate_hz: aoip::AES67_RATE_HZ,
            ptp_domain,
            media: [aoip::Aes67FlowMedia::EMPTY; aoip::H16_MAX_FLOWS],
            media_len: 0,
            ptime_us_max: 0,
        }
    } else {
        let text = std::fs::read_to_string(sdp_path).map_err(|error| {
            capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                format!("读取 AES67 SDP 失败 {sdp_path}: {error}"),
            )
        })?;
        let flows = parse_aes67_sdp_sessions(&text)?;
        let set = aoip::Aes67FlowSet::new(&flows).map_err(|error| {
            aoip_error_with("AES67 SDP flow 校验（L24/48k/≤8ch/组播/ts-refclk）", error)
        })?;
        aoip::validate_aes67_capture(&set, &flow_config, ptp_domain)
            .map_err(|error| aoip_error_with("AES67 SDP ↔ 显式 flow 配置交叉校验", error))?
    };
    aoip::validate_aes67_service(&service, &plan).map_err(|error| {
        aoip_error_with(
            "AES67 服务就绪校验（daemon_ready / 采样率 / PTP domain）",
            error,
        )
    })?;
    let card = config.capture_aes67_card.trim();
    let spec = plan
        .alsa_endpoint(cards_text, card)
        .map_err(|error| aoip_error_with("AES67 RAVENNA 稳定 card id / ALSA 端点解析", error))?;
    let spec = finish_aoip_spec(config, spec)?;
    let ptp_lock = capture_ptp_lock_state(config)?;
    aoip::preflight(&spec, ptp_lock).map_err(|error| {
        aoip_error_with(
            "AoIP 启动前预检（PTP 锁/唯一来源；PTP 未锁不得开始采集）",
            error,
        )
    })?;
    let device_name = alsa_endpoint_name(&spec)?;
    let role_map = AoipRoleMap::build(&plan.container, graph_input_channels)?;
    let layout = role_map.layout;
    Ok(AoipCapturePlan {
        source: CaptureSource::Aes67,
        mode: CaptureMode::Aes67,
        spec,
        container: plan.container,
        role_map,
        layout,
        ptp_lock,
        device_name,
        detail: AoipPlanDetail::Aes67 {
            card: card.to_string(),
            ptp_domain,
            flow_count: plan.flow_count,
            encoding: plan.encoding.as_str(),
            sdp: sdp_path.to_string(),
            daemon_ready: config.capture_aes67_daemon_ready,
        },
    })
}

/// 拆分 AES67 SDP 文件里的多条会话文本（每条以 `v=0` 开始；H16 每 flow 一个 SDP）。
///
/// T239：ravenna（T43 aoip 解析）与 userspace（khaen-aoip-aes67 解析）共用同一拆分语义，
/// 保证两条传输对同一 SDP 文件的 flow 数/边界判断一致。
fn split_aes67_sdp_session_texts(text: &str) -> Result<Vec<&str>, String> {
    let mut sessions: Vec<&str> = Vec::new();
    let mut begin = 0usize;
    let mut offset = 0usize;
    let mut started = false;
    for line in text.split_inclusive('\n') {
        if line.trim() == "v=0" {
            if started {
                sessions.push(&text[begin..offset]);
            }
            begin = offset;
            started = true;
        }
        offset += line.len();
    }
    if started {
        sessions.push(&text[begin..]);
    }
    if sessions.is_empty() {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            "AES67 SDP 里没有 v=0 会话（需要 RFC 4566 子集；不猜格式）",
        ));
    }
    Ok(sessions)
}

/// 拆分 AES67 SDP 文件里的多条会话（每条以 `v=0` 开始；H16 每 flow 一个 SDP）。
fn parse_aes67_sdp_sessions(text: &str) -> Result<Vec<aoip::Aes67FlowSdp<'_>>, String> {
    let sessions = split_aes67_sdp_session_texts(text)?;
    let mut flows = Vec::with_capacity(sessions.len());
    for session in sessions {
        if !session.contains("m=") {
            continue;
        }
        flows.push(
            aoip::parse_aes67_sdp(session)
                .map_err(|error| aoip_error_with("AES67 SDP 解析（单 flow 会话）", error))?,
        );
    }
    if flows.is_empty() {
        return Err(capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            "AES67 SDP 里没有 m=audio 会话（multi m=/video/IPv6 一律拒收）",
        ));
    }
    Ok(flows)
}

/// AoIP 端点一次 period 读取结果。
///
/// 生产构造点只有 Linux `AoipAlsaReader` 与单测脚本读取器；非 Linux 生产 build 无构造点。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
enum AoipPeriodRead {
    /// 读到 frames 帧（>0），已按容器通道写进 planar（stride = block）。
    Data {
        /// 本 period 实际帧数（可为 short period；1..=block）。
        frames: u32,
    },
    /// 本 period 未取到数据（非阻塞 EAGAIN/xrun 恢复/流等待）：按 T43 停滞处理，不是错误。
    Stalled,
}

/// AoIP 端点读取错误（T43 冻结码 + 细节文本）。
struct AoipReadError {
    error: aoip::AoipError,
    detail: String,
}

/// AoIP capture 端点读取器（真实 = Linux ALSA capture；fake/单测 = 脚本化 period）。
trait AoipPeriodSource {
    /// 读一个 period 到 planar f32（容器通道 × stride；dense）。
    fn read_period(
        &mut self,
        dst: &mut [f32],
        stride: usize,
        block: usize,
    ) -> Result<AoipPeriodRead, AoipReadError>;
    /// 读取器种类（证据/report 用；不参与 RT 决策）。
    fn kind(&self) -> &'static str;
    /// 累计 xrun/停滞恢复次数（证据用；默认 0）。
    fn xruns(&self) -> u32 {
        0
    }
    /// 停止/关闭端点（幂等；退出路径调用）。
    fn close(&mut self) {}
}

/// AoIP capture 运行对象：端点状态机 + per-period planar 缓冲 + 统计。
struct AoipCapture {
    plan: AoipCapturePlan,
    reader: Box<dyn AoipPeriodSource>,
    endpoint: aoip::AoipEndpoint<'static>,
    /// 容器通道 × block 的 planar（启动期一次分配；块循环只复用）。
    planar: Vec<f32>,
    blocks: u64,
    data_blocks: u64,
    silent_blocks: u64,
    muted_blocks: u64,
    short_periods: u32,
    stall_events: u32,
    loss_events: u32,
    resume_events: u32,
}

impl AoipCapture {
    /// 启动：T43 端点 new -> start（epoch=1；PTP 已锁由 preflight 保证）+ 预分配 planar。
    fn start(
        plan: AoipCapturePlan,
        reader: Box<dyn AoipPeriodSource>,
        block: usize,
        now_ns: u64,
    ) -> Result<Self, String> {
        if plan.spec.period_frames as usize != block {
            return Err(capture_error(
                aoip::AoipError::BlockSize.name(),
                format!(
                    "AoIP 端点 period {} != host block {}（backend=alsa 要求 block_size == period_frames）",
                    plan.spec.period_frames, block
                ),
            ));
        }
        let mut endpoint = aoip::AoipEndpoint::new(plan.spec).map_err(|error| aoip_error(error))?;
        if let Err(error) = endpoint.start(1, plan.ptp_lock, now_ns) {
            return Err(capture_error(
                error.name(),
                format!(
                    "AoIP 端点启动被拒（PTP 锁断言={}）：PTP 未锁不得开始采集；真实 T19 servo 归 T26 NOT_WIRED",
                    plan.ptp_lock.as_str()
                ),
            ));
        }
        let channels = plan.container.capture_channels() as usize;
        let planar = vec![0.0f32; channels.checked_mul(block).unwrap_or(0)];
        Ok(Self {
            plan,
            reader,
            endpoint,
            planar,
            blocks: 0,
            data_blocks: 0,
            silent_blocks: 0,
            muted_blocks: 0,
            short_periods: 0,
            stall_events: 0,
            loss_events: 0,
            resume_events: 0,
        })
    }

    fn lost_error(&self) -> String {
        capture_error(
            aoip::AoipError::StreamLost.name(),
            format!(
                "AoIP 持续失流（blocks={} silent={} endpoint={}）：T43 要求新 epoch + reopen；host epoch owner（T19/T26）NOT_WIRED -> fail closed；真实网络/PTP NOT_RUN",
                self.blocks,
                self.silent_blocks,
                self.endpoint.state().as_str()
            ),
        )
    }

    /// 推进 T43 失流监视器：停滞 -> 本块起整块静音；持续失流 -> fail closed（需新 epoch）。
    fn poll(&mut self, now_ns: u64) -> Result<(), String> {
        match self.endpoint.poll(now_ns) {
            aoip::StreamEvent::Stalled { .. } => {
                self.stall_events = self.stall_events.saturating_add(1);
                Ok(())
            }
            aoip::StreamEvent::Lost { .. } => {
                self.loss_events = self.loss_events.saturating_add(1);
                Err(self.lost_error())
            }
            _ => Ok(()),
        }
    }

    fn note_event(&mut self, event: aoip::StreamEvent) {
        match event {
            aoip::StreamEvent::Resumed { .. } => {
                self.resume_events = self.resume_events.saturating_add(1);
            }
            aoip::StreamEvent::Stalled { .. } => {
                self.stall_events = self.stall_events.saturating_add(1);
            }
            aoip::StreamEvent::Lost { .. } => {
                self.loss_events = self.loss_events.saturating_add(1);
            }
            _ => {}
        }
    }

    /// 一个 block：读端点 period -> on_data 语义 -> 显式角色映射写入图输入。
    ///
    /// 停滞/无数据：整块静音（不清旧数据、不半块）；端点错误/持续失流：fail closed。
    fn fill_block(
        &mut self,
        input: &mut [f32],
        stride: usize,
        block: usize,
        now_ns: u64,
    ) -> Result<(), String> {
        self.poll(now_ns)?;
        if self.endpoint.state() == aoip::AoipState::Lost {
            return Err(self.lost_error());
        }
        match self.reader.read_period(&mut self.planar, block, block) {
            Ok(AoipPeriodRead::Data { frames }) => {
                let frames = frames as usize;
                if frames == 0 || frames > block {
                    return Err(capture_error(
                        aoip::AoipError::DeviceParams.name(),
                        format!("AoIP period 帧数 {frames} 非法（1..={block}）"),
                    ));
                }
                if frames < block {
                    self.short_periods = self.short_periods.saturating_add(1);
                }
                let event = self
                    .endpoint
                    .on_data(now_ns, frames as u32)
                    .map_err(|error| {
                        capture_error(
                            error.name(),
                            format!(
                            "AoIP 端点状态机拒绝数据（旧 epoch/Lost 未 reopen；epoch={} state={}）",
                            self.endpoint.epoch_id(),
                            self.endpoint.state().as_str()
                        ),
                        )
                    })?;
                self.note_event(event);
                self.fill_graph_input(input, stride, block, frames);
                self.blocks = self.blocks.saturating_add(1);
                self.data_blocks = self.data_blocks.saturating_add(1);
                Ok(())
            }
            Ok(AoipPeriodRead::Stalled) => {
                input.fill(0.0);
                self.blocks = self.blocks.saturating_add(1);
                self.silent_blocks = self.silent_blocks.saturating_add(1);
                Ok(())
            }
            Err(error) => {
                let event = self.endpoint.on_source_eof(now_ns);
                if matches!(event, aoip::StreamEvent::Lost { .. }) {
                    self.loss_events = self.loss_events.saturating_add(1);
                }
                Err(capture_error(
                    error.error.name(),
                    format!(
                        "{}（AoIP 端点失流：整块静音 + 需新 epoch/reopen；host epoch owner NOT_WIRED -> fail closed）",
                        error.detail
                    ),
                ))
            }
        }
    }

    /// 显式角色映射：图输入通道 i <- 容器通道 capture_index[i]；空槽恒静音。
    fn fill_graph_input(&self, input: &mut [f32], stride: usize, block: usize, frames: usize) {
        let map = &self.plan.role_map;
        let mut channel = 0usize;
        while channel < map.capture_index.len() {
            let start = channel * stride;
            if start + block > input.len() {
                break;
            }
            match map.capture_index[channel] {
                Some(capture) => {
                    let source_start = capture as usize * stride;
                    let n = frames.min(block);
                    input[start..start + n]
                        .copy_from_slice(&self.planar[source_start..source_start + n]);
                    if n < block {
                        input[start + n..start + block].fill(0.0);
                    }
                }
                None => input[start..start + block].fill(0.0),
            }
            channel += 1;
        }
    }

    /// run 报告 capture 段（端点/模式/PTP/角色/失流计数 + 明确 NOT_RUN 注记）。
    fn report_json(&self, blocks: u64, exit_reason: &str) -> String {
        use std::fmt::Write as _;
        let counters = self.endpoint.counters();
        let detail = match &self.plan.detail {
            AoipPlanDetail::Inferno {
                pcm,
                device_id,
                clock_path,
                iface,
                rx_channels,
            } => format!(
                ",\"inferno\":{{\"pcm\":\"{}\",\"device_id\":\"{}\",\"clock_path\":\"{}\",\"iface\":\"{}\",\"rx_channels\":{}}}",
                json_escape_text(pcm),
                json_escape_text(device_id),
                json_escape_text(clock_path),
                json_escape_text(iface),
                rx_channels
            ),
            AoipPlanDetail::Aes67 {
                card,
                ptp_domain,
                flow_count,
                encoding,
                sdp,
                daemon_ready,
            } => format!(
                ",\"aes67\":{{\"card\":\"{}\",\"ptp_domain\":{},\"flow_count\":{},\"encoding\":\"{}\",\"sdp\":\"{}\",\"daemon_ready\":{}}}",
                json_escape_text(card),
                ptp_domain,
                flow_count,
                encoding,
                json_escape_text(sdp),
                daemon_ready
            ),
        };
        let mut report = String::with_capacity(768);
        let _ = write!(
            report,
            "{{\"source\":\"{}\",\"mode\":\"{}\",\"transport\":\"alsa\",\"reader\":\"{}\",\"device\":\"{}\",\"sample_format\":\"S32_LE\",\"container_channels\":{},\"graph_input_channels\":{},\"layout\":\"{}\",\"sample_rate\":{},\"period_frames\":{},\"buffer_frames\":{},\"ptp_source\":\"{}\",\"ptp_lock\":\"{}\",\"epoch_id\":{},\"state\":\"{}\",\"roles\":{},\"blocks\":{},\"data_blocks\":{},\"silent_blocks\":{},\"muted_blocks\":{},\"short_periods\":{},\"frames\":{},\"data_events\":{},\"stall_events\":{},\"loss_events\":{},\"resume_events\":{},\"xruns\":{},\"exit_reason\":\"{}\",\"not_run\":\"真实 AoIP daemon/PTP/网络与板端出声 NOT_RUN\"{}}}",
            self.plan.source.as_str(),
            self.plan.mode.as_str(),
            self.reader.kind(),
            json_escape_text(&self.plan.device_name),
            self.plan.container.capture_channels(),
            self.plan.role_map.capture_index.len(),
            self.plan.layout,
            self.plan.spec.sample_rate_hz,
            self.plan.spec.period_frames,
            self.plan.spec.buffer_frames,
            self.plan.spec.ptp_source.as_str(),
            self.plan.ptp_lock.as_str(),
            self.endpoint.epoch_id(),
            self.endpoint.state().as_str(),
            self.plan.role_map.report_json(),
            blocks,
            self.data_blocks,
            self.silent_blocks,
            self.muted_blocks,
            self.short_periods,
            counters.frames,
            counters.data_events,
            self.stall_events,
            self.loss_events,
            self.resume_events,
            self.reader.xruns(),
            exit_reason,
            detail,
        );
        report
    }

    /// 停止端点（幂等；退出路径调用）。
    fn stop(&mut self) {
        self.endpoint.stop();
        self.reader.close();
    }
}

/// 启动**计划**阶段（纯校验，不打开设备）：端点/SDP/角色/PTP 预检 fail fast。
///
/// 在图构建后、backend 设备打开前调用；设备打开在 web/worker/缓冲初始化之后（F5）。
fn plan_aoip_capture(
    config: &HostRunConfig,
    graph_input_channels: usize,
) -> Result<Option<AoipCapturePlan>, String> {
    let source = CaptureSource::parse(&config.capture_source)?;
    if !source.is_aoip() {
        return Ok(None);
    }
    // T239：source=aes67 的 userspace 传输由自研接收端计划接管（不读 /proc/asound、
    // 不解析 RAVENNA 稳定 card id；两条传输互斥）。
    if source == CaptureSource::Aes67
        && Aes67Transport::parse(&config.capture_aes67_transport)? == Aes67Transport::Userspace
    {
        return Ok(None);
    }
    let cards_text = read_asound_cards_text(source)?;
    aoip_capture_plan(config, graph_input_channels, &cards_text)
}

/// 启动**设备**阶段（web/worker/缓冲初始化之后）：打开 Linux ALSA capture + T43 端点 start。
fn start_aoip_capture(
    plan: Option<AoipCapturePlan>,
    config: &HostRunConfig,
) -> Result<Option<AoipCapture>, String> {
    let plan = match plan {
        Some(plan) => plan,
        None => return Ok(None),
    };
    let reader = open_aoip_period_source(&plan)?;
    let block = config.block_size as usize;
    AoipCapture::start(plan, reader, block, clock::mono_now_ns()).map(Some)
}

/// 读 `/proc/asound/cards`（仅 AES67 稳定 card id 解析需要；其它平台明确 NOT_RUN）。
fn read_asound_cards_text(source: CaptureSource) -> Result<String, String> {
    if source != CaptureSource::Aes67 {
        return Ok(String::new());
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/asound/cards").map_err(|error| {
            capture_error(
                ERR_HOST_CAPTURE_CONFIG,
                format!("读取 /proc/asound/cards 失败：{error}（AES67 稳定 card id 解析需要）"),
            )
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(capture_error(
            aoip::AoipError::Unsupported.name(),
            format!(
                "capture.source=aes67 需要 Linux target 读 /proc/asound/cards 解析 RAVENNA 端点（本 target={}）；真实端点 NOT_RUN",
                std::env::consts::OS
            ),
        ))
    }
}

/// 打开 AoIP capture 读取器：Linux = 真实 ALSA capture（非阻塞 S32_LE）；其它平台明确 NOT_RUN。
#[cfg(target_os = "linux")]
fn open_aoip_period_source(plan: &AoipCapturePlan) -> Result<Box<dyn AoipPeriodSource>, String> {
    Ok(Box::new(AoipAlsaReader::open(plan)?))
}

/// 非 Linux target：不编译真实 ALSA 依赖，AoIP capture 端点明确 NOT_RUN。
#[cfg(not(target_os = "linux"))]
fn open_aoip_period_source(plan: &AoipCapturePlan) -> Result<Box<dyn AoipPeriodSource>, String> {
    Err(capture_error(
        aoip::AoipError::Unsupported.name(),
        format!(
            "capture.source={} 需要 Linux target 打开真实 AoIP ALSA capture（本 target={}）；真实端点/daemon/PTP NOT_RUN",
            plan.source.as_str(),
            std::env::consts::OS
        ),
    ))
}

/// ALSA errno -> host 稳定错误文本（Linux only；复用 T17 errno 映射）。
#[cfg(target_os = "linux")]
fn aoip_alsa_error(context: &str, error: alsa::Error) -> String {
    let mapped = khaen_rt_host::alsa::alsa_errno_to_error(error.errno());
    capture_error(
        aoip::AoipError::from(mapped).name(),
        format!("{context}（ALSA errno {}）", error.errno()),
    )
}

/// Linux 真实 AoIP ALSA capture 读取器（inferno 命名 PCM / RAVENNA hw:<card>,0）。
///
/// 非阻塞（`PCM::new(..., true)`）：一个 period 未就绪 -> EAGAIN -> 由调用方按 T43 停滞
/// 整块静音；xrun -> prepare 后同样按停滞处理；其它错误 -> 失流（fail closed）。
/// **真实设备打开/读取未在板端运行（NOT_RUN）**。
#[cfg(target_os = "linux")]
struct AoipAlsaReader {
    pcm: alsa::pcm::PCM,
    channels: usize,
    period: usize,
    period_bytes: Vec<u8>,
    device: String,
    xruns: u32,
}

#[cfg(target_os = "linux")]
impl AoipAlsaReader {
    fn open(plan: &AoipCapturePlan) -> Result<Self, String> {
        use alsa::pcm::{Access, Format, HwParams};
        use alsa::ValueOr;
        let spec = &plan.spec;
        let channels = plan.container.capture_channels() as usize;
        let period = spec.period_frames as usize;
        let device = plan.device_name.clone();
        let pcm =
            alsa::pcm::PCM::new(&device, alsa::Direction::Capture, true).map_err(|error| {
                aoip_alsa_error(&format!("打开 AoIP capture 端点 {device} 失败"), error)
            })?;
        {
            let params = HwParams::any(&pcm)
                .map_err(|error| aoip_alsa_error("查询 AoIP capture hwparams 失败", error))?;
            params
                .set_access(Access::RWInterleaved)
                .map_err(|error| aoip_alsa_error("设置 AoIP capture access 失败", error))?;
            params
                .set_format(Format::S32LE)
                .map_err(|error| aoip_alsa_error("设置 AoIP capture S32_LE 失败", error))?;
            params
                .set_channels(channels as u32)
                .map_err(|error| aoip_alsa_error("设置 AoIP capture 通道数失败", error))?;
            params
                .set_rate(spec.sample_rate_hz, ValueOr::Nearest)
                .map_err(|error| aoip_alsa_error("设置 AoIP capture 采样率失败", error))?;
            let _ = params
                .set_period_size_near(period as alsa::pcm::Frames, ValueOr::Nearest)
                .map_err(|error| aoip_alsa_error("设置 AoIP capture period 失败", error))?;
            let _ = params
                .set_buffer_size_near(spec.buffer_frames as alsa::pcm::Frames)
                .map_err(|error| aoip_alsa_error("设置 AoIP capture buffer 失败", error))?;
            pcm.hw_params(&params)
                .map_err(|error| aoip_alsa_error("应用 AoIP capture hwparams 失败", error))?;
            let current = pcm
                .hw_params_current()
                .map_err(|error| aoip_alsa_error("回读 AoIP capture hwparams 失败", error))?;
            if current
                .get_format()
                .map_err(|error| aoip_alsa_error("回读 format 失败", error))?
                != Format::S32LE
            {
                return Err(capture_error(
                    aoip::AoipError::Unsupported.name(),
                    "AoIP capture 实际格式不是 S32_LE（T43 两条既有链路都以 S32_LE 交付）"
                        .to_string(),
                ));
            }
            if current
                .get_channels()
                .map_err(|error| aoip_alsa_error("回读通道数失败", error))?
                != channels as u32
            {
                return Err(capture_error(
                    aoip::AoipError::Channels.name(),
                    format!("AoIP capture 实际通道数 != 容器 {channels}（T43 容器映射契约）"),
                ));
            }
            if current
                .get_rate()
                .map_err(|error| aoip_alsa_error("回读采样率失败", error))?
                != spec.sample_rate_hz
            {
                return Err(capture_error(
                    aoip::AoipError::Rate.name(),
                    format!(
                        "AoIP capture 实际采样率 != {}（协议 profile：AES67 48k / Inferno 96k）",
                        spec.sample_rate_hz
                    ),
                ));
            }
            if current
                .get_period_size()
                .map_err(|error| aoip_alsa_error("回读 period 失败", error))?
                != period as alsa::pcm::Frames
            {
                return Err(capture_error(
                    aoip::AoipError::BlockSize.name(),
                    format!("AoIP capture 实际 period != host block {period}（需要重规划/新 epoch，NOT_WIRED）"),
                ));
            }
        }
        let _ = pcm.prepare();
        let _ = pcm.start();
        Ok(Self {
            pcm,
            channels,
            period,
            period_bytes: vec![0u8; period * channels * 4],
            device,
            xruns: 0,
        })
    }
}

#[cfg(target_os = "linux")]
impl AoipPeriodSource for AoipAlsaReader {
    fn read_period(
        &mut self,
        dst: &mut [f32],
        stride: usize,
        block: usize,
    ) -> Result<AoipPeriodRead, AoipReadError> {
        if block != self.period || stride < self.period || dst.len() < self.channels * stride {
            return Err(AoipReadError {
                error: aoip::AoipError::BadLayout,
                detail: format!(
                    "AoIP planar 缓冲/period 不匹配（block {block} period {} stride {stride} channels {} dst {} 设备 {}）",
                    self.period,
                    self.channels,
                    dst.len(),
                    self.device
                ),
            });
        }
        match self.pcm.io_bytes().readi(&mut self.period_bytes) {
            Ok(frames) => {
                let frames = usize::try_from(frames).map_err(|_| AoipReadError {
                    error: aoip::AoipError::BadLayout,
                    detail: format!("ALSA capture 帧数为负/溢出（{}）", self.device),
                })?;
                let frames = frames.min(self.period);
                if frames == 0 {
                    return Ok(AoipPeriodRead::Stalled);
                }
                let mut frame = 0usize;
                while frame < frames {
                    let mut channel = 0usize;
                    while channel < self.channels {
                        let offset = (frame * self.channels + channel) * 4;
                        let sample = i32::from_le_bytes([
                            self.period_bytes[offset],
                            self.period_bytes[offset + 1],
                            self.period_bytes[offset + 2],
                            self.period_bytes[offset + 3],
                        ]);
                        // 与 T22 format.rs S32 decode 同式：(i32 as f32) / 2^31。
                        dst[channel * stride + frame] = sample as f32 / 2_147_483_648.0;
                        channel += 1;
                    }
                    frame += 1;
                }
                Ok(AoipPeriodRead::Data {
                    frames: frames as u32,
                })
            }
            Err(error) => {
                // 非阻塞 capture：EAGAIN(11) = 本 period 数据未到（T43 停滞，不是错误）。
                if error.errno() == 11 {
                    return Ok(AoipPeriodRead::Stalled);
                }
                let mapped = khaen_rt_host::alsa::alsa_errno_to_error(error.errno());
                if mapped == khaen_rt_host::alsa::AlsaError::Xrun {
                    let _ = self.pcm.prepare();
                    self.xruns = self.xruns.saturating_add(1);
                    return Ok(AoipPeriodRead::Stalled);
                }
                Err(AoipReadError {
                    error: aoip::AoipError::from(mapped),
                    detail: format!(
                        "AoIP ALSA capture 读取失败（{}；errno {}）",
                        self.device,
                        error.errno()
                    ),
                })
            }
        }
    }

    fn kind(&self) -> &'static str {
        "linux-alsa"
    }

    fn xruns(&self) -> u32 {
        self.xruns
    }

    fn close(&mut self) {
        let _ = self.pcm.drop();
    }
}

/// [run-loop] AoIP capture 阶段：读端点 period -> 图输入；失流/状态机错误 fail closed。
///
/// SafetyMute/gate 拒绝的块按 core-v1 §8.2「不处理新块」：不消费端点数据、输入清零，
/// 只推进 T43 监视器时间轴（停滞/失流计数仍真实）。
fn aoip_input_phase(
    capture: Option<&mut AoipCapture>,
    input: &mut [f32],
    stride: usize,
    block: usize,
    mute: bool,
    now_ns: u64,
) -> Result<(), String> {
    let capture = match capture {
        Some(capture) => capture,
        None => return Ok(()),
    };
    if mute {
        capture.poll(now_ns)?;
        input.fill(0.0);
        capture.muted_blocks = capture.muted_blocks.saturating_add(1);
        return Ok(());
    }
    capture.fill_block(input, stride, block, now_ns)
}

// ---------------------------------------------------------------------------
// T239：capture source=aes67 的用户态传输（自研 RTP 接收端 → host 图输入）
//
// 与 T74/T43 的 aes67（aes67-daemon + RAVENNA ALSA 内核卡）同 source/mode，由
// [capture] aes67_transport=ravenna|userspace 二选一（互斥）：
// - ravenna   = 既有 ALSA 端点路径（不复用本段）；
// - userspace = 本段：SDP 文件 → khaen-aoip-aes67（UDP 组播/单播、RTP L24/48k、抖动缓冲）
//               → 每 flow 128 帧整块 → H16 容器/角色显式映射 → planar f32 图输入。
//
// 语义与 motu/inferno 对齐（T43）：
// - 容器/角色映射直接复用 T235 适配层（Aes67RxCapture；与 T43 H16 同口径，禁止静默丢弃/零填）；
// - **媒体时基合成**（T239-crackle）：fill_block_paced = origin+latency+delivered，墙钟只做
//   监视器/hold 计时；旧"墙钟 target"会把 ALSA duplex/USB 节拍量化变成 NotDue 静音插块 +
//   resync 丢块（7–10% 爆音），已由证据 ae67-crackle-20261122 复现；
// - 停滞（Warmup/Underrun/NotDue）→ 本块整块静音 + 计数（不半块、不残留旧样本/不重复块）；
// - 持续失流（T43 StreamMonitor：>2×stall 门限）→ strict 模式 fail closed（RT_E_DEVICE_LOST，
//   需新 epoch/reopen）；stay/hold 模式（默认对 NSP 链路开启）改为保持静音 + 内部 reanchor，
//   静音超过 aes67_stall_fail_close_ms 才 fail closed + 稳定错误码；
// - 退出/超时：max_blocks/duration/stop_signal 与既有 run 一致；退出前 stop() 收包线程；
// - backend=fake（无播放设备节拍）时 userspace capture 自带块节拍（48k/128=2.667 ms），
//   否则无设备时钟的块循环会跑飞（jitter reader 全部 NotDue/Underrun）。
//
// NOT_RUN：真实板端网卡/IGMP/交换机/组播、PTP/PHC 时钟对齐（UsrvClock 非 PTP，SDP 的
// ts-refclk/ptp domain 只记录不接线）、真实 H16 源、板端电平/听音。
// ---------------------------------------------------------------------------

use khaen_aoip_aes67::clock::{MediaClock as _, UsrvClock};
use khaen_aoip_aes67::jitter::BlockStatus;
use khaen_aoip_aes67::receiver::{ReceiverConfig, ReceiverError};
use khaen_aoip_aes67::sdp::{SdpDocument, SdpError};
use khaen_rt_host::aes67_rx::{
    Aes67RxAdapter, Aes67RxCapture, Aes67RxFlowPlan, AES67_RX_BLOCK_FRAMES,
    AES67_RX_LAYOUT_512_CONTENT,
};

/// 单条 userspace flow 的静态描述（SDP 生效值；bind 前即可给出证据）。
#[derive(Clone, Debug, PartialEq, Eq)]
struct Aes67UserFlowInfo {
    index: u8,
    media: String,
    port: u16,
    multicast: bool,
    channels: u16,
}

/// userspace AES67 启动计划（SDP 解析 + flow 组合 + 角色映射；不打开 socket）。
#[derive(Debug)]
struct Aes67UserCapturePlan {
    adapter: Aes67RxAdapter,
    receiver_configs: Vec<ReceiverConfig>,
    container: aoip::H16Container,
    layout: &'static str,
    /// 源 flow 布局名（2.0-2ch / 4.1.2-7ch / 4.1.2-8ch / h16-dual-flow / custom-slots）。
    flow_layout: &'static str,
    graph_input_channels: usize,
    sdp_path: String,
    interfaces: ReceiverInterfaces,
    group_allow: GroupAllow,
    source_allow: SourceAllow,
    io_priority: Option<i32>,
    stall_blocks: u32,
    /// T239-crackle：流结束保持（standalone `--stay-on-stream-end` 同语义）。
    stay_on_stream_end: bool,
    /// 连续静音请求内部 reanchor 的门限（ms；0 = 不请求）。
    silence_hold_ms: u32,
    /// 上述门限换算成块（0 = 不请求）。
    silence_hold_blocks: u32,
    /// hold 静音总时长 fail-closed 门限（ms；0 = 永不）。
    stall_fail_close_ms: u32,
    /// 上述门限换算成块（0 = 永不）。
    stall_fail_close_blocks: u32,
    /// stale-jitter 看门狗开关。
    reanchor_watchdog: bool,
    /// [capture] ptp_lock 原生值（userspace 传输不以此门控；报告如实标注 PTP 未接线）。
    ptp_lock: String,
    flow_info: Vec<Aes67UserFlowInfo>,
}

/// userspace capture 的**单调时基**读数（仅用于失流监视器 / hold 计时 / 报告）。
///
/// T239-crackle 起，块读取不再用墙钟推导媒体 target：媒体时基由适配层
/// `Aes67RxCapture::fill_block_paced` 按 `origin + latency + delivered` 合成
/// （与 standalone `tools/aes67-rx` 同口径）。墙钟 target 会被 ALSA duplex/USB 节拍
/// 量化成 NotDue（静音插块）+ resync（丢块）的 7–10% 爆音（见证据
/// `rt-stack/evidence/aes67-crackle-20261122/`）。本函数仍给监视器提供同一单调时基。
fn aes67_user_clock_ns() -> u64 {
    UsrvClock::new(AES67_USER_RATE_HZ).now_ns()
}

/// 毫秒 → 128 帧块数（向上取整；48k/128 ≈ 2.667ms/块；0ms = 0 块）。
fn aes67_ms_to_blocks(ms: u32) -> u32 {
    if ms == 0 {
        return 0;
    }
    let frames = ms as u64 * AES67_USER_RATE_HZ as u64 / 1_000;
    let block = AES67_USER_BLOCK_FRAMES as u64;
    ((frames + block - 1) / block).max(1) as u32
}

/// 僵停/失流门限（0 = T43 默认 DEFAULT_MAX_STALL_PERIODS；Lost = 2×N）。
fn aes67_user_stall_blocks(config: &HostRunConfig) -> u32 {
    if config.capture_aes67_stall_blocks == 0 {
        aoip::DEFAULT_MAX_STALL_PERIODS
    } else {
        config.capture_aes67_stall_blocks
    }
}

/// SDP 解析错误 → host AoIP 冻结错误码（与 T43 同码；细节保留 ERR_SDP_* 便于证据定位）。
fn map_aes67_sdp_error(context: &str, error: SdpError) -> String {
    let code = match error {
        SdpError::UnsupportedRate => aoip::AoipError::Rate.name(),
        SdpError::BadChannels => aoip::AoipError::Channels.name(),
        SdpError::UnsupportedEncoding
        | SdpError::UnsupportedPayload
        | SdpError::UnsupportedMedia
        | SdpError::BadRefClk
        | SdpError::MissingRefClk => aoip::AoipError::Unsupported.name(),
        SdpError::Io => ERR_HOST_CAPTURE_CONFIG,
        _ => aoip::AoipError::DeviceParams.name(),
    };
    capture_error(code, format!("{context}：{}", error.name()))
}

/// 接收端构造错误 → host AoIP 冻结错误码（不新增冻结码）。
fn map_aes67_receiver_error(context: &str, error: ReceiverError) -> String {
    match &error {
        ReceiverError::Sdp(sdp) => map_aes67_sdp_error(context, *sdp),
        ReceiverError::Jitter(_) => capture_error(
            aoip::AoipError::BadLayout.name(),
            format!("{context}：{}", error.name()),
        ),
        ReceiverError::ClockNotWired => capture_error(
            aoip::AoipError::PtpUnavailable.name(),
            format!("{context}：{}", error.name()),
        ),
        _ => capture_error(
            aoip::AoipError::DeviceParams.name(),
            format!("{context}：{}", error.name()),
        ),
    }
}

/// T235 适配层稳定错误（ERR_AES67RX_*）→ host AoIP 冻结错误码（T43 同语义）。
fn map_aes67rx_error(context: &str, error: String) -> String {
    let code = if error.starts_with("ERR_AES67RX_ROLES") {
        aoip::AoipError::Roles.name()
    } else if error.starts_with("ERR_AES67RX_LAYOUT") {
        aoip::AoipError::BadLayout.name()
    } else if error.starts_with("ERR_AES67RX_BLOCK") {
        aoip::AoipError::BlockSize.name()
    } else if error.starts_with("ERR_AES67RX_SOCKET") {
        aoip::AoipError::Unsupported.name()
    } else if error.starts_with("ERR_AES67RX_JITTER") {
        aoip::AoipError::BadLayout.name()
    } else {
        aoip::AoipError::DeviceParams.name()
    };
    capture_error(code, format!("{context}：{error}"))
}

/// 由已解析 flow 数/通道数派生 H16 flow 计划（不猜：双 flow 只认 H16 8+4 / 8+8）。
fn aes67_user_flow_plan(
    config: &HostRunConfig,
    flows: &[ReceiverConfig],
    graph_input_channels: usize,
) -> Result<Aes67RxFlowPlan, String> {
    let slots = parse_aes67_rx_slots(&config.capture_aes67_rx_slots)?;
    match flows.len() {
        1 => {
            let channels = flows[0].channels as usize;
            // 7ch 单 flow = 4.1.2 SMPTE2110.(41,2) 源：只接 8ch strict(4.1.2) 图输入
            // （ch6 空、LFE 落 ch7）。2.0 图请用 2ch flow（不做隐式上/下混）。
            if channels == 7 && graph_input_channels != 8 {
                return Err(capture_error(
                    aoip::AoipError::Roles.name(),
                    format!(
                        "AES67 userspace 单 flow 7ch（4.1.2 SMPTE2110.(41,2)）只支持 8ch strict(4.1.2) 图输入（got {graph_input_channels}）；2.0 图请用 2ch flow"
                    ),
                ));
            }
            let slots = match slots {
                // 7ch 4.1.2：显式 H16 逻辑 slot 顺序（FL FR SL SR TFL TFR LFE），LFE 落 graph ch7。
                None if channels == 7 => AES67_SLOTS_412_7CH.to_vec(),
                // 单 flow 无覆盖 = H16 TX1..N（标准容器前 N 个 slot；2ch = FL/FR）。
                None => (0..channels as u8).collect::<Vec<u8>>(),
                Some(slots) => slots,
            };
            let flow = aoip::H16FlowConfig::try_from_slots(&[&slots]).map_err(|error| {
                capture_error(
                    aoip::AoipError::DeviceParams.name(),
                    format!(
                        "AES67 userspace 单 flow slot 计划非法（{slots:?}）：{}",
                        error.name()
                    ),
                )
            })?;
            Ok(Aes67RxFlowPlan::Custom(flow))
        }
        2 => {
            if slots.is_some() {
                return Err(capture_error(
                    ERR_HOST_CAPTURE_CONFIG,
                    "AES67 userspace 双 flow 由 H16 8+4 / 8+8 自动派生容器计划，不接受 aes67_rx_slots 覆盖",
                ));
            }
            match (flows[0].channels, flows[1].channels) {
                (8, 4) => Ok(Aes67RxFlowPlan::Logical12),
                (8, 8) => Ok(Aes67RxFlowPlan::Container16),
                other => Err(capture_error(
                    aoip::AoipError::Channels.name(),
                    format!(
                        "AES67 userspace 双 flow 只支持 H16 8+4（12ch 逻辑容器）或 8+8（16ch 容器）；got {:?}",
                        other
                    ),
                )),
            }
        }
        count => Err(capture_error(
            aoip::AoipError::DeviceParams.name(),
            format!(
                "AES67 userspace 只支持 1 条（显式 slot 计划）或 2 条（H16 8+4/8+8）flow；got {count} 条（SDP 多会话）"
            ),
        )),
    }
}

/// userspace 传输启动计划：SDP 文件 → flow 接收配置 + H16 容器/角色映射（不打开 socket）。
///
/// 只在 source=aes67 且 transport=userspace 时返回 Some；其它 source/transport 返回 None。
fn plan_aes67_user_capture(
    config: &HostRunConfig,
    graph_input_channels: usize,
) -> Result<Option<Aes67UserCapturePlan>, String> {
    let source = CaptureSource::parse(&config.capture_source)?;
    if source != CaptureSource::Aes67
        || Aes67Transport::parse(&config.capture_aes67_transport)? != Aes67Transport::Userspace
    {
        return Ok(None);
    }
    // 启动期纯解析校验（块长/速率/独占字段/SDP 存在性；不打开 socket）。
    validate_aes67_userspace_config(config)?;
    let sdp_path = config.capture_aes67_sdp.trim().to_string();
    let text = std::fs::read_to_string(&sdp_path).map_err(|error| {
        capture_error(
            ERR_HOST_CAPTURE_CONFIG,
            format!("读取 AES67 SDP 失败 {sdp_path}: {error}"),
        )
    })?;
    let sessions = split_aes67_sdp_session_texts(&text)?;
    let interfaces = parse_aes67_iface(&config.capture_aes67_iface)?;
    let group_allow = parse_aes67_group_allow(&config.capture_aes67_group_allow)?;
    let source_allow = parse_aes67_source_allow(&config.capture_aes67_source_allow)?;
    let io_priority = if config.capture_aes67_io_priority == 0 {
        None
    } else {
        Some(i32::from(config.capture_aes67_io_priority))
    };
    let mut receiver_configs = Vec::with_capacity(sessions.len());
    let mut flow_info = Vec::with_capacity(sessions.len());
    for (index, session_text) in sessions.iter().enumerate() {
        let document = SdpDocument::from_text(*session_text);
        let session = document.parse_validated().map_err(|error| {
            map_aes67_sdp_error(
                &format!(
                    "AES67 SDP 第 {} 条会话不合规（AES67 media profile）",
                    index + 1
                ),
                error,
            )
        })?;
        let connection = session.connection().ok_or_else(|| {
            capture_error(
                aoip::AoipError::DeviceParams.name(),
                format!("AES67 SDP 第 {} 条会话缺 c= 连接地址", index + 1),
            )
        })?;
        // task-320：组白名单只对组播 flow 有语义；多接口只对组播 join 有语义。两者
        // 错配都 fail-closed（不静默忽略配置、不静默只用主接口）。先于接收端构造检查，
        // 保证错误信息是 host 配置级（比库层 ERR_RX_CONFIG 更具体、可操作）。
        let multicast = connection.is_multicast();
        let group = connection.ipv4();
        if multicast {
            if !group_allow.is_empty() && !group_allow.contains(group) {
                return Err(capture_error(
                    ERR_HOST_CAPTURE_CONFIG,
                    format!(
                        "AES67 SDP 第 {} 条会话声明的组播组 {group} 不在 [capture] aes67_group_allow 内（允许：{}）：拒绝启动（不 join 未授权组）",
                        index + 1,
                        group_allow.describe()
                    ),
                ));
            }
        } else {
            if !group_allow.is_empty() {
                return Err(capture_error(
                    ERR_HOST_CAPTURE_CONFIG,
                    format!(
                        "AES67 SDP 第 {} 条会话是单播 flow（{group}），但配置了 [capture] aes67_group_allow：组播组白名单只对组播 flow 有语义，拒绝启动",
                        index + 1
                    ),
                ));
            }
            if interfaces.len() > 1 {
                return Err(capture_error(
                    ERR_HOST_CAPTURE_CONFIG,
                    format!(
                        "AES67 SDP 第 {} 条会话是单播 flow，而 [capture] aes67_iface 配置了 {} 个接口：单播只能 bind 一个地址（多接口仅组播 join 支持），拒绝启动（不静默只用主接口）",
                        index + 1,
                        interfaces.len()
                    ),
                ));
            }
        }
        let mut receiver = ReceiverConfig::from_session_with_interfaces(&session, interfaces)
            .map_err(|error| map_aes67_receiver_error("AES67 SDP → 接收端配置", error))?;
        receiver.io_priority = io_priority;
        receiver.group_allow = group_allow;
        receiver.source_allow = source_allow;
        // T265-followup：抖动缓冲延迟/环容量可配（0 = AES67 默认 4 ms / 8192 帧）。
        if config.capture_aes67_jitter_latency_us != 0
            || config.capture_aes67_jitter_ring_frames != 0
        {
            use khaen_aoip_aes67::jitter::{
                JitterConfig, DEFAULT_LATENCY_US, DEFAULT_RING_CAPACITY_FRAMES,
            };
            let latency_us = if config.capture_aes67_jitter_latency_us == 0 {
                DEFAULT_LATENCY_US
            } else {
                config.capture_aes67_jitter_latency_us
            };
            let ring_frames = if config.capture_aes67_jitter_ring_frames == 0 {
                DEFAULT_RING_CAPACITY_FRAMES
            } else {
                config.capture_aes67_jitter_ring_frames as usize
            };
            receiver.jitter = JitterConfig::new(
                receiver.channels,
                AES67_USER_RATE_HZ,
                AES67_RX_BLOCK_FRAMES,
                ring_frames,
                latency_us,
            )
            .map_err(|error| {
                map_aes67_receiver_error(
                    "AES67 抖动缓冲配置（latency/ring）",
                    ReceiverError::Jitter(error),
                )
            })?;
        }
        receiver
            .validate()
            .map_err(|error| map_aes67_receiver_error("AES67 接收端配置校验", error))?;
        flow_info.push(Aes67UserFlowInfo {
            index: index as u8,
            media: connection.ipv4().to_string(),
            port: receiver.port,
            multicast: receiver.multicast,
            channels: receiver.channels,
        });
        receiver_configs.push(receiver);
    }
    let flow_plan = aes67_user_flow_plan(config, &receiver_configs, graph_input_channels)?;
    let content_512 = config.content_input_layout.trim() == "nsp-5.1.2";
    if content_512 && !config.capture_aes67_rx_slots.trim().is_empty() {
        return Err(capture_error(
            ERR_HOST_CONTENT_CONFIG,
            "输入布局 nsp-5.1.2 按线序进图，不接受 aes67_rx_slots 覆盖",
        ));
    }
    if content_512
        && (graph_input_channels != 8
            || receiver_configs.len() != 1
            || receiver_configs[0].channels != 8)
    {
        return Err(capture_error(
            ERR_HOST_CONTENT_CONFIG,
            format!(
                "输入布局 nsp-5.1.2 只接受一条 8 通道 AES67 flow 和 8 通道图输入（got {} flow / {} ch / 图 {}）",
                receiver_configs.len(),
                receiver_configs.first().map(|flow| flow.channels).unwrap_or(0),
                graph_input_channels
            ),
        ));
    }
    let adapter = if content_512 {
        Aes67RxAdapter::new_content_order(flow_plan, graph_input_channels as u16)
    } else {
        Aes67RxAdapter::new(flow_plan, graph_input_channels as u16)
    }
    .map_err(|error| map_aes67rx_error("AES67 userspace flow/角色计划", error))?;
    let container =
        aoip::H16Container::from_flow_config(&flow_plan.flow_config()).map_err(|error| {
            capture_error(
                error.name(),
                format!("AES67 userspace H16 容器映射：{}", error.name()),
            )
        })?;
    if receiver_configs.len() != adapter.flow_count() {
        return Err(capture_error(
            aoip::AoipError::DeviceParams.name(),
            format!(
                "AES67 userspace flow 数不符：SDP {} 条 vs 计划 {} 条",
                receiver_configs.len(),
                adapter.flow_count()
            ),
        ));
    }
    let layout = if content_512 {
        AES67_RX_LAYOUT_512_CONTENT
    } else {
        capture_layout_roles(graph_input_channels)?.0
    };
    // 源 flow 布局（证据用；7ch 4.1.2 与 8ch 4.1.2 的图输入布局相同，但 flow 宽度/LFE 位置不同）。
    let flow_layout = if content_512 {
        CAPTURE_FLOW_LAYOUT_512_8CH
    } else {
        match receiver_configs.len() {
            1 => match (receiver_configs[0].channels, graph_input_channels) {
                (2, 2) => CAPTURE_FLOW_LAYOUT_2CH,
                (7, 8) => CAPTURE_FLOW_LAYOUT_412_7CH,
                (8, 8) => CAPTURE_FLOW_LAYOUT_412_8CH,
                _ => CAPTURE_FLOW_LAYOUT_CUSTOM,
            },
            2 => CAPTURE_FLOW_LAYOUT_H16_DUAL,
            _ => CAPTURE_FLOW_LAYOUT_CUSTOM,
        }
    };
    // T239-crackle：hold/reanchor 参数在启动期定死（块数换算一次，热路径只读）。
    let stay_on_stream_end = config.capture_aes67_stay_on_stream_end;
    let silence_hold_ms = config.capture_aes67_silence_hold_ms;
    let stall_fail_close_ms = config.capture_aes67_stall_fail_close_ms;
    Ok(Some(Aes67UserCapturePlan {
        adapter,
        receiver_configs,
        container,
        layout,
        flow_layout,
        graph_input_channels,
        sdp_path,
        interfaces,
        group_allow,
        source_allow,
        io_priority,
        stall_blocks: aes67_user_stall_blocks(config),
        stay_on_stream_end,
        silence_hold_ms,
        silence_hold_blocks: if stay_on_stream_end {
            aes67_ms_to_blocks(silence_hold_ms)
        } else {
            0
        },
        stall_fail_close_ms,
        stall_fail_close_blocks: aes67_ms_to_blocks(stall_fail_close_ms),
        reanchor_watchdog: config.capture_aes67_reanchor_watchdog,
        ptp_lock: config.capture_ptp_lock.trim().to_string(),
        flow_info,
    }))
}

/// userspace AES67 capture 运行对象（start 时打开 socket/收包线程；块循环只 fill）。
struct Aes67UserCapture {
    plan: Aes67UserCapturePlan,
    capture: Aes67RxCapture,
    monitor: aoip::StreamMonitor,
    /// backend=fake 时是否启用 userspace 块节拍（无播放设备时钟）。
    pacing: bool,
    paced_blocks: u64,
    blocks: u64,
    data_blocks: u64,
    silent_blocks: u64,
    muted_blocks: u64,
    warmup_blocks: u64,
    underrun_blocks: u64,
    not_due_blocks: u64,
    stall_events: u32,
    loss_events: u32,
    resume_events: u32,
    /// 连续静音块数（hold / stall_fail_close 判定；Ok 块清零）。
    silent_streak_blocks: u64,
    /// SafetyMute 连续块数（恢复后避免播放过期数据 → 请求内部 reanchor）。
    muted_streak_blocks: u64,
    /// hold 事件数（达到 silence_hold 门限请求 reanchor 的次数）。
    hold_events: u64,
    /// 内部 reanchor 请求次数（hold / 监视器 / 看门狗共用）。
    reanchor_requests: u64,
    /// stale-jitter 看门狗命中次数 / 连续 strike 数。
    watchdog_events: u64,
    watchdog_strikes: u32,
    /// 看门狗块计数 + IO 快照（启动期一次分配；热路径零分配）。
    watchdog_blocks: u64,
    watchdog_prev: Vec<Option<(u64, u64)>>,
    watchdog_now: Vec<(u64, u64)>,
}

impl Aes67UserCapture {
    /// 打开全部 flow 的 UDP 接收端并启动 T43 同口径失流监视器（启动期；非 RT）。
    fn start(plan: Aes67UserCapturePlan, now_ns: u64, pacing: bool) -> Result<Self, String> {
        let capture = Aes67RxCapture::bind(&plan.receiver_configs, plan.adapter)
            .map_err(|error| map_aes67rx_error("AES67 userspace 接收端绑定失败", error))?;
        Self::from_capture(plan, capture, now_ns, pacing)
    }

    /// 由已就绪的适配器构造（单测/嵌入式；不打开 socket）。
    fn from_capture(
        plan: Aes67UserCapturePlan,
        capture: Aes67RxCapture,
        now_ns: u64,
        pacing: bool,
    ) -> Result<Self, String> {
        let flow_count = plan.receiver_configs.len();
        let mut monitor = aoip::StreamMonitor::new(
            AES67_RX_BLOCK_FRAMES as u32,
            AES67_USER_RATE_HZ,
            plan.stall_blocks,
        )
        .map_err(|error| {
            capture_error(
                error.name(),
                format!(
                    "AES67 userspace 失流监视器参数非法（block={AES67_RX_BLOCK_FRAMES} rate={AES67_USER_RATE_HZ} stall={}）",
                    plan.stall_blocks
                ),
            )
        })?;
        monitor.start(now_ns);
        Ok(Self {
            plan,
            capture,
            monitor,
            pacing,
            paced_blocks: 0,
            blocks: 0,
            data_blocks: 0,
            silent_blocks: 0,
            muted_blocks: 0,
            warmup_blocks: 0,
            underrun_blocks: 0,
            not_due_blocks: 0,
            stall_events: 0,
            loss_events: 0,
            resume_events: 0,
            silent_streak_blocks: 0,
            muted_streak_blocks: 0,
            hold_events: 0,
            reanchor_requests: 0,
            watchdog_events: 0,
            watchdog_strikes: 0,
            watchdog_blocks: 0,
            watchdog_prev: vec![None; flow_count],
            watchdog_now: vec![(0, 0); flow_count],
        })
    }

    fn lost_error(&self) -> String {
        capture_error(
            aoip::AoipError::StreamLost.name(),
            format!(
                "AES67 userspace 持续失流（blocks={} silent={} stall_blocks={} state={}）：T43 要求新 epoch + reopen；host epoch owner（T19/T26）NOT_WIRED -> fail closed；真实网络/PTP NOT_RUN",
                self.blocks,
                self.silent_blocks,
                self.plan.stall_blocks,
                self.monitor.state().as_str()
            ),
        )
    }

    /// 推进 T43 失流监视器（停滞 -> 整块静音；持续失流 -> fail closed）。
    ///
    /// T239-crackle：stay/hold 模式下 Lost **不**直接 fail closed，而是请求内部 reanchor
    /// 并重置监视器（进程内恢复）；strict 模式（stay=false）保持 T43 旧行为。
    fn poll_hold(&mut self, now_ns: u64) -> Result<(), String> {
        match self.monitor.poll(now_ns) {
            aoip::MonitorEvent::Stalled { .. } => {
                self.stall_events = self.stall_events.saturating_add(1);
                Ok(())
            }
            aoip::MonitorEvent::Lost { .. } => {
                self.loss_events = self.loss_events.saturating_add(1);
                if self.plan.stay_on_stream_end {
                    self.request_internal_reanchor();
                    self.monitor.reset();
                    self.monitor.start(now_ns);
                    Ok(())
                } else {
                    Err(self.lost_error())
                }
            }
            _ => Ok(()),
        }
    }

    fn note_event(&mut self, event: aoip::MonitorEvent) {
        match event {
            aoip::MonitorEvent::Started => {}
            aoip::MonitorEvent::Resumed { .. } => {
                self.resume_events = self.resume_events.saturating_add(1);
            }
            aoip::MonitorEvent::Stalled { .. } => {
                self.stall_events = self.stall_events.saturating_add(1);
            }
            aoip::MonitorEvent::Lost { .. } => {
                self.loss_events = self.loss_events.saturating_add(1);
            }
            aoip::MonitorEvent::None => {}
        }
    }

    fn note_paced(&mut self) {
        self.paced_blocks = self.paced_blocks.saturating_add(1);
    }

    /// 一个 block：poll 监视器 -> **合成媒体时钟**读 userspace capture 整块 -> 数据/整块静音。
    ///
    /// 与 AoipCapture::fill_block 同语义：末完成整块（不半块），任一 flow 缺数据整块静音
    /// 由 T235 适配层保证；停滞不重置数据（不重放旧样本）。
    ///
    /// T239-crackle：媒体时基由适配层合成（origin+latency+delivered），本函数**不**再用
    /// 墙钟推导 target；`now_ns` 只用于失流监视器/hold 计时（单一单调时基）。
    /// stay/hold：NSP 会话间隙保持静音等待 + 内部 reanchor（不整进程重启）；
    /// 静音总时长超过 `aes67_stall_fail_close_ms` 才 fail closed（默认 10s）。
    fn fill_block(
        &mut self,
        input: &mut [f32],
        stride: usize,
        block: usize,
        now_ns: u64,
    ) -> Result<(), String> {
        self.poll_hold(now_ns)?;
        let status = self
            .capture
            .fill_block_paced(input, stride, block)
            .map_err(|error| map_aes67rx_error("AES67 userspace 读块失败", error))?;
        self.blocks = self.blocks.saturating_add(1);
        match status {
            BlockStatus::Ok => {
                let event = self.on_data_hold(now_ns, block as u32)?;
                self.note_event(event);
                self.data_blocks = self.data_blocks.saturating_add(1);
                self.silent_streak_blocks = 0;
                self.muted_streak_blocks = 0;
            }
            other => {
                match other {
                    BlockStatus::Warmup => {
                        self.warmup_blocks = self.warmup_blocks.saturating_add(1)
                    }
                    BlockStatus::Underrun => {
                        self.underrun_blocks = self.underrun_blocks.saturating_add(1)
                    }
                    BlockStatus::NotDue => {
                        self.not_due_blocks = self.not_due_blocks.saturating_add(1)
                    }
                    BlockStatus::Ok => {}
                }
                self.silent_blocks = self.silent_blocks.saturating_add(1);
                self.silent_streak_blocks = self.silent_streak_blocks.saturating_add(1);
                self.hold_tick()?;
            }
        }
        self.watchdog_tick();
        if let Some(error) = self.watchdog_fault() {
            return Err(error);
        }
        Ok(())
    }

    /// 数据登记（hold 语义）：监视器要求新 epoch 时内部 reanchor + 重置监视器后重试一次。
    fn on_data_hold(&mut self, now_ns: u64, frames: u32) -> Result<aoip::MonitorEvent, String> {
        match self.monitor.on_data(now_ns, frames) {
            Ok(event) => Ok(event),
            Err(_) if self.plan.stay_on_stream_end => {
                self.request_internal_reanchor();
                self.monitor.reset();
                self.monitor.start(now_ns);
                self.monitor.on_data(now_ns, frames).map_err(|error| {
                    capture_error(
                        error.name(),
                        format!(
                            "AES67 userspace 失流监视器拒绝数据（state={}；需新 epoch）",
                            self.monitor.state().as_str()
                        ),
                    )
                })
            }
            Err(error) => Err(capture_error(
                error.name(),
                format!(
                    "AES67 userspace 失流监视器拒绝数据（state={}；需新 epoch）",
                    self.monitor.state().as_str()
                ),
            )),
        }
    }

    /// 静音块 hold 判定：达到 silence_hold 门限请求一次内部 reanchor；超过
    /// stall_fail_close 门限才 fail closed（0 = 永不）。
    fn hold_tick(&mut self) -> Result<(), String> {
        if self.plan.stay_on_stream_end
            && self.plan.silence_hold_blocks > 0
            && self.silent_streak_blocks == self.plan.silence_hold_blocks as u64
        {
            self.hold_events = self.hold_events.saturating_add(1);
            self.request_internal_reanchor();
        }
        let fail_blocks = self.plan.stall_fail_close_blocks;
        if fail_blocks > 0 && self.silent_streak_blocks >= fail_blocks as u64 {
            return Err(capture_error(
                aoip::AoipError::StreamLost.name(),
                format!(
                    "AES67 userspace 静音保持超时（silent_streak={} blocks ≥ stall_fail_close={} blocks ≈ {} ms）：hold 已耗尽，fail closed 交 systemd 重启；真实网络/PTP NOT_RUN",
                    self.silent_streak_blocks,
                    fail_blocks,
                    self.plan.stall_fail_close_ms
                ),
            ));
        }
        Ok(())
    }

    /// 请求内部重锚（进程内恢复：下一个包作为新流首包；接收端 ring/origin/帧号重置）。
    /// `from_readers` 测试路径没有 socket（转发数 0），但请求次数仍如实计数。
    fn request_internal_reanchor(&mut self) {
        let _ = self.capture.request_reanchor();
        self.reanchor_requests = self.reanchor_requests.saturating_add(1);
    }

    /// stale-jitter 看门狗（≈500ms 一次）：datagrams 在涨但 accepted 不涨 = 新会话包被旧
    /// 锚点全部判 late；请求内部 reanchor，连续 3 次无效才 fail closed 交 systemd。
    fn watchdog_tick(&mut self) {
        if !self.plan.reanchor_watchdog || self.watchdog_now.is_empty() {
            return;
        }
        self.watchdog_blocks = self.watchdog_blocks.saturating_add(1);
        if self.watchdog_blocks % AES67_WATCHDOG_INTERVAL_BLOCKS != 0 {
            return;
        }
        let written = self.capture.io_stats_into(&mut self.watchdog_now);
        let mut stale = false;
        for index in 0..written {
            let current = self.watchdog_now[index];
            if let Some(previous) = self.watchdog_prev[index] {
                let datagrams = current.0.saturating_sub(previous.0);
                let accepted = current.1.saturating_sub(previous.1);
                if datagrams > AES67_WATCHDOG_MIN_DATAGRAMS && accepted == 0 {
                    stale = true;
                }
            }
            self.watchdog_prev[index] = Some(current);
        }
        if stale {
            self.watchdog_strikes = self.watchdog_strikes.saturating_add(1);
            self.watchdog_events = self.watchdog_events.saturating_add(1);
            self.request_internal_reanchor();
        } else {
            self.watchdog_strikes = 0;
        }
    }

    /// 看门狗连续命中是否超限（返回 Err 由 fill_block 传播 → fail closed 交 systemd）。
    fn watchdog_fault(&self) -> Option<String> {
        if self.watchdog_strikes < AES67_WATCHDOG_MAX_STRIKES {
            return None;
        }
        Some(capture_error(
            aoip::AoipError::StreamLost.name(),
            format!(
                "AES67 userspace stale-jitter 看门狗连续 {} 次判新增包被 jitter 拒绝（datagrams 涨 accepted 不涨；内部 reanchor 无效）：fail closed 交 systemd 重锚；真实网络/PTP NOT_RUN",
                AES67_WATCHDOG_MAX_STRIKES
            ),
        ))
    }

    /// SafetyMute/gate 拒绝块：不消费新块，输入清零；只推进失流监视器（同 AoipCapture::poll）。
    fn muted_block(&mut self, input: &mut [f32], now_ns: u64) -> Result<(), String> {
        self.poll_hold(now_ns)?;
        input.fill(0.0);
        self.muted_blocks = self.muted_blocks.saturating_add(1);
        self.muted_streak_blocks = self.muted_streak_blocks.saturating_add(1);
        // SafetyMute 期间不消费 capture：超过 hold 门限后请求内部 reanchor，
        // 避免恢复后继续播放已经过期/被 overrun 的数据。
        if self.plan.stay_on_stream_end
            && self.plan.silence_hold_blocks > 0
            && self.muted_streak_blocks == self.plan.silence_hold_blocks as u64
        {
            self.request_internal_reanchor();
        }
        Ok(())
    }

    fn stop(&mut self) {
        self.capture.stop();
    }

    /// 证据 JSON（NONRT；退出路径调用）。含 SDP/flow/socket/jitter 统计 + 适配层原始报告。
    fn report_json(&self, exit_reason: &str) -> String {
        use std::fmt::Write as _;
        let counters = self.capture.counters();
        let receivers = self.capture.receiver_stats();
        let locals = self.capture.local_addrs();
        let capture_flows = self.capture.flow_info();
        let mut json = String::with_capacity(2_048);
        let interface = match self.plan.interfaces.primary() {
            Some(address) => address.to_string(),
            None => "0.0.0.0".to_string(),
        };
        let interfaces = self
            .plan
            .interfaces
            .addresses()
            .iter()
            .map(|address| format!("\"{address}\""))
            .collect::<Vec<_>>()
            .join(",");
        let group_allow = self
            .plan
            .group_allow
            .groups()
            .iter()
            .map(|group| format!("\"{group}\""))
            .collect::<Vec<_>>()
            .join(",");
        let source_allow = self
            .plan
            .source_allow
            .cidrs()
            .iter()
            .map(|cidr| format!("\"{cidr}\""))
            .collect::<Vec<_>>()
            .join(",");
        let _ = write!(
            json,
            "{{\"source\":\"aes67\",\"mode\":\"aes67\",\"transport\":\"{}\",\
\"reader\":\"khaen-aoip-aes67\",\"sdp\":\"{}\",\"interface\":\"{}\",\
\"interfaces\":[{}],\"group_allow\":[{}],\"source_allow\":[{}],\"sample_rate\":{},\
\"period_frames\":{},\"flow_count\":{},\"container_channels\":{},\"graph_input_channels\":{},\
\"layout\":\"{}\",\"flow_layout\":\"{}\",\"jitter_latency_us\":{},\"jitter_ring_frames\":{},\
\"clock\":\"usrvclock\",\"ptp\":\"not-wired\",\"ptp_lock\":\"{}\",\
\"io_priority\":{},\"pacing\":\"{}\",\"paced_blocks\":{},\"blocks\":{},\"data_blocks\":{},\
\"silent_blocks\":{},\"muted_blocks\":{},\"warmup_blocks\":{},\"underrun_blocks\":{},\
\"not_due_blocks\":{},\"frames\":{},\"stall_blocks\":{},\"stall_events\":{},\
\"loss_events\":{},\"resume_events\":{},\"stay_on_stream_end\":{},\
\"silence_hold_ms\":{},\"silence_hold_blocks\":{},\"stall_fail_close_ms\":{},\
\"stall_fail_close_blocks\":{},\"reanchor_watchdog\":{},\
\"silent_streak_blocks\":{},\"hold_events\":{},\"reanchor_requests\":{},\
\"watchdog_events\":{},\"watchdog_strikes\":{},\"muted_streak_blocks\":{},\
\"state\":\"{}\",\"exit_reason\":\"{}\",\
\"not_run\":\"真实板端网卡/IGMP/PTP/电平/听音 NOT_RUN；PTP/PHC 未接线（UsrvClock 非 PTP）\",\
\"flows\":[",
            Aes67Transport::Userspace.as_str(),
            json_escape_text(&self.plan.sdp_path),
            interface,
            interfaces,
            group_allow,
            source_allow,
            AES67_USER_RATE_HZ,
            AES67_RX_BLOCK_FRAMES,
            self.plan.adapter.flow_count(),
            self.plan.container.capture_channels(),
            self.plan.graph_input_channels,
            self.plan.layout,
            self.plan.flow_layout,
            self.plan
                .receiver_configs
                .first()
                .map(|receiver| receiver.jitter.latency_us)
                .unwrap_or(0),
            self.plan
                .receiver_configs
                .first()
                .map(|receiver| receiver.jitter.capacity_frames)
                .unwrap_or(0),
            json_escape_text(&self.plan.ptp_lock),
            match self.plan.io_priority {
                Some(priority) => priority.to_string(),
                None => "null".to_string(),
            },
            if self.pacing {
                "userspace-block-timer"
            } else {
                "device-clock"
            },
            self.paced_blocks,
            self.blocks,
            self.data_blocks,
            self.silent_blocks,
            self.muted_blocks,
            self.warmup_blocks,
            self.underrun_blocks,
            self.not_due_blocks,
            counters.frames_delivered,
            self.plan.stall_blocks,
            self.stall_events,
            self.loss_events,
            self.resume_events,
            self.plan.stay_on_stream_end,
            self.plan.silence_hold_ms,
            self.plan.silence_hold_blocks,
            self.plan.stall_fail_close_ms,
            self.plan.stall_fail_close_blocks,
            self.plan.reanchor_watchdog,
            self.silent_streak_blocks,
            self.hold_events,
            self.reanchor_requests,
            self.watchdog_events,
            self.watchdog_strikes,
            self.muted_streak_blocks,
            self.monitor.state().as_str(),
            json_escape_text(exit_reason)
        );
        for (index, flow) in self.plan.flow_info.iter().enumerate() {
            if index > 0 {
                json.push(',');
            }
            let bound = capture_flows.get(index);
            let local = locals
                .get(index)
                .map(|address| address.to_string())
                .unwrap_or_else(|| "none".to_string());
            let stats = receivers.get(index).copied().unwrap_or_default();
            let _ = write!(
                json,
                "{{\"index\":{},\"media\":\"{}\",\"port\":{},\"multicast\":{},\
\"channels\":{},\"local_addr\":\"{}\",\"channels_bound\":{},\
\"rcvbuf_requested\":{},\"io\":{{\"datagrams\":{},\"bytes\":{},\
\"rtp_packets\":{},\"accepted_packets\":{},\"accepted_frames\":{},\
\"filtered_source\":{},\"dropped_packets\":{},\"malformed_packets\":{},\
\"oversized_datagrams\":{},\
\"socket_errors\":{}}},\"jitter\":{{\"packets\":{},\"frames_received\":{},\
\"frames_played\":{},\"underruns\":{},\"warmups\":{},\"not_due\":{},\"overruns\":{},\
\"late_packets\":{},\"lost_packets\":{},\"duplicate_packets\":{},\
\"out_of_order_packets\":{},\"timestamp_jumps\":{},\"stream_restarts\":{},\
\"resync_blocks\":{},\"high_water_frames\":{}}}}}",
                flow.index,
                json_escape_text(&flow.media),
                flow.port,
                flow.multicast,
                flow.channels,
                json_escape_text(&local),
                bound.map(|bound| bound.channels).unwrap_or(flow.channels),
                self.plan
                    .receiver_configs
                    .get(index)
                    .map(|config| config.rcvbuf_bytes)
                    .unwrap_or(0),
                stats.io.datagrams,
                stats.io.bytes,
                stats.io.rtp_packets,
                stats.io.accepted_packets,
                stats.io.accepted_frames,
                stats.io.filtered_source,
                stats.io.dropped_packets,
                stats.io.malformed_packets,
                stats.io.oversized_datagrams,
                stats.io.socket_errors,
                stats.jitter.packets,
                stats.jitter.frames_received,
                stats.jitter.frames_played,
                stats.jitter.underruns,
                stats.jitter.warmups,
                stats.jitter.not_due,
                stats.jitter.overruns,
                stats.jitter.late_packets,
                stats.jitter.lost_packets,
                stats.jitter.duplicate_packets,
                stats.jitter.out_of_order_packets,
                stats.jitter.timestamp_jumps,
                stats.jitter.stream_restarts,
                stats.jitter.resync_blocks,
                stats.jitter.high_water_frames
            );
        }
        json.push_str("],\"adapter\":");
        json.push_str(&self.capture.report_json());
        json.push('}');
        json
    }
}

/// 无播放设备（backend=fake）时 userspace capture 的块节拍（48k/128 = 2.667 ms）。
///
/// 真实 backend=alsa 由阻塞 writei 提供设备时钟节拍；fake 无设备时钟，若不限速块循环会
/// 跑飞（jitter reader 全部 NotDue/Underrun）。本 pacer 只用于 userspace capture + fake
/// backend 的静音链路测试，不用于真实 RT 生产路径（生产 = 设备时钟）。
struct UserCapturePacer {
    next: std::time::Instant,
    period: std::time::Duration,
    blocks_waited: u64,
}

impl UserCapturePacer {
    fn new(block: usize, rate: u32) -> Self {
        let block_ns = (block as u64)
            .saturating_mul(1_000_000_000)
            .checked_div(rate.max(1) as u64)
            .unwrap_or(0);
        Self {
            next: std::time::Instant::now() + std::time::Duration::from_nanos(block_ns),
            period: std::time::Duration::from_nanos(block_ns),
            blocks_waited: 0,
        }
    }

    /// 等到下一 block 时刻；已落后则不补跑突发（不与媒体时钟赛跑，从当前重新起算）。
    fn wait_next(&mut self) {
        let now = std::time::Instant::now();
        if now < self.next {
            let remain = self.next - now;
            let margin = std::time::Duration::from_micros(300);
            if remain > margin {
                std::thread::sleep(remain - margin);
            }
            while std::time::Instant::now() < self.next {
                std::hint::spin_loop();
            }
            self.next += self.period;
        } else {
            self.next = now + self.period;
        }
        self.blocks_waited = self.blocks_waited.saturating_add(1);
    }
}

/// capture 运行对象：ravenna/inferno 的 ALSA 端点 或 aes67 用户态接收端（二者互斥）。
enum CaptureInput {
    Aoip(AoipCapture),
    Aes67User(Aes67UserCapture),
}

impl CaptureInput {
    fn stop(&mut self) {
        match self {
            CaptureInput::Aoip(capture) => capture.stop(),
            CaptureInput::Aes67User(capture) => capture.stop(),
        }
    }

    /// fake backend（无播放设备节拍）时，userspace capture 需要自带块节拍。
    fn pacer(&self, block: usize, rate: u32) -> Option<UserCapturePacer> {
        match self {
            CaptureInput::Aes67User(capture) if capture.pacing => {
                Some(UserCapturePacer::new(block, rate))
            }
            _ => None,
        }
    }

    fn note_paced(&mut self) {
        if let CaptureInput::Aes67User(capture) = self {
            capture.note_paced();
        }
    }
}

/// 启动计划：T43 AoIP（inferno/ravenna）或 T239 userspace AES67（二选一）。
enum CaptureInputPlan {
    Aoip(AoipCapturePlan),
    Aes67User(Aes67UserCapturePlan),
}

/// 计划阶段（纯校验，不打开设备）：两种传输按 source/transport 唯一派发。
fn plan_capture_input(
    config: &HostRunConfig,
    graph_input_channels: usize,
) -> Result<Option<CaptureInputPlan>, String> {
    if let Some(plan) = plan_aoip_capture(config, graph_input_channels)? {
        return Ok(Some(CaptureInputPlan::Aoip(plan)));
    }
    Ok(plan_aes67_user_capture(config, graph_input_channels)?.map(CaptureInputPlan::Aes67User))
}

/// 设备阶段：打开端点（T43 ALSA 或 userspace UDP socket）；both 在 web/worker/缓冲初始化后。
fn start_capture_input(
    plan: Option<CaptureInputPlan>,
    config: &HostRunConfig,
    pace_with_block_timer: bool,
) -> Result<Option<CaptureInput>, String> {
    let plan = match plan {
        Some(plan) => plan,
        None => return Ok(None),
    };
    match plan {
        CaptureInputPlan::Aoip(plan) => {
            start_aoip_capture(Some(plan), config).map(|capture| capture.map(CaptureInput::Aoip))
        }
        CaptureInputPlan::Aes67User(plan) => {
            Aes67UserCapture::start(plan, aes67_user_clock_ns(), pace_with_block_timer)
                .map(|capture| Some(CaptureInput::Aes67User(capture)))
        }
    }
}

/// [run-loop] capture 段：T43 AoIP 或 T239 userspace AES67（语义一致：停滞静音/失流 fail closed）。
fn capture_input_phase(
    capture: Option<&mut CaptureInput>,
    input: &mut [f32],
    stride: usize,
    block: usize,
    mute: bool,
    now_ns: u64,
) -> Result<(), String> {
    match capture {
        None => Ok(()),
        Some(CaptureInput::Aoip(capture)) => {
            aoip_input_phase(Some(capture), input, stride, block, mute, now_ns)
        }
        Some(CaptureInput::Aes67User(capture)) => {
            // 忽略调用方 host 时钟：userspace 传输必须用接收端同一 UsrvClock 时基。
            let _ = now_ns;
            let media_now = aes67_user_clock_ns();
            if mute {
                capture.muted_block(input, media_now)
            } else {
                capture.fill_block(input, stride, block, media_now)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// T71 前置：backend=alsa 真实 Linux ALSA 播放接线（MOTU hw:4,0 / S24_3LE / 22ch /
// 48k）。fake/file 契约不变：backend=fake 时本段只产旧契约 JSON，不打开任何设备。
//
// 真实硬件打开/读写只在 #[cfg(target_os = "linux")] 的 open_alsa_sink 中；Windows/
// macOS 返回明确的 ERR_BACKEND_UNAVAILABLE 文本（本机用 mock ALSA 抽象做单测）。
// 真实设备读写/xrun/恢复的板端验证 NOT_RUN（证据 cursor-fix-alsa-backend/）。
// ---------------------------------------------------------------------------

use khaen_core::events::{
    EV_EPOCH_START, EV_INPUT_OVERRUN, EV_OUTPUT_UNDERRUN, EV_SAFETY_MUTE_ENTER, LANE_CAPTURE,
    LANE_CONTROL, LANE_PLAYBACK, SEVERITY_ERROR, SEVERITY_FATAL, SEVERITY_INFO,
};
use khaen_rt_host::alsa::{
    AlsaError, DuplexActual, DuplexSession, DuplexState, PcmDuplexBackend, RecoveryAction,
    StreamDirection,
};
use khaen_rt_host::fault::{EventLanes, FaultCounters};

/// 本 target 的真实 ALSA backend 类型：Linux = `LinuxAlsaBackend`；其它 = 无硬件占位
/// `NoHwBackend`（open_alsa_sink 在非 Linux 直接返回错误，不会构造该类型）。
#[cfg(target_os = "linux")]
type HostAlsaBackend = khaen_rt_host::alsa::LinuxAlsaBackend;
#[cfg(not(target_os = "linux"))]
type HostAlsaBackend = khaen_rt_host::alsa::NoHwBackend;

/// 一次 IO 动作上报给 host 事件环（T21 lane/kind；host 负责 event_record 序号）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SinkEvent {
    kind: u16,
    lane: u8,
    severity: u8,
    value_a: u32,
    value_b: u32,
}

/// 一个 block 的 capture 阶段结果（core-v1 §8.2 SafetyMute 语义：安全静音期间不处理新块）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureOutcome {
    /// 正常读到一个 capture period（事件为 xrun/恢复耗尽等顺延上报）。
    Captured(Option<SinkEvent>),
    /// 会话处于 SafetyMute（Muted 等非 Running 状态）：本块不读 capture、不送新输入、
    /// 不跑 graph，只由 host 写预分配静音并计数；事件为进入静音的专用事件。
    SafetyMute {
        /// 进入静音的专用事件（每次进入恰一次）。
        event: Option<SinkEvent>,
        /// 进入静音的原因码（RtStatus u32；loop 级汇总事件使用）。
        reason: u32,
    },
}

impl CaptureOutcome {
    /// 本块顺延/新产生的事件。
    fn event(self) -> Option<SinkEvent> {
        match self {
            CaptureOutcome::Captured(event) => event,
            CaptureOutcome::SafetyMute { event, .. } => event,
        }
    }

    /// SafetyMute 原因码（非 SafetyMute 返回 None）。
    fn safety_mute_reason(self) -> Option<u32> {
        match self {
            CaptureOutcome::Captured(_) => None,
            CaptureOutcome::SafetyMute { reason, .. } => Some(reason),
        }
    }
}
/// host 侧 playback 设备 sink 抽象（fake 路径 = None；alsa 路径 = PcmPlaybackSink）。
///
/// 稳态块级方法只用预分配缓冲（启动时一次分配），不新建线程、不做设备无关格式化。
trait PlaybackSink {
    /// 读一个 capture period（保持 capture 流健康；loopback 模式下同时解成 f32）。
    ///
    /// F1：Muted/非 Running 会话不得被当作不可恢复错误；返回
    /// `CaptureOutcome::SafetyMute`，由 run loop 跳过本块处理后写静音。
    fn read_capture(&mut self) -> Result<CaptureOutcome, String>;
    /// 是否 loopback 输入模式（true 时 host 用它替换合成输入）。
    fn has_loopback_input(&self) -> bool;
    /// 把最近一次 capture period 的 Toslink FL/FR 写进 graph 输入 planar 缓冲。
    fn fill_loopback_input(
        &self,
        dst: &mut [f32],
        stride: usize,
        samples: usize,
    ) -> Result<(), String>;
    /// 把一个 graph 输出块转换成设备字节并写 playback period（mute = 安全静音）。
    fn write_block(
        &mut self,
        output: &[f32],
        stride: usize,
        frames: usize,
        mute: bool,
    ) -> Result<Option<SinkEvent>, String>;
    /// 起播/恢复后预填 playback 缓冲（静音 cushion；T17 D-07：设备仍由首次 writei
    /// 自动启动）。默认 no-op（fake/file 路径不使用真实设备时钟）。
    fn prime_playback(&mut self) -> Result<(), String> {
        Ok(())
    }
    /// 停流：drain → close → 释放 owner（幂等；非 Running 允许）。
    fn stop(&mut self) -> Result<(), String>;
    /// 证据/JSON 报告（`alsa` 字段；backend=fake 不使用）。
    fn report(&self) -> String;
}

/// ALSA sink 计数（T21 口径：xrun/恢复/mute/fault；非 RT 导出）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AlsaSinkCounters {
    blocks: u64,
    bytes_written: u64,
    /// 起播/恢复预填的 playback device period 数（静音 cushion；不计入 blocks/bytes_written）。
    prime_blocks: u64,
    /// 预填写出的设备字节数（证据用；不计入 bytes_written）。
    prime_bytes: u64,
    silence_blocks: u64,
    xruns: u32,
    underruns: u32,
    overruns: u32,
    recoveries: u32,
    muted_blocks: u32,
    faults: u32,
    short_reads: u32,
    /// 自上次成功 playback 写起连续 underrun/设备丢失 IO 错误数（T21 value_a；§8.2）。
    consecutive_underruns: u32,
    /// 自上次成功 capture 读起连续 overrun/设备丢失 IO 错误数（T21 value_a；§8.2）。
    consecutive_overruns: u32,
}

/// 真实/模拟 duplex session 上的 playback sink。
///
/// 缓冲全部在启动时一次分配：capture 设备字节 + capture planar（loopback）+ 22ch
/// playback planar + playback 设备字节；块循环只填充/复用。
struct PcmPlaybackSink<B: PcmDuplexBackend> {
    session: DuplexSession<B>,
    actual: DuplexActual,
    mode: AlsaIoMode,
    /// graph 输出（container）声道数。
    containers: usize,
    /// 配置里的设备串（证据/JSON；不参与 RT）。
    device: String,
    capture_bytes: Vec<u8>,
    capture_planar: Vec<f32>,
    playback_planar: Vec<f32>,
    device_bytes: Vec<u8>,
    fmt: FormatConfig,
    dither: format::DitherState,
    fmt_stats: format::FormatStats,
    counters: AlsaSinkCounters,
    t21: FaultCounters,
    /// 进入 Muted 的原因码（RtStatus u32；EV_SAFETY_MUTE_ENTER.value_a）。
    mute_reason: u32,
    mute_event_pushed: bool,
    /// 下一 block 再上报的事件（一个 block 只推 1 个事件；恢复事件顺延）。
    pending_event: Option<SinkEvent>,
    /// 起播/恢复后需要预填 playback cushion（下一块写前执行一次）。
    prime_needed: bool,
    /// 本 epoch 起播预填是否已做过（显式 prime 幂等：重复调用不再灌 cushion）。
    primed_once: bool,
    /// planrt：[capture] tee 文件（None = 关闭）。host 播放同时把 capture period 设备字节
    /// 原样落盘（MOTU 20ch S24_3LE 交错），无需另外 arecord → 不与 host duplex 抢设备。
    tee_file: Option<std::fs::File>,
    /// planrt：tee 路径（证据/报告用）。
    tee_path: String,
    /// planrt：tee 已写字节数。
    tee_bytes: u64,
    /// planrt：tee 写失败次数（非 0 = 落盘不完整，报告如实给出）。
    tee_errors: u32,
}

impl<B: PcmDuplexBackend> PcmPlaybackSink<B> {
    /// 由已协商/启动的 session + actual 构造（启动期一次分配；校验通道/布局契约）。
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn started(
        config: &HostRunConfig,
        containers: usize,
        mode: AlsaIoMode,
        session: DuplexSession<B>,
        actual: DuplexActual,
    ) -> Result<Self, String> {
        let n_in = actual.hw.n_in as usize;
        let n_out = actual.hw.n_out as usize;
        let period = actual.hw.period_frames as usize;
        if containers == 0 || containers > n_out {
            return Err(format!(
                "graph container 声道数 {containers} 超出设备 playback {n_out}（ALSA 播放映射）"
            ));
        }
        if mode == AlsaIoMode::Loopback && n_in <= ALSA_LOOPBACK_CAPTURE_CHANNELS[1] {
            return Err(format!(
                "alsa.io_mode=loopback 需要 actual capture > ch{}（got {n_in}）",
                ALSA_LOOPBACK_CAPTURE_CHANNELS[1]
            ));
        }
        let fmt = FormatConfig {
            hw: actual.hw,
            dither: DitherCfg::Off,
        };
        format::validate(&fmt)
            .map_err(|status| format!("ALSA actual format 契约失败：{}", status.code()))?;
        let capture_bytes = vec![
            0u8;
            actual.period_bytes(StreamDirection::Capture).map_err(
                |error| format!("ALSA capture period bytes：{}", error.name())
            )?
        ];
        let device_bytes = vec![
            0u8;
            actual.period_bytes(StreamDirection::Playback).map_err(
                |error| format!("ALSA playback period bytes：{}", error.name())
            )?
        ];
        Ok(Self {
            session,
            actual,
            mode,
            containers,
            device: config.device.clone(),
            capture_bytes,
            capture_planar: vec![0.0f32; n_in * period],
            playback_planar: vec![0.0f32; n_out * period],
            device_bytes,
            fmt,
            dither: format::DitherState::for_config(&fmt),
            fmt_stats: format::FormatStats::default(),
            counters: AlsaSinkCounters::default(),
            t21: FaultCounters::new(),
            mute_reason: 0,
            mute_event_pushed: false,
            pending_event: None,
            prime_needed: false,
            primed_once: false,
            tee_file: if config.capture_tee.trim().is_empty() {
                None
            } else {
                Some(
                    std::fs::File::create(config.capture_tee.trim()).map_err(|err| {
                        format!("创建 [capture] tee 文件失败 {}: {err}", config.capture_tee)
                    })?,
                )
            },
            tee_path: config.capture_tee.trim().to_string(),
            tee_bytes: 0,
            tee_errors: 0,
        })
    }

    fn state_name(&self) -> String {
        format!("{:?}", self.session.state())
    }

    /// 一个 block 只推 1 个事件：已有 first 时把 second 留到下一 block。
    fn combine_event(
        &mut self,
        first: Option<SinkEvent>,
        second: Option<SinkEvent>,
    ) -> Option<SinkEvent> {
        match (first, second) {
            (Some(first), Some(second)) => {
                self.pending_event = Some(second);
                Some(first)
            }
            (Some(first), None) => Some(first),
            (None, Some(second)) => Some(second),
            (None, None) => None,
        }
    }

    /// 进入 Muted 时产生一次 EV_SAFETY_MUTE_ENTER（每次进入恰一次，不重复刷事件）。
    fn mute_event_once(&mut self) -> Option<SinkEvent> {
        if self.session.state() == DuplexState::Muted && !self.mute_event_pushed {
            self.mute_event_pushed = true;
            Some(self.safety_mute_event())
        } else {
            None
        }
    }

    /// EV_SAFETY_MUTE_ENTER（core-v1 §8.2 冻结表：severity=4；value_a=原因码、
    /// value_b=epoch_id；与 fault.rs supervisor 的 SEVERITY_FATAL/reason 口径一致）。
    fn safety_mute_event(&self) -> SinkEvent {
        SinkEvent {
            kind: EV_SAFETY_MUTE_ENTER,
            lane: LANE_CONTROL,
            severity: SEVERITY_FATAL,
            value_a: self.mute_reason_or_default(),
            value_b: self.session.epoch_id().min(u32::MAX as u64) as u32,
        }
    }

    /// 进入静音的原因码：优先记录 T17 最后错误；无错误记录时按设备丢失（407）保守标注。
    fn mute_reason_or_default(&self) -> u32 {
        if self.mute_reason != 0 {
            self.mute_reason
        } else {
            RtStatus::RT_E_DEVICE_LOST.code()
        }
    }
    /// 构造 SafetyMute 结果（原因码取已记录的进入原因；无记录时保守按设备丢失）。
    fn safety_mute_outcome(&self, event: Option<SinkEvent>) -> CaptureOutcome {
        CaptureOutcome::SafetyMute {
            event,
            reason: self.mute_reason_or_default(),
        }
    }

    /// 驱动 T17 恢复状态机直到 Recovered / Exhausted / Faulted。
    fn drive_recovery(&mut self) -> Result<Option<SinkEvent>, String> {
        let mut steps = 0u32;
        while self.session.state() == DuplexState::Recovering {
            if steps >= ALSA_RECOVERY_STEP_LIMIT {
                self.counters.faults = self.counters.faults.saturating_add(1);
                self.t21.record_fault_enter();
                return Err(format!(
                    "ALSA 恢复未收敛（steps={steps}, state={}）",
                    self.state_name()
                ));
            }
            steps += 1;
            match self.session.recovery_step() {
                RecoveryAction::Recovered { epoch_id } => {
                    self.counters.recoveries = self.counters.recoveries.saturating_add(1);
                    self.t21.record_block_done();
                    // F-v5-2：恢复 = Suspend/Prepare/Resume，playback sw 缓冲被清空；
                    // 不重新预填就回到「1 period 余量」→ 下一写后必然再 underrun。
                    self.prime_needed = true;
                    return Ok(Some(SinkEvent {
                        kind: EV_EPOCH_START,
                        lane: LANE_CONTROL,
                        severity: SEVERITY_INFO,
                        value_a: epoch_id.min(u32::MAX as u64) as u32,
                        value_b: self.counters.recoveries,
                    }));
                }
                RecoveryAction::Exhausted => {
                    // T17 恢复重试耗尽 → Muted（SafetyMute）：记录原因并计入 T21
                    // safety_mute_entries（F3），本事件即本 epoch 唯一一次进入事件（F2）。
                    self.mute_reason = self
                        .session
                        .last_error()
                        .map(|error| error.status().code())
                        .unwrap_or_else(|| RtStatus::RT_E_DEVICE_LOST.code());
                    self.mute_event_pushed = true;
                    self.t21.record_safety_mute_enter();
                    return Ok(Some(self.safety_mute_event()));
                }
                RecoveryAction::Faulted => {
                    self.counters.faults = self.counters.faults.saturating_add(1);
                    self.t21.record_fault_enter();
                    return Err(format!(
                        "ALSA 恢复进入 Faulted（state={}）",
                        self.state_name()
                    ));
                }
                RecoveryAction::Recovering { .. } => {}
                RecoveryAction::None => break,
            }
        }
        if self.session.state() == DuplexState::Faulted {
            self.counters.faults = self.counters.faults.saturating_add(1);
            self.t21.record_fault_enter();
            return Err(format!(
                "ALSA session Faulted（state={}）",
                self.state_name()
            ));
        }
        Ok(None)
    }

    /// 起播/恢复后预填 playback cushion（只在 Running 生效；其它状态推迟到可写时）。
    ///
    /// F-v5-2 根因（release-v5 板窗）：每个输出周期都 underrun（underruns == alsa.blocks-2），
    /// 但块循环本身≈实时（capture.blocks 与墙钟同步）、CPU 13.5%、主线程 schedstat wait≈0
    /// → 不是调度饥饿：playback 由首次 writei 自动启动（T17 D-07）且只写 1 period，
    /// sw 缓冲永远只有 1 period 余量；块循环任意 ε 抖动（T45 park 唤醒/格式化）就让设备
    /// 先跑空 → EPIPE → 恢复 prepare 清空缓冲 → 下一写再启动 → 再跑空，形成
    /// 写-恢复交替（main:alsa blocks ≈ 2:1，写入音频只占墙钟 ~44%）。
    /// 预填 = buffer_periods-2 个 period 静音（MOTU 1024/128 → 6 period = 16 ms 余量；
    /// 不加大 buffer/period，只是用满 T17 既有的 1024 帧 sw 缓冲）。启动/恢复各一次。
    fn prime_if_needed(&mut self) -> Result<(), String> {
        if !self.prime_needed || self.session.state() != DuplexState::Running {
            return Ok(());
        }
        let period_bytes = self
            .actual
            .period_bytes(StreamDirection::Playback)
            .map_err(|error| format!("ALSA playback period bytes：{}", error.name()))?;
        let written = self.session.prime_playback_silence().map_err(|error| {
            format!(
                "ALSA 起播/恢复预填失败：{}（state={}）",
                error.name(),
                self.state_name()
            )
        })?;
        self.prime_needed = false;
        self.primed_once = true;
        let periods = if period_bytes == 0 {
            0
        } else {
            (written / period_bytes) as u64
        };
        self.counters.prime_blocks = self.counters.prime_blocks.saturating_add(periods);
        self.counters.prime_bytes = self.counters.prime_bytes.saturating_add(written as u64);
        Ok(())
    }

    /// IO 错误 → xrun/fault 计数 + 事件 + 恢复状态机（不可恢复 = fail closed 返回错误）。
    fn handle_io_error(
        &mut self,
        error: AlsaError,
        playback: bool,
    ) -> Result<Option<SinkEvent>, String> {
        if !error.device_recoverable() {
            self.counters.faults = self.counters.faults.saturating_add(1);
            self.t21.record_fault_enter();
            return Err(format!(
                "ALSA 不可恢复错误：{}（state={}）",
                error.name(),
                self.state_name()
            ));
        }
        let is_xrun = error == AlsaError::Xrun;
        if is_xrun {
            self.counters.xruns = self.counters.xruns.saturating_add(1);
        }
        // F2 / core-v1 §8.2 冻结表：EV_INPUT_OVERRUN / EV_OUTPUT_UNDERRUN
        //   severity = 3（error）
        //   value_a = 自上次成功同类 IO 起连续同类错误数（consecutive）
        //   value_b = 设备缓冲容量（buffer_frames / period_frames 个 period；MOTU=8）
        // 设备 xrun 的“容量”按硬件缓冲可容纳的 period 数计（无 SPSC ring 时等价语义）。
        let capacity_periods =
            (self.actual.hw.buffer_frames / self.actual.hw.period_frames.max(1)).max(1);
        let (kind, lane, consecutive) = if playback {
            self.counters.underruns = self.counters.underruns.saturating_add(1);
            self.counters.consecutive_underruns =
                self.counters.consecutive_underruns.saturating_add(1);
            if is_xrun {
                self.t21.record_output_underrun();
            }
            (
                EV_OUTPUT_UNDERRUN,
                LANE_PLAYBACK,
                self.counters.consecutive_underruns,
            )
        } else {
            self.counters.overruns = self.counters.overruns.saturating_add(1);
            self.counters.consecutive_overruns =
                self.counters.consecutive_overruns.saturating_add(1);
            if is_xrun {
                self.t21.record_input_overrun();
            }
            (
                EV_INPUT_OVERRUN,
                LANE_CAPTURE,
                self.counters.consecutive_overruns,
            )
        };
        let event = SinkEvent {
            kind,
            lane,
            severity: SEVERITY_ERROR,
            value_a: consecutive,
            value_b: capacity_periods,
        };
        // session.io_* 已把状态置 Recovering（xrun/设备丢失）；驱动 Suspend→Prepare→Resume。
        // 本 block 先报 xrun；恢复成功/耗尽事件顺延到下一 block（一个 block 只推 1 事件）。
        if let Some(recovered) = self.drive_recovery()? {
            self.pending_event = Some(recovered);
        }
        Ok(Some(event))
    }
}

impl<B: PcmDuplexBackend> PlaybackSink for PcmPlaybackSink<B> {
    fn read_capture(&mut self) -> Result<CaptureOutcome, String> {
        let mut event = self.pending_event.take();
        // F1：非 Running 状态不能调 session.io_read（会返回 ERR_SWITCH_BUSY；旧实现把
        // 它判为不可恢复 → run loop exit 1）。按 core-v1 §8.2：SafetyMute 期间不处理新块。
        match self.session.state() {
            DuplexState::Running => {}
            DuplexState::Recovering => {
                // 恢复进行中：先推进到终态；成功则本块仍可正常读，耗尽则本块起 SafetyMute。
                let recovered = self.drive_recovery()?;
                event = self.combine_event(event, recovered);
                if self.session.state() != DuplexState::Running {
                    self.capture_planar.fill(0.0);
                    return Ok(self.safety_mute_outcome(event));
                }
            }
            DuplexState::Muted => {
                // SafetyMute：本块不读 capture、不送新输入、不跑 graph；只由 host 写静音。
                self.capture_planar.fill(0.0);
                let mute_event = self.mute_event_once();
                event = self.combine_event(event, mute_event);
                return Ok(self.safety_mute_outcome(event));
            }
            DuplexState::Faulted => {
                // 不可恢复 fault 仍 fail closed（§5.1：Faulted 只能重启/完整重新 admission）。
                return Err(format!(
                    "ALSA 会话 Faulted（不可恢复，fail closed；state={}）",
                    self.state_name()
                ));
            }
            other => {
                return Err(format!(
                    "ALSA capture 状态非法：{other:?}（block 循环期望 Running/Recovering/Muted/Faulted）"
                ));
            }
        }
        match self.session.io_read(&mut self.capture_bytes) {
            Ok(read) => {
                self.counters.consecutive_overruns = 0;
                let expected = self.capture_bytes.len();
                if read < expected {
                    self.counters.short_reads = self.counters.short_reads.saturating_add(1);
                    let tail = read.min(expected);
                    self.capture_bytes[tail..].fill(0);
                }
                // planrt：tee 在格式转换之前写**原始设备字节**（20ch S24_3LE 交错），
                // 使录制与 host 播放共用同一 capture 流（不需要 arecord 独占设备）。
                if self.tee_file.is_some() {
                    use std::io::Write as _;
                    let take = read.min(self.capture_bytes.len());
                    let result = {
                        let file = self.tee_file.as_mut().expect("tee_file checked");
                        file.write_all(&self.capture_bytes[..take])
                    };
                    match result {
                        Ok(()) => self.tee_bytes = self.tee_bytes.saturating_add(take as u64),
                        Err(_) => {
                            self.tee_errors = self.tee_errors.saturating_add(1);
                            self.tee_file = None;
                        }
                    }
                }
                if self.mode == AlsaIoMode::Loopback {
                    let stats = format::convert_capture(
                        &self.fmt,
                        &self.capture_bytes,
                        &mut self.capture_planar,
                    )
                    .map_err(|status| format!("convert_capture：{}", status.code()))?;
                    self.fmt_stats.merge(stats);
                    self.t21.record_nonfinite_in(stats.nonfinite_in);
                }
                Ok(CaptureOutcome::Captured(event))
            }
            Err(error) => {
                // F4：读失败（xrun/设备丢失）不得把上一 period 的旧样本当新输入重放；
                // 清零 capture_planar，本块 loopback 输入为静音（短读 tail 亦已清零）。
                self.capture_planar.fill(0.0);
                let result = self.handle_io_error(error, false)?;
                Ok(CaptureOutcome::Captured(self.combine_event(event, result)))
            }
        }
    }

    fn has_loopback_input(&self) -> bool {
        self.mode == AlsaIoMode::Loopback
    }

    fn fill_loopback_input(
        &self,
        dst: &mut [f32],
        stride: usize,
        samples: usize,
    ) -> Result<(), String> {
        if self.mode != AlsaIoMode::Loopback {
            return Ok(());
        }
        let period = self.actual.hw.period_frames as usize;
        let n_in = self.actual.hw.n_in as usize;
        let frames = samples.min(period);
        let ch0 = ALSA_LOOPBACK_CAPTURE_CHANNELS[0];
        let ch1 = ALSA_LOOPBACK_CAPTURE_CHANNELS[1];
        if ch1 >= n_in || dst.len() < stride + frames {
            return Err(format!(
                "loopback 输入缓冲/通道不匹配（ch {ch1} >= n_in {n_in} 或 dst 太小）"
            ));
        }
        dst[..frames].copy_from_slice(&self.capture_planar[ch0 * period..ch0 * period + frames]);
        dst[stride..stride + frames]
            .copy_from_slice(&self.capture_planar[ch1 * period..ch1 * period + frames]);
        Ok(())
    }

    fn write_block(
        &mut self,
        output: &[f32],
        stride: usize,
        frames: usize,
        mute: bool,
    ) -> Result<Option<SinkEvent>, String> {
        let period = self.actual.hw.period_frames as usize;
        let n_out = self.actual.hw.n_out as usize;
        let frames = frames.min(period);
        let mut event = self.pending_event.take();
        if self.session.state() == DuplexState::Recovering {
            let recovered = self.drive_recovery()?;
            event = self.combine_event(event, recovered);
        }
        // F-v5-2：起播/恢复后的第一块写之前把 playback 缓冲预填到 cushion。
        self.prime_if_needed()?;
        let mute_block = mute || self.session.mute_required();
        self.playback_planar.fill(0.0);
        if !mute_block {
            let mut channel = 0usize;
            while channel < self.containers {
                let src_start = channel * stride;
                let dst_start = ALSA_PLAYBACK_CHANNEL_BASE + channel;
                if src_start
                    .checked_add(frames)
                    .map(|end| end > output.len())
                    .unwrap_or(true)
                    || dst_start >= n_out
                {
                    return Err(format!(
                        "graph 输出/设备声道映射越界（ch {channel}, containers {}, n_out {n_out}）",
                        self.containers
                    ));
                }
                self.playback_planar[dst_start * period..dst_start * period + frames]
                    .copy_from_slice(&output[src_start..src_start + frames]);
                channel += 1;
            }
        } else {
            self.counters.silence_blocks = self.counters.silence_blocks.saturating_add(1);
        }
        let stats = format::convert_playback_stateful(
            &self.fmt,
            &mut self.dither,
            &self.playback_planar,
            &mut self.device_bytes,
        )
        .map_err(|status| format!("convert_playback：{}", status.code()))?;
        self.fmt_stats.merge(stats);
        self.t21
            .record_nonfinite_out(stats.nonfinite_out.saturating_add(stats.nonfinite_in));
        self.t21.record_clipped(stats.clipped);
        if self.session.state() == DuplexState::Faulted {
            self.counters.faults = self.counters.faults.saturating_add(1);
            self.t21.record_fault_enter();
            return Err(format!("ALSA 设备 Faulted（state={}）", self.state_name()));
        }
        let result = if self.session.state() == DuplexState::Running {
            self.session.io_write(&self.device_bytes)
        } else {
            // Recovering/Muted：T17 只允许静音写（保持设备时钟与 block 节奏）。
            self.session.write_silence()
        };
        match result {
            Ok(written) => {
                self.counters.consecutive_underruns = 0;
                self.counters.blocks = self.counters.blocks.saturating_add(1);
                self.counters.bytes_written =
                    self.counters.bytes_written.saturating_add(written as u64);
                if self.session.state() != DuplexState::Running {
                    // Recovering/Muted：只写预分配静音（保持设备时钟与 block 节奏）；
                    // Muted 首个静音块顺推本 epoch 唯一一次 EV_SAFETY_MUTE_ENTER。
                    self.counters.muted_blocks = self.counters.muted_blocks.saturating_add(1);
                    if event.is_none() {
                        event = self.mute_event_once();
                    }
                }
                Ok(event)
            }
            Err(error) => {
                let result = self.handle_io_error(error, true)?;
                Ok(self.combine_event(event, result))
            }
        }
    }

    fn prime_playback(&mut self) -> Result<(), String> {
        // 显式起播预填只做一次（幂等）；恢复路径由 drive_recovery 置 prime_needed。
        if !self.primed_once {
            self.prime_needed = true;
        }
        self.prime_if_needed()
    }

    fn stop(&mut self) -> Result<(), String> {
        if self.session.state() == DuplexState::Closed {
            self.session.backend_mut().close();
            return Ok(());
        }
        self.session.stop().map_err(|error| {
            format!(
                "ALSA stop/drain 失败：{}（state={}）",
                error.name(),
                self.state_name()
            )
        })
    }

    fn report(&self) -> String {
        let counters = &self.counters;
        let t21 = self.t21.snapshot();
        let actual = &self.actual;
        format!(
            "{{\"backend\":\"linux-alsa\",\"device\":\"{}\",\"owner\":\"host\",\"gate\":\"operator-config(backend=alsa)\",\"sample_format\":\"S24_3LE\",\"sample_rate\":{},\"period_frames\":{},\"buffer_frames\":{},\"capture_channels\":{},\"playback_channels\":{},\"hwparams_hash\":{},\"actual\":{{\"n_in\":{},\"n_out\":{},\"period_frames\":{},\"buffer_frames\":{},\"in_stride_bytes\":{},\"out_stride_bytes\":{},\"hash\":{}}},\"io_mode\":\"{}\",\"blocks\":{},\"bytes_written\":{},\"prime_blocks\":{},\"prime_bytes\":{},\"silence_blocks\":{},\"xruns\":{},\"underruns\":{},\"overruns\":{},\"recoveries\":{},\"muted_blocks\":{},\"faults\":{},\"short_reads\":{},\"format\":{{\"nonfinite_in\":{},\"nonfinite_out\":{},\"clipped\":{}}},\"t21\":{{\"input_overruns\":{},\"output_underruns\":{},\"fault_entries\":{},\"safety_mute_entries\":{}}},\"session_state\":\"{}\",\"tee\":{{\"path\":\"{}\",\"bytes\":{},\"errors\":{}}}}}",
            json_escape_text(&self.device),
            actual.hw.sample_rate,
            actual.hw.period_frames,
            actual.hw.buffer_frames,
            actual.hw.n_in,
            actual.hw.n_out,
            actual.hash,
            actual.hw.n_in,
            actual.hw.n_out,
            actual.hw.period_frames,
            actual.hw.buffer_frames,
            actual.hw.in_stride_bytes,
            actual.hw.out_stride_bytes,
            actual.hash,
            self.mode.name(),
            counters.blocks,
            counters.bytes_written,
            counters.prime_blocks,
            counters.prime_bytes,
            counters.silence_blocks,
            counters.xruns,
            counters.underruns,
            counters.overruns,
            counters.recoveries,
            counters.muted_blocks,
            counters.faults,
            counters.short_reads,
            self.fmt_stats.nonfinite_in,
            self.fmt_stats.nonfinite_out,
            self.fmt_stats.clipped,
            t21.input_overruns,
            t21.output_underruns,
            t21.fault_entries,
            t21.safety_mute_entries,
            self.state_name(),
            json_escape_text(&self.tee_path),
            self.tee_bytes,
            self.tee_errors
        )
    }
}

/// 硬件打开失败/参数不符时给操作者的定位提示（T17 错误码不变）。
#[allow(dead_code)]
fn alsa_error_hint(error: AlsaError) -> &'static str {
    match error {
        AlsaError::CardNotFound => "card 未找到：检查 /proc/asound/cards 与 alsa.device 稳定 id",
        AlsaError::CardAmbiguous => "card 不唯一：用稳定 card id 精确匹配",
        AlsaError::DeviceBusy => "设备被占用：确认旧栈/CueMix 未打开 hw 设备",
        AlsaError::HwOpenDenied => "硬件闸门拒绝（backend=alsa 应为显式操作者选择）",
        AlsaError::Unsupported | AlsaError::BackendUnavailable => {
            "格式/能力不支持（要求 S24_3LE 48k/96k）"
        }
        AlsaError::Rate => "采样率不支持（白名单 48000/96000；192k 禁用）",
        AlsaError::BlockSize => "period 不支持（64/128）",
        AlsaError::Channels => "通道不足（MOTU 实测 20 in / 22 out）",
        AlsaError::DeviceParams => "actual 参数不满足（period/buffer/rate/format）",
        AlsaError::StrideAlign => "stride 不可表达",
        AlsaError::BadLayout => "缓冲/长度契约失败",
        AlsaError::SwitchBusy => "设备状态机非法迁移",
        _ => "设备错误（T17 启动/恢复状态机）",
    }
}

// ---------------------------------------------------------------------------
// planrt：backend=fake + [sink] out —— 离线渲染 sink（plan 图真实输出落盘，不打开设备）
// ---------------------------------------------------------------------------

/// 离线渲染 sink：把每个 block 的 graph 输出（containers × block_size）按 raw 设备格式写盘。
///
/// 用途：板端 plan-arena 图（T66 L_G=4096 / 25 paths）当前 DSP 吞吐低于实时，无法真设备实时
/// 播放；用本 sink 让**同一份 plan、同一 4-worker 图**把 8ch 内容文件真实跑完并落盘，再用
/// aplay 做同路径声学复测（测量路径不再依赖 RT 但输出仍是真实 plan 图的输出）。
/// 不打开 /dev/snd，不产生任何出声；backend=fake 契约 JSON 不变。
struct FileSink {
    path: String,
    file: std::fs::File,
    containers: usize,
    period: usize,
    bytes_per_sample: usize,
    playback_planar: Vec<f32>,
    device: Vec<u8>,
    fmt: FormatConfig,
    dither: format::DitherState,
    fmt_stats: format::FormatStats,
    blocks: u64,
    frames: u64,
    bytes: u64,
}

impl FileSink {
    fn create(config: &HostRunConfig, containers: usize) -> Result<Self, String> {
        if containers == 0 || containers > khaen_core::rt::MAX_OUTPUT_CHANNELS {
            return Err(format!(
                "[sink] out: graph container 声道数 {containers} 超出 1..=MAX_OUTPUT_CHANNELS"
            ));
        }
        let format_name = config.sink_out_format.trim().to_ascii_lowercase();
        let sample_format_id = match format_name.as_str() {
            "s24_3le" | "s24" => sample_format::S24_3LE_INTERLEAVED,
            "f32le" | "f32" => sample_format::F32_INTERLEAVED,
            other => {
                return Err(format!(
                    "[sink] format {other} 只支持 s24_3le|f32le（离线渲染 raw 输出）"
                ))
            }
        };
        let period = config.block_size as usize;
        let bytes_per_sample = if sample_format_id == sample_format::S24_3LE_INTERLEAVED {
            3usize
        } else {
            4usize
        };
        let hw = TspiHwParamsV1 {
            abi_version: ABI_VERSION,
            sample_format: sample_format_id,
            n_in: 2,
            n_out: containers as u16,
            sample_rate: config.sample_rate,
            period_frames: period as u32,
            buffer_frames: (period.saturating_mul(8)) as u32,
            in_stride_bytes: (period.saturating_mul(2).saturating_mul(bytes_per_sample)) as u32,
            out_stride_bytes: (period
                .saturating_mul(containers)
                .saturating_mul(bytes_per_sample)) as u32,
            flags: 0,
            reserved: [0; 4],
        };
        let fmt = FormatConfig {
            hw,
            dither: DitherCfg::Off,
        };
        format::validate(&fmt)
            .map_err(|status| format!("[sink] out format 契约失败：{}", status.code()))?;
        let file = std::fs::File::create(&config.sink_out).map_err(|err| {
            format!(
                "创建 [sink] out 文件失败 {}: {err}（离线渲染输出无法落盘）",
                config.sink_out
            )
        })?;
        Ok(Self {
            path: config.sink_out.clone(),
            file,
            containers,
            period,
            bytes_per_sample,
            playback_planar: vec![0.0f32; containers * period],
            device: vec![0u8; period * containers * bytes_per_sample],
            fmt,
            dither: format::DitherState::for_config(&fmt),
            fmt_stats: format::FormatStats::default(),
            blocks: 0,
            frames: 0,
            bytes: 0,
        })
    }
}

impl PlaybackSink for FileSink {
    fn read_capture(&mut self) -> Result<CaptureOutcome, String> {
        // 离线渲染不读设备：host 走 synth 输入，再由 [capture] source=file 覆盖。
        Ok(CaptureOutcome::Captured(None))
    }

    fn has_loopback_input(&self) -> bool {
        false
    }

    fn fill_loopback_input(
        &self,
        _dst: &mut [f32],
        _stride: usize,
        _samples: usize,
    ) -> Result<(), String> {
        Ok(())
    }

    fn write_block(
        &mut self,
        output: &[f32],
        stride: usize,
        frames: usize,
        mute: bool,
    ) -> Result<Option<SinkEvent>, String> {
        let frames = frames.min(self.period);
        let mut channel = 0usize;
        while channel < self.containers {
            let src_start = channel * stride;
            let dst_start = channel * self.period;
            if src_start.saturating_add(frames) > output.len()
                || dst_start.saturating_add(frames) > self.playback_planar.len()
            {
                return Err(format!(
                    "[sink] out: graph 输出越界（ch {channel}, containers {}, frames {frames}）",
                    self.containers
                ));
            }
            if mute {
                self.playback_planar[dst_start..dst_start + frames].fill(0.0);
            } else {
                self.playback_planar[dst_start..dst_start + frames]
                    .copy_from_slice(&output[src_start..src_start + frames]);
            }
            channel += 1;
        }
        let stats = format::convert_playback_stateful(
            &self.fmt,
            &mut self.dither,
            &self.playback_planar,
            &mut self.device,
        )
        .map_err(|status| format!("[sink] out convert_playback：{}", status.code()))?;
        self.fmt_stats.merge(stats);
        use std::io::Write as _;
        self.file
            .write_all(&self.device)
            .map_err(|err| format!("[sink] out 写盘失败 {}: {err}", self.path))?;
        self.blocks = self.blocks.saturating_add(1);
        self.frames = self.frames.saturating_add(frames as u64);
        self.bytes = self.bytes.saturating_add(self.device.len() as u64);
        Ok(None)
    }

    fn prime_playback(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn stop(&mut self) -> Result<(), String> {
        use std::io::Write as _;
        self.file
            .flush()
            .map_err(|err| format!("[sink] out flush 失败 {}: {err}", self.path))
    }

    fn report(&self) -> String {
        format!(
            "{{\"backend\":\"file\",\"device\":\"{}\",\"owner\":\"host\",\"gate\":\"operator-config(fake+sink.out)\",\"sample_format\":\"{}\",\"sample_rate\":{},\"period_frames\":{},\"channels\":{},\"io_mode\":\"offline-render\",\"blocks\":{},\"frames\":{},\"bytes\":{},\"format\":{{\"nonfinite_in\":{},\"nonfinite_out\":{},\"clipped\":{}}}}}",
            json_escape_text(&self.path),
            if self.bytes_per_sample == 3 { "S24_3LE" } else { "F32_LE" },
            self.fmt.hw.sample_rate,
            self.period,
            self.containers,
            self.blocks,
            self.frames,
            self.bytes,
            self.fmt_stats.nonfinite_in,
            self.fmt_stats.nonfinite_out,
            self.fmt_stats.clipped
        )
    }
}

/// backend 选择：fake = 旧契约 JSON（不打开设备）；alsa = 打开真实 duplex 并返回 sink。
fn prepare_backend(
    config: &HostRunConfig,
    containers: usize,
) -> Result<(String, Option<Box<dyn PlaybackSink>>), String> {
    prepare_backend_with_sink(config, containers, None)
}

/// `prepare_backend` + 可选 sink 注入（**仅单测**：run-loop + mock ALSA sink 集成测试用；
/// 生产入口 `prepare_backend` 恒传 None）。注入时跳过 backend 打开（mock 不触碰 /dev/snd）。
fn prepare_backend_with_sink(
    config: &HostRunConfig,
    containers: usize,
    injected_sink: Option<Box<dyn PlaybackSink>>,
) -> Result<(String, Option<Box<dyn PlaybackSink>>), String> {
    if let Some(sink) = injected_sink {
        return Ok((String::new(), Some(sink)));
    }
    match config.backend.as_str() {
        "fake" => {
            let contract = fake_alsa_contract_json(config)?;
            if config.sink_out.trim().is_empty() {
                Ok((contract, None))
            } else {
                // planrt：离线渲染（不打开设备）。plan 图仍走真实 4-worker/all-ack 块循环，
                // 输出按 raw 设备格式落盘，供 aplay 做同路径声学复测。
                let sink = FileSink::create(config, containers)?;
                Ok((contract, Some(Box::new(sink))))
            }
        }
        "alsa" => {
            let mode = AlsaIoMode::parse(&config.alsa_io_mode)?;
            let sink = open_alsa_sink(config, containers, mode)?;
            Ok((String::new(), Some(Box::new(sink))))
        }
        other => Err(format!(
            "unsupported backend: {other}（allowed: fake|alsa）"
        )),
    }
}

/// 打开真实 Linux ALSA duplex 设备（启动期；非 RT），协商 S24_3LE/48k 并启动流。
#[cfg(target_os = "linux")]
fn open_alsa_sink(
    config: &HostRunConfig,
    containers: usize,
    mode: AlsaIoMode,
) -> Result<PcmPlaybackSink<HostAlsaBackend>, String> {
    use khaen_rt_host::alsa::{
        parse_card_entry, DeviceSelector, DuplexRequest, HwOpenGate, LinuxAlsaBackend,
        RecoveryPolicy, RequiredDeviceWidth,
    };
    let spec = parse_alsa_device(&config.device)?;
    let cards_text = std::fs::read_to_string("/proc/asound/cards").map_err(|err| {
        format!("read /proc/asound/cards: {err}（稳定 card id 解析需要；真实设备板端 NOT_RUN）")
    })?;
    let card_id = match &spec.card {
        AlsaCardKey::Id(id) => id.clone(),
        AlsaCardKey::Index(index) => {
            let mut found: Option<String> = None;
            for line in cards_text.lines() {
                if let Some(entry) = parse_card_entry(line) {
                    if entry.index == *index {
                        found = Some(entry.id.to_string());
                        break;
                    }
                }
            }
            found.ok_or_else(|| {
                format!("card index {index} 不在 /proc/asound/cards（拒绝猜测 hw:4,0）")
            })?
        }
    };
    let selector = DeviceSelector::by_id(&card_id, spec.device);
    let request = DuplexRequest {
        sample_rate: config.sample_rate,
        period_frames: config.period_frames,
        buffer_frames: config.buffer_frames,
        min_input_channels: config.capture_channels,
        min_output_channels: config.playback_channels,
        format_priority: DuplexRequest::DEFAULT_FORMAT_PRIORITY,
        allow_file_fake_formats: false,
        capture_sanitize: true,
    };
    // backend=alsa 是操作者的显式选择（fake 仍是默认）；S1 默认 Denied 语义由 fake 路径保留。
    let mut session = DuplexSession::new(
        LinuxAlsaBackend::new(),
        request,
        RecoveryPolicy::new(2, 3),
        HwOpenGate::allowed(),
    );
    let actual = session.start(&selector, &cards_text).map_err(|error| {
        format!(
            "ALSA open/negotiate/start 失败：{}（{}）",
            error.name(),
            alsa_error_hint(error)
        )
    })?;
    if actual.hw.period_frames != config.block_size as u32 {
        let _ = session.stop();
        return Err(format!(
            "actual period {} != block_size {}：需要重规划/新 epoch（本构建 NOT_WIRED）",
            actual.hw.period_frames, config.block_size
        ));
    }
    if mode == AlsaIoMode::Loopback {
        let width = RequiredDeviceWidth::new(
            ALSA_LOOPBACK_CAPTURE_CHANNELS[1] as u16 + 1,
            containers as u16,
            ALSA_LOOPBACK_CAPTURE_CHANNELS[1] as u16,
            containers.saturating_sub(1) as u16,
        );
        actual.validate_width(&width).map_err(|error| {
            format!(
                "loopback 设备宽度不足：{}（cap {} / pb {}）",
                error.name(),
                actual.hw.n_in,
                actual.hw.n_out
            )
        })?;
    }
    PcmPlaybackSink::started(config, containers, mode, session, actual)
}

/// 非 Linux target：真实 ALSA 后端未编译（target-specific 依赖），返回明确错误。
#[cfg(not(target_os = "linux"))]
fn open_alsa_sink(
    _config: &HostRunConfig,
    _containers: usize,
    _mode: AlsaIoMode,
) -> Result<PcmPlaybackSink<HostAlsaBackend>, String> {
    Err(format!(
        "backend=alsa 仅 Linux target 编译真实 PCM（本 target={}：alsa 依赖 target-specific）。本机用 fake/mock 单测；真实设备/板端 NOT_RUN",
        std::env::consts::OS
    ))
}

/// 把 sink 事件推进 host 事件环（T21 lane；seq 单调）。
fn push_sink_event(
    lanes: &mut EventLanes,
    seq: &mut u64,
    block_seq: u64,
    event: Option<SinkEvent>,
) {
    if let Some(event) = event {
        lanes.push(&event_record(
            *seq,
            block_seq,
            event.kind,
            event.lane,
            event.severity,
            event.value_a,
            event.value_b,
        ));
        *seq = seq.saturating_add(1);
    }
}

/// [NONRT/run-loop] 一个 block 的 ALSA capture 阶段。
///
/// - 正常：读一个 capture period 并把 graph 输入填好（loopback 取 Toslink ch12/13 或 synth），
///   返回 `None`；
/// - SafetyMute（Muted 等非 Running，F1）：core-v1 §8.2「不处理新块」——不读新块数据、
///   不送 graph 输入（清零），返回 `Some(原因码)`，由调用方写静音并计数；
/// - 不可恢复 fault：fail closed（先 stop/drain 再返回错误，exit 1 语义不变）。
fn alsa_capture_phase(
    sink: &mut dyn PlaybackSink,
    input: &mut [f32],
    stride: usize,
    block: usize,
    lanes: &mut EventLanes,
    event_seq: &mut u64,
    block_seq: u64,
) -> Result<Option<u32>, String> {
    match sink.read_capture() {
        Ok(outcome) => {
            let safety_mute_reason = outcome.safety_mute_reason();
            push_sink_event(lanes, event_seq, block_seq, outcome.event());
            if safety_mute_reason.is_none() {
                if sink.has_loopback_input() {
                    sink.fill_loopback_input(input, stride, block)?;
                } else {
                    synth_input_into(block_seq as usize, input, stride, block);
                }
            } else {
                // SafetyMute：不处理新块——清空输入，避免任何上一块残留进入图。
                input.fill(0.0);
            }
            Ok(safety_mute_reason)
        }
        Err(error) => Err(fail_closed_sink_error(
            sink,
            "ALSA capture 读取失败",
            &error,
        )),
    }
}

/// [NONRT/run-loop] 一个 block 的 ALSA playback 阶段（mute=true 时写预分配静音）。
fn alsa_write_phase(
    sink: &mut dyn PlaybackSink,
    output: &[f32],
    stride: usize,
    block: usize,
    mute: bool,
    lanes: &mut EventLanes,
    event_seq: &mut u64,
    block_seq: u64,
) -> Result<(), String> {
    match sink.write_block(output, stride, block, mute) {
        Ok(event) => {
            push_sink_event(lanes, event_seq, block_seq, event);
            Ok(())
        }
        Err(error) => Err(fail_closed_sink_error(sink, "ALSA 播放失败", &error)),
    }
}

/// fatal 路径（O3）：先尝试 drain/close 再返回错误；原错误为主，stop 失败附注不掩盖。
fn fail_closed_sink_error(sink: &mut dyn PlaybackSink, context: &str, error: &str) -> String {
    let report = sink.report();
    match sink.stop() {
        Ok(()) => format!("{context}：{error}；alsa={report}"),
        Err(stop_error) => format!("{context}：{error}；stop 亦失败：{stop_error}；alsa={report}"),
    }
}

/// 极简 HTTP 探测（run 启动时验证 Web 端口可达；只读 status line + body 前缀）。
fn http_probe(bind: &str, path: &str, timeout_ms: u64) -> Result<(u16, String), String> {
    use std::io::{Read, Write};
    let mut stream =
        std::net::TcpStream::connect(bind).map_err(|err| format!("connect {bind}: {err}"))?;
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(timeout_ms)));
    let request = format!("GET {path} HTTP/1.1\r\nHost: {bind}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("write: {err}"))?;
    let mut body = String::new();
    stream
        .read_to_string(&mut body)
        .map_err(|err| format!("read: {err}"))?;
    let status = body
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| format!("no status line: {}", body.lines().next().unwrap_or("")))?;
    let payload = match body.find("\r\n\r\n") {
        Some(index) => body[index + 4..].chars().take(80).collect::<String>(),
        None => String::new(),
    };
    Ok((status, payload))
}

/// 等待 TCP 端口可连接（spawn 的 daemon 就绪）。
fn wait_port(bind: &str, timeout_ms: u64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    while std::time::Instant::now() < deadline {
        if std::net::TcpStream::connect(bind).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

struct WebReport {
    mode: &'static str,
    bind: String,
    probe_status: Option<u16>,
    probe_body: String,
    daemon_pid: Option<u32>,
    child_terminated: bool,
    note: String,
}

/// Web 控制面接线：external（deploy 两 unit 契约，web 由 tspi-rt-daemon 提供）或
/// spawn（单进程开发/验证：host 拉起同目录 tspi-rt-daemon 子进程并探测端口）。
fn start_web(config: &HostRunConfig) -> Result<(Option<std::process::Child>, WebReport), String> {
    if config.daemon_config.is_empty() {
        let (status, body) = match http_probe(&config.web_bind, "/healthz", 300) {
            Ok((status, body)) => (Some(status), body),
            Err(_) => (None, String::new()),
        };
        return Ok((
            None,
            WebReport {
                mode: "external",
                bind: config.web_bind.clone(),
                probe_status: status,
                probe_body: body,
                daemon_pid: None,
                child_terminated: false,
                note: "web served by tspi-rt-daemon.service (deploy 两 unit 契约)".to_string(),
            },
        ));
    }
    let exe = std::env::current_exe().map_err(|err| format!("current_exe: {err}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "current_exe has no parent".to_string())?;
    let daemon = dir.join(if cfg!(windows) {
        "tspi-rt-daemon.exe"
    } else {
        "tspi-rt-daemon"
    });
    if !daemon.is_file() {
        return Err(format!(
            "web.mode=spawn 但同目录缺少 tspi-rt-daemon（{}）",
            daemon.display()
        ));
    }
    let mut child = std::process::Command::new(&daemon)
        .arg("serve")
        .arg("--config")
        .arg(&config.daemon_config)
        .arg("--exit-after-ms")
        .arg("8000")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| format!("spawn {}: {err}", daemon.display()))?;
    let pid = child.id();
    let ready = wait_port(&config.web_bind, 3000);
    let (status, body) = if ready {
        match http_probe(&config.web_bind, "/healthz", 500) {
            Ok((status, body)) => (Some(status), body),
            Err(err) => {
                let _ = child.kill();
                return Err(format!("web probe failed after spawn: {err}"));
            }
        }
    } else {
        let _ = child.kill();
        return Err(format!(
            "spawned daemon did not listen on {} within 3000ms",
            config.web_bind
        ));
    };
    Ok((
        Some(child),
        WebReport {
            mode: "spawn",
            bind: config.web_bind.clone(),
            probe_status: status,
            probe_body: body,
            daemon_pid: Some(pid),
            child_terminated: false,
            note: "single-process dev/verify mode (deploy uses daemon unit)".to_string(),
        },
    ))
}

fn event_record(
    seq: u64,
    block_seq: u64,
    kind: u16,
    lane: u8,
    severity: u8,
    value_a: u32,
    value_b: u32,
) -> khaen_core::abi::TspiEventV1 {
    khaen_core::abi::TspiEventV1 {
        seq,
        mono_ns: clock::mono_now_ns(),
        epoch_id: EPOCH_ID,
        block_seq,
        kind,
        lane,
        severity,
        value_a,
        value_b,
        reserved0: 0,
        payload: [0; 2],
    }
}

fn last_good_report(path: &str) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let mut hasher = Sha256::new();
            for byte in &bytes {
                hasher.update(&[*byte]);
            }
            format!(
                "{{\"present\":true,\"bytes\":{},\"sha256_prefix\":\"{}\"}}",
                bytes.len(),
                hasher.finish_hex_prefix()
            )
        }
        Err(_) => "{\"present\":false}".to_string(),
    }
}

// ---------------------------------------------------------------------------
// T203/T210：中置下混内容适配接入（rt-host 自持 content_map；capture 帧进图之前）
// ---------------------------------------------------------------------------
//
// 背景（task-200/task-203）：4.1.2 输出布局 7 个角色没有 C，5.1 内容的 C 声道在 plan 的
// A_map「选择/零填」处没有任何 role 可选中而被丢弃（core-v1 §1/§11.10；plan 运行期禁止
// 混音、A_map 每 role 至多一源且源唯一）。修复必须落在 plan 之前的**内容适配层**：capture
// 帧进图前把 C 以标准 −3 dB 预混进 FL/FR、C 源置零（防重复混），plan 的 A_map/graph/RT 图
// 语义完全不变。
//
// task-210（Cursor v1.1 N-10）：内容适配由 rt-host `content_map` 模块自持，rt-host 不再
// 依赖/调用 compat（S1 兼容层）；本段只做 host 入口接线（不复制映射表/系数）：
// - 布局/角色唯一来源 = T57 overlay layout-presets.yml（`content_map::LayoutPresets` 严格
//   读取，与 compat/control 消费同一 overlay 文件），host 不写第二份 role→channel 表；
// - 启动期（NONRT）由 `content_map::CenterDownmixPlan::build` 判定 Applied /
//   DisabledCenterPresent / DisabledNoCenterContent；未知布局、宽度不符、中置声明与布局
//   矛盾、D7 headroom 配置非法一律拒绝启动（不静默、不猜、不降级）；
// - 块循环内对预分配 planar 输入缓冲调用 apply_planar_f32_in_place（纯标量、零堆分配、
//   无 panic 分支），仅 !mute 块应用；有中置/无中置内容时逐位 passthrough；
// - 真实板端/ALSA 出声 NOT_RUN（Windows/CI 只 fake/file 端到端；证据
//   evidence/center-downmix-host/）。
//
// [content] 配置（input_layout + output_layout 都留空 = 不启用，既有 2.0/4.1.2 路径不变）：
//   layout_presets    = "/etc/tspi-audio/layout-presets.yml"（T57 overlay；启用时缺文件拒绝）
//   input_layout      = "h16-5.1"（内容输入布局；其通道数必须等于 plan.n_in，逐通道对应）
//   output_layout     = "4.1.2"（输出布局；决定中置是否存在；容器宽度必须等于 plan.n_out）
//   center            = "auto"（auto|present|absent；声明与布局矛盾时拒绝）
//   program_peak_dbfs = -18.0（D7 headroom 基准；预测峰值超 −6 dBFS 门时全局 trim）

/// [content] 段稳定错误码（host 侧；明细里带 content_map 稳定错误码 E_CHANNEL_MAP_*）。
const ERR_HOST_CONTENT_CONFIG: &str = "ERR_HOST_CONTENT_CONFIG";
const ERR_HOST_CONTENT_LAYOUT: &str = "ERR_HOST_CONTENT_LAYOUT";
const ERR_HOST_CONTENT_APPLY: &str = "ERR_HOST_CONTENT_APPLY";
/// T57 overlay layout 预设默认安装路径（板端真相；与 web config 同口径）。
const DEFAULT_LAYOUT_PRESETS_PATH: &str = "/etc/tspi-audio/layout-presets.yml";

/// [content] 是否声明了内容适配（两个布局任一非空；配置错误由构建期统一拒绝）。
fn content_config_declared(config: &HostRunConfig) -> bool {
    !config.content_input_layout.trim().is_empty()
        || !config.content_output_layout.trim().is_empty()
}

/// [content] center -> content_map CenterSpeakerState（只接受 auto|present|absent，不猜）。
fn parse_center_speaker_state(text: &str) -> Result<CenterSpeakerState, String> {
    match text.trim() {
        "auto" => Ok(CenterSpeakerState::Auto),
        "present" => Ok(CenterSpeakerState::Present),
        "absent" => Ok(CenterSpeakerState::Absent),
        other => Err(capture_error(
            ERR_HOST_CONTENT_CONFIG,
            format!("[content] center 必须是 auto|present|absent（got {other}）"),
        )),
    }
}

/// T57 输出布局的容器宽度（= max(outputs)+1；4.1.2 → 8，5.1 → 6；必须等于 plan.n_out）。
fn output_layout_container_channels(
    presets: &LayoutPresets,
    layout_id: &str,
) -> Result<usize, String> {
    let layout = presets.output_layout(layout_id).ok_or_else(|| {
        capture_error(
            ERR_HOST_CONTENT_LAYOUT,
            format!("输出布局 {layout_id:?} 不在 T57 overlay layout 预设中"),
        )
    })?;
    let max_output = layout
        .roles
        .iter()
        .flat_map(|role| role.outputs.iter().copied())
        .max();
    Ok(max_output.map(|value| value as usize + 1).unwrap_or(0))
}

/// 输入布局角色 → planar 通道必须完整覆盖 0..channels（file/plan 逐通道对应；
/// 非连续/缺通道 = 无法保证内容声道顺序，拒绝启动而不是静默错位）。
fn validate_source_channel_coverage(source: &CenterDownmixSource) -> Result<(), String> {
    let mut seen = vec![false; source.channels];
    for (role, channel) in &source.role_channels {
        if *channel >= source.channels {
            return Err(capture_error(
                ERR_HOST_CONTENT_LAYOUT,
                format!(
                    "输入布局 {} 角色 {role} 通道 {channel} 越界（channels={}）",
                    source.layout_id, source.channels
                ),
            ));
        }
        seen[*channel] = true;
    }
    if let Some(missing) = seen.iter().position(|covered| !*covered) {
        return Err(capture_error(
            ERR_HOST_CONTENT_LAYOUT,
            format!(
                "输入布局 {} 未覆盖 planar 通道 {missing}（逐通道对应无法保证，拒绝静默错位）",
                source.layout_id
            ),
        ));
    }
    Ok(())
}

/// 中置下混内容适配器（启动期构建；RT 只调用 apply_block，不分配）。
#[derive(Debug)]
struct ContentAdapter {
    plan: CenterDownmixPlan,
}

impl ContentAdapter {
    /// 由 T57 预设 + 显式布局 id 构建（不读配置；单测直接调用）。
    fn build(
        presets: &LayoutPresets,
        input_layout_id: &str,
        output_layout_id: &str,
        center_state: CenterSpeakerState,
        program_peak_dbfs: f64,
        expected_input_channels: usize,
        expected_output_channels: usize,
    ) -> Result<Self, String> {
        let source =
            CenterDownmixSource::from_layout(presets, input_layout_id).map_err(|error| {
                capture_error(
                    ERR_HOST_CONTENT_LAYOUT,
                    format!("输入布局 {input_layout_id:?} 解析失败：{error}"),
                )
            })?;
        let target =
            CenterDownmixTarget::from_layout(presets, output_layout_id).map_err(|error| {
                capture_error(
                    ERR_HOST_CONTENT_LAYOUT,
                    format!("输出布局 {output_layout_id:?} 解析失败：{error}"),
                )
            })?;
        validate_source_channel_coverage(&source)?;
        if source.channels != expected_input_channels {
            return Err(capture_error(
                ERR_HOST_CONTENT_LAYOUT,
                format!(
                    "输入布局 {} 通道数 {} != 图/plan 输入宽度 {expected_input_channels}（strict：不做隐式上/下混）",
                    source.layout_id, source.channels
                ),
            ));
        }
        let output_container = output_layout_container_channels(presets, output_layout_id)?;
        if output_container != expected_output_channels {
            return Err(capture_error(
                ERR_HOST_CONTENT_LAYOUT,
                format!(
                    "输出布局 {output_layout_id} 容器宽度 {output_container} != plan.n_out {expected_output_channels}（布局声明与 plan 不一致，拒绝启动）"
                ),
            ));
        }
        let config = CenterDownmixConfig::auto()
            .with_center_state(center_state)
            .with_program_peak_dbfs(program_peak_dbfs);
        let plan = CenterDownmixPlan::build(&source, &target, &config).map_err(|error| {
            capture_error(
                ERR_HOST_CONTENT_LAYOUT,
                format!("中置下混计划被拒（{}）：{error}", error.code_str()),
            )
        })?;
        Ok(Self { plan })
    }

    /// 由 [content] 配置构建；未配置 → Ok(None)（既有路径完全不变）。
    fn from_config(
        config: &HostRunConfig,
        expected_input_channels: usize,
        expected_output_channels: usize,
    ) -> Result<Option<Self>, String> {
        let input_layout = config.content_input_layout.trim();
        let output_layout = config.content_output_layout.trim();
        if input_layout.is_empty() && output_layout.is_empty() {
            return Ok(None);
        }
        if input_layout.is_empty() || output_layout.is_empty() {
            return Err(capture_error(
                ERR_HOST_CONTENT_CONFIG,
                "[content] input_layout 与 output_layout 必须同时配置（或都留空 = 不启用中置下混；拒绝半配置静默 passthrough）",
            ));
        }
        let center_state = parse_center_speaker_state(&config.content_center)?;
        let mut presets_path = config.content_layout_presets.trim();
        if presets_path.is_empty() {
            presets_path = DEFAULT_LAYOUT_PRESETS_PATH;
        }
        let presets =
            LayoutPresets::load_overlay_file(std::path::Path::new(presets_path)).map_err(
                |error| {
                    capture_error(
                        ERR_HOST_CONTENT_LAYOUT,
                        format!(
                            "读取 T57 overlay layout 预设失败 {presets_path}：{error}（启用 [content] 时缺文件/非法一律拒绝，不降级）"
                        ),
                    )
                },
            )?;
        Self::build(
            &presets,
            input_layout,
            output_layout,
            center_state,
            config.content_program_peak_dbfs,
            expected_input_channels,
            expected_output_channels,
        )
        .map(Some)
    }

    /// 稳定状态字符串（applied / disabled_center_present / disabled_no_center_content）。
    fn status_str(&self) -> &'static str {
        self.plan.status.as_str()
    }

    /// 内容输入布局 id（file capture 布局名/报告用）。
    fn input_layout_id(&self) -> &str {
        &self.plan.source.layout_id
    }

    /// 内容源域 planar 通道数（= 图/plan 输入宽度，构建期已校验一致）。
    fn source_channels(&self) -> usize {
        self.plan.source.channels
    }

    /// 是否启用下混（仅 Applied 改变样本）。
    fn is_applied(&self) -> bool {
        self.plan.is_applied()
    }

    /// [RT] 块循环：在 capture 帧进图前应用（passthrough 仅校验长度，样本逐位不变）。
    fn apply_block(&self, planar: &mut [f32], frames: usize) -> Result<(), String> {
        self.plan
            .apply_planar_f32_in_place(planar, frames)
            .map_err(|error| {
                capture_error(
                    ERR_HOST_CONTENT_APPLY,
                    format!("中置下混应用被拒（{}）：{error}", error.code_str()),
                )
            })
    }

    /// 证据/报告 JSON（状态/通道/系数/D7 headroom/应用块数；板端 NOT_RUN）。
    fn report_json(&self, applied_blocks: u64) -> String {
        let headroom = &self.plan.headroom;
        let optional = |value: Option<usize>| {
            value
                .map(|channel| channel.to_string())
                .unwrap_or_else(|| "null".to_string())
        };
        format!(
            concat!(
                "{{\"adapter\":\"rt_host::content_map\",\"configured\":true,\"status\":\"{}\",\"applied\":{},",
                "\"input_layout\":\"{}\",\"output_layout\":\"{}\",\"source_channels\":{},",
                "\"center_channel\":{},\"left_channel\":{},\"right_channel\":{},",
                "\"center_gain_db\":{:.3},\"center_gain_linear\":{:.6},",
                "\"headroom\":{{\"program_peak_dbfs\":{:.3},\"worst_case_added_db\":{:.3},",
                "\"predicted_peak_dbfs\":{:.3},\"peak_limit_dbfs\":{:.3},\"min_headroom_db\":{:.3},",
                "\"trim_db\":{:.3},\"predicted_peak_after_trim_dbfs\":{:.3},",
                "\"headroom_after_trim_db\":{:.3},\"within_policy\":{}}},",
                "\"applied_blocks\":{},\"plan_semantics\":\"unchanged\",\"board\":\"NOT_RUN\"}}"
            ),
            self.status_str(),
            self.is_applied(),
            json_escape_text(&self.plan.source.layout_id),
            json_escape_text(&self.plan.target.layout_id),
            self.plan.source.channels,
            optional(self.plan.center_channel),
            optional(self.plan.left_channel),
            optional(self.plan.right_channel),
            headroom.center_gain_db,
            headroom.center_gain_linear,
            headroom.program_peak_dbfs,
            headroom.worst_case_added_db,
            headroom.predicted_peak_dbfs,
            headroom.peak_limit_dbfs,
            headroom.min_headroom_db,
            headroom.trim_db,
            headroom.predicted_peak_after_trim_dbfs,
            headroom.headroom_after_trim_db,
            headroom.within_policy,
            applied_blocks
        )
    }
}

/// run 报告的 content 段（未配置时显式 disabled/passthrough，不冒充 applied）。
fn content_report_json(adapter: Option<&ContentAdapter>, applied_blocks: u64) -> String {
    match adapter {
        Some(adapter) => adapter.report_json(applied_blocks),
        None => concat!(
            "{\"adapter\":\"none\",\"configured\":false,\"status\":\"disabled\",",
            "\"note\":\"[content] 未配置中置下混内容适配（passthrough；plan 语义不变）\",",
            "\"applied_blocks\":0}"
        )
        .to_string(),
    }
}

// ---------------------------------------------------------------------------
// T185 / P1 播放链：host 加载 T66 plan/校准图（graph = "plan-arena"）
// ---------------------------------------------------------------------------
//
// 流程（NONRT 启动期，全部真实执行）：
//   读 plan.bin（可选 compile-report.json manifest 交叉校验）
//   -> header 结构/magic/ABI/rate/block/CRC-32C 校验
//   -> T20 PlanRegistry::admit（真实 parse_plan_arena：表排序/taps/系数区间/graph_id/admission）
//   -> 由 plan 表还原 host 直算图（factorized：G path 表 + P 对角表 + role 元数据，
//      逐项折叠为 DirectTopology/DirectCoeffsF32；不近似、不丢 PEQ/延迟/极性/增益）
//   -> T20 版本 apply（stage -> arm -> RT block 边界 commit_swap_at_block_boundary -> complete_swap）
//   -> 常驻 4 worker 块循环执行 plan 图 -> 每块真实 all-ack（note_block_all_ack）-> 播放。
//
// 坏 plan（CRC/结构/manifest sha256/graph_id 不符）-> 拒绝启动；配置了 plan_last_good 时
// 回退 last-good（fresh registry 重新准入，不冒充版本倒退 INV-L7），否则明确报错退出，
// 不静默降级到 stereo_identity。fused 形态 plan（P 已折入 F，无法无损拆成 A/G/P）明确拒绝。
// NOT_WIRED（不得声称）：fused plan、运行中 plan 热切（本模式单版本启动加载）、真实 ALSA
// 设备（由 backend=alsa 接线提供；板端验证 NOT_RUN）。

/// factorized plan 的 path 表标志位（core-v1 §11.10；与 T66 打包逐位一致）。
const PLAN_PATH_FLAG_DIAG_BASELINE: u8 = 1 << 0;
/// fused 形态标志：P 已折入 path 系数，本模式明确拒绝（不静默近似）。
const PLAN_PATH_FLAG_FUSED: u8 = 1 << 1;
/// role 表标志位（abi.rs TspiRoleV1.flags）。
const PLAN_ROLE_FLAG_ENABLED: u16 = 1 << 0;
const PLAN_ROLE_FLAG_ZERO_FILL: u16 = 1 << 1;

use khaen_core::direct::{DIRECT_MAX_CONTAINERS, DIRECT_MAX_SOURCES};
use khaen_core::plan::{PlanBuf, PlanToken};
use khaen_core::workers::TspiRtControlV1;
use khaen_rt_host::admission::MockMemory;
use khaen_rt_host::swap::{commit_swap_at_block_boundary, PlanRegistry, TspiSwapAckV1};

/// 读 plan header（128 B；repr(C) POD，无指针/Drop；read_unaligned 不要求对齐）。
fn read_plan_header(bytes: &[u8]) -> Result<TspiPlanHeaderV1, String> {
    if bytes.len() < 128 {
        return Err(format!(
            "plan 字节 {} < 128（header 不完整；不是 T66 compile/plan.bin）",
            bytes.len()
        ));
    }
    // SAFETY: TspiPlanHeaderV1 是 repr(C) 全整数字段 POD（无指针/无隐式 Drop）；调用方保证
    // 前 128 B 已初始化；read_unaligned 允许任意对齐；任意 bit pattern 对整数字段合法，随后
    // 由 verify_plan_header 做逐字段范围校验（magic/ABI/CRC 等）。
    Ok(unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const TspiPlanHeaderV1) })
}

/// plan header 结构/ABI/CRC-32C 校验（坏 plan 的第一个门；T20 parse 在其后再做表级校验）。
fn verify_plan_header(bytes: &[u8], header: &TspiPlanHeaderV1) -> Result<(), String> {
    if header.magic != TSPI_PLAN_MAGIC {
        return Err(format!(
            "plan header.magic={:#010x} != TSPI_PLAN_MAGIC={:#010x}（不是 plan.bin，拒绝启动）",
            header.magic, TSPI_PLAN_MAGIC
        ));
    }
    // GAP-T66-3：v1（无 role IIR）与 v2（per-role IIR/PEQ）都接受；其它版本明确拒绝。
    if !plan_abi_supported(header.abi_version) {
        return Err(format!(
            "plan header.abi_version={} 不被本 host 支持（只接受 plan ABI v1/v2；拒绝启动）",
            header.abi_version
        ));
    }
    if header.header_size != 128 {
        return Err(format!(
            "plan header.header_size={} != 128（ABI 不符，拒绝启动）",
            header.header_size
        ));
    }
    if header.precision != 0 {
        return Err(format!(
            "plan header.precision={}（v1 只允许 0=f32，拒绝启动）",
            header.precision
        ));
    }
    if header.reserved0 != 0 || header.reserved1 != 0 || header.reserved2 != 0 {
        return Err("plan header reserved 字段非 0（ABI/布局损坏，拒绝启动）".to_string());
    }
    if header.abi_version == TSPI_PLAN_ABI_V1 {
        if header.reserved3 != [0u32; 3] {
            return Err("plan ABI v1 header reserved3 非 0（ABI/布局损坏，拒绝启动）".to_string());
        }
    } else {
        // plan ABI v2：reserved3[0]=iir_table_offset、reserved3[1]=iir_entry_count、reserved3[2]=0。
        if header.reserved3[2] != 0 {
            return Err("plan ABI v2 header reserved3[2] 必须为 0（拒绝启动）".to_string());
        }
        if (header.reserved3[0] == 0) != (header.reserved3[1] == 0) {
            return Err(
                "plan ABI v2 IIR 表 offset/count 必须成对为 0 或非 0（拒绝启动）".to_string()
            );
        }
        if header.reserved3[1] > 16 {
            return Err(format!(
                "plan ABI v2 IIR 表项数 {} 超过 16（拒绝启动）",
                header.reserved3[1]
            ));
        }
        if header.reserved3[0] != 0 {
            let offset = header.reserved3[0] as usize;
            let count = header.reserved3[1] as usize;
            let end = offset
                .checked_add(PLAN_IIR_HEADER_BYTES)
                .and_then(|value| value.checked_add(count * PLAN_IIR_ENTRY_BYTES));
            match end {
                Some(end)
                    if offset >= 128
                        && offset % 8 == 0
                        && end <= bytes.len() =>
                {
                    let iir_header: khaen_core::abi::TspiPlanIirHeaderV2 =
                        unsafe { std::ptr::read_unaligned(bytes[offset..].as_ptr() as *const _) };
                    if iir_header.magic != khaen_core::abi::TSPI_PLAN_IIR_MAGIC
                        || iir_header.version != khaen_core::abi::TSPI_PLAN_IIR_VERSION
                        || iir_header.entry_bytes as usize != PLAN_IIR_ENTRY_BYTES
                        || iir_header.entry_count as usize != count
                        || iir_header.reserved0 != 0
                    {
                        return Err(
                            "plan ABI v2 IIR 表头 magic/version/entry 不符（拒绝启动）".to_string()
                        );
                    }
                }
                _ => {
                    return Err(format!(
                        "plan ABI v2 IIR 表区间 [{offset},) 越界/未 8B 对齐（count={count}，拒绝启动）"
                    ))
                }
            }
        }
    }
    if !khaen_core::rt::SAMPLE_RATES_HZ.contains(&header.sample_rate) {
        return Err(format!(
            "plan header.sample_rate={} 不在白名单 48000/96000（拒绝启动）",
            header.sample_rate
        ));
    }
    if !khaen_core::rt::BLOCK_SIZES.contains(&header.block_size) {
        return Err(format!(
            "plan header.block_size={} 不在 64/128（拒绝启动）",
            header.block_size
        ));
    }
    if header.workers != 4 && header.workers != 2 {
        return Err(format!(
            "plan header.workers={} 只允许 2/4（拒绝启动）",
            header.workers
        ));
    }
    if header.n_in == 0 || header.n_in as usize > khaen_core::rt::MAX_INPUT_CHANNELS {
        return Err(format!(
            "plan header.n_in={} 超出 1..={}（拒绝启动）",
            header.n_in,
            khaen_core::rt::MAX_INPUT_CHANNELS
        ));
    }
    if header.n_out == 0 || header.n_out as usize > khaen_core::rt::MAX_OUTPUT_CHANNELS {
        return Err(format!(
            "plan header.n_out={} 超出 1..={}（拒绝启动）",
            header.n_out,
            khaen_core::rt::MAX_OUTPUT_CHANNELS
        ));
    }
    if header.total_coeff_bytes as usize > bytes.len() {
        return Err(format!(
            "plan header.total_coeff_bytes={} 超过文件 {} B（拒绝启动）",
            header.total_coeff_bytes,
            bytes.len()
        ));
    }
    // header CRC-32C：本字段（offset 124，ABI v1.1.2 errata E1 定稿）置 0 后覆盖前 128 B。
    let mut crc_input = [0u8; 128];
    crc_input.copy_from_slice(&bytes[..128]);
    crc_input[124..128].fill(0);
    let crc = khaen_core::nodes::crc32c(&crc_input);
    if crc != header.header_crc32c {
        return Err(format!(
            "plan header_crc32c 校验失败：文件写 {:#010x}，按 header 计算 {:#010x}（plan 损坏/被篡改，拒绝启动）",
            header.header_crc32c, crc
        ));
    }
    Ok(())
}

/// 完整 SHA-256 hex（64 字符；T185 plan sha256/manifest 交叉校验用）。
fn sha256_hex_full(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex32(&hasher.finish_bytes())
}

/// 从文本 JSON 取第一个字符串字段值（只用于 T66 compile-report.json 的已知字段；
/// 不引入 serde，也不把 manifest 当契约源——缺失即拒绝）。
fn json_string_field(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut start = 0usize;
    while let Some(relative) = text[start..].find(&needle) {
        let after_key = start + relative + needle.len();
        start = after_key;
        let Some(colon_relative) = text[after_key..].find(':') else {
            continue;
        };
        let value = text[after_key + colon_relative + 1..].trim_start();
        if let Some(rest) = value.strip_prefix('"') {
            if let Some(end) = rest.find('"') {
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

/// plan manifest 路径解析：文件直接用；目录按 T66 artifact 布局找 compile/compile-report.json。
fn resolve_plan_manifest(path: &str) -> Result<Option<String>, String> {
    let path = path.trim();
    if path.is_empty() {
        return Ok(None);
    }
    let candidate = std::path::Path::new(path);
    if candidate.is_dir() {
        for relative in ["compile/compile-report.json", "compile-report.json"] {
            let joined = candidate.join(relative);
            if joined.is_file() {
                return Ok(Some(joined.to_string_lossy().into_owned()));
            }
        }
        return Err(format!(
            "plan_manifest 目录下找不到 compile/compile-report.json（T66 artifact 布局）：{path}"
        ));
    }
    Ok(Some(path.to_string()))
}

/// 从 plan arena 的系数区取一条 FIR（f32 LE；非有限值直接拒收，不做静默截尾）。
fn plan_coeffs_f32(
    blob: &[u8],
    offset_bytes: u64,
    len_bytes: u32,
    taps: u32,
    what: &str,
) -> Result<Vec<f32>, String> {
    let taps = taps as usize;
    let expected = taps
        .checked_mul(4)
        .ok_or_else(|| format!("{what}: taps*4 溢出"))?;
    if len_bytes as usize != expected {
        return Err(format!(
            "{what}: coeff_len_bytes={len_bytes} != taps*4={expected}（plan 表损坏）"
        ));
    }
    let start = offset_bytes as usize;
    let end = start
        .checked_add(expected)
        .ok_or_else(|| format!("{what}: 系数区间溢出"))?;
    if end > blob.len() {
        return Err(format!(
            "{what}: 系数区间 [{start},{end}) 超出系数区 {} B（plan 表损坏）",
            blob.len()
        ));
    }
    let mut coeffs = Vec::with_capacity(taps);
    for chunk in blob[start..end].chunks_exact(4) {
        let value = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        if !value.is_finite() {
            return Err(format!("{what}: 系数非有限（NaN/Inf），拒绝加载"));
        }
        coeffs.push(value);
    }
    Ok(coeffs)
}

/// T20 parse/admit + 版本 apply 成功后的 plan 上下文（registry 必须存活：ctl 的
/// current_plan_ptr 指向 registry 内的 arena）。
struct PlanArenaPrepared {
    header: TspiPlanHeaderV1,
    graph_id: [u8; 32],
    graph: Graph,
    version: u32,
    registry: PlanRegistry,
    swap: TspiSwapAckV1,
    ctl: TspiRtControlV1,
    admission_backend: &'static str,
    manifest_state: String,
    plan_sha256: String,
    plan_bytes_len: usize,
    adapter_json: String,
    path: String,
    apply_init_status: u32,
}

/// NSP 5.1.2 线序进 4.1.2 容器时，plan 只能是这 7 条直通（out, in）。
/// 中置线序 2 不在表里，由 content_map 折进前左/前右。
const NSP512_TO_412_PATHS: [(u16, u16); 7] = [
    (0, 0),
    (1, 1),
    (2, 4),
    (3, 5),
    (4, 6),
    (5, 7),
    (7, 3),
];

fn validate_nsp512_to_412_paths(paths: &[khaen_core::abi::TspiPathV1]) -> Result<(), String> {
    let mut got: Vec<(u16, u16)> = paths
        .iter()
        .map(|path| (path.out_index, path.in_index))
        .collect();
    got.sort();
    let mut expect = NSP512_TO_412_PATHS.to_vec();
    expect.sort();
    if got != expect {
        return Err(format!(
            "[content] nsp-5.1.2 → 4.1.2 的 plan path 必须是 out,in = {expect:?}（中置 in=2 不进 plan，由 content_map 下混）；实得 {got:?}。identity 对角表会把中置错位，拒绝启动"
        ));
    }
    Ok(())
}

/// 加载并校验一份 plan（读文件 -> header CRC -> manifest 交叉校验 -> T20 admit -> 版本 apply）。
fn prepare_plan_arena(
    config: &HostRunConfig,
    plan_path: &str,
    manifest_path: &str,
) -> Result<PlanArenaPrepared, String> {
    let plan_path = plan_path.trim();
    if plan_path.is_empty() {
        return Err(
            "graph=plan-arena 需要配置 [paths] plan = <T66 compile/plan.bin 路径>".to_string(),
        );
    }
    let bytes =
        std::fs::read(plan_path).map_err(|err| format!("读取 plan 失败 {plan_path}: {err}"))?;
    let header = read_plan_header(&bytes)?;
    verify_plan_header(&bytes, &header)?;
    if header.sample_rate != config.sample_rate {
        return Err(format!(
            "plan.sample_rate={} != config.sample_rate={}（plan 与 host/设备参数不一致，拒绝启动）",
            header.sample_rate, config.sample_rate
        ));
    }
    if header.block_size != config.block_size {
        return Err(format!(
            "plan.block_size={} != config.block_size={}（plan 与 host 块长不一致，拒绝启动）",
            header.block_size, config.block_size
        ));
    }
    if header.workers != config.workers {
        return Err(format!(
            "plan.workers={} != config.workers={}（plan 与 host worker profile 不一致，拒绝启动，避免 ack 标签错配）",
            header.workers, config.workers
        ));
    }
    if config.backend == "alsa" {
        if header.n_in > config.capture_channels {
            return Err(format!(
                "plan.n_in={} > alsa.capture_channels={}（设备采集宽度不足，拒绝启动）",
                header.n_in, config.capture_channels
            ));
        }
        if header.n_out > config.playback_channels {
            return Err(format!(
                "plan.n_out={} > alsa.playback_channels={}（设备播放宽度不足，拒绝启动）",
                header.n_out, config.playback_channels
            ));
        }
    }
    let plan_sha256 = sha256_hex_full(&bytes);
    let manifest_state = match resolve_plan_manifest(manifest_path)? {
        None => "none".to_string(),
        Some(manifest_file) => {
            let text = std::fs::read_to_string(&manifest_file)
                .map_err(|err| format!("读取 plan manifest 失败 {manifest_file}: {err}"))?;
            let expected_plan_sha = json_string_field(&text, "plan_sha256").ok_or_else(|| {
                format!("plan manifest 缺少 plan_sha256 字段（不是 T66 compile-report.json）：{manifest_file}")
            })?;
            if !expected_plan_sha.eq_ignore_ascii_case(&plan_sha256) {
                return Err(format!(
                    "plan sha256 不符：manifest plan_sha256={expected_plan_sha}，实际 {plan_sha256}（plan 与 compile-report 不匹配，拒绝启动）"
                ));
            }
            let expected_graph =
                json_string_field(&text, "host_graph_sha256").ok_or_else(|| {
                    format!(
                        "plan manifest 缺少 host_graph_sha256 字段（不是 T66 compile-report.json）：{manifest_file}"
                    )
                })?;
            let actual_graph = hex32(&header.graph_id);
            if !expected_graph.eq_ignore_ascii_case(&actual_graph) {
                return Err(format!(
                    "graph_id 不符：manifest host_graph_sha256={expected_graph}，plan header graph_id={actual_graph}（artifact 与 plan 不匹配，拒绝启动）"
                ));
            }
            "verified".to_string()
        }
    };
    let graph_id = header.graph_id;
    let plan_bytes_len = bytes.len();
    let token = PlanToken::new(PlanBuf::new(bytes), graph_id);
    let mut env = AdmissionEnv::new();
    env.sample_rate_hz = header.sample_rate;
    env.block_size = header.block_size;
    env.rt_workers = header.workers;
    env.rt_privilege_ok = true;
    env.clock_source_ok = true;
    env.cpu_set_ok = true;
    env.cpu_set_mask = 0x0f;
    env.allowed_cpu = [0, 1, 2, 3];
    env.actual_cpu = [0, 1, 2, 3];
    env.expected_workers = header.workers;
    env.sched_priority = [70, 70, 70, 70];
    let mut registry = PlanRegistry::new();
    let mut mock_memory = MockMemory::new();
    let admitted = if cfg!(target_os = "linux") && config.backend == "alsa" {
        // 生产路径：真实平台内存后端（Linux mlockall/RLIMIT/预触；非 Linux fail closed）。
        registry.admit(token, &env)
    } else {
        // fake/离线：注入 MockMemory（与 T66/T20 单测同口径），报告里显式标注 admission=mock。
        registry.admit_with(token, &env, &mut mock_memory)
    };
    let version = admitted.map_err(|status| {
        format!(
            "T20 parse/admit 拒绝 plan（status {}）：magic/ABI/表排序/taps/系数区间/graph_id/admission 任一不成立，拒绝启动",
            status.code()
        )
    })?;
    let admission_backend = if cfg!(target_os = "linux") && config.backend == "alsa" {
        "platform"
    } else {
        "mock"
    };
    let (graph, adapter_json) = {
        let view = registry
            .plan_view(version)
            .ok_or_else(|| "T20 admitted plan view 缺失（registry 内部不一致）".to_string())?;
        if config.content_input_layout.trim() == "nsp-5.1.2"
            && config.content_output_layout.trim() == "4.1.2"
        {
            validate_nsp512_to_412_paths(view.paths)?;
        }
        let iir = registry
            .iir_entries(version)
            .ok_or_else(|| "T20 admitted plan IIR 表缺失（registry 内部不一致）".to_string())?;
        graph_from_plan(&view, iir)?
    };
    // T20 版本 apply（§5.3）：admit -> stage -> arm（ctl pending+ARMED）-> RT block 边界
    // commit -> complete_swap（ctl 回 IDLE、current=该版本）。本模式单版本启动加载。
    let mut swap = TspiSwapAckV1::new();
    let ctl = TspiRtControlV1::new();
    let stage = registry
        .stage(version)
        .map_err(|status| format!("T20 stage 失败（status {}）", status.code()))?;
    let arm_status = registry.arm(stage, &ctl, &mut swap, EPOCH_ID);
    if !arm_status.is_ok() {
        return Err(format!("T20 arm 失败（status {}）", arm_status.code()));
    }
    let committed = commit_swap_at_block_boundary(&ctl)
        .map_err(|status| format!("T20 block 边界 commit 失败（status {}）", status.code()))?;
    if committed != version {
        return Err(format!(
            "T20 commit 版本不符：commit={committed} != admit={version}（拒绝启动）"
        ));
    }
    registry
        .complete_swap(&ctl, &swap)
        .map_err(|status| format!("T20 complete_swap 失败（status {}）", status.code()))?;
    if registry.current_version() != Some(version) {
        return Err(
            "T20 complete_swap 后 current_version 与 admit 版本不一致（拒绝启动）".to_string(),
        );
    }
    let arena_ptr = registry
        .arena_ptr(version)
        .ok_or_else(|| "T20 registry 缺少 arena_ptr（拒绝启动）".to_string())?;
    if ctl.current_plan_ptr.load(Ordering::Acquire) != arena_ptr {
        return Err("T20 apply 后 ctl.current_plan_ptr != arena_ptr（拒绝启动）".to_string());
    }
    Ok(PlanArenaPrepared {
        header,
        graph_id,
        graph,
        version,
        registry,
        swap,
        ctl,
        admission_backend,
        manifest_state,
        plan_sha256,
        plan_bytes_len,
        adapter_json,
        path: plan_path.to_string(),
        apply_init_status: arm_status.code(),
    })
}

/// factorized plan 表 -> host 直算图（T12 A/G/P 语义）：
/// - path 表 = G 路由（对角 path 含 delta_D baseline；offdiag 为 g cell）；
/// - diag 表 = P 对角 FIR；role 表的 delay/polarity/gain_q15 折入 P 系数（与 kernel 等价）；
/// - role.out_index = scatter；diag path 的 in_index = 该 role 的 A_map source。
///
/// 任一不能无损映射的形态（fused/P 已折入、PEQ、非单射 A_map/scatter、超容量）明确拒绝。
fn graph_from_plan(
    view: &PlanView<'_>,
    iir_entries: &[TspiRoleIirV2],
) -> Result<(Graph, String), String> {
    let header = view.header;
    let paths = view.paths;
    let diags = view.diags;
    let roles = view.roles;
    let blob = view.coeffs;
    let n_src = header.n_in as usize;
    let containers = header.n_out as usize;
    if n_src == 0 || n_src > DIRECT_MAX_SOURCES {
        return Err(format!(
            "plan.n_in={n_src} 超出 host 直算图源通道容量 1..={DIRECT_MAX_SOURCES}（拒绝加载）"
        ));
    }
    if containers == 0 || containers > DIRECT_MAX_CONTAINERS {
        return Err(format!(
            "plan.n_out={containers} 超出 host 直算图容器通道容量 1..={DIRECT_MAX_CONTAINERS}（拒绝加载）"
        ));
    }
    if roles.is_empty() || roles.len() > DIRECT_MAX_ROLES {
        return Err(format!(
            "plan role 表条数 {} 超出 host 直算图容量 1..={DIRECT_MAX_ROLES}（拒绝加载）",
            roles.len()
        ));
    }
    if paths.is_empty() {
        return Err(
            "plan path 表为空（没有可加载的校准路径；identity 请显式用 graph=stereo_identity）"
                .to_string(),
        );
    }
    if let Some(path) = paths
        .iter()
        .find(|path| path.flags & PLAN_PATH_FLAG_FUSED != 0)
    {
        return Err(format!(
            "plan path(out={}, in={}) 是 fused 形态（F=P*G 已折入 path 系数），无法无损拆成 A/G/P；fused plan 加载 NOT_WIRED，拒绝启动（不静默近似）",
            path.out_index, path.in_index
        ));
    }
    // ---- role 表：scatter / role 元数据（gain_q15、delay、极性、zero_fill） ----
    let mut role_by_container: [u16; DIRECT_MAX_CONTAINERS] = [u16::MAX; DIRECT_MAX_CONTAINERS];
    let mut scatter = [0u16; DIRECT_MAX_ROLES];
    let mut role_gain = [1.0f32; DIRECT_MAX_ROLES];
    let mut role_delay = [0u32; DIRECT_MAX_ROLES];
    let mut role_polarity = [1i8; DIRECT_MAX_ROLES];
    let mut role_zero_fill = [false; DIRECT_MAX_ROLES];
    for (index, role) in roles.iter().enumerate() {
        if role.role_id as usize != index {
            return Err(format!(
                "role 表第 {index} 项 role_id={} != 表序（host 依赖 role_id=0..N-1 顺序；plan 表不一致，拒绝加载）",
                role.role_id
            ));
        }
        if role.out_index as usize >= containers {
            return Err(format!(
                "role[{index}].out_index={} >= plan.n_out={containers}（scatter 越界，拒绝加载）",
                role.out_index
            ));
        }
        if role.peq_count != 0 && header.abi_version == TSPI_PLAN_ABI_V1 {
            return Err(format!(
                "role[{index}].peq_count={}：plan abi_version=1 无 role IIR 系数区（GAP-T66-3），host 不静默丢弃 PEQ，拒绝加载",
                role.peq_count
            ));
        }
        if role.polarity != 1 && role.polarity != -1 {
            return Err(format!(
                "role[{index}].polarity={} 只允许 1/-1（拒绝加载）",
                role.polarity
            ));
        }
        if role.gain_q15 < 0 {
            return Err(format!(
                "role[{index}].gain_q15={} 非法（Q15 线性增益必须 >=0，拒绝加载）",
                role.gain_q15
            ));
        }
        if role.flags & !(PLAN_ROLE_FLAG_ENABLED | PLAN_ROLE_FLAG_ZERO_FILL) != 0 {
            return Err(format!(
                "role[{index}].flags={:#06x} 含未定义位（拒绝加载）",
                role.flags
            ));
        }
        if role_by_container[role.out_index as usize] != u16::MAX {
            return Err(format!(
                "plan role scatter 非单射：container {} 被多个 role 映射（拒绝加载）",
                role.out_index
            ));
        }
        role_by_container[role.out_index as usize] = index as u16;
        scatter[index] = role.out_index;
        role_gain[index] = role.gain_q15 as f32 / 32767.0;
        role_delay[index] = role.delay_samples;
        role_polarity[index] = role.polarity;
        role_zero_fill[index] =
            role.flags & PLAN_ROLE_FLAG_ZERO_FILL != 0 || role.flags & PLAN_ROLE_FLAG_ENABLED == 0;
    }
    // ---- plan ABI v2 per-role IIR（PEQ）：解码/硬门/按容器级联（GAP-T66-3） ----
    let iir = build_role_iir(view, iir_entries, &role_by_container, containers)?;
    // ---- P 行来源：diag 表（每 out container 至多一条对角 FIR；fused plan 的 diag 表为空） ----
    let mut p_fir_by_role: Vec<Option<Vec<f32>>> = vec![None; roles.len()];
    for diag in diags {
        if diag.out_index as usize >= containers {
            return Err(format!(
                "plan diag 表 out_index={} >= plan.n_out={containers}（拒绝加载）",
                diag.out_index
            ));
        }
        let role_index = role_by_container[diag.out_index as usize];
        if role_index == u16::MAX {
            return Err(format!(
                "plan diag 表 out_index={} 不对应任何 role（plan 表不一致，拒绝加载）",
                diag.out_index
            ));
        }
        if diag.taps == 0 {
            return Err(format!(
                "plan diag 表 out_index={} taps=0（拒绝加载）",
                diag.out_index
            ));
        }
        if diag.delay_samples != 0 {
            return Err(format!(
                "plan diag 表 out_index={} delay_samples={} != 0（role 延迟/极性必须由 role 元数据承载，拒绝加载）",
                diag.out_index, diag.delay_samples
            ));
        }
        let what = format!("diag(out={})", diag.out_index);
        let coeffs = plan_coeffs_f32(
            blob,
            diag.coeff_offset_bytes,
            diag.coeff_len_bytes,
            diag.taps,
            &what,
        )?;
        if p_fir_by_role[role_index as usize].replace(coeffs).is_some() {
            return Err(format!(
                "plan diag 表 out_index={} 重复（plan 表不一致，拒绝加载）",
                diag.out_index
            ));
        }
    }
    // ---- path 表：diag（baseline+source）与 offdiag（g cell） ----
    let mut diag_by_role: Vec<Option<(u16, u32, Vec<f32>)>> = vec![None; roles.len()];
    let mut offdiag_raw: Vec<(usize, u16, Vec<f32>)> = Vec::new();
    for path in paths {
        let role_index = role_by_container[path.out_index as usize];
        if role_index == u16::MAX {
            return Err(format!(
                "plan path out_index={} 不对应任何 role（plan 表不一致，拒绝加载）",
                path.out_index
            ));
        }
        let role_index = role_index as usize;
        let what = format!("path(out={}, in={})", path.out_index, path.in_index);
        let coeffs = plan_coeffs_f32(
            blob,
            path.coeff_offset_bytes,
            path.coeff_len_bytes,
            path.taps,
            &what,
        )?;
        if path.flags & PLAN_PATH_FLAG_DIAG_BASELINE != 0 {
            if diag_by_role[role_index].is_some() {
                return Err(format!(
                    "role {role_index} 有重复 diag path（({}, {})；plan 表不一致，拒绝加载）",
                    path.out_index, path.in_index
                ));
            }
            diag_by_role[role_index] = Some((path.in_index, path.delay_samples, coeffs));
        } else {
            if path.delay_samples != 0 {
                return Err(format!(
                    "{what}.delay_samples={} != 0：host 直算图只支持 diag baseline D（offdiag 无 baseline），拒绝加载",
                    path.delay_samples
                ));
            }
            offdiag_raw.push((role_index, path.in_index, coeffs));
        }
    }
    // ---- src_of_role / baseline D：diag path 的 in_index 即该 role 的 A_map source ----
    let mut src_of_role = [DIRECT_NO_SOURCE; DIRECT_MAX_ROLES];
    let mut src_to_role = [u16::MAX; DIRECT_MAX_SOURCES];
    let mut d_samples: Option<u32> = None;
    for (role_index, entry) in diag_by_role.iter().enumerate() {
        let Some((source, delay, _)) = entry else {
            if role_zero_fill[role_index] {
                continue;
            }
            return Err(format!(
                "role {role_index}（container {}）没有 diag path（无法从 plan 恢复 A_map source；非 zero-fill role 必须带 baseline diag），拒绝加载",
                scatter[role_index]
            ));
        };
        let source = *source as usize;
        if source >= n_src {
            return Err(format!(
                "role {role_index} diag source={source} >= plan.n_in={n_src}（拒绝加载）"
            ));
        }
        if src_to_role[source] != u16::MAX {
            return Err(format!(
                "A_map 非单射：source {source} 被 role {} 与 {role_index} 同时选中（host 直算图不支持，拒绝加载）",
                src_to_role[source]
            ));
        }
        src_to_role[source] = role_index as u16;
        src_of_role[role_index] = source as u16;
        match d_samples {
            None => d_samples = Some(*delay),
            Some(existing) if existing == *delay => {}
            Some(existing) => {
                return Err(format!(
                    "diag path baseline D 不一致：{existing} vs {delay}（plan 表不一致，拒绝加载）"
                ))
            }
        }
    }
    let d_samples = d_samples.unwrap_or(0);
    let l_g = paths
        .iter()
        .map(|path| path.taps as usize)
        .max()
        .unwrap_or(1)
        .max(1);
    if d_samples as usize >= l_g {
        return Err(format!(
            "plan baseline D={d_samples} 不小于 G 支撑 L_G={l_g}（diag 系数不含 baseline；plan 表不一致，拒绝加载）"
        ));
    }
    // ---- G 单元（M x M x L_G）：diag cell = diag path - delta_D；offdiag cell = path ----
    let mut cells: Vec<((usize, usize), Vec<f32>)> = Vec::new();
    let mut participants_set: Vec<usize> = Vec::new();
    for (role_index, entry) in diag_by_role.iter().enumerate() {
        let Some((_source, _delay, coeffs)) = entry else {
            continue;
        };
        let mut cell = vec![0.0f32; l_g];
        let copy = coeffs.len().min(l_g);
        cell[..copy].copy_from_slice(&coeffs[..copy]);
        cell[d_samples as usize] -= 1.0;
        if cell.iter().any(|value| *value != 0.0) {
            if !participants_set.contains(&role_index) {
                participants_set.push(role_index);
            }
            cells.push(((role_index, role_index), cell));
        }
    }
    for (role_index, in_source, coeffs) in offdiag_raw {
        let source = in_source as usize;
        if source >= n_src {
            return Err(format!(
                "offdiag path 引用 source={source} >= plan.n_in={n_src}（拒绝加载）"
            ));
        }
        let in_role = src_to_role[source];
        if in_role == u16::MAX {
            return Err(format!(
                "offdiag path(out_container={}, source={source}) 的 source 没有对应 role（A_map 不完整，拒绝加载）",
                scatter[role_index]
            ));
        }
        let in_role = in_role as usize;
        if in_role == role_index {
            return Err(format!(
                "offdiag path 指向自身（role {role_index}，flags 未标 diag；plan 表不一致，拒绝加载）"
            ));
        }
        let mut cell = vec![0.0f32; l_g];
        let copy = coeffs.len().min(l_g);
        cell[..copy].copy_from_slice(&coeffs[..copy]);
        if cell.iter().any(|value| *value != 0.0) {
            if !participants_set.contains(&role_index) {
                participants_set.push(role_index);
            }
            if !participants_set.contains(&in_role) {
                participants_set.push(in_role);
            }
            if cells
                .iter()
                .any(|((cell_out, cell_in), _)| *cell_out == role_index && *cell_in == in_role)
            {
                return Err(format!(
                    "G cell ({role_index}, {in_role}) 重复（plan path 表不一致，拒绝加载）"
                ));
            }
            cells.push(((role_index, in_role), cell));
        }
    }
    participants_set.sort_unstable();
    let mut participants = participants_set;
    if participants.is_empty() {
        participants.push(0);
    }
    let m = participants.len();
    if m > DIRECT_MAX_PARTICIPANTS {
        return Err(format!(
            "plan 推断 participants={m} 超出 host 直算图容量 <= {DIRECT_MAX_PARTICIPANTS}（拒绝加载）"
        ));
    }
    let mut g = vec![0.0f32; m * m * l_g];
    for ((out_role, in_role), cell) in cells {
        let out_pos = participants
            .iter()
            .position(|role| *role == out_role)
            .ok_or_else(|| "participants 索引丢失（内部错误）".to_string())?;
        let in_pos = participants
            .iter()
            .position(|role| *role == in_role)
            .ok_or_else(|| "participants 索引丢失（内部错误）".to_string())?;
        let base = (out_pos * m + in_pos) * l_g;
        g[base..base + l_g].copy_from_slice(&cell);
    }
    // ---- P 行：diag 表 P FIR + role 的 delay/polarity/gain_q15 折入（kernel 等价） ----
    let mut p_rows: Vec<Vec<f32>> = Vec::with_capacity(roles.len());
    for role_index in 0..roles.len() {
        let p_fir = match &p_fir_by_role[role_index] {
            Some(coeffs) => coeffs.clone(),
            None => vec![1.0f32],
        };
        let delay = role_delay[role_index] as usize;
        let scale = role_gain[role_index] * role_polarity[role_index] as f32;
        let mut effective = vec![0.0f32; p_fir.len() + delay];
        for (index, value) in p_fir.iter().enumerate() {
            effective[index + delay] += value * scale;
        }
        p_rows.push(effective);
    }
    let l_p = p_rows.iter().map(|row| row.len()).max().unwrap_or(1).max(1);
    let mut p = vec![0.0f32; roles.len() * l_p];
    for (index, row) in p_rows.iter().enumerate() {
        p[index * l_p..index * l_p + row.len()].copy_from_slice(row);
    }
    // ---- 组 DirectTopology（A = delta，l_a=1；G/P 按 plan 表；D 为 baseline） ----
    let mut topo = blank_topo(
        n_src as u16,
        roles.len() as u16,
        containers as u16,
        d_samples,
        1,
        l_g as u16,
        l_p as u16,
        m as u16,
    );
    topo.src_of_role = src_of_role;
    topo.scatter = scatter;
    for (index, role) in participants.iter().enumerate() {
        topo.participants[index] = *role as u16;
    }
    let iir_role_count = iir.role_count();
    let iir_section_count = iir.section_count();
    let graph = Graph {
        topo,
        a: vec![1.0f32; n_src],
        g,
        p,
        iir,
    };
    // 拓扑容量门（引擎相关）：direct 引擎融合支撑上限 = DIRECT_MAX_TAPS(4096)；partitioned
    // 引擎可承载 extended profile 的 TAPS_EXTENDED_MAX(16384)（T08 UPOLS：Q=ceil(L_F/B) <=
    // MAX_PARTITIONS=256，B=64 时 16384 恰为上界）。生产 4096-tap P_post FIR 的融合长度
    // L_F = L_A + L_G + L_P - 2 = 4103 落在此区间——**加载门不能再用 direct 的 4096 把
    // partitioned 可承载的 plan 拒掉**（engine=partitioned 的 status 110 根因）。
    // 这里只做“至少 partitioned 可承载”的宽门；具体引擎门在 run_plan_arena_command 里
    // 按 effective engine 严格判定（direct 失败仍 fail closed，不静默降级）。
    let topology = graph
        .topo
        .validate_with_fused_taps(PARTITIONED_MAX_TAPS_EXTENDED);
    if !topology.is_ok() {
        return Err(format!(
            "plan -> host 直算图拓扑不兼容（status {}）：taps/D/通道/role/participant/scatter 超出 DIRECT_MAX_* 容量，拒绝加载",
            topology.code()
        ));
    }
    let participants_json = participants
        .iter()
        .map(|role| role.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let scatter_json = roles
        .iter()
        .map(|role| role.out_index.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let adapter_json = format!(
        "{{\"n_src\":{},\"n_role\":{},\"container_channels\":{},\"l_a\":1,\"l_g\":{},\"l_p\":{},\"d_samples\":{},\"m\":{},\"paths\":{},\"diags\":{},\"roles\":{},\"plan_abi\":{},\"role_iir_roles\":{},\"role_iir_sections\":{},\"participants\":[{}],\"scatter\":[{}]}}",
        n_src,
        roles.len(),
        containers,
        l_g,
        l_p,
        d_samples,
        m,
        paths.len(),
        diags.len(),
        roles.len(),
        header.abi_version,
        iir_role_count,
        iir_section_count,
        participants_json,
        scatter_json
    );
    Ok((graph, adapter_json))
}


/// plan ABI v2 per-role IIR(PEQ) 解码 + 硬门 + 每容器级联（GAP-T66-3）。
///
/// 硬门（失败即拒绝加载，不静默近似）：plan ABI 必须 v2（v1 无系数区）；每条表项必须命中
/// role 表、role.peq_count == section_count、order==2/flags/reserved 为 0；容器唯一；系数
/// 区间 64B 对齐且在系数 blob 内、长度 = section_count×5×4；全部系数有限且 biquad 稳定
/// （|a2|<1 且 |a1|<1+a2）；顺序即级联顺序（同一容器多段按表项顺序串联）。
fn build_role_iir(
    view: &PlanView<'_>,
    entries: &[TspiRoleIirV2],
    role_by_container: &[u16; DIRECT_MAX_CONTAINERS],
    containers: usize,
) -> Result<RoleIirCascade, String> {
    let mut cascade = RoleIirCascade::empty();
    if entries.is_empty() {
        if view.roles.iter().any(|role| role.peq_count != 0) {
            return Err(
                "plan role.peq_count>0 但缺少 ABI v2 IIR 表（PEQ 无法落地，拒绝加载）".to_string(),
            );
        }
        return Ok(cascade);
    }
    if view.header.abi_version != TSPI_PLAN_ABI_V2 {
        return Err(format!(
            "plan 带 {} 条 IIR 表项但 abi_version={}（只有 plan ABI v2 有 role IIR 区，拒绝加载）",
            entries.len(),
            view.header.abi_version
        ));
    }
    if entries.len() > DIRECT_MAX_ROLES {
        return Err(format!(
            "plan IIR 表项数 {} 超过 host 直算图 role 容量 {DIRECT_MAX_ROLES}（拒绝加载）",
            entries.len()
        ));
    }
    let blob = view.coeffs;
    let mut previous_role: Option<u16> = None;
    let mut covered = 0usize;
    for (entry_index, entry) in entries.iter().enumerate() {
        if entry.order != 2
            || entry.flags != 0
            || entry.reserved0 != 0
            || entry.reserved1 != 0
            || entry.reserved2 != 0
        {
            return Err(format!(
                "plan IIR 表项[{entry_index}]（role_id={}）order/flags/reserved 非法（只支持 order=2 biquad）",
                entry.role_id
            ));
        }
        if let Some(previous) = previous_role {
            if entry.role_id <= previous {
                return Err(format!(
                    "plan IIR 表 role_id 未严格升序（{previous} -> {}），拒绝加载",
                    entry.role_id
                ));
            }
        }
        previous_role = Some(entry.role_id);
        let role_index = view
            .roles
            .iter()
            .position(|role| role.role_id == entry.role_id)
            .ok_or_else(|| {
                format!(
                    "plan IIR 表项[{entry_index}] role_id={} 不在 role 表（拒绝加载）",
                    entry.role_id
                )
            })?;
        let role = &view.roles[role_index];
        if role.peq_count != entry.section_count {
            return Err(format!(
                "plan role[{role_index}].peq_count={} 与 IIR 表项 section_count={} 不一致（拒绝加载）",
                role.peq_count, entry.section_count
            ));
        }
        if entry.section_count == 0 || entry.section_count as usize > 32 {
            return Err(format!(
                "plan role[{role_index}] IIR section_count={} 越界（1..=32，拒绝加载）",
                entry.section_count
            ));
        }
        let container = role.out_index as usize;
        if container >= containers || container >= DIRECT_MAX_CONTAINERS {
            return Err(format!(
                "plan role[{role_index}].out_index={container} 超出 plan.n_out={containers}（IIR 无法落位，拒绝加载）"
            ));
        }
        if role_by_container[container] != role_index as u16 {
            return Err(format!(
                "plan role[{role_index}] IIR 容器 {container} 与 role scatter 不一致（拒绝加载）"
            ));
        }
        if cascade.counts[container] != 0 {
            return Err(format!(
                "plan 容器 {container} 重复 IIR 表项（拒绝加载）"
            ));
        }
        let expected_len = entry.section_count as usize * 4 * PLAN_IIR_SECTION_COEFFS;
        if entry.coeff_len_bytes as usize != expected_len {
            return Err(format!(
                "plan role[{role_index}] IIR coeff_len_bytes={} != section_count×5×4={expected_len}（拒绝加载）",
                entry.coeff_len_bytes
            ));
        }
        let start = entry.coeff_offset_bytes as usize;
        let end = start
            .checked_add(expected_len)
            .ok_or_else(|| "plan IIR 系数区间溢出（拒绝加载）".to_string())?;
        if start % 64 != 0 || end > blob.len() {
            return Err(format!(
                "plan role[{role_index}] IIR 系数区间 [{start},{end}) 越界/未 64B 对齐（拒绝加载）"
            ));
        }
        let mut section_index = 0usize;
        while section_index < entry.section_count as usize {
            let base = start + section_index * 4 * PLAN_IIR_SECTION_COEFFS;
            let mut coeffs = [0.0f32; PLAN_IIR_SECTION_COEFFS];
            let mut k = 0usize;
            while k < PLAN_IIR_SECTION_COEFFS {
                let at = base + k * 4;
                coeffs[k] = f32::from_le_bytes([blob[at], blob[at + 1], blob[at + 2], blob[at + 3]]);
                if !coeffs[k].is_finite() {
                    return Err(format!(
                        "plan role[{role_index}] IIR 系数非有限（NaN/Inf，段 {section_index}，拒绝加载）"
                    ));
                }
                k += 1;
            }
            let (b0, b1, b2, a1, a2) = (coeffs[0], coeffs[1], coeffs[2], coeffs[3], coeffs[4]);
            if !(a2.abs() < 1.0 && a1.abs() < 1.0 + a2) {
                return Err(format!(
                    "plan role[{role_index}] IIR 段 {section_index} 不稳定（|a2|<1 且 |a1|<1+a2 不成立：a1={a1}, a2={a2}），拒绝加载"
                ));
            }
            cascade.sections.push(RoleBiquad { b0, b1, b2, a1, a2 });
            section_index += 1;
        }
        cascade.starts[container] = (cascade.sections.len() - entry.section_count as usize) as u32;
        cascade.counts[container] = entry.section_count;
        covered += 1;
    }
    if covered != entries.len() {
        return Err("plan IIR 表项覆盖不一致（内部错误，拒绝加载）".to_string());
    }
    for (role_index, role) in view.roles.iter().enumerate() {
        if role.peq_count != 0 && cascade.counts[role.out_index as usize] == 0 {
            return Err(format!(
                "plan role[{role_index}].peq_count={} 缺 IIR 表项（拒绝加载）",
                role.peq_count
            ));
        }
    }
    Ok(cascade)
}

// ---------------------------------------------------------------------------
// task-246：plan-arena DSP 引擎之二 —— shared-FFT partitioned convolution（T08 UPOLS）
// ---------------------------------------------------------------------------

/// plan-arena partitioned 引擎的启动期上下文：固定 plan/state 缓冲 + 布局。
///
/// 语义与 direct 4-worker 路径**完全相同**（A/G/P 与 D 在 build_f32 内融合为 route FIR，
/// T03 SPEC §2.7）：每输入通道每块一次 FFT（X_hist 共享 b-p 历史），25 path 的频域乘加
/// 共享输入频谱，每输出角色一次 IFFT + overlap-save。与 direct 的差别只是 f32 舍入路径
/// （FFT 旋转因子），T08 冻结判据 rel L2 <= 1e-5（证据：本任务 render-verify 对比）。
///
/// [NONRT] 构建（plan_data/state 一次分配）；[RT] run_block 只借用固定缓冲，不分配/不锁。
struct PartitionedRuntime {
    layout: PartitionedLayout,
    plan: PartitionedPlan,
    plan_data: Vec<f32>,
    state: Vec<f32>,
}

impl PartitionedRuntime {
    /// [NONRT] 由已校验 plan 图构建 plan_data（twiddle + 每路由每分区频谱）与 state。
    fn build(graph: &Graph, block: usize) -> Result<Self, String> {
        // partitioned 引擎 extended 口径：4096-tap P_post FIR 的融合 L_F=4103 可承载
        // （direct 仍受 DIRECT_MAX_TAPS=4096 约束；选择引擎在加载门之后按 effective 判定）。
        let layout = PartitionedLayout::build_extended(&graph.topo, block).map_err(|status| {
            format!("PartitionedLayout::build 失败（status {}）", status.code())
        })?;
        let mut plan_data = vec![0.0f32; layout.plan_samples()];
        let mut build_scratch = vec![0.0f32; layout.build_scratch_samples()];
        let plan = PartitionedPlan::build_f32_extended(
            &graph.topo,
            block,
            &graph.coeffs(),
            &mut plan_data,
            &mut build_scratch,
        )
        .map_err(|status| {
            format!(
                "PartitionedPlan::build_f32_extended 失败（status {}）",
                status.code()
            )
        })?;
        drop(build_scratch);
        let state = vec![0.0f32; layout.state_samples()];
        Ok(Self {
            layout,
            plan,
            plan_data,
            state,
        })
    }

    /// [RT] 执行一个 block（生产路径 full B；流末 partial 由 T08 DRAIN 语义处理）。
    fn run_block(
        &mut self,
        graph: &Graph,
        input: &[f32],
        in_stride: usize,
        output: &mut [f32],
        out_stride: usize,
        block: usize,
        valid_frames: usize,
    ) -> Result<(), String> {
        let status = partitioned_block_f32(
            &graph.topo,
            &self.plan,
            &self.plan_data,
            &mut self.state,
            input,
            in_stride,
            output,
            out_stride,
            block,
            valid_frames,
        );
        if status.is_ok() {
            Ok(())
        } else {
            Err(format!("partitioned kernel 返回 status {}", status.code()))
        }
    }
}

/// 引擎选择结果（启动期一次；显式 fail closed 或 auto 回退都在 reason 里如实记录）。
struct EngineChoice {
    /// 配置请求值（direct|partitioned|auto）。
    requested: String,
    /// 实际生效引擎（direct|partitioned）。
    effective: &'static str,
    /// 选择/回退原因（进 plan JSON）。
    reason: String,
    /// partitioned 运行时（effective=partitioned 时存在）。
    partitioned: Option<PartitionedRuntime>,
}

impl EngineChoice {
    fn is_partitioned(&self) -> bool {
        self.effective == "partitioned"
    }
}

/// [NONRT] 由配置 + plan 图选择 DSP 引擎。
///
/// - direct：T09 4-worker B2 直算（f32 逐位口径，原行为）；
/// - partitioned：T08 共享 FFT UPOLS 单线程内核（G-4096 的实时余量来源）；
/// - auto：plan 融合支撑 L_F >= 2048 时选 partitioned（G-2560/G-4096），否则 direct
///   （identity/2x2/测试图保持逐位口径）。partitioned 构建失败时 auto 回退 direct 并
///   warning；显式 engine=partitioned 则 fail closed（不静默降级）。
fn select_plan_engine(config: &HostRunConfig, graph: &Graph) -> Result<EngineChoice, String> {
    let l_f = (graph.topo.l_a as usize + graph.topo.l_g as usize + graph.topo.l_p as usize)
        .saturating_sub(2);
    let requested = config.dsp_engine.clone();
    let use_partitioned = match requested.as_str() {
        "partitioned" => true,
        "direct" => false,
        _ => l_f >= 2048,
    };
    if !use_partitioned {
        let reason = if requested == "direct" {
            "requested=direct".to_string()
        } else {
            format!("auto: l_f={l_f} < 2048")
        };
        return Ok(EngineChoice {
            requested,
            effective: "direct",
            reason,
            partitioned: None,
        });
    }
    match PartitionedRuntime::build(graph, config.block_size as usize) {
        Ok(runtime) => {
            let reason = if requested == "auto" {
                format!("auto: l_f={l_f} >= 2048")
            } else {
                format!("requested=partitioned l_f={l_f}")
            };
            Ok(EngineChoice {
                requested,
                effective: "partitioned",
                reason,
                partitioned: Some(runtime),
            })
        }
        Err(error) => {
            if requested == "partitioned" {
                Err(format!(
                    "[dsp] engine=partitioned 启动被拒绝（fail closed，不静默降级）：{error}"
                ))
            } else {
                Ok(EngineChoice {
                    requested,
                    effective: "direct",
                    reason: format!("auto 回退 direct：partitioned 构建失败：{error}"),
                    partitioned: None,
                })
            }
        }
    }
}

/// task-253：装载并校验 12 kHz MIMO 修正支路（仅在配置 [mimo12k] artifact 时启用）。
///
/// host 侧拓扑：plan 图只承担直通支路（输出 = feed_j 延迟 Δsys），本模块从图输入取
/// participants 馈送、跑 12k 修正支路并加到对应容器输出。因此要求：
/// - plan 是直通形态：L_A=L_P=1、G=identity（g 全零）、P=identity（p 全 1）、baseline D=Δsys；
/// - plan role 表顺序 == 产物 participants 顺序（本仓 pack 脚本保证；host 只校验数量）。
/// 任一不满足 -> fail closed（不静默近似），因为带 G/P FIR 的 plan 没有暴露 G_modal 后的中间
/// 缓冲，无法把 12k 修正量插到 P_post 之前（属 plan ABI 扩展范围，见证据 NOT-RUN）。
fn prepare_mimo12k_branch(
    config: &HostRunConfig,
    graph: &Graph,
    block: usize,
) -> Result<Option<khaen_rt_host::mimo12k::Mimo12kBranch>, String> {
    let path = config.mimo12k_artifact.trim();
    if path.is_empty() {
        return Ok(None);
    }
    let topo = graph.topo;
    if topo.l_a != 1 || topo.l_p != 1 {
        return Err(format!(
            "mimo12k 接线要求直通 plan 的 L_A=L_P=1（实际 L_A={} L_P={}）：带 A/P FIR 的 plan 需 plan ABI 扩展，fail closed",
            topo.l_a, topo.l_p
        ));
    }
    if graph.g.iter().any(|value| *value != 0.0) {
        return Err(
            "mimo12k 接线要求 plan G=identity（g 全零）：本 plan 已含 G 系数，会与 12k 修正支路重复，fail closed"
                .to_string(),
        );
    }
    if graph.p.iter().any(|value| *value != 1.0) {
        return Err(
            "mimo12k 接线要求 P_post=identity（本 plan 含 target FIR）；P_post 在 G_modal 之后，host 图未暴露中间缓冲，fail closed"
                .to_string(),
        );
    }
    let artifact = khaen_rt_host::mimo12k::load_artifact(std::path::Path::new(path))?;
    let m = artifact.params.participants.len();
    if m != topo.n_role as usize {
        return Err(format!(
            "mimo12k participants={m} != plan roles={}（role 顺序必须等于产物 participants 顺序）",
            topo.n_role
        ));
    }
    if topo.d_samples != artifact.params.dsys_samples {
        return Err(format!(
            "mimo12k Δsys 不一致：plan baseline D={} != artifact.dsys_samples={}（直通支路必须延迟同一 Δsys）",
            topo.d_samples, artifact.params.dsys_samples
        ));
    }
    let mut mapping = Vec::with_capacity(m);
    for role in 0..m {
        let src = topo.src_of_role[role];
        if src == DIRECT_NO_SOURCE {
            return Err(format!(
                "mimo12k plan role {role} 无 A_map source（直通 plan 不完整，fail closed）"
            ));
        }
        if src as usize >= topo.n_src as usize {
            return Err(format!(
                "mimo12k plan role {role} source={src} >= n_src={}（fail closed）",
                topo.n_src
            ));
        }
        mapping.push((role, src as usize, topo.scatter[role] as usize));
    }
    let branch = khaen_rt_host::mimo12k::Mimo12kBranch::prepare(artifact, mapping, block)?;
    Ok(Some(branch))
}

/// T185：graph="plan-arena" 主实现 —— 加载 T66 plan/校准图并用真实 4 worker 块循环播放。
///
/// 与既有 run 的 fake/alsa 契约保持同一 JSON 骨架（status/mode/schema/swap/fault/scheduler/
/// alsa/web/last_good/warnings），额外给出 plan 段（来源/字节 sha256/graph_id/T20 版本/
/// admission 后端/回退来源/adapter 拓扑）。真实 ALSA 由 prepare_backend 的 backend=alsa
/// 接线提供（rt-alsa3）；本函数不打开设备、不新增线程模式、不合成 ack。
fn run_plan_arena_command(
    config: &HostRunConfig,
    config_path: &str,
    max_blocks: u64,
    duration_ms: u64,
    mut warnings: Vec<String>,
) -> Result<String, String> {
    use std::fmt::Write as _;
    use khaen_core::events::{
        EV_DEADLINE_MISS, EV_RUNNING, EV_SAFETY_MUTE_ENTER, EV_STARTING, EV_STOPPED, LANE_CONTROL,
        LANE_WATCHDOG, SEVERITY_INFO, SEVERITY_WARNING,
    };
    use khaen_core::rt::gate_ns_floor;
    use khaen_rt_host::fault::{EventLaneReader, EventLanes, Watchdog, WatchdogAction};
    use khaen_rt_host::scheduler::SchedGate;

    validate_backend_config(config)?;
    // 1) plan 加载 + T20 parse/admit/apply（primary；失败且配置 last-good 时回退）。
    let (mut prepared, plan_source, rollback) = match prepare_plan_arena(
        config,
        &config.plan,
        &config.plan_manifest,
    ) {
        Ok(prepared) => (prepared, "primary", "none".to_string()),
        Err(primary_error) => {
            if config.plan_last_good.trim().is_empty() {
                return Err(format!(
                        "plan-arena 启动被拒绝：{primary_error}（未配置 [paths] plan_last_good，无 last-good 可回退；不静默降级）"
                    ));
            }
            let fallback = prepare_plan_arena(config, &config.plan_last_good, "").map_err(
                    |last_good_error| {
                        format!(
                            "plan-arena 启动被拒绝：{primary_error}；last-good 也不可用：{last_good_error}"
                        )
                    },
                )?;
            warnings.push(format!(
                    "primary plan 被拒绝（{primary_error}）；已回退 last-good（fresh registry 重新准入，plan_version 从 1 起）"
                ));
            (
                fallback,
                "last_good",
                "rejected_primary_readmit_last_good".to_string(),
            )
        }
    };
    let plan_version = prepared.version;
    let expected_workers = prepared.header.workers;
    let containers = prepared.header.n_out as usize;
    let graph = &prepared.graph;
    // T203：中置下混内容适配（启动期构建；capture 帧进图之前应用；plan 语义不变）。
    // 未配置 [content] → None（既有路径完全不变）；配置非法/布局与 plan 不一致 → 拒绝启动。
    let content_adapter = ContentAdapter::from_config(
        config,
        prepared.header.n_in as usize,
        prepared.header.n_out as usize,
    )?;
    // T74 file-input：plan 图输入宽度（n_in）决定 strict 布局（2=2.0 / 6=5.1 / 8=4.1.2）；
    // 6ch 5.1 仅在 [content] 内容布局声明时启用。
    let mut file_input = prepare_capture_source(
        config,
        prepared.header.n_in as usize,
        content_adapter.as_ref(),
    )?;
    // T74 前置 + T239：capture 启动计划（T43 AoIP 端点或 userspace AES67；纯解析 fail fast，
    // 设备/socket 打开在其后 F5）。
    let capture_plan = plan_capture_input(config, prepared.header.n_in as usize)?;
    let coeffs = graph.coeffs();
    // task-253：可选 MIMO 12 kHz 低频修正支路（配置 [mimo12k] artifact 才启用；默认路径不变）。
    // 启动期一次装载/校验（失败 fail closed），块循环里零分配运行。
    let mut mimo12k = prepare_mimo12k_branch(config, graph, config.block_size as usize)?;
    let mut mimo12k_blocks = 0u64;
    // task-246：启动期一次决定 DSP 引擎（direct 4-worker B2 | partitioned 共享 FFT）。
    // auto 回退/选择原因如实进 plan JSON（不静默改变语义：同一 plan 的 A/G/P 与 D 不变）。
    let mut engine = select_plan_engine(config, graph)?;
    let use_partitioned = engine.is_partitioned();
    if let Some(warning) = engine.reason.strip_prefix("auto 回退 direct：") {
        warnings.push(format!(
            "[dsp] engine=auto 未启用 partitioned，回退 direct：{warning}"
        ));
    }
    if engine.requested != engine.effective && engine.requested != "auto" {
        warnings.push(format!(
            "[dsp] engine={} 实际生效 {}（{}）",
            engine.requested, engine.effective, engine.reason
        ));
    }
    // 引擎相关拓扑容量门（选择之后按 effective engine 判定）：
    // - direct 需要 DirectLayout（逐位直算 arena/历史缓冲；冻结上限 L_F <= DIRECT_MAX_TAPS=4096），
    //   超限仍 fail closed（保留旧文案/status 110 语义，不静默降级）；
    // - partitioned 的 plan/state 在 PartitionedRuntime 内（build_extended 已校验 L_F <=
    //   TAPS_EXTENDED_MAX=16384），不需要 DirectLayout。
    let layout = if use_partitioned {
        None
    } else {
        Some(
            graph
                .layout()
                .map_err(|status| format!("plan 图 layout 失败（status {}）", status.code()))?,
        )
    };
    // partitioned 引擎不需要 4-worker plan；direct 引擎启动期构建 T09 静态 DAG。
    let workers_plan = if use_partitioned {
        None
    } else {
        Some(
            WorkerDagPlan::build(&graph.topo, config.block_size, config.workers, plan_version)
                .map_err(|status| format!("WorkerDagPlan::build(plan-graph): {}", status.code()))?,
        )
    };
    let (sched, sched_topo) = build_sched_plan(config)?;
    // F6/task-216：apply_affinity=true 时在启动期真实写内核（mlockall 在此、worker/main 随后）；
    // 任何失败返回 Err（fail closed），不允许 warn 后继续假装 RT。
    // task-246：partitioned 是主线程单线程执行器——主线程绑首个 DSP CPU（RK3576 A73 簇）；
    // direct 仍按 T45 计划（主线程非 DSP CPU + 4 worker DSP CPU）。
    let mut rt_runtime = if use_partitioned {
        RtRuntime::prepare_partitioned(config, &sched)?
    } else {
        RtRuntime::prepare(config, &sched)?
    };
    if let Some(warning) = rt_not_applied_warning(config) {
        warnings.push(warning);
    }
    // 2) web（非 RT 探测/子进程）先于设备启动；F5：真实 ALSA 设备打开/流启动推迟到
    // web 探测与 worker/缓冲初始化之后，避免启动期探测 > 1 个 period 造成首块 xrun。
    let (mut child, mut web) = start_web(config)?;
    let block = config.block_size as usize;
    let stride = block;
    let n_in = graph.topo.n_src as usize;
    let input_channels = n_in.max(2);
    // F-12：块循环零分配/不新建线程（worker/缓冲全部启动时一次创建）。
    let mut workers = if use_partitioned {
        None
    } else {
        let plan = workers_plan
            .as_ref()
            .ok_or_else(|| "direct 引擎缺少 worker plan（内部错误）".to_string())?;
        let started = match rt_runtime.as_ref() {
            Some(runtime) => BlockWorkers::start_rt(plan, &coeffs, runtime.worker_targets())
                .map_err(|error| format!("BlockWorkers::start_rt(plan-graph): {error}"))?,
            None => BlockWorkers::start(plan, &coeffs)
                .map_err(|status| format!("BlockWorkers::start(plan-graph): {}", status.code()))?,
        };
        Some(started)
    };
    if let Some(runtime) = rt_runtime.as_mut() {
        if !use_partitioned {
            runtime.mark_workers_applied();
        }
    }
    // direct 用 T09 direct state；partitioned 的 state 在 PartitionedRuntime 内（启动期分配）。
    let mut state = if use_partitioned {
        Vec::new()
    } else {
        vec![
            0.0f32;
            layout
                .as_ref()
                .expect("direct layout 已校验")
                .state_samples()
        ]
    };
    // plan ABI v2 role IIR(PEQ)：每段 2 state，启动期一次分配；块循环零分配（GAP-T66-3）。
    let mut iir_state = vec![0.0f32; graph.iir.state_len()];
    let mut input = vec![0.0f32; input_channels * stride];
    let mut output = vec![0.0f32; containers * stride];
    let ctl = prepared.ctl;
    let mut lanes = EventLanes::new();
    let mut event_seq = 0u64;
    let mut watchdog = Watchdog::new();
    let mut watchdog_actions = 0u32;
    let mut deadline_misses = 0u64;
    let mut gate_rejects = 0u64;
    let mut safety_mute_blocks = 0u64;
    let mut ack_blocks = 0u64;
    // 真实执行过引擎的块数（direct/partitioned 各自口径；mute/gate 块不计）。
    let mut engine_blocks = 0u64;
    // T203：本 run 真正应用中置下混的块数（mute 块不计；报告/证据用）。
    let mut content_applied_blocks = 0u64;
    let mut blocks = 0u64;
    let mut budget = TickBudget::new(TICK_BUDGET);
    let safe_mode_mute = config.safe_mode == "mute";
    let mut safety_mute_reason = 0u32;
    // 2b) backend sink（fake 契约 / alsa 真实 duplex 由 prepare_backend 接线）。
    let (fake_alsa_contract, mut sink) = prepare_backend(config, containers)?;
    // F-v5-2：起播预填（同 run_command_with_sink；plan-arena 生产路径同样受 host 输出
    // 节拍缺陷影响，不能只修旧 stereo/2x2 路径）。
    if let Some(sink_box) = sink.as_mut() {
        if let Err(error) = sink_box.prime_playback() {
            return Err(fail_closed_sink_error(
                &mut **sink_box,
                "ALSA 起播预填失败",
                &error,
            ));
        }
    }
    // T74 前置 + T239：capture 端点/socket（inferno/ravenna 或 userspace AES67）也在
    // web/worker/缓冲初始化之后打开（F5）；fake backend 无设备时钟时由 userspace 自带节拍。
    let fake_backend = sink.is_none();
    let mut capture_input = start_capture_input(capture_plan, config, fake_backend)?;
    let mut capture_pacer = capture_input
        .as_ref()
        .and_then(|capture| capture.pacer(config.block_size as usize, config.sample_rate));
    // F6/task-216：所有子进程/线程创建完成后，主/I-O owner 线程设为 SCHED_FIFO 并绑非 DSP CPU。
    if let Some(runtime) = rt_runtime.as_mut() {
        runtime.apply_main()?;
    }
    let sched_json = scheduler_report_json(&sched, rt_runtime.as_ref());
    let started = std::time::Instant::now();
    let deadline = if duration_ms == 0 {
        None
    } else {
        Some(started + std::time::Duration::from_millis(duration_ms))
    };
    lanes.push(&event_record(
        event_seq,
        0,
        EV_STARTING,
        LANE_CONTROL,
        SEVERITY_INFO,
        0,
        0,
    ));
    event_seq += 1;
    let mut exit_reason = "max_blocks";
    loop {
        if crate::STOP_REQUESTED.load(std::sync::atomic::Ordering::Acquire) {
            exit_reason = "stop_signal";
            break;
        }
        if max_blocks != 0 && blocks >= max_blocks {
            break;
        }
        if let Some(limit) = deadline {
            if std::time::Instant::now() >= limit {
                exit_reason = "duration";
                break;
            }
        }
        // T239：backend=fake 时 userspace AES67 capture 自带块节拍（无设备时钟）。
        if let Some(pacer) = capture_pacer.as_mut() {
            pacer.wait_next();
            if let Some(capture) = capture_input.as_mut() {
                capture.note_paced();
            }
        }
        let gate = sched.gate_block(&sched_topo);
        let admit = matches!(gate, SchedGate::Admit);
        if !admit {
            gate_rejects += 1;
        }
        let mut mute_this_block = safe_mode_mute || !admit || block_stats_failed();
        if safety_mute_reason == 0 {
            if !admit {
                safety_mute_reason = RtStatus::RT_E_GUARD_FAIL.code();
            } else if block_stats_failed() {
                safety_mute_reason = RtStatus::RT_E_DEADLINE_HARD.code();
            }
        }
        // backend=alsa：先读一个 capture period（保持 capture 流健康，兼作回环输入/设备时钟）。
        if let Some(sink) = sink.as_mut() {
            if let Some(reason) = alsa_capture_phase(
                sink.as_mut(),
                &mut input,
                stride,
                block,
                &mut lanes,
                &mut event_seq,
                blocks,
            )? {
                // F1 SafetyMute：本块不处理新块（输入已清零），只写静音并计数。
                safety_mute_reason = reason;
                mute_this_block = true;
            }
        } else {
            synth_input_into(blocks as usize, &mut input, stride, block);
        }
        // T74 file-input：文件块覆盖 synth/loopback 输入（loopback 组合已在启动校验拒绝）。
        let file_eof_after_block = file_input_phase(
            file_input.as_mut(),
            &mut input,
            stride,
            block,
            mute_this_block,
        );
        // T74 前置 + T239：AoIP/用户态 AES67 capture period -> 图输入（与 file 同优先级，配置互斥）。
        capture_input_phase(
            capture_input.as_mut(),
            &mut input,
            stride,
            block,
            mute_this_block,
            clock::mono_now_ns(),
        )?;
        // 校准模式：节目样本已经取走，这里换成虚拟输入。播放模式立即返回，不改缓冲。
        khaen_rt_host::cal_control::overlay_live(&mut input, stride, block);
        // T203：capture 帧进图之前做中置缺失内容适配（C −3 dB 预混进 FL/FR、C 源置零；
        // 有中置/无中置内容 = 逐位 passthrough）。仅 !mute 块应用（mute 块不消费输入）。
        if !mute_this_block {
            if let Some(adapter) = content_adapter.as_ref() {
                adapter
                    .apply_block(&mut input[..n_in * stride], block)
                    .map_err(|error| {
                        format!("plan 图 block {blocks}: 中置下混应用失败：{error}")
                    })?;
                // 只有真正 Applied（C 被消费）的块计入证据；passthrough 块不计。
                if adapter.is_applied() {
                    content_applied_blocks += 1;
                }
            }
        }
        output.iter_mut().for_each(|sample| *sample = 0.0);
        budget.reset();
        let tick_entry = budget
            .read(&ctl)
            .map_err(|_| "tick budget entry read failed".to_string())?;
        if !mute_this_block {
            if let Some(runtime) = engine.partitioned.as_mut() {
                // partitioned：主线程单线程共享 FFT UPOLS（T08 生产入口；无 worker 线程）。
                runtime
                    .run_block(graph, &input, stride, &mut output, stride, block, block)
                    .map_err(|error| {
                        format!("plan 图 block {blocks} 执行失败（partitioned）：{error}")
                    })?;
                engine_blocks += 1;
            } else if let Some(workers) = workers.as_mut() {
                workers
                    .run_block_4thread(&mut state, &input, stride, &mut output, stride, block)
                    .map_err(|status| {
                        format!("plan 图 block {blocks} 执行失败：{}", status.code())
                    })?;
                engine_blocks += 1;
            } else {
                return Err(format!(
                    "plan 图 block {blocks}：引擎未初始化（内部错误，拒绝继续）"
                ));
            }
            // plan ABI v2 role IIR(PEQ)：P FIR 之后、sink 之前按容器级联（GAP-T66-3）。
            // SafetyMute/gate 块不进入本分支（不消费新块、不推进 IIR 状态，与引擎口径一致）。
            if !graph.iir.is_empty() {
                apply_role_iir_block(
                    &graph.iir,
                    &mut iir_state,
                    &mut output,
                    stride,
                    containers,
                    block,
                );
            }
            // task-253：直通 plan 输出 = feed(n-Δsys)；把 12k 修正支路加到参与者容器。
            if let Some(branch) = mimo12k.as_mut() {
                branch
                    .apply_block(&input, stride, &mut output, stride)
                    .map_err(|error| {
                        format!("plan 图 block {blocks}: mimo12k 应用失败：{error}")
                    })?;
                mimo12k_blocks += 1;
            }
            khaen_rt_host::cal_control::apply_live(&input, stride, &mut output, stride, block);
            khaen_rt_host::cal_control::note_chain_block();
        } else {
            safety_mute_blocks += 1;
        }
        // backend=alsa：plan 图输出（或安全静音）→ T22 S24_3LE → playback period。
        if let Some(sink) = sink.as_mut() {
            alsa_write_phase(
                sink.as_mut(),
                &output,
                stride,
                block,
                mute_this_block,
                &mut lanes,
                &mut event_seq,
                blocks,
            )?;
        }
        let tick_exit = budget
            .read(&ctl)
            .map_err(|_| "tick budget exit read failed".to_string())?;
        let compute = tick_exit.wrapping_sub(tick_entry);
        // 只有**真正执行完**的 block 才是 all-ack 证据（mute/gate 拒绝的块没跑 worker，
        // 不产生 ack；不存在 synthetic/demo ack 冒充真实 all-ack）。
        if !mute_this_block {
            // direct 引擎：只有真实跑完 B1/B2/B3 的块才产生 4-worker all-ack（F-26 真实性）。
            // partitioned 引擎无 worker 线程：单执行器真实完成整块（覆盖 plan 全部 worker lane 的
            // 相位计算）后映射为 single-executor all-ack；报告 swap.single_executor=true 明示口径，
            // 不是 synthetic ack、也不是 4 个 worker 线程各自 ack。
            if let Some(workers) = workers.as_ref() {
                let executed_plan_version = workers.plan_version();
                let block_ack =
                    completed_block_ack(blocks, executed_plan_version, expected_workers);
                let ack_status = prepared.swap.note_block_all_ack(&block_ack);
                if !ack_status.is_ok() {
                    return Err(format!(
                        "plan block {blocks}: 真实 block all-ack 被 T20 拒绝：{}",
                        ack_status.code()
                    ));
                }
                ack_blocks += 1;
            } else if use_partitioned {
                // task-284：partitioned 单执行器 all-ack（T20 note_block_all_ack 的唯一 RT 证据）。
                // 只有真实执行过（engine_blocks 已递增）的块会到这里；mute/gate 块不产生 ack。
                let block_ack = completed_single_executor_block_ack(
                    blocks,
                    prepared.version,
                    expected_workers,
                );
                let ack_status = prepared.swap.note_block_all_ack(&block_ack);
                if !ack_status.is_ok() {
                    return Err(format!(
                        "plan block {blocks}: partitioned single-executor all-ack 被 T20 拒绝：{}",
                        ack_status.code()
                    ));
                }
                ack_blocks += 1;
            }
        }
        let gate_ns = if config.tick_freq_hz == 0 {
            0
        } else {
            compute.saturating_mul(1_000_000_000) / config.tick_freq_hz as u64
        };
        let deadline_ns = gate_ns_floor(config.sample_rate, config.block_size);
        if gate_ns > deadline_ns {
            deadline_misses += 1;
            lanes.push(&event_record(
                event_seq,
                blocks,
                EV_DEADLINE_MISS,
                LANE_WATCHDOG,
                SEVERITY_WARNING,
                compute.min(u32::MAX as u64) as u32,
                gate_ns.min(u32::MAX as u64) as u32,
            ));
            event_seq += 1;
        }
        if blocks % 8 == 0 {
            lanes.push(&event_record(
                event_seq,
                blocks,
                EV_RUNNING,
                LANE_CONTROL,
                SEVERITY_INFO,
                blocks.min(u32::MAX as u64) as u32,
                0,
            ));
            event_seq += 1;
        }
        ctl.heartbeat_seq
            .store(blocks, std::sync::atomic::Ordering::Release);
        let action = watchdog.observe(&ctl);
        if !matches!(action, WatchdogAction::None) {
            watchdog_actions += 1;
        }
        blocks += 1;
        if file_eof_after_block {
            exit_reason = "eof";
            break;
        }
    }
    // 仅 backend=fake（无 ALSA sink）需要 loop 级汇总事件：ALSA 路径的进入事件由 sink 在
    // 真实迁移点上报（每 epoch 恰一次），此处不重复合成。F2：severity=4、value_a=原因码、
    // value_b=epoch_id（core-v1 §8.2 冻结表；safe_mode 显式配置的原因码为 0）。
    if (safety_mute_blocks > 0 || safe_mode_mute) && sink.is_none() {
        lanes.push(&event_record(
            event_seq,
            blocks,
            EV_SAFETY_MUTE_ENTER,
            LANE_CONTROL,
            SEVERITY_FATAL,
            safety_mute_reason,
            EPOCH_ID as u32,
        ));
        event_seq += 1;
    }
    lanes.push(&event_record(
        event_seq,
        blocks,
        EV_STOPPED,
        LANE_CONTROL,
        SEVERITY_INFO,
        blocks.min(u32::MAX as u64) as u32,
        0,
    ));
    // 优雅退出：停止新块 -> 事件 drain（非 RT 读端）-> swap retire -> web/sink 收尾。
    let mut event_counts = [0u32; 64];
    let mut drained = 0u32;
    for lane_index in 0..8u8 {
        if let Some(lane) = lanes.lane(lane_index) {
            let reader = EventLaneReader::new(lane);
            while let Some(event) = reader.next() {
                if (event.kind as usize) < event_counts.len() {
                    event_counts[event.kind as usize] += 1;
                    drained += 1;
                }
            }
        }
    }
    let dropped = lanes.dropped_total();
    let mut graceful = true;
    let alsa_owner_json = match sink {
        Some(mut sink) => {
            let report = sink.report();
            if let Err(error) = sink.stop() {
                warnings.push(format!("ALSA 优雅退出失败：{error}"));
                graceful = false;
            }
            report
        }
        None => fake_alsa_contract,
    };
    prepared.swap.sync_retire_ready_flag();
    let swap_ok = prepared.swap.validate().is_ok();
    let swap_acks = prepared.swap.ack_mask_value().count_ones();
    let swap_ready = prepared.swap.is_retire_ready();
    let retire_ready_source = if swap_ready {
        "note_block_all_ack"
    } else {
        "none"
    };
    if let Some(child) = child.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
        web.child_terminated = true;
    }
    let registry_stats = prepared.registry.stats();
    let current_version = prepared.registry.current_version().unwrap_or(0);
    let event_kinds: Vec<String> = event_counts
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        .map(|(kind, count)| format!("\"{}\":{}", kind, count))
        .collect();
    let warnings_json = warnings
        .iter()
        .map(|warning| format!("\"{}\"", json_escape_text(warning)))
        .collect::<Vec<_>>()
        .join(",");
    // task-246：engine 段（请求/生效/原因/真实执行块数 + partitioned 定量参数）。
    let engine_json = match engine.partitioned.as_ref() {
        Some(runtime) => format!(
            "{{\"requested\":\"{}\",\"effective\":\"partitioned\",\"reason\":\"{}\",\"blocks\":{},\"kernel\":\"shared-fft-upols-f32\",\"fft_size\":{},\"partitions\":{},\"routes\":{},\"taps\":{},\"plan_samples\":{},\"state_samples\":{}}}",
            json_escape_text(&engine.requested),
            json_escape_text(&engine.reason),
            engine_blocks,
            runtime.layout.fft_size,
            runtime.layout.partitions,
            runtime.layout.n_routes,
            runtime.layout.taps,
            runtime.layout.plan_samples(),
            runtime.layout.state_samples()
        ),
        None => format!(
            "{{\"requested\":\"{}\",\"effective\":\"direct\",\"reason\":\"{}\",\"blocks\":{},\"kernel\":\"direct-4worker-b2-f32\"}}",
            json_escape_text(&engine.requested),
            json_escape_text(&engine.reason),
            engine_blocks
        ),
    };
    let engine_dsp = if use_partitioned {
        "partitioned-fft-upols"
    } else {
        "direct-4worker-plan-graph"
    };
    // task-253：mimo12k 段（未配置时 enabled=false，不改变既有报告字段）。
    let mimo12k_json = match mimo12k.as_ref() {
        Some(branch) => branch.report_json(mimo12k_blocks),
        None => "{\"enabled\":false}".to_string(),
    };
    let plan_json = format!(
        "{{\"source\":\"{}\",\"path\":\"{}\",\"bytes\":{},\"sha256\":\"{}\",\"graph_id\":\"{}\",\"version\":{},\"parse\":\"t20-ok\",\"header_crc32c\":\"ok\",\"manifest\":\"{}\",\"form\":\"factorized\",\"admission\":\"{}\",\"dsp\":\"{}\",\"rollback\":\"{}\",\"t20\":{{\"admitted\":{},\"committed\":{},\"current_version\":{}}},\"adapter\":{}}}",
        plan_source,
        json_escape_text(&prepared.path),
        prepared.plan_bytes_len,
        prepared.plan_sha256,
        hex32(&prepared.graph_id),
        prepared.version,
        prepared.manifest_state,
        prepared.admission_backend,
        engine_dsp,
        rollback,
        registry_stats.admitted,
        registry_stats.committed,
        current_version,
        prepared.adapter_json
    );
    if let Some(capture) = capture_input.as_mut() {
        capture.stop();
    }
    let capture_json = capture_report_json(
        config,
        file_input.as_ref(),
        capture_input.as_ref(),
        blocks,
        exit_reason,
    );
    let content_json = content_report_json(content_adapter.as_ref(), content_applied_blocks);
    let mut report = String::with_capacity(2048);
    report.push_str("{\"status\":\"ok\",\"mode\":\"khaen-rt-host-run\"");
    let _ = write!(report, ",\"schema\":\"{}\"", HOST_CONFIG_SCHEMA);
    let _ = write!(report, ",\"config\":\"{}\"", json_escape_text(config_path));
    report.push_str(",\"graph\":\"plan-arena\"");
    let _ = write!(
        report,
        ",\"block_size\":{},\"sample_rate\":{},\"workers\":{}",
        config.block_size, config.sample_rate, config.workers
    );
    let _ = write!(
        report,
        ",\"blocks\":{},\"frames\":{}",
        blocks,
        blocks * config.block_size as u64
    );
    let _ = write!(report, ",\"safe_mode\":\"{}\"", config.safe_mode);
    let _ = write!(report, ",\"graceful\":{}", graceful);
    let _ = write!(report, ",\"capture\":{}", capture_json);
    let _ = write!(report, ",\"content\":{}", content_json);
    let _ = write!(report, ",\"plan\":{}", plan_json);
    let _ = write!(report, ",\"engine\":{}", engine_json);
    let _ = write!(report, ",\"mimo12k\":{}", mimo12k_json);
    let _ = write!(report, ",\"alsa\":{}", alsa_owner_json);
    let _ = write!(report, ",\"scheduler\":{}", sched_json);
    let _ = write!(
        report,
        ",\"swap\":{{\"init_status\":{},\"acks\":{},\"switches\":1,\"retire_ready\":{},\"valid\":{},\"synthetic\":false,\"retire_ready_source\":\"{}\",\"all_ack_blocks\":{},\"plan_version\":{},\"quarantine_entered\":0,\"quarantine_released\":0,\"quarantine_remaining\":0,\"executor\":\"{}\",\"single_executor\":{}}}",
        prepared.apply_init_status,
        swap_acks,
        swap_ready,
        swap_ok,
        retire_ready_source,
        ack_blocks,
        prepared.version,
        engine_dsp,
        use_partitioned
    );
    let _ = write!(
        report,
        ",\"fault\":{{\"events_drained\":{},\"dropped\":{},\"by_kind\":{{{}}},\"watchdog_actions\":{},\"deadline_misses\":{},\"gate_rejects\":{},\"safety_mute_blocks\":{}}}",
        drained,
        dropped,
        event_kinds.join(","),
        watchdog_actions,
        deadline_misses,
        gate_rejects,
        safety_mute_blocks
    );
    let _ = write!(
        report,
        ",\"web\":{{\"mode\":\"{}\",\"bind\":\"{}\",\"probe_status\":{},\"probe_body\":\"{}\",\"daemon_pid\":{},\"child_terminated\":{},\"note\":\"{}\"}}",
        web.mode,
        json_escape_text(&web.bind),
        web.probe_status
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string()),
        json_escape_text(&web.probe_body),
        web.daemon_pid
            .map(|value| value.to_string())
            .unwrap_or_else(|| "null".to_string()),
        web.child_terminated,
        json_escape_text(&web.note)
    );
    let _ = write!(
        report,
        ",\"last_good\":{},\"warnings\":[{}]}}",
        last_good_report(&config.last_good),
        warnings_json
    );
    Ok(report)
}

// ---------------------------------------------------------------------------
// T185 单测：plan 表 -> host 直算图 adapter + plan-arena run --config 端到端（fake 后端）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod plan_arena_tests {
    use super::*;
    use khaen_core::abi::{TspiDiagFirV1, TspiPathV1, TspiRoleV1};

    const T_RATE: u32 = 48_000;
    const T_BLOCK: u16 = 128;
    const T_GRAPH_ID: [u8; 32] = [0x5a; 32];

    #[derive(Clone, Debug)]
    struct TestRole {
        out_index: u16,
        flags: u16,
        delay_samples: u32,
        gain_q15: i16,
        polarity: i8,
        peq_count: u16,
    }

    #[derive(Clone, Debug)]
    struct TestPath {
        in_index: u16,
        out_index: u16,
        taps: u32,
        delay_samples: u32,
        flags: u8,
        coeffs: Vec<f32>,
    }

    #[derive(Clone, Debug)]
    struct TestDiag {
        out_index: u16,
        taps: u32,
        coeffs: Vec<f32>,
    }

    #[derive(Clone, Debug)]
    struct TestIir {
        role_id: u16,
        sections: Vec<[f32; 5]>,
    }

    struct TestPlan {
        n_in: u16,
        n_out: u16,
        roles: Vec<TestRole>,
        paths: Vec<TestPath>,
        diags: Vec<TestDiag>,
        /// plan ABI v2 per-role IIR（空 = 走 v1 布局；非空 = v2 + IIR 表）。
        iir: Vec<TestIir>,
    }

    impl TestPlan {
        /// 2x2 factorized stereo 直通（diag baseline D=0，G=I，P=delta）。
        fn stereo_identity() -> Self {
            Self {
                n_in: 2,
                n_out: 2,
                iir: Vec::new(),
                roles: vec![
                    TestRole {
                        out_index: 0,
                        flags: PLAN_ROLE_FLAG_ENABLED,
                        delay_samples: 0,
                        gain_q15: 32767,
                        polarity: 1,
                        peq_count: 0,
                    },
                    TestRole {
                        out_index: 1,
                        flags: PLAN_ROLE_FLAG_ENABLED,
                        delay_samples: 0,
                        gain_q15: 32767,
                        polarity: 1,
                        peq_count: 0,
                    },
                ],
                paths: vec![
                    TestPath {
                        in_index: 0,
                        out_index: 0,
                        taps: 2,
                        delay_samples: 0,
                        flags: PLAN_PATH_FLAG_DIAG_BASELINE,
                        coeffs: vec![1.0, 0.0],
                    },
                    TestPath {
                        in_index: 1,
                        out_index: 1,
                        taps: 2,
                        delay_samples: 0,
                        flags: PLAN_PATH_FLAG_DIAG_BASELINE,
                        coeffs: vec![1.0, 0.0],
                    },
                ],
                diags: vec![
                    TestDiag {
                        out_index: 0,
                        taps: 1,
                        coeffs: vec![1.0],
                    },
                    TestDiag {
                        out_index: 1,
                        taps: 1,
                        coeffs: vec![1.0],
                    },
                ],
            }
        }

        /// 2x2：G 有 offdiag cell（out0 混入 in1*0.5），用于 adapter/bit-exact 测试。
        fn stereo_offdiag() -> Self {
            let mut plan = Self::stereo_identity();
            plan.paths.push(TestPath {
                in_index: 1,
                out_index: 0,
                taps: 2,
                delay_samples: 0,
                flags: 0,
                coeffs: vec![0.5, 0.0],
            });
            plan.paths
                .sort_by_key(|path| (path.out_index, path.in_index));
            plan
        }

        /// 4.1.2 真实形状：8ch 容器、7 物理角色（FL0 FR1 SL2 SR3 HFL4 HFR5 SUB7，ch6 空）。
        fn multichannel_412() -> Self {
            let role_containers = [0u16, 1, 2, 3, 4, 5, 7];
            let mut roles = Vec::new();
            let mut paths = Vec::new();
            let mut diags = Vec::new();
            for out_index in role_containers {
                roles.push(TestRole {
                    out_index,
                    flags: PLAN_ROLE_FLAG_ENABLED,
                    delay_samples: 0,
                    gain_q15: 32767,
                    polarity: 1,
                    peq_count: 0,
                });
                paths.push(TestPath {
                    in_index: out_index,
                    out_index,
                    taps: 2,
                    delay_samples: 0,
                    flags: PLAN_PATH_FLAG_DIAG_BASELINE,
                    coeffs: vec![1.0, 0.0],
                });
                diags.push(TestDiag {
                    out_index,
                    taps: 1,
                    coeffs: vec![1.0],
                });
            }
            Self {
                n_in: 8,
                n_out: 8,
                roles,
                paths,
                diags,
                iir: Vec::new(),
            }
        }

        /// T203：5.1 内容输入（6ch: FL FR C LFE SL SR）-> 4.1.2 输出（8ch 容器 / 7 角色）。
        ///
        /// A_map: FL←0 FR←1 SL←4 SR←5 SUB1←3；内容 C(ch2) 不被任何 role 选中（task-200
        /// LAYER-ANALYSIS 的真实丢弃点）；HFL/HFR 无节目源 -> 显式 zero_fill。host 中置下混
        /// 必须在 capture 帧进本图之前把 C 混进 FL/FR（否则 C 内容丢失）。
        fn multichannel_412_from_51_content() -> Self {
            let roles_spec: [(u16, Option<u16>); 7] = [
                (0, Some(0)),
                (1, Some(1)),
                (2, Some(4)),
                (3, Some(5)),
                (4, None),
                (5, None),
                (7, Some(3)),
            ];
            let mut roles = Vec::new();
            let mut paths = Vec::new();
            let mut diags = Vec::new();
            for (out_index, source) in roles_spec {
                let flags = match source {
                    Some(_) => PLAN_ROLE_FLAG_ENABLED,
                    None => PLAN_ROLE_FLAG_ZERO_FILL,
                };
                roles.push(TestRole {
                    out_index,
                    flags,
                    delay_samples: 0,
                    gain_q15: 32767,
                    polarity: 1,
                    peq_count: 0,
                });
                let Some(in_index) = source else { continue };
                paths.push(TestPath {
                    in_index,
                    out_index,
                    taps: 2,
                    delay_samples: 0,
                    flags: PLAN_PATH_FLAG_DIAG_BASELINE,
                    coeffs: vec![1.0, 0.0],
                });
                diags.push(TestDiag {
                    out_index,
                    taps: 1,
                    coeffs: vec![1.0],
                });
            }
            Self {
                n_in: 6,
                n_out: 8,
                roles,
                paths,
                diags,
                iir: Vec::new(),
            }
        }

        /// T203：5.1 内容输入 -> 5.1 输出（输出布局含 C = 有中置配置；host 必须 passthrough）。
        fn multichannel_51_center_present() -> Self {
            let mut roles = Vec::new();
            let mut paths = Vec::new();
            let mut diags = Vec::new();
            let mut out_index = 0u16;
            while out_index < 6 {
                roles.push(TestRole {
                    out_index,
                    flags: PLAN_ROLE_FLAG_ENABLED,
                    delay_samples: 0,
                    gain_q15: 32767,
                    polarity: 1,
                    peq_count: 0,
                });
                paths.push(TestPath {
                    in_index: out_index,
                    out_index,
                    taps: 2,
                    delay_samples: 0,
                    flags: PLAN_PATH_FLAG_DIAG_BASELINE,
                    coeffs: vec![1.0, 0.0],
                });
                diags.push(TestDiag {
                    out_index,
                    taps: 1,
                    coeffs: vec![1.0],
                });
                out_index += 1;
            }
            Self {
                n_in: 6,
                n_out: 6,
                roles,
                paths,
                diags,
                iir: Vec::new(),
            }
        }
    }

    fn f32_blob(values: &[f32]) -> Vec<u8> {
        let mut out = Vec::with_capacity(values.len() * 4);
        for value in values {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out
    }

    fn path_bytes(path: &TspiPathV1) -> [u8; 32] {
        let mut out = [0u8; 32];
        let mut offset = 0usize;
        for bytes in [
            path.in_index.to_le_bytes().as_slice(),
            path.out_index.to_le_bytes().as_slice(),
            path.taps.to_le_bytes().as_slice(),
            path.delay_samples.to_le_bytes().as_slice(),
        ] {
            out[offset..offset + bytes.len()].copy_from_slice(bytes);
            offset += bytes.len();
        }
        out[offset] = path.polarity as u8;
        out[offset + 1] = path.flags;
        offset += 2;
        out[offset..offset + 2].copy_from_slice(&path.reserved0.to_le_bytes());
        offset += 2;
        out[offset..offset + 8].copy_from_slice(&path.coeff_offset_bytes.to_le_bytes());
        offset += 8;
        out[offset..offset + 4].copy_from_slice(&path.coeff_len_bytes.to_le_bytes());
        offset += 4;
        out[offset..offset + 4].copy_from_slice(&path.reserved1.to_le_bytes());
        out
    }

    fn diag_bytes(diag: &TspiDiagFirV1) -> [u8; 32] {
        let mut out = [0u8; 32];
        let mut offset = 0usize;
        out[offset..offset + 2].copy_from_slice(&diag.out_index.to_le_bytes());
        offset += 2;
        out[offset..offset + 2].copy_from_slice(&diag.flags.to_le_bytes());
        offset += 2;
        out[offset..offset + 4].copy_from_slice(&diag.taps.to_le_bytes());
        offset += 4;
        out[offset..offset + 4].copy_from_slice(&diag.delay_samples.to_le_bytes());
        offset += 4;
        out[offset..offset + 4].copy_from_slice(&diag.reserved0.to_le_bytes());
        offset += 4;
        out[offset..offset + 8].copy_from_slice(&diag.coeff_offset_bytes.to_le_bytes());
        offset += 8;
        out[offset..offset + 4].copy_from_slice(&diag.coeff_len_bytes.to_le_bytes());
        offset += 4;
        out[offset..offset + 4].copy_from_slice(&diag.reserved1.to_le_bytes());
        out
    }

    fn role_bytes(role: &TspiRoleV1) -> [u8; 32] {
        let mut out = [0u8; 32];
        let mut offset = 0usize;
        out[offset..offset + 2].copy_from_slice(&role.role_id.to_le_bytes());
        offset += 2;
        out[offset..offset + 2].copy_from_slice(&role.out_index.to_le_bytes());
        offset += 2;
        out[offset..offset + 2].copy_from_slice(&role.flags.to_le_bytes());
        offset += 2;
        out[offset..offset + 2].copy_from_slice(&role.reserved0.to_le_bytes());
        offset += 2;
        out[offset..offset + 4].copy_from_slice(&role.delay_samples.to_le_bytes());
        offset += 4;
        out[offset..offset + 2].copy_from_slice(&role.gain_q15.to_le_bytes());
        offset += 2;
        out[offset] = role.polarity as u8;
        out[offset + 1] = role.reserved1;
        offset += 2;
        out[offset..offset + 2].copy_from_slice(&role.peq_count.to_le_bytes());
        offset += 2;
        out[offset..offset + 2].copy_from_slice(&role.reserved2.to_le_bytes());
        offset += 2;
        out[offset..offset + 4].copy_from_slice(&role.name_hash.to_le_bytes());
        offset += 4;
        out[offset..offset + 4].copy_from_slice(&role.reserved3.to_le_bytes());
        offset += 4;
        out[offset..offset + 4].copy_from_slice(&role.reserved4.to_le_bytes());
        out
    }

    fn header_raw(header: &TspiPlanHeaderV1) -> [u8; 128] {
        // repr(C, align(64)) 128 B 布局 == T66 header_bytes() 的字段序（ABI v1.1.2 E1 定稿）。
        let mut bytes = [0u8; 128];
        // SAFETY: 只读复制 header 的前 128 B（结构体 size=128，纯整数字段）。
        unsafe {
            std::ptr::copy_nonoverlapping(
                header as *const TspiPlanHeaderV1 as *const u8,
                bytes.as_mut_ptr(),
                128,
            );
        }
        bytes
    }

    fn iir_header_bytes(header: &khaen_core::abi::TspiPlanIirHeaderV2) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&header.magic.to_le_bytes());
        out[4..6].copy_from_slice(&header.version.to_le_bytes());
        out[6..8].copy_from_slice(&header.entry_bytes.to_le_bytes());
        out[8..10].copy_from_slice(&header.entry_count.to_le_bytes());
        out[10..12].copy_from_slice(&header.reserved0.to_le_bytes());
        out[12..16].copy_from_slice(&header.table_crc32c.to_le_bytes());
        out
    }

    fn iir_entry_bytes(entry: &TspiRoleIirV2) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[0..2].copy_from_slice(&entry.role_id.to_le_bytes());
        out[2..4].copy_from_slice(&entry.order.to_le_bytes());
        out[4..6].copy_from_slice(&entry.section_count.to_le_bytes());
        out[6..8].copy_from_slice(&entry.flags.to_le_bytes());
        out[8..16].copy_from_slice(&entry.coeff_offset_bytes.to_le_bytes());
        out[16..20].copy_from_slice(&entry.coeff_len_bytes.to_le_bytes());
        out[20..24].copy_from_slice(&entry.reserved0.to_le_bytes());
        out[24..28].copy_from_slice(&entry.reserved1.to_le_bytes());
        out[28..32].copy_from_slice(&entry.reserved2.to_le_bytes());
        out
    }

    /// 构造与 T66 打包逐字节同布局的 plan 字节。
    ///
    /// v1（plan.iir 空）：header 128B + path/diag/role 表 + 64B 对齐系数区（旧布局不变）。
    /// v2（plan.iir 非空，GAP-T66-3）：abi_version=2、role 表后接 IIR 表（ctl_meta 为空）、
    /// 系数区追加 IIR blob，header.reserved3=[iir_table_offset, entry_count, 0]。
    #[allow(clippy::type_complexity)]
    fn plan_parts(
        plan: &TestPlan,
    ) -> (
        TspiPlanHeaderV1,
        Vec<TspiPathV1>,
        Vec<TspiDiagFirV1>,
        Vec<TspiRoleV1>,
        Vec<TspiRoleIirV2>,
        Vec<u8>,
        Vec<u8>,
    ) {
        let mut blobs: Vec<Vec<u8>> = Vec::new();
        for path in &plan.paths {
            blobs.push(f32_blob(&path.coeffs));
        }
        for diag in &plan.diags {
            blobs.push(f32_blob(&diag.coeffs));
        }
        for iir in &plan.iir {
            let mut bytes: Vec<u8> = Vec::new();
            for section in &iir.sections {
                for value in section {
                    bytes.extend_from_slice(&value.to_le_bytes());
                }
            }
            blobs.push(bytes);
        }
        let mut region: Vec<u8> = Vec::new();
        let mut offsets: Vec<u64> = Vec::new();
        for blob in &blobs {
            let offset = ((region.len() + 63) / 64) * 64;
            region.resize(offset, 0);
            offsets.push(offset as u64);
            region.extend_from_slice(blob);
        }
        let path_table_offset = 128u32;
        let diag_table_offset = path_table_offset + plan.paths.len() as u32 * 32;
        let role_table_offset = diag_table_offset + plan.diags.len() as u32 * 32;
        let ctl_meta_offset = role_table_offset + plan.roles.len() as u32 * 32;
        let max_taps = plan
            .paths
            .iter()
            .map(|path| path.taps)
            .chain(plan.diags.iter().map(|diag| diag.taps))
            .max()
            .unwrap_or(1);
        let mut paths = Vec::with_capacity(plan.paths.len());
        for (index, path) in plan.paths.iter().enumerate() {
            paths.push(TspiPathV1 {
                in_index: path.in_index,
                out_index: path.out_index,
                taps: path.taps,
                delay_samples: path.delay_samples,
                polarity: 1,
                flags: path.flags,
                reserved0: 0,
                coeff_offset_bytes: offsets[index],
                coeff_len_bytes: path.taps * 4,
                reserved1: 0,
            });
        }
        let mut diags = Vec::with_capacity(plan.diags.len());
        for (index, diag) in plan.diags.iter().enumerate() {
            diags.push(TspiDiagFirV1 {
                out_index: diag.out_index,
                flags: 0,
                taps: diag.taps,
                delay_samples: 0,
                reserved0: 0,
                coeff_offset_bytes: offsets[plan.paths.len() + index],
                coeff_len_bytes: diag.taps * 4,
                reserved1: 0,
            });
        }
        let mut roles = Vec::with_capacity(plan.roles.len());
        for (index, role) in plan.roles.iter().enumerate() {
            roles.push(TspiRoleV1 {
                role_id: index as u16,
                out_index: role.out_index,
                flags: role.flags,
                reserved0: 0,
                delay_samples: role.delay_samples,
                gain_q15: role.gain_q15,
                polarity: role.polarity,
                reserved1: 0,
                peq_count: role.peq_count,
                reserved2: 0,
                name_hash: 0,
                reserved3: 0,
                reserved4: 0,
            });
        }
        let mut iir_entries: Vec<TspiRoleIirV2> = Vec::with_capacity(plan.iir.len());
        let mut iir_table: Vec<u8> = Vec::new();
        let mut reserved3 = [0u32; 3];
        if !plan.iir.is_empty() {
            let iir_table_offset = ctl_meta_offset;
            for (index, iir) in plan.iir.iter().enumerate() {
                let sections = iir.sections.len() as u16;
                iir_entries.push(TspiRoleIirV2 {
                    role_id: iir.role_id,
                    order: 2,
                    section_count: sections,
                    flags: 0,
                    coeff_offset_bytes: offsets[plan.paths.len() + plan.diags.len() + index],
                    coeff_len_bytes: sections as u32 * 5 * 4,
                    reserved0: 0,
                    reserved1: 0,
                    reserved2: 0,
                });
            }
            let header_bytes = core::mem::size_of::<khaen_core::abi::TspiPlanIirHeaderV2>();
            let crc_header = khaen_core::abi::TspiPlanIirHeaderV2 {
                magic: khaen_core::abi::TSPI_PLAN_IIR_MAGIC,
                version: khaen_core::abi::TSPI_PLAN_IIR_VERSION,
                entry_bytes: 32,
                entry_count: iir_entries.len() as u16,
                reserved0: 0,
                table_crc32c: 0,
            };
            let mut crc_input: Vec<u8> = Vec::new();
            crc_input.extend_from_slice(&iir_header_bytes(&crc_header));
            for entry in &iir_entries {
                crc_input.extend_from_slice(&iir_entry_bytes(entry));
            }
            for iir in &plan.iir {
                for section in &iir.sections {
                    for value in section {
                        crc_input.extend_from_slice(&value.to_le_bytes());
                    }
                }
            }
            let table_crc32c = khaen_core::nodes::crc32c(&crc_input);
            let table_header = khaen_core::abi::TspiPlanIirHeaderV2 {
                magic: khaen_core::abi::TSPI_PLAN_IIR_MAGIC,
                version: khaen_core::abi::TSPI_PLAN_IIR_VERSION,
                entry_bytes: 32,
                entry_count: iir_entries.len() as u16,
                reserved0: 0,
                table_crc32c,
            };
            iir_table.extend_from_slice(&iir_header_bytes(&table_header));
            for entry in &iir_entries {
                iir_table.extend_from_slice(&iir_entry_bytes(entry));
            }
            reserved3 = [iir_table_offset, iir_entries.len() as u32, 0];
        }
        let abi_version = if plan.iir.is_empty() {
            ABI_VERSION
        } else {
            TSPI_PLAN_ABI_V2
        };
        let header = TspiPlanHeaderV1 {
            magic: TSPI_PLAN_MAGIC,
            abi_version,
            header_size: 128,
            plan_version: 0,
            sample_rate: T_RATE,
            block_size: T_BLOCK,
            n_in: plan.n_in,
            n_out: plan.n_out,
            low_band_count: 2,
            path_count: plan.paths.len() as u16,
            diag_fir_count: plan.diags.len() as u16,
            max_taps,
            workers: WORKERS,
            flags: 0,
            precision: 0,
            reserved0: 0,
            graph_id: T_GRAPH_ID,
            total_coeff_bytes: region.len() as u32,
            path_table_offset,
            diag_table_offset,
            in_stride: T_BLOCK as u32,
            out_stride: T_BLOCK as u32,
            role_table_offset,
            role_table_count: plan.roles.len() as u16,
            reserved1: 0,
            affinity_table_offset: 0,
            affinity_count: 0,
            reserved2: 0,
            ctl_meta_offset,
            ctl_meta_len: 0,
            reserved3,
            header_crc32c: 0,
        };
        (header, paths, diags, roles, iir_entries, iir_table, region)
    }
    fn plan_file_bytes(plan: &TestPlan) -> Vec<u8> {
        let (mut header, paths, diags, roles, _iir_entries, iir_table, region) = plan_parts(plan);
        let mut header_bytes = header_raw(&header);
        header_bytes[124..128].fill(0);
        header.header_crc32c = khaen_core::nodes::crc32c(&header_bytes);
        let mut out = header_raw(&header).to_vec();
        for path in &paths {
            out.extend_from_slice(&path_bytes(path));
        }
        for diag in &diags {
            out.extend_from_slice(&diag_bytes(diag));
        }
        for role in &roles {
            out.extend_from_slice(&role_bytes(role));
        }
        out.extend_from_slice(&iir_table);
        out.extend(region);
        out
    }

    fn adapter(plan: &TestPlan) -> Result<(Graph, String), String> {
        let (header, paths, diags, roles, iir_entries, _iir_table, blob) = plan_parts(plan);
        let view = PlanView {
            header: &header,
            paths: &paths,
            diags: &diags,
            roles: &roles,
            coeffs: &blob,
        };
        graph_from_plan(&view, &iir_entries)
    }

    // ---- G/P/role 元数据 adapter 语义 ----

    #[test]
    fn t185_plan_graph_adapter_identity_topology() {
        let (graph, adapter_json) = adapter(&TestPlan::stereo_identity()).expect("identity plan");
        assert_eq!(graph.topo.n_src, 2);
        assert_eq!(graph.topo.n_role, 2);
        assert_eq!(graph.topo.container_channels, 2);
        assert_eq!(graph.topo.l_a, 1);
        assert_eq!(graph.topo.l_g, 2);
        assert_eq!(graph.topo.l_p, 1);
        assert_eq!(graph.topo.d_samples, 0);
        assert_eq!(graph.topo.m, 1, "无 offdiag/diag g cell 时最小 participant");
        assert_eq!(graph.topo.src_of_role[0], 0);
        assert_eq!(graph.topo.src_of_role[1], 1);
        assert_eq!(graph.topo.scatter[..2], [0, 1]);
        assert!(graph.g.iter().all(|value| *value == 0.0));
        assert!(graph.a.iter().all(|value| *value == 1.0));
        assert_eq!(graph.p, vec![1.0, 1.0]);
        assert!(adapter_json.contains("\"l_g\":2"));
        assert!(adapter_json.contains("\"m\":1"));
    }

    #[test]
    fn t185_plan_graph_adapter_offdiag_cell_and_participants() {
        let (graph, adapter_json) = adapter(&TestPlan::stereo_offdiag()).expect("offdiag plan");
        assert_eq!(graph.topo.m, 2, "offdiag cell 两端都是 participant");
        assert_eq!(graph.topo.participants[..2], [0, 1]);
        // g[(out_pos=0)*m+(in_pos=1)] 的第 0 tap = 0.5。
        assert_eq!(graph.g[(0 * 2 + 1) * 2], 0.5);
        assert_eq!(graph.g[(0 * 2 + 1) * 2 + 1], 0.0);
        assert!(adapter_json.contains("\"participants\":[0,1]"));
    }

    #[test]
    fn t185_plan_graph_adapter_folds_role_delay_polarity_gain() {
        let mut plan = TestPlan::stereo_identity();
        plan.roles[1].delay_samples = 2;
        plan.roles[1].polarity = -1;
        plan.roles[1].gain_q15 = 16384; // ~0.5 线性
        let (graph, _) = adapter(&plan).expect("role metadata plan");
        assert_eq!(graph.topo.l_p, 3, "P 行 = p_fir + role delay 折叠");
        let scale = 16384.0f32 / 32767.0;
        assert_eq!(graph.p[1 * 3], 0.0);
        assert_eq!(graph.p[1 * 3 + 1], 0.0);
        let expected = -1.0 * scale;
        assert!(
            (graph.p[1 * 3 + 2] - expected).abs() < 1e-6,
            "role1 P 行 delay/polarity/gain 折叠错误: {:?}",
            &graph.p[1 * 3..1 * 3 + 3]
        );
        assert_eq!(graph.p[0 * 3], 1.0);
    }

    #[test]
    fn t185_plan_graph_rejects_fused_form() {
        let mut plan = TestPlan::stereo_identity();
        plan.paths[0].flags |= PLAN_PATH_FLAG_FUSED;
        let error = match adapter(&plan) {
            Ok(_) => panic!("fused plan 必须明确拒绝"),
            Err(error) => error,
        };
        assert!(error.contains("fused"), "{error}");
        assert!(error.contains("拒绝启动"), "{error}");
    }

    #[test]
    fn t185_plan_graph_rejects_role_peq() {
        let mut plan = TestPlan::stereo_identity();
        plan.roles[0].peq_count = 1;
        let error = match adapter(&plan) {
            Ok(_) => panic!("peq_count>0 必须明确拒绝"),
            Err(error) => error,
        };
        assert!(error.contains("peq_count"), "{error}");
        assert!(error.contains("GAP-T66-3"), "{error}");
    }


    // ---- plan ABI v2：per-role IIR(PEQ)（GAP-T66-3） ----

    /// RBJ cookbook peaking EQ（a0 归一化）→ plan v2 系数 [b0,b1,b2,a1,a2]。
    fn peaking_biquad(fs: f64, f0_hz: f64, gain_db: f64, q: f64) -> [f32; 5] {
        let a = 10f64.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f64::consts::PI * f0_hz / fs;
        let alpha = w0.sin() / (2.0 * q);
        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * w0.cos();
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * w0.cos();
        let a2 = 1.0 - alpha / a;
        [
            (b0 / a0) as f32,
            (b1 / a0) as f32,
            (b2 / a0) as f32,
            (a1 / a0) as f32,
            (a2 / a0) as f32,
        ]
    }

    /// 解析 biquad 幅频响应（dB；与被 host 消费的 f32 系数同口径）。
    fn biquad_response_db(coeffs: &[f32; 5], fs: f64, freq_hz: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq_hz / fs;
        let (b0, b1, b2) = (coeffs[0] as f64, coeffs[1] as f64, coeffs[2] as f64);
        let (a1, a2) = (coeffs[3] as f64, coeffs[4] as f64);
        let num_re = b0 + b1 * w.cos() + b2 * (2.0 * w).cos();
        let num_im = -(b1 * w.sin() + b2 * (2.0 * w).sin());
        let den_re = 1.0 + a1 * w.cos() + a2 * (2.0 * w).cos();
        let den_im = -(a1 * w.sin() + a2 * (2.0 * w).sin());
        let num = (num_re * num_re + num_im * num_im).sqrt();
        let den = (den_re * den_re + den_im * den_im).sqrt();
        20.0 * (num / den).log10()
    }

    /// 经真实 host 路径（T20 图 -> 4-worker DAG）渲染 impulse，并在 sink 之前应用
    /// plan ABI v2 role IIR（与 run_plan_arena_command 同一 `apply_role_iir_block`）。
    fn render_plan_impulse(plan: &TestPlan, blocks: usize) -> Vec<Vec<f32>> {
        let (graph, _) = adapter(plan).expect("plan adapter");
        let worker_plan = graph.plan().expect("worker plan");
        let layout = graph.layout().expect("layout");
        let coeffs = graph.coeffs();
        let n_src = graph.topo.n_src as usize;
        let containers = graph.topo.container_channels as usize;
        let block = T_BLOCK as usize;
        let mut state = vec![0.0f32; layout.state_samples()];
        let mut iir_state = vec![0.0f32; graph.iir.state_len()];
        let mut input = vec![0.0f32; n_src * block];
        let mut output = vec![0.0f32; containers * block];
        let mut workers = BlockWorkers::start(&worker_plan, &coeffs).expect("start workers");
        let mut collected: Vec<Vec<f32>> = vec![Vec::with_capacity(blocks * block); containers];
        for b in 0..blocks {
            input.iter_mut().for_each(|value| *value = 0.0);
            if b == 0 {
                input[0] = 1.0;
            }
            output.iter_mut().for_each(|value| *value = 0.0);
            workers
                .run_block_4thread(&mut state, &input, block, &mut output, block, block)
                .expect("run block");
            apply_role_iir_block(
                &graph.iir,
                &mut iir_state,
                &mut output,
                block,
                containers,
                block,
            );
            for channel in 0..containers {
                collected[channel]
                    .extend_from_slice(&output[channel * block..(channel + 1) * block]);
            }
        }
        collected
    }

    fn dft_mag_db(samples: &[f32], freq_hz: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq_hz / T_RATE as f64;
        let mut re = 0.0f64;
        let mut im = 0.0f64;
        for (index, value) in samples.iter().enumerate() {
            let phase = w * index as f64;
            re += *value as f64 * phase.cos();
            im -= *value as f64 * phase.sin();
        }
        20.0 * (re * re + im * im).sqrt().log10()
    }

    #[test]
    fn t_abi_iir_identity_plan_is_unchanged() {
        // v1 identity plan（无 IIR）仍然 bit-exact 直通：impulse -> impulse。
        let output = render_plan_impulse(&TestPlan::stereo_identity(), 2);
        assert!((output[0][0] - 1.0).abs() < 1e-6, "identity[0][0]={}", output[0][0]);
        assert!(
            output[0][1..].iter().all(|value| value.abs() < 1e-6),
            "identity 应无拖尾"
        );
        assert!(
            output[1].iter().all(|value| value.abs() < 1e-6),
            "FR 无输入应静音"
        );
    }

    #[test]
    fn t_abi_iir_peq_plan_response_matches_analytic() {
        // S2 known PEQ：100 Hz、−6 dB、Q1.0 @48k；plan ABI v2 -> host 4-worker 图 -> IIR 后置。
        let coeffs = peaking_biquad(T_RATE as f64, 100.0, -6.0, 1.0);
        let mut plan = TestPlan::stereo_identity();
        plan.roles[0].peq_count = 1;
        plan.iir.push(TestIir {
            role_id: 0,
            sections: vec![coeffs],
        });
        let (graph, adapter_json) = adapter(&plan).expect("ABI v2 PEQ plan");
        assert_eq!(graph.iir.section_count(), 1);
        assert_eq!(graph.iir.role_count(), 1);
        assert!(adapter_json.contains("\"plan_abi\":2"), "{adapter_json}");
        assert!(adapter_json.contains("\"role_iir_sections\":1"), "{adapter_json}");
        // 2 s（750 块）足够 100 Hz/Q1 的 IR 衰减到可忽略；DFT 分辨率 1 Hz。
        let blocks = 750usize;
        let output = render_plan_impulse(&plan, blocks);
        let samples = &output[0];
        assert!(
            (samples[0] as f64 - coeffs[0] as f64).abs() < 1e-5,
            "IR[0]={} != b0={}",
            samples[0],
            coeffs[0]
        );
        let mut max_deviation = 0.0f64;
        let mut worst_freq = 0.0f64;
        let mut freq = 20.0f64;
        while freq <= 20_000.0 {
            let measured = dft_mag_db(samples, freq);
            let expected = biquad_response_db(&coeffs, T_RATE as f64, freq);
            let deviation = (measured - expected).abs();
            if deviation > max_deviation {
                max_deviation = deviation;
                worst_freq = freq;
            }
            freq *= 2.0f64.powf(1.0 / 12.0);
        }
        assert!(
            max_deviation <= 0.5,
            "plan ABI v2 PEQ 输出响应 vs 解析最大偏差 {max_deviation:.3} dB @ {worst_freq:.1} Hz（判据 0.5 dB）"
        );
        // 中心频点解析值 = −6 dB；输出必须落在 ±0.5 dB。
        let center_expected = biquad_response_db(&coeffs, T_RATE as f64, 100.0);
        let center_measured = dft_mag_db(samples, 100.0);
        assert!((center_expected + 6.0).abs() < 1e-3, "解析 100 Hz 应 ≈ -6 dB：{center_expected}");
        assert!(
            (center_measured - center_expected).abs() <= 0.5,
            "100 Hz 实测 {center_measured:.3} dB vs 解析 {center_expected:.3} dB"
        );
    }

    #[test]
    fn t_abi_iir_unstable_pole_is_rejected() {
        let mut plan = TestPlan::stereo_identity();
        plan.roles[0].peq_count = 1;
        // |a2|=1.2 > 1：极点单位圆外，host 必须拒绝（不静默跑发散 IIR）。
        plan.iir.push(TestIir {
            role_id: 0,
            sections: vec![[1.0, 0.0, 0.0, 0.0, 1.2]],
        });
        let error = match adapter(&plan) {
            Ok(_) => panic!("不稳定 biquad 必须拒绝"),
            Err(error) => error,
        };
        assert!(error.contains("不稳定"), "{error}");
    }

    #[test]
    fn t_abi_iir_v2_plan_runs_via_plan_arena_fake_backend() {
        // 真实 run --config 端到端（backend=fake；不打开设备）：v2 PEQ plan 必须被 host
        // 解析/准入并在 JSON 里如实上报 role IIR 段数。
        let coeffs = peaking_biquad(T_RATE as f64, 100.0, -6.0, 1.0);
        let mut plan = TestPlan::stereo_identity();
        plan.roles[0].peq_count = 1;
        plan.iir.push(TestIir {
            role_id: 0,
            sections: vec![coeffs],
        });
        let plan_path = temp_file("abi-iir-peq", &plan_file_bytes(&plan));
        let json = run_plan_config(Some(&plan_path), None, None, 4).expect("ABI v2 plan-arena run");
        let _ = std::fs::remove_file(&plan_path);
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        assert!(json.contains("\"graph\":\"plan-arena\""), "{json}");
        assert!(json.contains("\"parse\":\"t20-ok\""), "{json}");
        assert!(json.contains("\"plan_abi\":2"), "{json}");
        assert!(json.contains("\"role_iir_sections\":1"), "{json}");
    }

    #[test]
    fn t_abi_iir_bad_table_crc_is_rejected() {
        let coeffs = peaking_biquad(T_RATE as f64, 100.0, -6.0, 1.0);
        let mut plan = TestPlan::stereo_identity();
        plan.roles[0].peq_count = 1;
        plan.iir.push(TestIir {
            role_id: 0,
            sections: vec![coeffs],
        });
        let mut bytes = plan_file_bytes(&plan);
        // 翻转系数区最后一个字节（不影响 header CRC，只破坏 IIR 表 CRC）。
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        let plan_path = temp_file("abi-iir-bad-crc", &bytes);
        let error = run_plan_config(Some(&plan_path), None, None, 2)
            .expect_err("IIR 表 CRC 失败必须拒绝启动");
        let _ = std::fs::remove_file(&plan_path);
        assert!(error.contains("T20 parse/admit 拒绝 plan"), "{error}");
    }

    #[test]
    fn t_abi_iir_bad_table_magic_is_rejected() {
        let coeffs = peaking_biquad(T_RATE as f64, 100.0, -6.0, 1.0);
        let mut plan = TestPlan::stereo_identity();
        plan.roles[0].peq_count = 1;
        plan.iir.push(TestIir {
            role_id: 0,
            sections: vec![coeffs],
        });
        let mut bytes = plan_file_bytes(&plan);
        // IIR 表紧随 role 表：128 + 2*32(path) + 2*32(diag) + 2*32(role) = 320。
        let iir_offset = 128 + 2 * 32 + 2 * 32 + 2 * 32;
        bytes[iir_offset] ^= 0xFF;
        let plan_path = temp_file("abi-iir-bad-magic", &bytes);
        let error = run_plan_config(Some(&plan_path), None, None, 2)
            .expect_err("坏 IIR 表 magic 必须拒绝启动");
        let _ = std::fs::remove_file(&plan_path);
        assert!(error.contains("IIR 表头"), "{error}");
    }

    #[test]
    fn t185_plan_graph_bit_exact_against_direct_reference() {
        // adapter 图 -> 4 worker DAG 必须与 T07 直算逐位一致（含 4 个 block 的流式历史）。
        let (graph, _) = adapter(&TestPlan::stereo_offdiag()).expect("offdiag plan");
        let blocks = 4usize;
        let block = T_BLOCK as usize;
        let source: Vec<Vec<f32>> = (0..2u16)
            .map(|channel| {
                (0..blocks * block)
                    .map(|index| {
                        let sample = index as f32 * 0.001;
                        if channel == 0 {
                            sample.sin()
                        } else {
                            (sample * 1.7).cos() * 0.5
                        }
                    })
                    .collect()
            })
            .collect();
        let reference = direct_stream(&graph, &source, blocks).expect("direct reference");
        let plan = graph.plan().expect("worker plan");
        let coeffs = graph.coeffs();
        let layout = graph.layout().expect("layout");
        let containers = graph.topo.container_channels as usize;
        let mut state = vec![0.0f32; layout.state_samples()];
        let mut input = vec![0.0f32; 2 * block];
        let mut output = vec![0.0f32; containers * block];
        let mut workers = BlockWorkers::start(&plan, &coeffs).expect("start workers");
        let mut pipeline: Vec<Vec<f32>> = vec![Vec::new(); containers];
        for b in 0..blocks {
            for channel in 0..2usize {
                input[channel * block..(channel + 1) * block]
                    .copy_from_slice(&source[channel][b * block..(b + 1) * block]);
            }
            output.iter_mut().for_each(|sample| *sample = 0.0);
            workers
                .run_block_4thread(&mut state, &input, block, &mut output, block, block)
                .expect("run block");
            for channel in 0..containers {
                pipeline[channel]
                    .extend_from_slice(&output[channel * block..(channel + 1) * block]);
            }
        }
        assert_eq!(
            pipeline, reference,
            "plan adapter 图必须与 T07 直算 bit-exact"
        );
    }

    // ---- run --config 端到端（fake 后端；不打开设备） ----

    fn temp_file(prefix: &str, bytes: &[u8]) -> std::path::PathBuf {
        // cargo test 并行执行：文件名必须每次唯一（否则并发用例互相覆盖 config/plan）。
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
        static UNIQUE: AtomicU64 = AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, AtomicOrdering::Relaxed);
        let path = std::env::current_exe()
            .expect("current_exe")
            .parent()
            .expect("test exe dir")
            .join(format!(
                "khaen-rt-host-t185-{}-{}-{}.bin",
                std::process::id(),
                unique,
                prefix
            ));
        std::fs::write(&path, bytes).expect("write temp plan/config");
        path
    }

    fn manifest_json(
        plan_bytes: &[u8],
        graph_id: [u8; 32],
        plan_sha_override: Option<&str>,
        graph_override: Option<&str>,
    ) -> String {
        let plan_sha = plan_sha_override
            .map(str::to_string)
            .unwrap_or_else(|| sha256_hex_full(plan_bytes));
        let graph_hex = graph_override
            .map(str::to_string)
            .unwrap_or_else(|| hex32(&graph_id));
        format!(
            "{{\n  \"artifact\": {{ \"host_graph_sha256\": \"{graph_hex}\" }},\n  \"plan\": {{ \"graph_id\": \"{graph_hex}\", \"plan_sha256\": \"{plan_sha}\" }}\n}}\n"
        )
    }

    fn config_text(
        plan: Option<&std::path::Path>,
        manifest: Option<&std::path::Path>,
        last_good: Option<&std::path::Path>,
        graph_profile: &str,
        max_blocks: u64,
    ) -> String {
        let plan_value = plan
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let manifest_value = manifest
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let last_good_value = last_good
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!(
            concat!(
                "[host]\n",
                "block_size = 128\n",
                "sample_rate = 48000\n",
                "workers = 4\n",
                "graph = \"{}\"\n",
                "safe_mode = \"normal\"\n",
                "[alsa]\n",
                "backend = \"fake\"\n",
                "device = \"hw:UltraLitemk5,0\"\n",
                "capture_channels = 20\n",
                "playback_channels = 22\n",
                "period_frames = 128\n",
                "buffer_frames = 1024\n",
                "[rt]\n",
                "dsp_cpus = \"2-5\"\n",
                "tick_freq_hz = 1000000000\n",
                "[web]\n",
                "bind = \"127.0.0.1:1\"\n",
                "[paths]\n",
                "plan = \"{}\"\n",
                "plan_manifest = \"{}\"\n",
                "plan_last_good = \"{}\"\n",
                "[run]\n",
                "max_blocks = {}\n",
            ),
            graph_profile, plan_value, manifest_value, last_good_value, max_blocks
        )
    }

    fn run_plan_config(
        plan: Option<&std::path::Path>,
        manifest: Option<&std::path::Path>,
        last_good: Option<&std::path::Path>,
        max_blocks: u64,
    ) -> Result<String, String> {
        let text = config_text(plan, manifest, last_good, "plan-arena", max_blocks);
        let config_path = temp_file("ents-config", text.as_bytes());
        let result = run_command(config_path.to_str().expect("utf8 config path"), None, None);
        let _ = std::fs::remove_file(&config_path);
        result
    }

    #[test]
    fn t185_plan_arena_legal_plan_runs_and_real_all_acks() {
        let plan_path = temp_file("legal", &plan_file_bytes(&TestPlan::stereo_identity()));
        let json = run_plan_config(Some(&plan_path), None, None, 4).expect("plan-arena run");
        let _ = std::fs::remove_file(&plan_path);
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        assert!(json.contains("\"graph\":\"plan-arena\""), "{json}");
        assert!(json.contains("\"source\":\"primary\""), "{json}");
        assert!(json.contains("\"parse\":\"t20-ok\""), "{json}");
        assert!(json.contains("\"manifest\":\"none\""), "{json}");
        assert!(json.contains("\"form\":\"factorized\""), "{json}");
        assert!(json.contains("\"version\":1"), "{json}");
        assert!(json.contains("\"blocks\":4"), "{json}");
        assert!(json.contains("\"retire_ready\":true"), "{json}");
        assert!(
            json.contains("\"retire_ready_source\":\"note_block_all_ack\""),
            "{json}"
        );
        assert!(json.contains("\"synthetic\":false"), "{json}");
        assert!(json.contains("\"acks\":4"), "{json}");
        assert!(json.contains("\"all_ack_blocks\":4"), "{json}");
        assert!(json.contains("\"plan_version\":1"), "{json}");
        assert!(json.contains("\"admission\":\"mock\""), "{json}");
    }

    #[test]
    fn t185_plan_arena_crc_mismatch_is_rejected() {
        let mut bytes = plan_file_bytes(&TestPlan::stereo_identity());
        bytes[40] ^= 0xFF; // header.graph_id 区域：不改 magic/ABI，仅让 CRC-32C 失配
        let plan_path = temp_file("bad-crc", &bytes);
        let error =
            run_plan_config(Some(&plan_path), None, None, 2).expect_err("坏 CRC plan 必须拒绝启动");
        let _ = std::fs::remove_file(&plan_path);
        assert!(error.contains("header_crc32c"), "{error}");
        assert!(error.contains("拒绝启动"), "{error}");
    }

    #[test]
    fn t185_plan_arena_manifest_graph_id_mismatch_is_rejected() {
        let bytes = plan_file_bytes(&TestPlan::stereo_identity());
        let plan_path = temp_file("graph-mismatch", &bytes);
        let manifest = manifest_json(&bytes, T_GRAPH_ID, None, Some(&hex32(&[0x00; 32])));
        let manifest_path = temp_file("graph-mismatch-manifest", manifest.as_bytes());
        let error = run_plan_config(Some(&plan_path), Some(&manifest_path), None, 2)
            .expect_err("graph_id 不符必须拒绝启动");
        let _ = std::fs::remove_file(&plan_path);
        let _ = std::fs::remove_file(&manifest_path);
        assert!(error.contains("graph_id 不符"), "{error}");
    }

    #[test]
    fn t185_plan_arena_manifest_plan_sha_mismatch_is_rejected() {
        let bytes = plan_file_bytes(&TestPlan::stereo_identity());
        let plan_path = temp_file("sha-mismatch", &bytes);
        let manifest = manifest_json(&bytes, T_GRAPH_ID, Some(&hex32(&[0x11; 32])), None);
        let manifest_path = temp_file("sha-mismatch-manifest", manifest.as_bytes());
        let error = run_plan_config(Some(&plan_path), Some(&manifest_path), None, 2)
            .expect_err("plan sha256 不符必须拒绝启动");
        let _ = std::fs::remove_file(&plan_path);
        let _ = std::fs::remove_file(&manifest_path);
        assert!(error.contains("plan sha256 不符"), "{error}");
    }

    #[test]
    fn t185_plan_arena_manifest_verified_path_runs() {
        let bytes = plan_file_bytes(&TestPlan::stereo_identity());
        let plan_path = temp_file("manifest-ok", &bytes);
        let manifest = manifest_json(&bytes, T_GRAPH_ID, None, None);
        let manifest_path = temp_file("manifest-ok-report", manifest.as_bytes());
        let json = run_plan_config(Some(&plan_path), Some(&manifest_path), None, 2)
            .expect("manifest verified run");
        let _ = std::fs::remove_file(&plan_path);
        let _ = std::fs::remove_file(&manifest_path);
        assert!(json.contains("\"manifest\":\"verified\""), "{json}");
        assert!(json.contains("\"retire_ready\":true"), "{json}");
    }

    #[test]
    fn t185_plan_arena_bad_primary_falls_back_to_last_good() {
        let mut bad = plan_file_bytes(&TestPlan::stereo_identity());
        bad[40] ^= 0x01; // CRC 坏
        let bad_path = temp_file("fallback-bad", &bad);
        let good_path = temp_file(
            "fallback-good",
            &plan_file_bytes(&TestPlan::stereo_identity()),
        );
        let json = run_plan_config(Some(&bad_path), None, Some(&good_path), 4)
            .expect("last-good fallback run");
        let _ = std::fs::remove_file(&bad_path);
        let _ = std::fs::remove_file(&good_path);
        assert!(json.contains("\"source\":\"last_good\""), "{json}");
        assert!(
            json.contains("\"rollback\":\"rejected_primary_readmit_last_good\""),
            "{json}"
        );
        assert!(
            json.contains("\"version\":1"),
            "fresh registry 重新准入: {json}"
        );
        assert!(json.contains("\"retire_ready\":true"), "{json}");
        assert!(json.contains("\"all_ack_blocks\":4"), "{json}");
        assert!(
            json.contains("已回退 last-good"),
            "warnings 必须显式记录回退（不静默）: {json}"
        );
    }

    #[test]
    fn t185_plan_arena_bad_plan_without_last_good_is_rejected() {
        let mut bad = plan_file_bytes(&TestPlan::stereo_identity());
        bad[40] ^= 0x01;
        let bad_path = temp_file("no-fallback", &bad);
        let error =
            run_plan_config(Some(&bad_path), None, None, 2).expect_err("无 last-good 必须拒绝启动");
        let _ = std::fs::remove_file(&bad_path);
        assert!(error.contains("plan-arena 启动被拒绝"), "{error}");
        assert!(error.contains("plan_last_good"), "{error}");
    }

    #[test]
    fn t185_plan_arena_requires_plan_path() {
        let text = config_text(None, None, None, "plan-arena", 2);
        let config_path = temp_file("no-plan", text.as_bytes());
        let error = run_command(config_path.to_str().expect("utf8"), None, None)
            .expect_err("graph=plan-arena 未配置 plan 必须报错");
        let _ = std::fs::remove_file(&config_path);
        assert!(error.contains("[paths] plan"), "{error}");
    }

    #[test]
    fn t185_plan_arena_does_not_change_default_graph_path() {
        // 未选择 plan-arena 时既有 stereo_identity 路径不变（plan 配置字段存在也不影响）。
        let text = config_text(None, None, None, "stereo_identity", 2);
        let config_path = temp_file("default-graph", text.as_bytes());
        let json = run_command(config_path.to_str().expect("utf8"), None, None)
            .expect("stereo_identity run");
        let _ = std::fs::remove_file(&config_path);
        assert!(json.contains("\"graph\":\"stereo_identity\""), "{json}");
        assert!(!json.contains("\"graph\":\"plan-arena\""), "{json}");
        assert!(json.contains("\"synthetic\":false"), "{json}");
    }

    // -----------------------------------------------------------------------
    // T74 前置：file-input（4.1.2 8ch WAV）→ plan-arena 图 → fake 输出链路
    // -----------------------------------------------------------------------

    #[test]
    fn t74_file_input_412_8ch_plan_arena_plays_eof_graceful() {
        use crate::file_input_test_support as fx;
        let plan_path = temp_file(
            "t74-plan-412",
            &plan_file_bytes(&TestPlan::multichannel_412()),
        );
        let wav = fx::wav_bytes(8, 48_000, fx::WavKind::Pcm24, 200, |frame, channel| {
            if channel == 6 {
                0.0
            } else {
                ((frame + channel) as f32 * 0.01).sin() * 0.1
            }
        });
        let wav_path = fx::temp_file("t74-plan-412-wav", &wav);
        let mut text = config_text(Some(&plan_path), None, None, "plan-arena", 4);
        text.push_str(&format!(
            "[capture]\nsource = \"file\"\npath = \"{}\"\nformat = \"wav\"\neof = \"exit\"\n",
            wav_path.to_string_lossy()
        ));
        let config_path = temp_file("t74-plan-412-config", text.as_bytes());
        let result = run_command(config_path.to_str().expect("utf8 config path"), None, None);
        let _ = std::fs::remove_file(&plan_path);
        let _ = std::fs::remove_file(&wav_path);
        let _ = std::fs::remove_file(&config_path);
        let json = result.expect("8ch 4.1.2 plan-arena file run");
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        assert!(json.contains("\"graph\":\"plan-arena\""), "{json}");
        assert!(json.contains("\"capture\":{\"source\":\"file\""), "{json}");
        assert!(json.contains("\"channels\":8"), "{json}");
        assert!(json.contains("\"layout\":\"4.1.2\""), "{json}");
        assert!(json.contains("\"sample_format\":\"S24_3LE\""), "{json}");
        assert!(json.contains("\"eof_reached\":true"), "{json}");
        assert!(json.contains("\"exit_reason\":\"eof\""), "{json}");
        assert!(json.contains("\"blocks\":2"), "{json}");
        assert!(json.contains("\"blocks_available\":2"), "{json}");
        assert!(json.contains("\"tail_padding_frames\":56"), "{json}");
        assert!(json.contains("\"retire_ready\":true"), "{json}");
        assert!(json.contains("\"graceful\":true"), "{json}");
        assert!(json.contains("\"synthetic\":false"), "{json}");
        assert!(json.contains("\"admission\":\"mock\""), "{json}");
    }

    #[test]
    fn t74_file_input_plan_channels_mismatch_rejected() {
        use crate::file_input_test_support as fx;
        let plan_path = temp_file(
            "t74-plan-412-b",
            &plan_file_bytes(&TestPlan::multichannel_412()),
        );
        let wav = fx::wav_bytes(2, 48_000, fx::WavKind::Pcm24, 64, |_, _| 0.0);
        let wav_path = fx::temp_file("t74-plan-412-b-wav", &wav);
        let mut text = config_text(Some(&plan_path), None, None, "plan-arena", 2);
        text.push_str(&format!(
            "[capture]\nsource = \"file\"\npath = \"{}\"\nformat = \"wav\"\n",
            wav_path.to_string_lossy()
        ));
        let config_path = temp_file("t74-plan-412-b-config", text.as_bytes());
        let error = run_command(config_path.to_str().expect("utf8 config path"), None, None)
            .expect_err("plan n_in=8 + 2ch file 必须拒绝（strict）");
        let _ = std::fs::remove_file(&plan_path);
        let _ = std::fs::remove_file(&wav_path);
        let _ = std::fs::remove_file(&config_path);
        assert!(error.starts_with("ERR_HOST_FILE_CHANNELS"), "{error}");
        assert!(error.contains("2"), "{error}");
        assert!(error.contains("8"), "{error}");
    }

    // -----------------------------------------------------------------------
    // T203/T210：中置下混（rt-host 自持 content_map）接入 plan-arena capture -> 图 之前
    // -----------------------------------------------------------------------

    /// T57 overlay layout 预设（与 compat/control 消费同一文件；host 用 content_map 读取）。
    const LAYOUT_PRESETS_OVERLAY: &str =
        include_str!("../../../../audio/overlay/etc/tspi-audio/layout-presets.yml");

    fn layout_presets_path() -> std::path::PathBuf {
        temp_file("t203-layout-presets", LAYOUT_PRESETS_OVERLAY.as_bytes())
    }

    /// plan-arena + [content] 中置下混配置（布局/角色全部来自 T57 overlay preset）。
    fn content_config_text(
        plan: &std::path::Path,
        presets: &std::path::Path,
        input_layout: &str,
        output_layout: &str,
        content_extra: &str,
        capture_body: &str,
        max_blocks: u64,
    ) -> String {
        let mut text = config_text(Some(plan), None, None, "plan-arena", max_blocks);
        text.push_str(&format!(
            "[content]\nlayout_presets = \"{}\"\ninput_layout = \"{}\"\noutput_layout = \"{}\"\n{}\n[capture]\n{}[run]\nmax_blocks = {}\n",
            presets.to_string_lossy(),
            input_layout,
            output_layout,
            content_extra,
            capture_body,
            max_blocks
        ));
        text
    }

    fn run_temp_config(label: &str, text: &str) -> Result<String, String> {
        let config_path = temp_file(label, text.as_bytes());
        let result = run_command(config_path.to_str().expect("utf8 config path"), None, None);
        let _ = std::fs::remove_file(&config_path);
        result
    }

    #[test]
    fn t203_file_input_51_center_downmix_applies_before_plan_graph() {
        use crate::file_input_test_support as fx;
        let plan_path = temp_file(
            "t203-plan-51-412",
            &plan_file_bytes(&TestPlan::multichannel_412_from_51_content()),
        );
        let presets = layout_presets_path();
        // 6ch 5.1：FL FR C LFE SL SR；C 为可辨认满幅信号。
        let wav = fx::wav_bytes(6, 48_000, fx::WavKind::Pcm24, 300, |frame, channel| {
            if channel == 2 {
                (frame as f32 * 0.01).sin() * 0.5
            } else {
                (frame as f32 * 0.02 + channel as f32).sin() * 0.1
            }
        });
        let wav_path = fx::temp_file("t203-51-wav", &wav);
        let capture_body = format!(
            "source = \"file\"\npath = \"{}\"\nformat = \"wav\"\neof = \"exit\"\n",
            wav_path.to_string_lossy()
        );
        let text = content_config_text(
            &plan_path,
            &presets,
            "h16-5.1",
            "4.1.2",
            "center = \"auto\"",
            &capture_body,
            4,
        );
        let result = run_temp_config("t203-51-config", &text);
        for path in [&plan_path, &presets, &wav_path] {
            let _ = std::fs::remove_file(path);
        }
        let json = result.expect("5.1 文件 -> 4.1.2 plan-arena run");
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        assert!(json.contains("\"graph\":\"plan-arena\""), "{json}");
        // capture 布局来自 T57 输入布局（6ch 5.1），不是 2/8 strict。
        assert!(json.contains("\"channels\":6"), "{json}");
        assert!(json.contains("\"layout\":\"h16-5.1\""), "{json}");
        // 内容适配：Applied + C/FL/FR 通道 + 冻结 −3 dB + 默认 −18 dBFS program peak 不 trim。
        assert!(
            json.contains(
                "\"content\":{\"adapter\":\"rt_host::content_map\",\"configured\":true,\"status\":\"applied\",\"applied\":true"
            ),
            "{json}"
        );
        assert!(
            json.contains("\"input_layout\":\"h16-5.1\",\"output_layout\":\"4.1.2\""),
            "{json}"
        );
        assert!(json.contains("\"source_channels\":6"), "{json}");
        assert!(
            json.contains("\"center_channel\":2,\"left_channel\":0,\"right_channel\":1"),
            "{json}"
        );
        assert!(json.contains("\"center_gain_db\":-3.000"), "{json}");
        assert!(json.contains("\"center_gain_linear\":0.707946"), "{json}");
        assert!(json.contains("\"trim_db\":0.000"), "{json}");
        assert!(json.contains("\"within_policy\":true"), "{json}");
        assert!(json.contains("\"applied_blocks\":3"), "{json}");
        assert!(json.contains("\"plan_semantics\":\"unchanged\""), "{json}");
        // plan 语义不变：plan 输入宽度仍 6、adapter n_src=6（C 仍由 plan A_map 丢弃前的
        // 内容层消费，plan A_map/graph 未改）。
        assert!(json.contains("\"n_src\":6"), "{json}");
        assert!(json.contains("\"exit_reason\":\"eof\""), "{json}");
    }

    #[test]
    fn t203_file_input_51_center_present_is_passthrough() {
        use crate::file_input_test_support as fx;
        let plan_path = temp_file(
            "t203-plan-51-51",
            &plan_file_bytes(&TestPlan::multichannel_51_center_present()),
        );
        let presets = layout_presets_path();
        let wav = fx::wav_bytes(6, 48_000, fx::WavKind::Pcm24, 200, |frame, channel| {
            (frame as f32 * 0.01 + channel as f32).sin() * 0.1
        });
        let wav_path = fx::temp_file("t203-51-passthrough-wav", &wav);
        let capture_body = format!(
            "source = \"file\"\npath = \"{}\"\nformat = \"wav\"\neof = \"exit\"\n",
            wav_path.to_string_lossy()
        );
        let text = content_config_text(
            &plan_path,
            &presets,
            "h16-5.1",
            "5.1",
            "center = \"auto\"",
            &capture_body,
            4,
        );
        let result = run_temp_config("t203-51-passthrough-config", &text);
        for path in [&plan_path, &presets, &wav_path] {
            let _ = std::fs::remove_file(path);
        }
        let json = result.expect("5.1 -> 5.1（有中置）passthrough run");
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        // 输出布局含 C（有中置配置）：适配器 DisabledCenterPresent，不改任何样本/不计数。
        assert!(
            json.contains("\"status\":\"disabled_center_present\",\"applied\":false"),
            "{json}"
        );
        assert!(json.contains("\"applied_blocks\":0"), "{json}");
        assert!(json.contains("\"center_channel\":null"), "{json}");
    }

    #[test]
    fn t203_content_hot_program_peak_reports_global_trim() {
        use crate::file_input_test_support as fx;
        let plan_path = temp_file(
            "t203-plan-trim",
            &plan_file_bytes(&TestPlan::multichannel_412_from_51_content()),
        );
        let presets = layout_presets_path();
        let wav = fx::wav_bytes(6, 48_000, fx::WavKind::F32, 200, |frame, channel| {
            ((frame + channel) as f32 * 0.001).sin() * 0.9
        });
        let wav_path = fx::temp_file("t203-trim-wav", &wav);
        let capture_body = format!(
            "source = \"file\"\npath = \"{}\"\nformat = \"wav\"\neof = \"exit\"\n",
            wav_path.to_string_lossy()
        );
        let text = content_config_text(
            &plan_path,
            &presets,
            "h16-5.1",
            "4.1.2",
            "center = \"auto\"\nprogram_peak_dbfs = -3.0",
            &capture_body,
            4,
        );
        let result = run_temp_config("t203-trim-config", &text);
        for path in [&plan_path, &presets, &wav_path] {
            let _ = std::fs::remove_file(path);
        }
        let json = result.expect("hot program（-3 dBFS）trim run");
        // 预测峰值 = -3 + 4.649 = +1.649 dBFS，超 D7 -6 dBFS 门 -> trim ≈ -7.649 dB。
        assert!(json.contains("\"trim_db\":-7.6"), "{json}");
        assert!(json.contains("\"within_policy\":true"), "{json}");
        assert!(json.contains("\"applied_blocks\":2"), "{json}");
    }

    #[test]
    fn t203_content_unknown_input_layout_rejected() {
        let plan_path = temp_file(
            "t203-plan-bad-layout",
            &plan_file_bytes(&TestPlan::multichannel_412_from_51_content()),
        );
        let presets = layout_presets_path();
        let text = content_config_text(
            &plan_path,
            &presets,
            "no-such-input",
            "4.1.2",
            "center = \"auto\"",
            "",
            2,
        );
        let error = run_temp_config("t203-bad-layout-config", &text)
            .expect_err("未知输入布局必须拒绝启动（不静默 passthrough）");
        for path in [&plan_path, &presets] {
            let _ = std::fs::remove_file(path);
        }
        assert!(error.starts_with("ERR_HOST_CONTENT_LAYOUT"), "{error}");
        assert!(error.contains("E_CHANNEL_MAP_LAYOUT_UNKNOWN"), "{error}");
    }

    #[test]
    fn t203_content_requires_both_layouts() {
        let plan_path = temp_file(
            "t203-plan-half",
            &plan_file_bytes(&TestPlan::multichannel_412_from_51_content()),
        );
        let presets = layout_presets_path();
        let mut text = config_text(Some(&plan_path), None, None, "plan-arena", 2);
        text.push_str(&format!(
            "[content]\nlayout_presets = \"{}\"\ninput_layout = \"h16-5.1\"\n",
            presets.to_string_lossy()
        ));
        let error = run_temp_config("t203-half-config", &text)
            .expect_err("只配 input_layout 必须拒绝（不静默 passthrough）");
        for path in [&plan_path, &presets] {
            let _ = std::fs::remove_file(path);
        }
        assert!(error.starts_with("ERR_HOST_CONTENT_CONFIG"), "{error}");
    }

    #[test]
    fn t203_content_output_container_mismatch_rejected() {
        let plan_path = temp_file(
            "t203-plan-container",
            &plan_file_bytes(&TestPlan::multichannel_412_from_51_content()),
        );
        let presets = layout_presets_path();
        // plan.n_out=8（4.1.2 容器）但声明 5.1 输出布局（容器 6）：布局与 plan 不一致必须拒绝。
        let text = content_config_text(
            &plan_path,
            &presets,
            "h16-5.1",
            "5.1",
            "center = \"auto\"",
            "",
            2,
        );
        let error = run_temp_config("t203-container-config", &text)
            .expect_err("输出布局容器宽度与 plan.n_out 不符必须拒绝");
        for path in [&plan_path, &presets] {
            let _ = std::fs::remove_file(path);
        }
        assert!(error.starts_with("ERR_HOST_CONTENT_LAYOUT"), "{error}");
        assert!(error.contains("plan.n_out"), "{error}");
    }

    #[test]
    fn t203_content_center_policy_conflict_rejected() {
        let plan_path = temp_file(
            "t203-plan-policy",
            &plan_file_bytes(&TestPlan::multichannel_412_from_51_content()),
        );
        let presets = layout_presets_path();
        let text = content_config_text(
            &plan_path,
            &presets,
            "h16-5.1",
            "4.1.2",
            "center = \"present\"",
            "",
            2,
        );
        let error = run_temp_config("t203-policy-config", &text)
            .expect_err("声明中置存在但输出布局无 C 必须拒绝");
        for path in [&plan_path, &presets] {
            let _ = std::fs::remove_file(path);
        }
        assert!(error.starts_with("ERR_HOST_CONTENT_LAYOUT"), "{error}");
        assert!(
            error.contains("E_CHANNEL_MAP_CENTER_POLICY_CONFLICT"),
            "{error}"
        );
    }

    // ---- task-246：partitioned（shared-FFT）引擎 ----

    /// task-246：G 形状 plan（8ch 容器 / 5 role（slot 0,1,2,3,7）/ 5x5 G / L_G=taps / D）。
    ///
    /// diag path 系数 = 小 G cell + delta_D（graph_from_plan 会减掉 baseline 得到 cell）；
    /// offdiag path = 小 G cell。用于 partitioned 与 T07 direct 的数值对拍。
    fn t246_g_plan(taps: u32, d: u32) -> TestPlan {
        let containers = [0u16, 1, 2, 3, 7];
        let tiny = |seed: u64, t: usize| -> f32 {
            let value =
                ((seed.wrapping_mul(1103515245).wrapping_add(t as u64 * 12345)) >> 8) % 2000;
            value as f32 * 1.0e-5 - 0.01
        };
        let mut roles = Vec::new();
        let mut paths = Vec::new();
        let mut diags = Vec::new();
        for (slot, out_index) in containers.iter().enumerate() {
            roles.push(TestRole {
                out_index: *out_index,
                flags: PLAN_ROLE_FLAG_ENABLED,
                delay_samples: 0,
                gain_q15: 32767,
                polarity: 1,
                peq_count: 0,
            });
            let mut coeffs: Vec<f32> = (0..taps as usize)
                .map(|t| tiny(slot as u64 + 1, t))
                .collect();
            coeffs[d as usize] += 1.0;
            paths.push(TestPath {
                in_index: slot as u16,
                out_index: *out_index,
                taps,
                delay_samples: d,
                flags: PLAN_PATH_FLAG_DIAG_BASELINE,
                coeffs,
            });
            diags.push(TestDiag {
                out_index: *out_index,
                taps: 1,
                coeffs: vec![1.0],
            });
        }
        for (j, out_index) in containers.iter().enumerate() {
            for (k, _) in containers.iter().enumerate() {
                if j == k {
                    continue;
                }
                let coeffs: Vec<f32> = (0..taps as usize)
                    .map(|t| tiny((j * 8 + k) as u64 + 100, t))
                    .collect();
                paths.push(TestPath {
                    in_index: k as u16,
                    out_index: *out_index,
                    taps,
                    delay_samples: 0,
                    flags: 0,
                    coeffs,
                });
            }
        }
        paths.sort_by_key(|path| (path.out_index, path.in_index));
        TestPlan {
            n_in: 8,
            n_out: 8,
            roles,
            paths,
            diags,
            iir: Vec::new(),
        }
    }

    #[test]
    fn t246_partitioned_engine_matches_direct_reference() {
        // T08 冻结判据：partitioned f32 与 direct f32 参考 rel L2 <= 1e-5（FFT 舍入，非逐位）。
        let plan = t246_g_plan(257, 64);
        let (graph, _) = adapter(&plan).expect("g 形状 plan");
        let blocks = 3usize;
        let block = T_BLOCK as usize;
        let frames = blocks * block;
        let mut source = vec![vec![0.0f32; frames]; 8];
        for channel in 0..8usize {
            for n in 0..frames {
                source[channel][n] = ((n as f32) * 0.011 + channel as f32 * 0.7).sin() * 0.3;
            }
        }
        let direct = direct_stream(&graph, &source, blocks).expect("T07 direct 参考");
        let mut runtime = PartitionedRuntime::build(&graph, block).expect("partitioned build");
        let containers = graph.topo.container_channels as usize;
        let mut input = vec![0.0f32; 8 * block];
        let mut output = vec![0.0f32; containers * block];
        let mut pipeline: Vec<Vec<f32>> = vec![Vec::new(); containers];
        for b in 0..blocks {
            for channel in 0..8usize {
                input[channel * block..(channel + 1) * block]
                    .copy_from_slice(&source[channel][b * block..(b + 1) * block]);
            }
            output.iter_mut().for_each(|sample| *sample = 0.0);
            runtime
                .run_block(&graph, &input, block, &mut output, block, block, block)
                .expect("partitioned block");
            for channel in 0..containers {
                pipeline[channel]
                    .extend_from_slice(&output[channel * block..(channel + 1) * block]);
            }
        }
        let mut num = 0.0f64;
        let mut den = 0.0f64;
        let mut max_abs = 0.0f64;
        for channel in 0..containers {
            for n in 0..frames {
                let reference = direct[channel][n] as f64;
                let value = pipeline[channel][n] as f64;
                let diff = (reference - value).abs();
                num += diff * diff;
                den += reference * reference;
                if diff > max_abs {
                    max_abs = diff;
                }
            }
        }
        let rel_l2 = (num / den.max(1.0e-30)).sqrt();
        assert!(
            rel_l2 <= 1.0e-5,
            "partitioned vs direct rel_l2={rel_l2}（T08 f32 门 1e-5），max_abs={max_abs}"
        );
        assert!(max_abs < 1.0e-4, "partitioned max_abs={max_abs}");
        for channel in [4usize, 5, 6] {
            assert!(
                pipeline[channel].iter().all(|sample| *sample == 0.0),
                "未使用容器通道 {channel} 必须严格 +0.0"
            );
        }
    }

    #[test]
    fn t246_partitioned_engine_matches_direct_with_nonidentity_p_and_roles() {
        // 真实 plan 的 P 行可能带 T64 target FIR / role delay/polarity/gain；partitioned 的
        // build_f32 会把 A/G/P 融合为 route FIR（T03 SPEC §2.7）。这里用 P FIR + role 元数据
        // 验证融合语义与 direct 等价（T08 f32 门 1e-5）。
        let mut plan = t246_g_plan(257, 64);
        plan.diags[0].taps = 3;
        plan.diags[0].coeffs = vec![0.9, 0.2, -0.15];
        plan.roles[0].delay_samples = 2;
        plan.roles[1].polarity = -1;
        plan.roles[2].gain_q15 = 16384;
        let (graph, _) = adapter(&plan).expect("非恒等 P/role 的 g 形状 plan");
        assert!(graph.topo.l_p > 1, "P FIR/role delay 应折叠成 l_p>1");
        let blocks = 3usize;
        let block = T_BLOCK as usize;
        let frames = blocks * block;
        let mut source = vec![vec![0.0f32; frames]; 8];
        for channel in 0..8usize {
            for n in 0..frames {
                source[channel][n] = ((n as f32) * 0.009 + channel as f32 * 0.5).cos() * 0.25;
            }
        }
        let direct = direct_stream(&graph, &source, blocks).expect("T07 direct 参考");
        let mut runtime = PartitionedRuntime::build(&graph, block).expect("partitioned build");
        let containers = graph.topo.container_channels as usize;
        let mut input = vec![0.0f32; 8 * block];
        let mut output = vec![0.0f32; containers * block];
        let mut pipeline: Vec<Vec<f32>> = vec![Vec::new(); containers];
        for b in 0..blocks {
            for channel in 0..8usize {
                input[channel * block..(channel + 1) * block]
                    .copy_from_slice(&source[channel][b * block..(b + 1) * block]);
            }
            output.iter_mut().for_each(|sample| *sample = 0.0);
            runtime
                .run_block(&graph, &input, block, &mut output, block, block, block)
                .expect("partitioned block");
            for channel in 0..containers {
                pipeline[channel]
                    .extend_from_slice(&output[channel * block..(channel + 1) * block]);
            }
        }
        let mut num = 0.0f64;
        let mut den = 0.0f64;
        let mut max_abs = 0.0f64;
        for channel in 0..containers {
            for n in 0..frames {
                let reference = direct[channel][n] as f64;
                let value = pipeline[channel][n] as f64;
                let diff = (reference - value).abs();
                num += diff * diff;
                den += reference * reference;
                if diff > max_abs {
                    max_abs = diff;
                }
            }
        }
        let rel_l2 = (num / den.max(1.0e-30)).sqrt();
        assert!(
            rel_l2 <= 1.0e-5,
            "P FIR/role 融合 partitioned vs direct rel_l2={rel_l2}，max_abs={max_abs}"
        );
    }

    #[test]
    fn t246_dsp_engine_config_parse() {
        let mut warnings = Vec::new();
        let config = parse_host_config(
            "[host]\ngraph = \"plan-arena\"\n[dsp]\nengine = \"partitioned\"\n",
            &mut warnings,
        )
        .expect("parse [dsp] engine");
        assert_eq!(config.dsp_engine, "partitioned");
        let default_config = parse_host_config("[host]\ngraph = \"plan-arena\"\n", &mut warnings)
            .expect("parse default engine");
        assert_eq!(default_config.dsp_engine, "auto");
        let error = parse_host_config("[dsp]\nengine = \"bogus\"\n", &mut warnings)
            .expect_err("未知 engine 必须拒绝");
        assert!(error.contains("direct|partitioned|auto"), "{error}");
    }

    #[test]
    fn t246_auto_engine_selects_partitioned_for_large_plan() {
        // L_F = 1 + 2048 + 1 - 2 = 2048 -> auto 走 partitioned；单执行器真实完成整块后
        // task-284 把"覆盖 plan 全部 worker lane"映射为 single-executor all-ack（明示口径）。
        let plan = t246_g_plan(2048, 1024);
        let plan_path = temp_file("t246-auto-g", &plan_file_bytes(&plan));
        let json = run_plan_config(Some(&plan_path), None, None, 3).expect("plan-arena run");
        let _ = std::fs::remove_file(&plan_path);
        assert!(
            json.contains("\"engine\":{\"requested\":\"auto\",\"effective\":\"partitioned\""),
            "{json}"
        );
        assert!(json.contains("\"dsp\":\"partitioned-fft-upols\""), "{json}");
        assert!(json.contains("\"blocks\":3"), "{json}");
        assert!(json.contains("\"all_ack_blocks\":3"), "{json}");
        assert!(json.contains("\"retire_ready\":true"), "{json}");
        assert!(json.contains("\"acks\":4"), "{json}");
        assert!(
            json.contains("\"retire_ready_source\":\"note_block_all_ack\""),
            "{json}"
        );
        assert!(json.contains("\"synthetic\":false"), "{json}");
        assert!(json.contains("\"single_executor\":true"), "{json}");
        assert!(
            json.contains("\"executor\":\"partitioned-fft-upols\""),
            "{json}"
        );
    }

    #[test]
    fn t284_partitioned_mute_blocks_produce_no_ack() {
        // SafetyMute/未执行块不得产生 ack（单执行器 all-ack 也只覆盖真实执行过的块）。
        let plan = t246_g_plan(2048, 1024);
        let plan_path = temp_file("t284-part-mute", &plan_file_bytes(&plan));
        let mut text = config_text(Some(&plan_path), None, None, "plan-arena", 3)
            .replace("safe_mode = \"normal\"", "safe_mode = \"mute\"");
        text = text.replace("[paths]\n", "[dsp]\nengine = \"partitioned\"\n[paths]\n");
        let json = run_temp_config("t284-part-mute-config", &text).expect("partitioned mute run");
        let _ = std::fs::remove_file(&plan_path);
        assert!(
            json.contains("\"engine\":{\"requested\":\"partitioned\",\"effective\":\"partitioned\""),
            "{json}"
        );
        assert!(json.contains("\"blocks\":3"), "{json}");
        assert!(json.contains("\"all_ack_blocks\":0"), "{json}");
        assert!(json.contains("\"retire_ready\":false"), "{json}");
        assert!(json.contains("\"single_executor\":true"), "{json}");
    }

    /// task-part-adapter：带 `[dsp] engine` 的 plan-arena 端到端配置（fake 后端，不打开设备）。
    fn run_plan_config_engine(
        engine: &str,
        plan: &std::path::Path,
        max_blocks: u64,
    ) -> Result<String, String> {
        let text = config_text(Some(plan), None, None, "plan-arena", max_blocks).replace(
            "[paths]\n",
            &format!("[dsp]\nengine = \"{engine}\"\n[paths]\n"),
        );
        let config_path = temp_file("part-adapter-engine-config", text.as_bytes());
        let result = run_command(config_path.to_str().expect("utf8 config path"), None, None);
        let _ = std::fs::remove_file(&config_path);
        result
    }

    /// task-part-adapter：生产 4096-tap P_post FIR plan（融合 L_F=4103）形状。
    ///
    /// G 形状沿用 t246_g_plan(8, 0)（5 role / 5x5 G / L_G=8），把 role0 的 P_post FIR 设为
    /// 4096 tap -> 适配器拓扑 l_a=1、l_g=8、l_p=4096 -> L_F = 1+8+4096-2 = 4103（> direct 4096，
    /// <= partitioned extended 16384）。plan.header.max_taps 仍为 4096（T20 准入口径不变）。
    fn part_adapter_p4096_plan() -> TestPlan {
        let mut plan = t246_g_plan(8, 0);
        let taps = 4096usize;
        let coeffs: Vec<f32> = (0..taps)
            .map(|t| {
                if t == 0 {
                    1.0
                } else {
                    ((t * 37) % 101) as f32 * 1.0e-4 - 0.005
                }
            })
            .collect();
        plan.diags[0].taps = taps as u32;
        plan.diags[0].coeffs = coeffs;
        plan
    }

    #[test]
    fn part_adapter_4096_tap_p_plan_loads_and_runs_on_partitioned() {
        let plan = part_adapter_p4096_plan();
        // 1) 加载门（graph_from_plan）必须接受：partitioned extended 口径可承载 L_F=4103。
        let (graph, adapter_json) = adapter(&plan).expect("4096-tap P plan 必须能加载");
        assert_eq!(graph.topo.l_g, 8);
        assert_eq!(graph.topo.l_p, 4096);
        assert_eq!(graph.total_taps(), 4103);
        assert!(adapter_json.contains("\"l_p\":4096"), "{adapter_json}");
        // 2) direct 拓扑门保持旧口径（4096）：direct 引擎仍不能承载该图（不静默降级）。
        assert_eq!(
            graph.layout().err().map(|status| status.code()),
            Some(RtStatus::ERR_TAPS.code())
        );
        // 3) partitioned extended 构建 + 真实跑 block。
        let block = T_BLOCK as usize;
        let mut runtime = PartitionedRuntime::build(&graph, block).expect("extended partitioned");
        assert_eq!(runtime.layout.taps, 4103);
        assert_eq!(runtime.layout.partitions, 33);
        let containers = graph.topo.container_channels as usize;
        let mut input = vec![0.0f32; graph.topo.n_src as usize * block];
        let mut output = vec![0.0f32; containers * block];
        for b in 0..2usize {
            for (index, value) in input.iter_mut().enumerate() {
                *value = ((index + b) as f32 * 0.017).sin() * 0.25;
            }
            output.iter_mut().for_each(|value| *value = 0.0);
            runtime
                .run_block(&graph, &input, block, &mut output, block, block, block)
                .expect("extended partitioned block");
        }
        assert!(output.iter().all(|value| value.is_finite()));
        assert!(output.iter().any(|value| *value != 0.0));
    }

    #[test]
    fn part_adapter_4096_tap_p_plan_engine_matrix_end_to_end() {
        let plan = part_adapter_p4096_plan();
        let plan_path = temp_file("part-adapter-p4096", &plan_file_bytes(&plan));
        // engine=partitioned：加载并运行（engine.taps=4103 / partitions=33）。
        let json = run_plan_config_engine("partitioned", &plan_path, 3).expect("partitioned run");
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        assert!(
            json.contains(
                "\"engine\":{\"requested\":\"partitioned\",\"effective\":\"partitioned\""
            ),
            "{json}"
        );
        assert!(json.contains("\"dsp\":\"partitioned-fft-upols\""), "{json}");
        assert!(json.contains("\"taps\":4103"), "{json}");
        assert!(json.contains("\"partitions\":33"), "{json}");
        // engine=direct：保持旧行为 fail closed（status 110，不静默降级到 partitioned）。
        let error = run_plan_config_engine("direct", &plan_path, 3)
            .expect_err("direct 引擎承载 4103-tap 融合图必须 fail closed");
        assert!(error.contains("status 110"), "{error}");
        // engine=auto：L_F=4103 >= 2048 -> 选 partitioned（与 task-246 语义一致）。
        let json = run_plan_config_engine("auto", &plan_path, 3).expect("auto run");
        assert!(
            json.contains("\"engine\":{\"requested\":\"auto\",\"effective\":\"partitioned\""),
            "{json}"
        );
        let _ = std::fs::remove_file(&plan_path);
    }

    #[test]
    fn t203_content_requires_plan_arena_graph() {
        let text = format!(concat!(
            "[host]\nblock_size = 128\nsample_rate = 48000\nworkers = 4\n",
            "graph = \"stereo_identity\"\nsafe_mode = \"normal\"\n",
            "[alsa]\nbackend = \"fake\"\n",
            "[content]\ninput_layout = \"h16-5.1\"\noutput_layout = \"4.1.2\"\n",
            "[run]\nmax_blocks = 2\n"
        ));
        let error = run_temp_config("t203-legacy-graph-config", &text)
            .expect_err("旧 2ch 图 + [content] 必须显式拒绝");
        assert!(error.starts_with("ERR_HOST_CONTENT_CONFIG"), "{error}");
        assert!(error.contains("plan-arena"), "{error}");
    }

    #[test]
    fn t203_file_6ch_without_content_declaration_rejected() {
        use crate::file_input_test_support as fx;
        let plan_path = temp_file(
            "t203-plan-6ch-strict",
            &plan_file_bytes(&TestPlan::multichannel_412_from_51_content()),
        );
        let wav = fx::wav_bytes(6, 48_000, fx::WavKind::Pcm24, 32, |_, _| 0.1);
        let wav_path = fx::temp_file("t203-6ch-strict-wav", &wav);
        let mut text = config_text(Some(&plan_path), None, None, "plan-arena", 2);
        text.push_str(&format!(
            "[capture]\nsource = \"file\"\npath = \"{}\"\nformat = \"wav\"\n",
            wav_path.to_string_lossy()
        ));
        let error = run_temp_config("t203-6ch-strict-config", &text)
            .expect_err("未声明 [content] 时 6ch 文件仍按 strict 拒绝");
        for path in [&plan_path, &wav_path] {
            let _ = std::fs::remove_file(path);
        }
        assert!(error.starts_with("ERR_HOST_FILE_LAYOUT"), "{error}");
    }

    #[test]
    fn t203_content_unconfigured_keeps_plan_arena_path() {
        // 未配置 [content]：content 段显式 disabled（不冒充 applied），plan 路径行为不变。
        let plan_path = temp_file(
            "t203-plan-unconfigured",
            &plan_file_bytes(&TestPlan::stereo_identity()),
        );
        let result = run_plan_config(Some(&plan_path), None, None, 2);
        let _ = std::fs::remove_file(&plan_path);
        let json = result.expect("未配置 [content] 的 plan-arena run");
        assert!(
            json.contains(
                "\"content\":{\"adapter\":\"none\",\"configured\":false,\"status\":\"disabled\""
            ),
            "{json}"
        );
        assert!(json.contains("\"applied_blocks\":0"), "{json}");
        assert!(json.contains("\"status\":\"ok\""), "{json}");
    }
}

// ---------------------------------------------------------------------------
// T203/T210 单测：ContentAdapter（rt-host content_map 接入语义；不依赖 plan.bin/文件 I/O）
// ---------------------------------------------------------------------------
#[cfg(test)]
mod center_downmix_host_tests {
    use super::*;
    use khaen_rt_host::content_map::{
        center_downmix_worst_case_added_db, CenterDownmixHeadroom, CenterDownmixStatus,
        CENTER_DOWNMIX_GAIN_LINEAR,
    };

    const LAYOUT_PRESETS_OVERLAY: &str =
        include_str!("../../../../audio/overlay/etc/tspi-audio/layout-presets.yml");

    fn presets() -> LayoutPresets {
        LayoutPresets::parse_overlay(LAYOUT_PRESETS_OVERLAY.as_bytes())
            .expect("T57 overlay layout 预设必须可解析")
    }

    fn adapter_51_to_412() -> ContentAdapter {
        ContentAdapter::build(
            &presets(),
            "h16-5.1",
            "4.1.2",
            CenterSpeakerState::Auto,
            CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS,
            6,
            8,
        )
        .expect("5.1 -> 4.1.2 中置下混适配器")
    }

    /// 6ch 5.1 planar（FL FR C LFE SL SR）：每通道独立可辨，C 满幅 ±1。
    fn block_51(frames: usize) -> Vec<f32> {
        let mut planar = vec![0.0f32; 6 * frames];
        for frame in 0..frames {
            planar[frame] = 0.25;
            planar[frames + frame] = -0.25;
            planar[2 * frames + frame] = if frame % 2 == 0 { 1.0 } else { -1.0 };
            planar[3 * frames + frame] = 0.5;
            planar[4 * frames + frame] = 0.125;
            planar[5 * frames + frame] = -0.125;
        }
        planar
    }

    #[test]
    fn t203_adapter_status_channels_and_default_headroom() {
        let adapter = adapter_51_to_412();
        assert_eq!(adapter.status_str(), "applied");
        assert!(adapter.is_applied());
        assert_eq!(adapter.plan.status, CenterDownmixStatus::Applied);
        assert_eq!(adapter.plan.center_channel, Some(2));
        assert_eq!(adapter.plan.left_channel, Some(0));
        assert_eq!(adapter.plan.right_channel, Some(1));
        assert_eq!(adapter.source_channels(), 6);
        assert_eq!(adapter.input_layout_id(), "h16-5.1");
        // D7：默认 −18 dBFS program peak 预测峰值 −13.351 dBFS 在 −6 dBFS 门内 -> 不 trim。
        assert_eq!(adapter.plan.headroom.trim_db, 0.0);
        assert!(adapter.plan.headroom.within_policy);
        assert_eq!(adapter.plan.headroom.center_gain_db, -3.0);
    }

    #[test]
    fn t203_apply_mixes_center_minus_3db_and_zeroes_center() {
        let adapter = adapter_51_to_412();
        let frames = 128usize;
        let mut planar = block_51(frames);
        let before_other: Vec<f32> = planar[3 * frames..].to_vec();
        adapter.apply_block(&mut planar, frames).expect("apply");
        let gain = adapter.plan.headroom.center_gain_linear;
        assert_eq!(gain, CENTER_DOWNMIX_GAIN_LINEAR, "冻结 −3 dB 系数");
        assert!((gain - 0.707_945_8).abs() < 1e-6);
        for frame in 0..frames {
            let center = if frame % 2 == 0 { 1.0f32 } else { -1.0f32 };
            assert_eq!(planar[frame], 0.25 + gain * center, "FL 混入 C（−3 dB）");
            assert_eq!(
                planar[frames + frame],
                -0.25 + gain * center,
                "FR 混入 C（−3 dB）"
            );
            assert_eq!(planar[2 * frames + frame], 0.0, "C 源必须置零（防重复混）");
        }
        assert_eq!(
            &planar[3 * frames..],
            &before_other[..],
            "其余源（LFE/SL/SR）逐样本不变"
        );
    }

    #[test]
    fn t203_second_apply_does_not_repeat_center() {
        let adapter = adapter_51_to_412();
        let frames = 64usize;
        let mut planar = block_51(frames);
        adapter
            .apply_block(&mut planar, frames)
            .expect("first apply");
        let after_first = planar.clone();
        adapter
            .apply_block(&mut planar, frames)
            .expect("second apply");
        assert_eq!(planar, after_first, "C 已置零，重复 apply 不得再叠加中置");
    }

    #[test]
    fn t203_hot_program_trims_global_headroom() {
        let adapter = ContentAdapter::build(
            &presets(),
            "h16-5.1",
            "4.1.2",
            CenterSpeakerState::Auto,
            -3.0,
            6,
            8,
        )
        .expect("hot program（-3 dBFS）适配器");
        let expected = CenterDownmixHeadroom::compute(-3.0).expect("D7 headroom");
        assert!(expected.trim_db < 0.0);
        assert!(expected.within_policy);
        assert_eq!(adapter.plan.headroom.trim_db, expected.trim_db);
        assert!(
            (adapter.plan.headroom.worst_case_added_db - center_downmix_worst_case_added_db())
                .abs()
                < 1e-12
        );
        let frames = 32usize;
        let mut planar = block_51(frames);
        adapter
            .apply_block(&mut planar, frames)
            .expect("trim apply");
        let trim = expected.trim_linear();
        let gain = expected.center_gain_linear;
        for frame in 0..frames {
            let center = if frame % 2 == 0 { 1.0f32 } else { -1.0f32 };
            let trimmed_fl = 0.25f32 * trim;
            let trimmed_center = center * trim;
            assert_eq!(
                planar[frame],
                trimmed_fl + gain * trimmed_center,
                "trim 对全部源等量生效后再混入 C"
            );
            assert_eq!(planar[2 * frames + frame], 0.0, "C 源置零");
        }
    }

    #[test]
    fn t203_center_present_layout_is_bit_exact_passthrough() {
        let adapter = ContentAdapter::build(
            &presets(),
            "h16-5.1",
            "5.1",
            CenterSpeakerState::Auto,
            CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS,
            6,
            6,
        )
        .expect("5.1 输出（含 C）适配器");
        assert_eq!(adapter.status_str(), "disabled_center_present");
        assert!(!adapter.is_applied());
        let frames = 32usize;
        let mut planar = block_51(frames);
        let before = planar.clone();
        adapter
            .apply_block(&mut planar, frames)
            .expect("passthrough apply");
        assert_eq!(planar, before, "有中置配置必须逐位 passthrough");
        // passthrough 也不绕过缓冲契约（不是静默跳过）。
        let error = adapter
            .apply_block(&mut planar[..7], frames)
            .expect_err("长度不符必须拒收");
        assert!(error.starts_with(ERR_HOST_CONTENT_APPLY), "{error}");
    }

    #[test]
    fn t203_input_without_center_is_passthrough() {
        let adapter = ContentAdapter::build(
            &presets(),
            "pc-2ch",
            "4.1.2",
            CenterSpeakerState::Auto,
            CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS,
            2,
            8,
        )
        .expect("2ch -> 4.1.2 适配器");
        assert_eq!(adapter.status_str(), "disabled_no_center_content");
        assert!(!adapter.is_applied());
        let frames = 16usize;
        let mut planar: Vec<f32> = (0..2 * frames).map(|index| index as f32 * 0.01).collect();
        let before = planar.clone();
        adapter
            .apply_block(&mut planar, frames)
            .expect("no-center passthrough");
        assert_eq!(planar, before);
    }

    #[test]
    fn t203_apply_validates_buffer_without_panic() {
        let adapter = adapter_51_to_412();
        let error = adapter
            .apply_block(&mut [], 0)
            .expect_err("frames=0 必须拒收");
        assert!(error.starts_with(ERR_HOST_CONTENT_APPLY), "{error}");
        let mut short = vec![0.0f32; 6 * 4 - 1];
        let error = adapter
            .apply_block(&mut short, 4)
            .expect_err("长度不符必须拒收");
        assert!(error.starts_with(ERR_HOST_CONTENT_APPLY), "{error}");
    }

    #[test]
    fn t203_adapter_rejects_unknown_layouts_width_and_headroom() {
        let presets = presets();
        let base = || {
            ContentAdapter::build(
                &presets,
                "h16-5.1",
                "4.1.2",
                CenterSpeakerState::Auto,
                CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS,
                6,
                8,
            )
        };
        let error = ContentAdapter::build(
            &presets,
            "no-such-input",
            "4.1.2",
            CenterSpeakerState::Auto,
            CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS,
            6,
            8,
        )
        .expect_err("未知输入布局必须拒绝");
        assert!(error.starts_with(ERR_HOST_CONTENT_LAYOUT), "{error}");
        assert!(error.contains("E_CHANNEL_MAP_LAYOUT_UNKNOWN"), "{error}");
        let error = ContentAdapter::build(
            &presets,
            "h16-5.1",
            "4.1.2",
            CenterSpeakerState::Auto,
            CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS,
            2,
            8,
        )
        .expect_err("输入宽度不符必须拒绝");
        assert!(error.starts_with(ERR_HOST_CONTENT_LAYOUT), "{error}");
        assert!(error.contains("输入宽度"), "{error}");
        let error = ContentAdapter::build(
            &presets,
            "h16-5.1",
            "4.1.2",
            CenterSpeakerState::Auto,
            CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS,
            6,
            6,
        )
        .expect_err("输出容器宽度不符必须拒绝");
        assert!(error.starts_with(ERR_HOST_CONTENT_LAYOUT), "{error}");
        assert!(error.contains("plan.n_out"), "{error}");
        let error = ContentAdapter::build(
            &presets,
            "h16-5.1",
            "4.1.2",
            CenterSpeakerState::Auto,
            f64::NAN,
            6,
            8,
        )
        .expect_err("NaN program peak 必须拒绝");
        assert!(error.contains("E_CHANNEL_MAP_HEADROOM_CONFIG"), "{error}");
        // 合法基线仍可构建（上面各负向不改变正常路径）。
        assert_eq!(base().expect("合法基线").status_str(), "applied");
    }

    #[test]
    fn t203_adapter_rejects_center_policy_conflicts() {
        let presets = presets();
        let error = ContentAdapter::build(
            &presets,
            "h16-5.1",
            "4.1.2",
            CenterSpeakerState::Present,
            CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS,
            6,
            8,
        )
        .expect_err("声明 present 但输出布局无 C 必须拒绝");
        assert!(
            error.contains("E_CHANNEL_MAP_CENTER_POLICY_CONFLICT"),
            "{error}"
        );
        let error = ContentAdapter::build(
            &presets,
            "h16-5.1",
            "5.1",
            CenterSpeakerState::Absent,
            CENTER_DOWNMIX_DEFAULT_PROGRAM_PEAK_DBFS,
            6,
            6,
        )
        .expect_err("声明 absent 但输出布局含 C 必须拒绝（不改有中置配置）");
        assert!(
            error.contains("E_CHANNEL_MAP_CENTER_POLICY_CONFLICT"),
            "{error}"
        );
    }

    #[test]
    fn t203_from_config_requires_both_layouts_and_parses_center_state() {
        let mut config = HostRunConfig::default();
        assert!(ContentAdapter::from_config(&config, 2, 8)
            .expect("未配置 [content]")
            .is_none());
        config.content_input_layout = "h16-5.1".to_string();
        let error = ContentAdapter::from_config(&config, 6, 8)
            .expect_err("半配置（只有 input_layout）必须拒绝");
        assert!(error.starts_with(ERR_HOST_CONTENT_CONFIG), "{error}");
        config.content_output_layout = "4.1.2".to_string();
        config.content_center = "bogus".to_string();
        let error = ContentAdapter::from_config(&config, 6, 8).expect_err("center 非法值必须拒绝");
        assert!(error.starts_with(ERR_HOST_CONTENT_CONFIG), "{error}");
        config.content_center = "auto".to_string();
        config.content_layout_presets =
            "no-such-dir/definitely-missing-layout-presets.yml".to_string();
        let error = ContentAdapter::from_config(&config, 6, 8)
            .expect_err("启用 [content] 但 overlay 预设缺失必须拒绝（不降级）");
        assert!(error.starts_with(ERR_HOST_CONTENT_LAYOUT), "{error}");
    }

    #[test]
    fn t203_content_config_parser_reads_section() {
        let text = concat!(
            "[content]\n",
            "layout_presets = \"/etc/tspi-audio/layout-presets.yml\"\n",
            "input_layout = \"h16-5.1\"\n",
            "output_layout = \"4.1.2\"\n",
            "center = \"auto\"\n",
            "program_peak_dbfs = -3.0\n"
        );
        let mut warnings = Vec::new();
        let config = parse_host_config(text, &mut warnings).expect("parse [content]");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(config.content_input_layout, "h16-5.1");
        assert_eq!(config.content_output_layout, "4.1.2");
        assert_eq!(
            config.content_layout_presets,
            "/etc/tspi-audio/layout-presets.yml"
        );
        assert_eq!(config.content_center, "auto");
        assert_eq!(config.content_program_peak_dbfs, -3.0);
        assert!(content_config_declared(&config));
        // 默认：未声明（既有 stereo/4.1.2 路径完全不变）。
        assert!(!content_config_declared(&HostRunConfig::default()));
    }

    #[test]
    fn t203_file_capture_51_requires_content_declaration() {
        use crate::file_input_test_support as fx;
        let wav = fx::wav_bytes(6, 48_000, fx::WavKind::Pcm24, 16, |_, channel| {
            (channel as f32) * 0.01
        });
        let wav_path = fx::temp_file("t203-open-6ch", &wav);
        let mut config = HostRunConfig {
            capture_source: "file".to_string(),
            capture_format: "wav".to_string(),
            capture_eof: "exit".to_string(),
            capture_path: wav_path.to_string_lossy().into_owned(),
            ..HostRunConfig::default()
        };
        // 未声明 [content]：6ch 仍按 strict 拒绝（既有 2/8ch 行为不变）。
        let error = match FileCapture::open(&config, 6, None) {
            Ok(_) => panic!("6ch 无 [content] 必须拒绝"),
            Err(error) => error,
        };
        assert!(error.starts_with("ERR_HOST_FILE_LAYOUT"), "{error}");
        // 声明 5.1 内容布局：接受，布局名 = T57 输入布局 id。
        let adapter = adapter_51_to_412();
        let file = FileCapture::open(&config, 6, Some(&adapter)).expect("5.1 6ch WAV 应接受");
        assert_eq!(file.channels, 6);
        assert_eq!(file.layout, "h16-5.1");
        // raw 6ch 也只在声明内容布局时允许。
        config.capture_format = "raw".to_string();
        config.capture_file_channels = 6;
        config.capture_raw_format = "f32le".to_string();
        let raw = fx::raw_bytes(6, FileSampleKind::F32Le, 8, |_, _| 0.1);
        let raw_path = fx::temp_file("t203-open-6ch-raw", &raw);
        config.capture_path = raw_path.to_string_lossy().into_owned();
        let file = FileCapture::open(&config, 6, Some(&adapter)).expect("5.1 6ch raw 应接受");
        assert_eq!(file.channels, 6);
        let _ = std::fs::remove_file(&wav_path);
        let _ = std::fs::remove_file(&raw_path);
    }

    #[test]
    fn t203_capture_config_allows_6ch_only_with_content_section() {
        let mut config = HostRunConfig {
            capture_source: "file".to_string(),
            capture_format: "raw".to_string(),
            capture_file_channels: 6,
            capture_raw_format: "f32le".to_string(),
            capture_path: "x.raw".to_string(),
            ..HostRunConfig::default()
        };
        let error =
            validate_capture_config(&config).expect_err("未声明 [content] 的 raw 6ch 必须拒绝");
        assert!(error.starts_with("ERR_HOST_FILE_CHANNELS"), "{error}");
        config.content_input_layout = "h16-5.1".to_string();
        config.content_output_layout = "4.1.2".to_string();
        validate_capture_config(&config).expect("声明 [content] 后 raw 6ch 允许");
    }
}

/// run --config 主实现：返回 JSON 摘要字符串。
fn run_command(
    config_path: &str,
    blocks_override: Option<u64>,
    duration_override: Option<u64>,
) -> Result<String, String> {
    run_command_with_sink(config_path, blocks_override, duration_override, None)
}

/// run_command + 可选 sink 注入（**仅单测**：`run` 块循环 + mock ALSA sink 集成测试，
/// 覆盖 F1 Muted→下一块路径；生产入口 run_command 恒传 None）。
fn run_command_with_sink(
    config_path: &str,
    blocks_override: Option<u64>,
    duration_override: Option<u64>,
    injected_sink: Option<Box<dyn PlaybackSink>>,
) -> Result<String, String> {
    use khaen_core::events::{
        EV_DEADLINE_MISS, EV_RUNNING, EV_SAFETY_MUTE_ENTER, EV_STARTING, EV_STOPPED, LANE_CONTROL,
        LANE_WATCHDOG, SEVERITY_INFO, SEVERITY_WARNING,
    };
    use khaen_rt_host::fault::{EventLaneReader, EventLanes, Watchdog, WatchdogAction};
    use khaen_rt_host::scheduler::SchedGate;
    use khaen_rt_host::swap::{
        commit_swap_at_block_boundary, note_block_switched, GraphQuarantine, TspiSwapAckV1,
    };

    let (config, mut warnings) = load_host_config(config_path)?;
    if let Err(error) = khaen_rt_host::cal_control::spawn_server() {
        warnings.push(format!("校准 socket 未监听：{error}"));
    }
    let max_blocks = blocks_override.unwrap_or(config.max_blocks);
    let duration_ms = duration_override.unwrap_or(config.duration_ms);
    // T185：graph="plan-arena" 走 T66 plan/校准图加载路径（独立 4 worker 块循环）；
    // 既有 stereo_identity/2x2_offdiag 生产路径与 F-26 swap 证据语义完全不变。
    if config.graph == "plan-arena" {
        if injected_sink.is_some() {
            return Err(
                "单测注入 sink 不支持 graph=plan-arena（其 sink 由 prepare_backend 创建）"
                    .to_string(),
            );
        }
        return run_plan_arena_command(&config, config_path, max_blocks, duration_ms, warnings);
    }
    // T203：中置下混内容适配只接在 plan-arena 播放链（capture -> plan A_map 之前）。
    // 旧 stereo_identity/2x2_offdiag 图输入固定 2ch、没有 plan 内容适配语义 —— 显式拒绝，
    // 而不是静默忽略 [content] 配置（不谎报"已启用"）。
    if content_config_declared(&config) {
        return Err(capture_error(
            ERR_HOST_CONTENT_CONFIG,
            format!(
                "[content] 中置下混只支持 graph=plan-arena（got graph={}：旧 2ch 图不做内容适配；请配置 [paths] plan 或移除 [content]）",
                config.graph
            ),
        ));
    }
    let graph = graph_for(&config.graph)?;
    let plan = WorkerDagPlan::build(&graph.topo, config.block_size, config.workers, 1)
        .map_err(|status| format!("WorkerDagPlan::build: {}", status.code()))?;
    // F-26：同图 re-plan v2（拓扑/系数逐位相同，仅 plan_version/实例不同），启动/准入路径
    // 一次构建，供后半程把旧的"循环 ack_worker 演示"换成真实 T20 切换（块循环内不再分配）。
    let plan_v2 = WorkerDagPlan::build(&graph.topo, config.block_size, config.workers, 2)
        .map_err(|status| format!("WorkerDagPlan::build(v2): {}", status.code()))?;
    let coeffs = graph.coeffs();
    let layout = graph
        .layout()
        .map_err(|status| format!("DirectLayout::build: {}", status.code()))?;
    validate_backend_config(&config)?;
    // T74 file-input：启动期一次性读入/校验文件（非 RT；块循环只从内存解码，无文件 I/O），
    // 图输入宽度 strict 对齐（默认图 2ch = 2.0；plan-arena 的 n_in 在各自入口校验）。
    let mut file_input = prepare_capture_source(&config, graph.topo.n_src as usize, None)?;
    // T74 前置 + T239：capture 启动计划（T43 AoIP 端点或 userspace AES67；纯解析 fail fast）。
    let capture_plan = plan_capture_input(&config, graph.topo.n_src as usize)?;
    let (sched, topo) = build_sched_plan(&config)?;
    // F6/task-216：apply_affinity=true 时在启动期真实写内核（mlockall 在此、worker/main 随后）；
    // 任何失败返回 Err（fail closed），不允许 warn 后继续假装 RT。
    let mut rt_runtime = RtRuntime::prepare(&config, &sched)?;
    if let Some(warning) = rt_not_applied_warning(&config) {
        warnings.push(warning);
    }
    // T71 前置：backend 选择（fake = 旧契约 JSON 不打开设备；alsa = 真实 duplex sink）。
    // F5：真实设备打开/流启动推迟到 web 探测与 worker/缓冲初始化之后（见下方 2b）。
    let containers_for_sink = graph.topo.container_channels as usize;
    let (mut child, mut web) = start_web(&config)?;
    // F-26：swap 段只走真实证据路径——block 全部 worker 三 phase 完成后由
    // `completed_block_ack` 生成 block ack，再交 `note_block_all_ack`（唯一置 retire_ready 的
    // RT 入口）。初始 plan v1（无旧图）+ 后半程同图 re-plan v2（真实 commit + quarantine + all-ack）。
    let plan_v1_ptr = &plan as *const WorkerDagPlan as u64;
    let plan_v2_ptr = &plan_v2 as *const WorkerDagPlan as u64;
    let mut swap = TspiSwapAckV1::new();
    let mut swap_status = swap.init(1, config.workers, EPOCH_ID, 1, 0);
    let mut swap_switches = 1u32;
    let mut swap_ack_blocks = 0u64;
    let quarantine = GraphQuarantine::new();
    let mut quarantine_slot: Option<usize> = None;
    let mut quarantine_entered = 0u32;
    let mut quarantine_released = 0u32;
    let mut quarantine_pending_release = false;
    let mut lanes = EventLanes::new();
    let mut event_seq = 0u64;
    let mut watchdog = Watchdog::new();
    let mut watchdog_actions = 0u32;
    let ctl = khaen_core::workers::TspiRtControlV1::new();
    ctl.current_plan_version.store(1u32, Ordering::Release);
    // INV-L1：master 首个 block 之前 current_plan_ptr 指向生效图（之后只由 block 边界 commit 替换）。
    ctl.current_plan_ptr.store(plan_v1_ptr, Ordering::Release);
    let mut budget = TickBudget::new(TICK_BUDGET);
    let mut state = vec![0.0f32; layout.state_samples()];
    let block = config.block_size as usize;
    let stride = block;
    let containers = graph.topo.container_channels as usize;
    let mut input = vec![0.0f32; 2 * stride];
    let mut output = vec![0.0f32; containers * stride];
    // F-12：启动时一次创建常驻 worker + 预分配 partials；块循环只借用/复用。
    let mut workers = match rt_runtime.as_ref() {
        Some(runtime) => BlockWorkers::start_rt(&plan, &coeffs, runtime.worker_targets())
            .map_err(|error| format!("BlockWorkers::start_rt: {error}"))?,
        None => BlockWorkers::start(&plan, &coeffs)
            .map_err(|status| format!("BlockWorkers::start: {}", status.code()))?,
    };
    if let Some(runtime) = rt_runtime.as_mut() {
        runtime.mark_workers_applied();
    }
    let safe_mode_mute = config.safe_mode == "mute";
    let mut safety_mute_reason = 0u32;
    let mut safety_mute_blocks = 0u64;
    let mut gate_rejects = 0u64;
    let mut deadline_misses = 0u64;
    let mut blocks = 0u64;
    // 2b) backend sink：设备打开/流启动放到非 RT 初始化（web/worker/缓冲）之后。
    let (fake_alsa_contract, mut sink) =
        prepare_backend_with_sink(&config, containers_for_sink, injected_sink)?;
    // F-v5-2：起播预填（T17 D-07 不变：设备仍由首次 writei 自动启动）。块循环稳态只
    // 写 1 period/块，若不预填则 sw 缓冲只有 1 period 余量 → 每 2 次写 1 次 underrun。
    if let Some(sink_box) = sink.as_mut() {
        if let Err(error) = sink_box.prime_playback() {
            return Err(fail_closed_sink_error(
                &mut **sink_box,
                "ALSA 起播预填失败",
                &error,
            ));
        }
    }
    // T74 前置 + T239：capture 端点/socket（inferno/ravenna 或 userspace AES67）在
    // web/worker/缓冲初始化之后打开（F5）；fake backend 无设备时钟时由 userspace 自带节拍。
    let fake_backend = sink.is_none();
    let mut capture_input = start_capture_input(capture_plan, &config, fake_backend)?;
    let mut capture_pacer = capture_input
        .as_ref()
        .and_then(|capture| capture.pacer(config.block_size as usize, config.sample_rate));
    // F6/task-216：所有子进程/线程创建完成后，主/I-O owner 线程设为 SCHED_FIFO 并绑非 DSP CPU。
    if let Some(runtime) = rt_runtime.as_mut() {
        runtime.apply_main()?;
    }
    let sched_json = scheduler_report_json(&sched, rt_runtime.as_ref());
    let started = std::time::Instant::now();
    let deadline = if duration_ms == 0 {
        None
    } else {
        Some(started + std::time::Duration::from_millis(duration_ms))
    };
    lanes.push(&event_record(
        event_seq,
        0,
        EV_STARTING,
        LANE_CONTROL,
        SEVERITY_INFO,
        0,
        0,
    ));
    event_seq += 1;
    let mut exit_reason = "max_blocks";
    loop {
        if crate::STOP_REQUESTED.load(std::sync::atomic::Ordering::Acquire) {
            exit_reason = "stop_signal";
            break;
        }
        if max_blocks != 0 && blocks >= max_blocks {
            break;
        }
        if let Some(deadline) = deadline {
            if std::time::Instant::now() >= deadline {
                exit_reason = "duration";
                break;
            }
        }
        // T239：backend=fake 时 userspace AES67 capture 自带块节拍（无设备时钟）。
        if let Some(pacer) = capture_pacer.as_mut() {
            pacer.wait_next();
            if let Some(capture) = capture_input.as_mut() {
                capture.note_paced();
            }
        }
        let gate = sched.gate_block(&topo);
        let admit = matches!(gate, SchedGate::Admit);
        if !admit {
            gate_rejects += 1;
        }
        let mut mute_this_block = safe_mode_mute || !admit || block_stats_failed();
        if safety_mute_reason == 0 {
            if !admit {
                safety_mute_reason = RtStatus::RT_E_GUARD_FAIL.code();
            } else if block_stats_failed() {
                safety_mute_reason = RtStatus::RT_E_DEADLINE_HARD.code();
            }
        }
        // backend=alsa：先读一个 capture period（保持 capture 流健康，兼作回环输入/设备时钟）。
        if let Some(sink) = sink.as_mut() {
            if let Some(reason) = alsa_capture_phase(
                sink.as_mut(),
                &mut input,
                stride,
                block,
                &mut lanes,
                &mut event_seq,
                blocks,
            )? {
                // F1 SafetyMute：本块不处理新块（输入已清零），只写静音并计数。
                safety_mute_reason = reason;
                mute_this_block = true;
            }
        } else {
            synth_input_into(blocks as usize, &mut input, stride, block);
        }
        // T74 file-input：文件块覆盖 synth/loopback 输入（loopback 组合已在启动校验拒绝）。
        let file_eof_after_block = file_input_phase(
            file_input.as_mut(),
            &mut input,
            stride,
            block,
            mute_this_block,
        );
        // T74 前置 + T239：AoIP/用户态 AES67 capture period -> 图输入（与 file 同优先级，配置互斥）。
        capture_input_phase(
            capture_input.as_mut(),
            &mut input,
            stride,
            block,
            mute_this_block,
            clock::mono_now_ns(),
        )?;
        khaen_rt_host::cal_control::overlay_live(&mut input, stride, block);
        output.iter_mut().for_each(|sample| *sample = 0.0);
        budget.reset();
        let tick_entry = budget
            .read(&ctl)
            .map_err(|_| "tick budget entry read failed".to_string())?;
        if !mute_this_block {
            workers
                .run_block_4thread(&mut state, &input, stride, &mut output, stride, block)
                .map_err(|status| format!("run block {}: {}", blocks, status.code()))?;
            khaen_rt_host::cal_control::apply_live(&input, stride, &mut output, stride, block);
            khaen_rt_host::cal_control::note_chain_block();
        } else {
            safety_mute_blocks += 1;
        }
        // backend=alsa：图输出（或安全静音）→ T22 S24_3LE 设备字节 → playback period。
        if let Some(sink) = sink.as_mut() {
            alsa_write_phase(
                sink.as_mut(),
                &output,
                stride,
                block,
                mute_this_block,
                &mut lanes,
                &mut event_seq,
                blocks,
            )?;
        }
        let tick_exit = budget
            .read(&ctl)
            .map_err(|_| "tick budget exit read failed".to_string())?;
        let compute = tick_exit.wrapping_sub(tick_entry);
        // F-26：只有**真正执行完**的 block 才是 all-ack 证据（safe-mode/gate 拒绝的块没跑 worker，
        // 不产生 ack）；synthetic/demo ack 一律不写 retire_ready。
        if !mute_this_block {
            let plan_version = workers.plan_version();
            let block_ack = completed_block_ack(blocks, plan_version, config.workers);
            let ack_status = match quarantine_slot {
                Some(slot) if quarantine_pending_release => note_block_switched(
                    &swap,
                    &quarantine,
                    slot,
                    &block_ack,
                    1,
                    EPOCH_ID,
                    plan_version,
                ),
                _ => swap.note_block_all_ack(&block_ack),
            };
            if !ack_status.is_ok() {
                return Err(format!(
                    "block {blocks}: real block all-ack rejected: {}",
                    ack_status.code()
                ));
            }
            swap_ack_blocks += 1;
            if quarantine_pending_release {
                // T20：v2 首个 all-ack block 已把 4 worker 的旧图 quarantine 位清零；
                // 槽释放属于 [NONRT] control 职责，留到退出路径 try_release。
                quarantine_pending_release = false;
            }
        }
        let gate_ns = if config.tick_freq_hz == 0 {
            0
        } else {
            compute.saturating_mul(1_000_000_000) / config.tick_freq_hz as u64
        };
        let deadline_ns = khaen_core::rt::gate_ns_floor(config.sample_rate, config.block_size);
        if gate_ns > deadline_ns {
            deadline_misses += 1;
            lanes.push(&event_record(
                event_seq,
                blocks,
                EV_DEADLINE_MISS,
                LANE_WATCHDOG,
                SEVERITY_WARNING,
                compute.min(u32::MAX as u64) as u32,
                gate_ns.min(u32::MAX as u64) as u32,
            ));
            event_seq += 1;
        }
        if blocks % 8 == 0 {
            lanes.push(&event_record(
                event_seq,
                blocks,
                EV_RUNNING,
                LANE_CONTROL,
                SEVERITY_INFO,
                blocks.min(u32::MAX as u64) as u32,
                0,
            ));
            event_seq += 1;
        }
        ctl.heartbeat_seq
            .store(blocks, std::sync::atomic::Ordering::Release);
        let action = watchdog.observe(&ctl);
        if !matches!(action, WatchdogAction::None) {
            watchdog_actions += 1;
        }
        // T20 **真实** plan 切换（F-26：不再是"循环 ack_worker"演示）：control 侧 arm
        // （pending 写 + ARMED）→ master 在 block 边界 commit → 旧图 v1 入 quarantine →
        // 下一块 v2 的 all-ack 经 note_block_switched 清 quarantine 位。同图 re-plan（系数/拓扑
        // 逐位相同）；生产热切的不同图 plan arena/PlanRegistry 仍 NOT_WIRED。
        if max_blocks >= 4 && blocks == max_blocks / 2 && swap_switches == 1 {
            ctl.pending_plan_version.store(2, Ordering::Relaxed);
            ctl.pending_plan_ptr.store(plan_v2_ptr, Ordering::Relaxed);
            ctl.switch_state
                .store(khaen_core::rt::SWITCH_ARMED, Ordering::Release);
            let committed = commit_swap_at_block_boundary(&ctl)
                .map_err(|status| format!("commit swap v2: {}", status.code()))?;
            workers
                .switch_plan(&plan_v2)
                .map_err(|status| format!("workers switch_plan(v2): {}", status.code()))?;
            swap_status = swap.init(committed, config.workers, EPOCH_ID, committed, plan_v1_ptr);
            if !swap_status.is_ok() {
                return Err(format!("swap init v{}: {}", committed, swap_status.code()));
            }
            let slot = quarantine
                .enter(EPOCH_ID, 1, plan_v1_ptr, config.workers)
                .map_err(|status| format!("quarantine enter(v1): {}", status.code()))?;
            quarantine_slot = Some(slot);
            quarantine_entered += 1;
            quarantine_pending_release = true;
            swap_switches += 1;
        }
        blocks += 1;
        if file_eof_after_block {
            exit_reason = "eof";
            break;
        }
    }
    // 仅 backend=fake（无 ALSA sink）需要 loop 级汇总事件：ALSA 路径的进入事件由 sink 在
    // 真实迁移点上报（每 epoch 恰一次），此处不重复合成。F2：severity=4、value_a=原因码、
    // value_b=epoch_id（core-v1 §8.2 冻结表；safe_mode 显式配置的原因码为 0）。
    if (safety_mute_blocks > 0 || safe_mode_mute) && sink.is_none() {
        lanes.push(&event_record(
            event_seq,
            blocks,
            EV_SAFETY_MUTE_ENTER,
            LANE_CONTROL,
            SEVERITY_FATAL,
            safety_mute_reason,
            EPOCH_ID as u32,
        ));
        event_seq += 1;
    }
    lanes.push(&event_record(
        event_seq,
        blocks,
        EV_STOPPED,
        LANE_CONTROL,
        SEVERITY_INFO,
        blocks.min(u32::MAX as u64) as u32,
        0,
    ));
    // 优雅退出：停止新块（已停）-> 事件 drain（非 RT 读端）-> swap retire ready -> web 子进程收尾。
    let mut event_counts = [0u32; 64];
    let mut drained = 0u32;
    for lane_index in 0..8u8 {
        if let Some(lane) = lanes.lane(lane_index) {
            let reader = EventLaneReader::new(lane);
            while let Some(event) = reader.next() {
                if (event.kind as usize) < event_counts.len() {
                    event_counts[event.kind as usize] += 1;
                    drained += 1;
                }
            }
        }
    }
    let dropped = lanes.dropped_total();
    // backend=alsa：drain → close → 释放 owner；报告在停流前采集（含 xrun/恢复/mute 计数）。
    let mut graceful = true;
    let alsa_owner_json = match sink {
        Some(mut sink) => {
            let report = sink.report();
            if let Err(error) = sink.stop() {
                warnings.push(format!("ALSA 优雅退出失败：{error}"));
                graceful = false;
            }
            report
        }
        None => fake_alsa_contract,
    };
    // T20 退出路径（[NONRT] control 职责）：quarantine 槽只有 remaining==0（4 worker 全 ack）
    // 才允许 try_release；未 all-ack 则保持占用（INV-L5，安全优先，不强制释放）。
    if let Some(slot) = quarantine_slot {
        if quarantine.remaining_mask(slot) == Some(0) && quarantine.try_release(slot).is_ok() {
            quarantine_released += 1;
        }
    }
    let quarantine_remaining = quarantine_slot
        .and_then(|slot| quarantine.remaining_mask(slot))
        .unwrap_or(0);
    swap.sync_retire_ready_flag();
    let swap_ok = swap.validate().is_ok();
    let swap_acks = swap.ack_mask_value().count_ones();
    let swap_ready = swap.is_retire_ready();
    let swap_retire_ready_source = if swap_ready {
        "note_block_all_ack"
    } else {
        "none"
    };
    if let Some(child) = child.as_mut() {
        let _ = child.kill();
        let _ = child.wait();
        web.child_terminated = true;
    }
    let event_kinds: Vec<String> = event_counts
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        .map(|(kind, count)| format!("\"{}\":{}", kind, count))
        .collect();
    let warnings_json = warnings
        .iter()
        .map(|warning| format!("\"{}\"", json_escape_text(warning)))
        .collect::<Vec<_>>()
        .join(",");
    if let Some(capture) = capture_input.as_mut() {
        capture.stop();
    }
    let capture_json = capture_report_json(
        &config,
        file_input.as_ref(),
        capture_input.as_ref(),
        blocks,
        exit_reason,
    );
    Ok(format!(
        "{{\"status\":\"ok\",\"mode\":\"khaen-rt-host-run\",\"schema\":\"{}\",\"config\":\"{}\",\"graph\":\"{}\",\"block_size\":{},\"sample_rate\":{},\"workers\":{},\"blocks\":{},\"frames\":{},\"safe_mode\":\"{}\",\"graceful\":{},\"capture\":{},\"alsa\":{},\"scheduler\":{},\"swap\":{{\"init_status\":{},\"acks\":{},\"switches\":{},\"retire_ready\":{},\"valid\":{},\"synthetic\":false,\"retire_ready_source\":\"{}\",\"all_ack_blocks\":{},\"plan_version\":{},\"quarantine_entered\":{},\"quarantine_released\":{},\"quarantine_remaining\":{}}},\"fault\":{{\"events_drained\":{},\"dropped\":{},\"by_kind\":{{{}}},\"watchdog_actions\":{},\"deadline_misses\":{},\"gate_rejects\":{},\"safety_mute_blocks\":{}}},\"web\":{{\"mode\":\"{}\",\"bind\":\"{}\",\"probe_status\":{},\"probe_body\":\"{}\",\"daemon_pid\":{},\"child_terminated\":{},\"note\":\"{}\"}},\"last_good\":{},\"warnings\":[{}]}}",
        HOST_CONFIG_SCHEMA,
        json_escape_text(config_path),
        json_escape_text(&config.graph),
        config.block_size,
        config.sample_rate,
        config.workers,
        blocks,
        blocks * config.block_size as u64,
        config.safe_mode,
        graceful,
        capture_json,
        alsa_owner_json,
        sched_json,
        swap_status.code(),
        swap_acks,
        swap_switches,
        swap_ready,
        swap_ok,
        swap_retire_ready_source,
        swap_ack_blocks,
        swap.plan_version,
        quarantine_entered,
        quarantine_released,
        quarantine_remaining,
        drained,
        dropped,
        event_kinds.join(","),
        watchdog_actions,
        deadline_misses,
        gate_rejects,
        safety_mute_blocks,
        web.mode,
        json_escape_text(&web.bind),
        web.probe_status.map(|v| v.to_string()).unwrap_or_else(|| "null".to_string()),
        json_escape_text(&web.probe_body),
        web.daemon_pid.map(|v| v.to_string()).unwrap_or_else(|| "null".to_string()),
        web.child_terminated,
        json_escape_text(&web.note),
        last_good_report(&config.last_good),
        warnings_json
    ))
}

/// 占位：块失败判定（T26 语义为整块 invalid；本入口在 run_block_4thread 错误时直接返回）。
fn block_stats_failed() -> bool {
    false
}

fn usage_host_run() -> &'static str {
    concat!(
        "khaen-rt-host run --config <rt-host.toml> [--blocks N] [--duration-ms N]\n",
        "  config schema: khaen-rt-host/v1 (deploy/config/rt-host.toml.example)\n",
        "  backend: fake | alsa (alsa = Linux target only; real device/board NOT_RUN)\n",
        "  rate whitelist 48000/96000 (192k rejected); alsa.period_frames == block_size\n",
        "  graph: stereo_identity | 2x2_offdiag | plan-arena（T185：加载 T66 plan.bin 校准图）\n",
        "  plan-arena: [paths] plan = \"compile/plan.bin\" [+ plan_manifest/plan_last_good]\n",
        "  capture: [capture] source = \"synth\"|\"file\"|\"motu\"|\"inferno\"|\"aes67\"\n",
        "           file = 预渲染 WAV/raw 多声道 48k（S24_3LE|f32；2ch=2.0 / 8ch=4.1.2 strict；eof=silence|exit）\n",
        "           motu = MOTU Toslink ch12/13（mode=toslink + backend=alsa + io_mode=loopback）\n",
        "           inferno = pcm.inferno S32_LE/96k/16ch（mode=dante；PTPv1 虚拟钟；inferno_iface 必填）\n",
        "           aes67 = mode=aes67；aes67_transport=ravenna（默认）= RAVENNA LKM hw:<card>,0 S32_LE/48k\n",
        "                   aes67_transport=userspace = 自研用户态 RTP 接收端（SDP→组播/单播；48k/128 帧）\n",
        "                   userspace 需 aes67_sdp=<SDP 文件>；可选 aes67_iface（IPv4/网卡名；逗号列表或\n",
        "                   单行数组 = 多接口；组播每接口一条显式 join；空 = 0.0.0.0 旧行为）/aes67_io_priority/\n",
        "                   aes67_stall_blocks/aes67_rx_slots/aes67_group_allow/aes67_source_allow；\n",
        "                   backend=fake|alsa 均可（fake 无播放设备=静音链路）\n",
        "           mode 互斥 off|toslink|aes67|dante；AoIP 真实端点/daemon/PTP 板端 NOT_RUN\n",
        "  [rt] apply_affinity=true = 启动期真实写内核（Linux：mlockall + 主/I-O SCHED_FIFO + worker 亲和；\n",
        "      失败 fail closed 拒绝启动；false = 只计划，sched JSON 如实报 kernel_applied=false）\n",
        "  NOT_WIRED: fused-form plan load (explicitly rejected), IRQ thread affinity/priority\n"
    )
}

fn usage() -> &'static str {
    concat!(
        "khaen-rt-host (T26 host entry)\n",
        "usage:\n",
        "  khaen-rt-host selftest\n",
        "  khaen-rt-host fake-io [--blocks N]\n",
        "  khaen-rt-host admission-check\n",
        "  khaen-rt-host run --config <rt-host.toml> [--blocks N] [--duration-ms N]\n",
        "NOT_RUN: real ALSA device / board / RT/soak / control-plane REST-WS\n",
        "backend=alsa: Linux only; Windows/macOS returns ERR_BACKEND_UNAVAILABLE\n"
    )
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let command = args.get(1).map(String::as_str).unwrap_or("selftest");
    if command == "run" {
        let mut config_path: Option<String> = None;
        let mut run_blocks: Option<u64> = None;
        let mut duration_ms: Option<u64> = None;
        let mut index = 2usize;
        while index < args.len() {
            match args[index].as_str() {
                "--config" | "-c" => match args.get(index + 1) {
                    Some(value) => {
                        config_path = Some(value.clone());
                        index += 2;
                    }
                    None => {
                        eprintln!("missing --config value");
                        return ExitCode::from(2);
                    }
                },
                "--blocks" => match args.get(index + 1).and_then(|v| v.parse().ok()) {
                    Some(value) => {
                        run_blocks = Some(value);
                        index += 2;
                    }
                    None => {
                        eprintln!("bad --blocks value");
                        return ExitCode::from(2);
                    }
                },
                "--duration-ms" => match args.get(index + 1).and_then(|v| v.parse().ok()) {
                    Some(value) => {
                        duration_ms = Some(value);
                        index += 2;
                    }
                    None => {
                        eprintln!("bad --duration-ms value");
                        return ExitCode::from(2);
                    }
                },
                "-h" | "--help" => {
                    println!("{}", usage_host_run());
                    return ExitCode::SUCCESS;
                }
                other => {
                    eprintln!("run: unknown argument: {other}");
                    return ExitCode::from(2);
                }
            }
        }
        let Some(config_path) = config_path else {
            eprintln!("run 需要 --config <rt-host.toml>");
            eprintln!("{}", usage_host_run());
            return ExitCode::from(2);
        };
        install_stop_signals();
        return match run_command(&config_path, run_blocks, duration_ms) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!(
                    "{{\"status\":\"error\",\"mode\":\"khaen-rt-host-run\",\"error\":\"{}\"}}",
                    json_escape_text(&err)
                );
                ExitCode::from(1)
            }
        };
    }
    let mut blocks: u64 = 64;
    let mut index = 2usize;
    while index < args.len() {
        if args[index] == "--blocks" {
            if let Some(value) = args.get(index + 1) {
                match value.parse::<u64>() {
                    Ok(parsed) => blocks = parsed,
                    Err(_) => {
                        eprintln!("invalid --blocks value: {}", value);
                        return ExitCode::from(2);
                    }
                }
            }
            index += 2;
        } else {
            eprintln!("unknown argument: {}", args[index]);
            return ExitCode::from(2);
        }
    }
    let result = match command {
        "selftest" => selftest(),
        "fake-io" => fake_io_json(blocks),
        "admission-check" => admission_json(),
        "help" | "--help" | "-h" => {
            println!("{}", usage());
            return ExitCode::SUCCESS;
        }
        other => {
            eprintln!("unknown command: {}", other);
            eprintln!("{}", usage());
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(json) => {
            println!("{}", json);
            ExitCode::SUCCESS
        }
        Err(status) => {
            eprintln!("{{\"status\":\"error\",\"code\":{}}}", status.code());
            ExitCode::from(1)
        }
    }
}

// ---------------------------------------------------------------------------
// F-12 回归探针：仅 cfg(test) 的按线程计数 allocator
// ---------------------------------------------------------------------------

/// 计数 allocator：统计**当前线程**的堆分配次数，用来证明
/// BlockWorkers::run_block_4thread 在块循环内不分配。只在测试构建链接，
/// 生产 binary 不含本模块。
#[cfg(test)]
mod alloc_probe {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    const INACTIVE: u64 = u64::MAX;

    thread_local! {
        /// INACTIVE = 未记录；否则为当前线程累计分配（alloc / alloc_zeroed / realloc）。
        /// const 初始化 + Cell<u64> 无 Drop：分配器路径内访问不触发堆分配，避免递归。
        static ALLOCATIONS: Cell<u64> = const { Cell::new(INACTIVE) };
    }

    pub struct CountingAllocator;

    impl CountingAllocator {
        fn count() {
            ALLOCATIONS.with(|counter| {
                let current = counter.get();
                if current != INACTIVE {
                    counter.set(current.saturating_add(1));
                }
            });
        }
    }

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            Self::count();
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            Self::count();
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            Self::count();
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    /// 开始记录当前线程的分配（计数清零）。
    pub fn start_recording() {
        ALLOCATIONS.with(|counter| counter.set(0));
    }

    /// 停止记录并返回本区间分配次数。
    pub fn stop_recording() -> u64 {
        ALLOCATIONS.with(|counter| {
            let count = counter.get();
            counter.set(INACTIVE);
            count
        })
    }
}

#[cfg(test)]
#[global_allocator]
static TEST_COUNTING_ALLOCATOR: alloc_probe::CountingAllocator = alloc_probe::CountingAllocator;

#[cfg(test)]
mod file_input_test_support {
    use super::FileSampleKind;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 每次唯一的临时文件（cargo test 并发执行，互不覆盖）。
    pub fn temp_file(prefix: &str, bytes: &[u8]) -> PathBuf {
        static UNIQUE: AtomicU64 = AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::current_exe()
            .expect("current_exe")
            .parent()
            .expect("test exe dir")
            .join(format!(
                "khaen-rt-host-t74-{}-{}-{}",
                std::process::id(),
                unique,
                prefix
            ));
        std::fs::write(&path, bytes).expect("write temp file-input fixture");
        path
    }

    /// WAV 测试样本编码（Pcm16 用于负向；Pcm24/F32 为生产允许形态）。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum WavKind {
        Pcm16,
        Pcm24,
        F32,
    }

    impl WavKind {
        fn tag(self) -> u16 {
            match self {
                WavKind::F32 => 3,
                _ => 1,
            }
        }

        fn bits(self) -> u16 {
            match self {
                WavKind::Pcm16 => 16,
                WavKind::Pcm24 => 24,
                WavKind::F32 => 32,
            }
        }

        fn bytes(self) -> usize {
            (self.bits() / 8) as usize
        }
    }

    fn push_sample(out: &mut Vec<u8>, kind: WavKind, value: f32) {
        match kind {
            WavKind::Pcm16 => {
                let scaled = (value.clamp(-1.0, 1.0) * 32_768.0).round() as i32;
                let clamped = scaled.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                out.extend_from_slice(&clamped.to_le_bytes());
            }
            WavKind::Pcm24 => {
                let scaled = (value.clamp(-1.0, 1.0) * 8_388_608.0).round() as i32;
                let clamped = scaled.clamp(-8_388_608, 8_388_607);
                out.extend_from_slice(&clamped.to_le_bytes()[0..3]);
            }
            WavKind::F32 => out.extend_from_slice(&value.to_le_bytes()),
        }
    }

    /// 确定性 WAV（RIFF/WAVE + 16 B fmt + data；无额外 chunk）。
    pub fn wav_bytes(
        channels: u16,
        sample_rate: u32,
        kind: WavKind,
        frames: usize,
        mut gen: impl FnMut(usize, usize) -> f32,
    ) -> Vec<u8> {
        let block_align = channels as usize * kind.bytes();
        let data_len = frames * block_align;
        let mut out = Vec::with_capacity(44 + data_len);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&kind.tag().to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&((sample_rate as usize * block_align) as u32).to_le_bytes());
        out.extend_from_slice(&(block_align as u16).to_le_bytes());
        out.extend_from_slice(&kind.bits().to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_len as u32).to_le_bytes());
        let mut frame = 0usize;
        while frame < frames {
            let mut channel = 0usize;
            while channel < channels as usize {
                push_sample(&mut out, kind, gen(frame, channel));
                channel += 1;
            }
            frame += 1;
        }
        out
    }

    /// 确定性 raw 样本（帧优先交织；与生产 raw 解析同序）。
    pub fn raw_bytes(
        channels: usize,
        kind: FileSampleKind,
        frames: usize,
        mut gen: impl FnMut(usize, usize) -> f32,
    ) -> Vec<u8> {
        let mut out = Vec::with_capacity(frames * channels * kind.bytes());
        let mut frame = 0usize;
        while frame < frames {
            let mut channel = 0usize;
            while channel < channels {
                let value = gen(frame, channel);
                match kind {
                    FileSampleKind::S24Le => {
                        let scaled = (value.clamp(-1.0, 1.0) * 8_388_608.0).round() as i32;
                        let clamped = scaled.clamp(-8_388_608, 8_388_607);
                        out.extend_from_slice(&clamped.to_le_bytes()[0..3]);
                    }
                    FileSampleKind::F32Le => out.extend_from_slice(&value.to_le_bytes()),
                }
                channel += 1;
            }
            frame += 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nsp512_to_412_path_table_rejects_identity() {
        fn path_io(out_index: u16, in_index: u16) -> khaen_core::abi::TspiPathV1 {
            khaen_core::abi::TspiPathV1 {
                in_index,
                out_index,
                taps: 2,
                delay_samples: 0,
                polarity: 1,
                flags: 0,
                reserved0: 0,
                coeff_offset_bytes: 0,
                coeff_len_bytes: 8,
                reserved1: 0,
            }
        }
        let preset = [
            path_io(0, 0),
            path_io(1, 1),
            path_io(2, 4),
            path_io(3, 5),
            path_io(4, 6),
            path_io(5, 7),
            path_io(7, 3),
        ];
        validate_nsp512_to_412_paths(&preset).expect("预设 path 必须通过");
        let identity = [
            path_io(0, 0),
            path_io(1, 1),
            path_io(2, 2),
            path_io(3, 3),
            path_io(4, 4),
            path_io(5, 5),
            path_io(7, 7),
        ];
        let error = validate_nsp512_to_412_paths(&identity).expect_err("对角 identity 必须拒绝");
        assert!(error.contains("in=2"), "{error}");
    }

    #[test]
    fn t26_selftest_matches_direct_and_passes_contracts() {
        let report = selftest().expect("selftest must pass");
        assert!(report.contains("\"status\":\"ok\""));
        assert!(report.contains("\"bit_exact_graphs\":2"));
        assert!(report.contains("\"b2_threads\":4"));
    }

    #[test]
    fn t26_fake_io_runs_four_worker_pipeline() {
        let stats = fake_io(8).expect("fake-io must run");
        assert_eq!(stats.blocks, 8);
        assert_eq!(stats.frames, 8 * BLOCK as u64);
        assert_eq!(stats.b2_threads, WORKERS);
        assert_eq!(stats.spsc_pushed, 8);
        assert_eq!(stats.spsc_popped, 8);
        assert!(stats.device_bytes > 0);
        assert_eq!(stats.format_nonfinite, 0);
        assert!(!stats.output_sha_prefix.is_empty());
    }

    #[test]
    fn t26_admission_and_format_contracts_pass() {
        admission_check().expect("admission/format contracts must pass");
        let cfg = device_config();
        assert!(format::validate(&cfg).is_ok());
    }

    #[test]
    fn t26_block_loop_zero_alloc_and_no_new_threads() {
        // F-12 回归：块循环不得堆分配、不得新建线程（partials/worker 启动时一次分配/创建）。
        let graph = graph_stereo_identity();
        let plan = graph.plan().expect("plan");
        let coeffs = graph.coeffs();
        let layout = graph.layout().expect("layout");
        let block = BLOCK as usize;
        let stride = block;
        let containers = graph.topo.container_channels as usize;
        let n_workers = WORKERS as usize;
        let mut state = vec![0.0f32; layout.state_samples()];
        let mut input = vec![0.0f32; 2 * stride];
        let mut output = vec![0.0f32; containers * stride];
        let mut workers = BlockWorkers::start(&plan, &coeffs).expect("start persistent workers");
        assert_eq!(workers.worker_count(), n_workers, "常驻 worker 数");
        assert_eq!(
            workers.spawned_threads(),
            n_workers as u64,
            "启动时一次创建 4 个常驻 worker"
        );
        assert_eq!(
            workers.partials_bytes(),
            n_workers * std::mem::size_of::<WorkerPartials>(),
            "partials 启动时一次预分配"
        );
        // 预热一个 block（首次发布），不参与分配断言：只证明稳定态块循环零分配。
        synth_input_into(0, &mut input, stride, block);
        output.iter_mut().for_each(|sample| *sample = 0.0);
        workers
            .run_block_4thread(&mut state, &input, stride, &mut output, stride, block)
            .expect("warmup block");
        // 测量区间：只允许复用启动期缓冲。
        let spawned_before = workers.spawned_threads();
        alloc_probe::start_recording();
        let mut index = 1usize;
        let mut last_b2_threads = 0u8;
        let mut last_all_ok = false;
        while index <= 8 {
            synth_input_into(index, &mut input, stride, block);
            output.iter_mut().for_each(|sample| *sample = 0.0);
            let stats = workers
                .run_block_4thread(&mut state, &input, stride, &mut output, stride, block)
                .expect("streaming block");
            last_b2_threads = stats.b2_threads;
            last_all_ok = stats.all_ok;
            index += 1;
        }
        let allocations = alloc_probe::stop_recording();
        assert_eq!(
            allocations, 0,
            "F-12：块循环必须零堆分配，实测 {allocations} 次"
        );
        assert_eq!(
            workers.spawned_threads(),
            spawned_before,
            "F-12：块循环不得新建线程"
        );
        assert_eq!(last_b2_threads, WORKERS);
        assert!(last_all_ok);
    }

    #[test]
    fn t26_two_worker_profile_block_loop_bit_exact() {
        // 2 核降级 profile（config.workers=2）同样走常驻 worker + 预分配 partials，
        // 且与 T07 直算 bit-exact（T09：结果与 W 无关）。
        let graph = graph_stereo_identity();
        let plan =
            WorkerDagPlan::build(&graph.topo, BLOCK, 2, PLAN_VERSION).expect("2 worker plan");
        let coeffs = graph.coeffs();
        let layout = graph.layout().expect("layout");
        let block = BLOCK as usize;
        let stride = block;
        let containers = graph.topo.container_channels as usize;
        let blocks = 4usize;
        let mut state = vec![0.0f32; layout.state_samples()];
        let mut input = vec![0.0f32; 2 * stride];
        let mut output = vec![0.0f32; containers * stride];
        let mut workers = BlockWorkers::start(&plan, &coeffs).expect("start 2-worker pool");
        assert_eq!(workers.worker_count(), 2);
        let mut pipeline: Vec<Vec<f32>> = vec![Vec::new(); containers];
        let mut b = 0usize;
        while b < blocks {
            synth_input_into(b, &mut input, stride, block);
            output.iter_mut().for_each(|sample| *sample = 0.0);
            workers
                .run_block_4thread(&mut state, &input, stride, &mut output, stride, block)
                .expect("2 worker block");
            let mut channel = 0usize;
            while channel < containers {
                pipeline[channel]
                    .extend_from_slice(&output[channel * stride..channel * stride + block]);
                channel += 1;
            }
            b += 1;
        }
        // 参考输入（与 pipeline 相同的合成块序列）→ T07 直算。
        let mut reference_input: Vec<Vec<f32>> = vec![Vec::with_capacity(blocks * block); 2];
        let mut b = 0usize;
        while b < blocks {
            synth_input_into(b, &mut input, stride, block);
            let mut channel = 0usize;
            while channel < 2 {
                reference_input[channel]
                    .extend_from_slice(&input[channel * stride..channel * stride + block]);
                channel += 1;
            }
            b += 1;
        }
        let reference = direct_stream(&graph, &reference_input, blocks).expect("direct reference");
        let mut channel = 0usize;
        while channel < containers {
            assert_eq!(pipeline[channel].len(), reference[channel].len());
            let mut n = 0usize;
            while n < reference[channel].len() {
                assert_eq!(
                    pipeline[channel][n].to_bits(),
                    reference[channel][n].to_bits(),
                    "2 worker pipeline 必须与直算 bit-exact: channel {channel} sample {n}"
                );
                n += 1;
            }
            channel += 1;
        }
    }

    // -----------------------------------------------------------------------
    // F-26 回归：retire_ready 只由真实 block all-ack 置位；synthetic 不得冒充
    // -----------------------------------------------------------------------

    #[test]
    fn t26_real_all_ack_sets_retire_ready_synthetic_rejected() {
        use khaen_core::abi::TspiBlockAckV1;
        use khaen_rt_host::swap::TspiSwapAckV1;

        // 真实证据：completed_block_ack 只填「该块 4 worker 三 phase 全部完成」的事实。
        let ack = completed_block_ack(7, 1, 4);
        assert_eq!(ack.block_seq, 7);
        assert_eq!(ack.epoch_id, EPOCH_ID);
        assert_eq!(ack.plan_version, 1);
        assert_eq!(ack.expected_workers, 4);
        let mask = ack_mask(4);
        for phase in 0..3 {
            assert_eq!(
                ack.phase_done[phase].load(Ordering::Acquire),
                mask,
                "phase {phase} 位图必须 = ACK_MASK(4)"
            );
        }
        assert_eq!(ack.cutoff.load(Ordering::Acquire), 0);

        let mut swap = TspiSwapAckV1::new();
        assert!(swap.init(1, 4, EPOCH_ID, 1, 0).is_ok());
        assert_eq!(swap.retire_ready_value(), 0, "初始不得预置 retire_ready");
        assert!(swap.note_block_all_ack(&ack).is_ok());
        assert_eq!(swap.ack_mask_value(), mask);
        assert!(swap.is_retire_ready(), "真实 all-ack 才置 retire_ready");
        swap.sync_retire_ready_flag();
        assert!(swap.validate().is_ok());

        // synthetic/伪证据：partial phase / cutoff / 版本标签不符都必须被拒，
        // 且不得写 retire_ready（F-26：不得用合成 ack 冒充真实 all-ack）。
        let partial = {
            let a = TspiBlockAckV1::new(8, EPOCH_ID, 1, 4);
            a.phase_done[0].store(mask, Ordering::Release);
            a.phase_done[1].store(mask & 0b0111, Ordering::Release);
            a.phase_done[2].store(mask, Ordering::Release);
            a
        };
        let cutoff = completed_block_ack(9, 1, 4);
        cutoff.cutoff.store(1, Ordering::Release);
        let wrong_version = completed_block_ack(10, 2, 4);
        for (label, fake) in [
            ("partial-phase", partial),
            ("cutoff", cutoff),
            ("wrong-version", wrong_version),
        ] {
            let mut swap = TspiSwapAckV1::new();
            assert!(swap.init(1, 4, EPOCH_ID, 1, 0).is_ok());
            assert!(
                !swap.note_block_all_ack(&fake).is_ok(),
                "{label}: synthetic ack 必须被 note_block_all_ack 拒绝"
            );
            assert_eq!(
                swap.retire_ready_value(),
                0,
                "{label}: synthetic ack 不得写 retire_ready"
            );
            assert!(!swap.is_retire_ready(), "{label}: 不得冒充 all-ack");
        }
    }

    #[test]
    fn t26_real_switch_all_ack_releases_quarantine_only_after_ack() {
        use khaen_core::abi::TspiBlockAckV1;
        use khaen_rt_host::swap::{note_block_switched, GraphQuarantine, TspiSwapAckV1};

        let old_version = 1u32;
        let new_version = 2u32;
        let old_ptr = 0x1000u64;
        let mask = ack_mask(4);
        // 旧图 v1 入 quarantine（4 worker 位全待 ack）。
        let quarantine = GraphQuarantine::new();
        let slot = quarantine
            .enter(EPOCH_ID, old_version, old_ptr, 4)
            .expect("quarantine slot");
        let mut swap = TspiSwapAckV1::new();
        assert!(swap
            .init(new_version, 4, EPOCH_ID, new_version, old_ptr)
            .is_ok());
        // 未 all-ack 前：retire_ready=0 且槽不可释放（INV-L5）。
        assert!(quarantine.try_release(slot).is_err());
        assert_eq!(quarantine.remaining_mask(slot), Some(mask));
        // synthetic 半块（只 phase0 全到）不得清 quarantine 位、不得置 retire_ready。
        let partial = {
            let a = TspiBlockAckV1::new(1, EPOCH_ID, new_version, 4);
            a.phase_done[0].store(mask, Ordering::Release);
            a
        };
        assert!(!note_block_switched(
            &swap,
            &quarantine,
            slot,
            &partial,
            old_version,
            EPOCH_ID,
            new_version
        )
        .is_ok());
        assert_eq!(quarantine.remaining_mask(slot), Some(mask));
        assert_eq!(swap.retire_ready_value(), 0);
        // 真实 all-ack block：retire_ready 置位 + 4 worker quarantine 位清零，之后才可释放。
        let ack = completed_block_ack(2, new_version, 4);
        assert!(note_block_switched(
            &swap,
            &quarantine,
            slot,
            &ack,
            old_version,
            EPOCH_ID,
            new_version
        )
        .is_ok());
        assert!(swap.is_retire_ready());
        assert_eq!(quarantine.remaining_mask(slot), Some(0));
        assert!(quarantine.try_release(slot).is_ok());
        assert!(!quarantine.is_occupied(slot));
    }

    #[test]
    fn t26_block_workers_switch_plan_is_real_and_bit_exact() {
        // F-26：v1->v2 切换必须让 worker 真实使用新 plan（同一输入输出与 v1 逐位一致，
        // 因为同图 re-plan 只有 plan_version 不同），而不是只改控制面标签。
        let graph = graph_2x2_offdiag();
        let plan_v1 = graph.plan().expect("plan v1");
        let plan_v2 =
            WorkerDagPlan::build(&graph.topo, BLOCK, WORKERS, 2).expect("same-graph plan v2");
        let coeffs = graph.coeffs();
        let layout = graph.layout().expect("layout");
        let block = BLOCK as usize;
        let stride = block;
        let containers = graph.topo.container_channels as usize;
        let mut state = vec![0.0f32; layout.state_samples()];
        let mut input = vec![0.0f32; 2 * stride];
        let mut output = vec![0.0f32; containers * stride];
        // F-26 反例 plan（worker 数不同）：必须在 workers 之前声明——BlockWorkers<'a> 会借用
        // 其 plan 引用直到 Drop，后声明会 borrowed value does not live long enough。
        let two_worker = WorkerDagPlan::build(&graph.topo, BLOCK, 2, 3).expect("2 worker plan");
        let mut workers = BlockWorkers::start(&plan_v1, &coeffs).expect("start v1 workers");
        assert_eq!(workers.plan_version(), 1);
        let spawned_before = workers.spawned_threads();

        synth_input_into(0, &mut input, stride, block);
        output.iter_mut().for_each(|sample| *sample = 0.0);
        workers
            .run_block_4thread(&mut state, &input, stride, &mut output, stride, block)
            .expect("v1 block");
        let v1_output = output.clone();

        // block 边界切到 v2；状态清零后同一输入再跑一遍，必须逐位一致。
        assert!(workers.switch_plan(&plan_v2).is_ok());
        assert_eq!(workers.plan_version(), 2);
        assert_eq!(
            workers.spawned_threads(),
            spawned_before,
            "plan 切换不得重建 worker 线程"
        );
        state.iter_mut().for_each(|sample| *sample = 0.0);
        output.iter_mut().for_each(|sample| *sample = 0.0);
        workers
            .run_block_4thread(&mut state, &input, stride, &mut output, stride, block)
            .expect("v2 block");
        let mut n = 0usize;
        while n < output.len() {
            assert_eq!(
                output[n].to_bits(),
                v1_output[n].to_bits(),
                "同图 re-plan 块输出必须 bit-exact: sample {n}"
            );
            n += 1;
        }

        // worker 数不同的 plan 不允许切入（防止 ack 标签/ACK_MASK 错配）。
        assert_eq!(
            workers.switch_plan(&two_worker),
            Err(RtStatus::ERR_ADMISSION_SCHED)
        );
    }

    #[test]
    fn t26_run_config_real_all_ack_and_quarantine_release() {
        // F-26 端到端：run --config 的 swap JSON 必须显式标注证据来源，且
        // switches=2 / acks=4 / quarantine 进入+释放 全部来自真实 block all-ack。
        // 临时配置写到测试 exe 所在构建目录：不依赖 TEMP/TMP（本机 TEMP 指向 R:\Temp，
        // 部分会话下 std::env::temp_dir() 写入被拒 PermissionDenied），也不污染源码树。
        let config_path = std::env::current_exe()
            .expect("current_exe")
            .parent()
            .expect("test exe dir")
            .join(format!("khaen-rt-host-f26-swap-{}.toml", std::process::id()));
        let config = concat!(
            "[host]\n",
            "block_size = 128\n",
            "sample_rate = 48000\n",
            "workers = 4\n",
            "graph = \"2x2_offdiag\"\n",
            "safe_mode = \"normal\"\n",
            "[alsa]\n",
            "backend = \"fake\"\n",
            "device = \"hw:UltraLitemk5,0\"\n",
            "capture_channels = 20\n",
            "playback_channels = 22\n",
            "period_frames = 128\n",
            "buffer_frames = 1024\n",
            "[rt]\n",
            "dsp_cpus = \"2-5\"\n",
            "tick_freq_hz = 1000000000\n",
            "[web]\n",
            // external web（daemon.config 为空）只做一次立即失败的探测，不 spawn 子进程。
            "bind = \"127.0.0.1:1\"\n",
            "[run]\n",
            "max_blocks = 8\n",
        );
        std::fs::write(&config_path, config).expect("write temp rt-host config");
        let json = run_command(config_path.to_str().expect("utf8 config path"), None, None)
            .expect("run --config must pass");
        let _ = std::fs::remove_file(&config_path);
        assert!(json.contains("\"status\":\"ok\""), "run 失败: {json}");
        assert!(
            json.contains("\"synthetic\":false"),
            "swap 段必须显式标注非 synthetic: {json}"
        );
        assert!(
            json.contains("\"retire_ready_source\":\"note_block_all_ack\""),
            "retire_ready 必须来自真实 block all-ack: {json}"
        );
        assert!(json.contains("\"acks\":4"), "4 worker 真实 ack: {json}");
        assert!(json.contains("\"switches\":2"), "v1+v2 真实切换: {json}");
        assert!(json.contains("\"retire_ready\":true"), "{json}");
        assert!(json.contains("\"valid\":true"), "{json}");
        assert!(json.contains("\"quarantine_entered\":1"), "{json}");
        assert!(
            json.contains("\"quarantine_released\":1"),
            "v2 all-ack 后 quarantine 必须释放: {json}"
        );
        assert!(json.contains("\"quarantine_remaining\":0"), "{json}");
        assert!(json.contains("\"all_ack_blocks\":8"), "{json}");
        assert!(json.contains("\"plan_version\":2"), "{json}");
        assert!(
            !json.contains("\"synthetic\":true"),
            "不存在 synthetic ack 冒充真实: {json}"
        );
    }

    // ---------------------------------------------------------------------
    // T71 前置：backend=alsa 播放接线单测（mock ALSA 抽象；真实设备/板端 NOT_RUN）
    // ---------------------------------------------------------------------

    use khaen_rt_host::alsa::{
        DeviceSelector, DuplexRequest, HwOpenGate, HwParamsPlan, RawActual, RecoverStage,
        RecoveryPolicy, ResolvedDevice, StreamActual, StreamCaps,
    };

    const CARDS_MOTU_LINE: &str = " 4 [UltraLitemk5   ]: USB-Audio - UltraLite-mk5\n";

    /// 可记录 IO 的 mock ALSA 后端（实现 T17 PcmDuplexBackend trait；不触碰 /dev/snd）。
    struct RecordingBackend {
        capture_caps: StreamCaps,
        playback_caps: StreamCaps,
        opened: bool,
        started: bool,
        writes: Vec<u8>,
        write_lengths: Vec<usize>,
        read_calls: u32,
        write_calls: u32,
        drain_calls: u32,
        recover_calls: u32,
        next_read_error: Option<AlsaError>,
        next_write_error: Option<AlsaError>,
        /// 再成功写 N 次后，下一次 write_period 注入一次 Xrun（模拟起播预填后的真实写）
        write_error_after: Option<u32>,
        recover_failures: u8,
        short_read: Option<usize>,
    }

    impl RecordingBackend {
        fn motu() -> Self {
            let fake = khaen_rt_host::alsa::FakeBackend::motu_ultralite_mk5();
            Self {
                capture_caps: fake.capture_caps,
                playback_caps: fake.playback_caps,
                opened: false,
                started: false,
                writes: Vec::new(),
                write_lengths: Vec::new(),
                read_calls: 0,
                write_calls: 0,
                drain_calls: 0,
                recover_calls: 0,
                next_read_error: None,
                next_write_error: None,
                write_error_after: None,
                recover_failures: 0,
                short_read: None,
            }
        }

        fn exact_actual(plan: &HwParamsPlan) -> RawActual {
            let capture = StreamActual {
                channels: plan.capture_channels,
                sample_format: plan.sample_format,
                sample_rate: plan.sample_rate,
                period_frames: plan.period_frames,
                buffer_frames: plan.buffer_frames,
            };
            let playback = StreamActual {
                channels: plan.playback_channels,
                ..capture
            };
            RawActual { capture, playback }
        }
    }

    impl khaen_rt_host::alsa::PcmDuplexBackend for RecordingBackend {
        fn open(&mut self, _device: &ResolvedDevice) -> Result<(), AlsaError> {
            self.opened = true;
            Ok(())
        }

        fn stream_caps(
            &mut self,
            direction: khaen_rt_host::alsa::StreamDirection,
        ) -> Result<StreamCaps, AlsaError> {
            Ok(match direction {
                khaen_rt_host::alsa::StreamDirection::Capture => self.capture_caps,
                khaen_rt_host::alsa::StreamDirection::Playback => self.playback_caps,
            })
        }

        fn apply_hw_params(&mut self, plan: &HwParamsPlan) -> Result<RawActual, AlsaError> {
            Ok(Self::exact_actual(plan))
        }

        fn start(&mut self) -> Result<(), AlsaError> {
            self.started = true;
            Ok(())
        }

        fn read_period(&mut self, dst: &mut [u8]) -> Result<usize, AlsaError> {
            if let Some(error) = self.next_read_error.take() {
                return Err(error);
            }
            if !self.started {
                return Err(AlsaError::DeviceLost);
            }
            self.read_calls += 1;
            let written = self.short_read.unwrap_or(dst.len()).min(dst.len());
            for (index, byte) in dst[..written].iter_mut().enumerate() {
                *byte = index as u8;
            }
            Ok(written)
        }

        fn write_period(&mut self, src: &[u8]) -> Result<usize, AlsaError> {
            if let Some(error) = self.next_write_error.take() {
                return Err(error);
            }
            if let Some(remaining) = self.write_error_after {
                if remaining == 0 {
                    self.write_error_after = None;
                    return Err(AlsaError::Xrun);
                }
                self.write_error_after = Some(remaining - 1);
            }
            if !self.started {
                return Err(AlsaError::DeviceLost);
            }
            self.write_calls += 1;
            self.write_lengths.push(src.len());
            self.writes.extend_from_slice(src);
            Ok(src.len())
        }

        fn drain(&mut self) -> Result<(), AlsaError> {
            self.drain_calls += 1;
            self.started = false;
            Ok(())
        }

        fn recover(&mut self, _stage: RecoverStage) -> Result<(), AlsaError> {
            self.recover_calls += 1;
            if self.recover_failures > 0 {
                self.recover_failures -= 1;
                return Err(AlsaError::DeviceLost);
            }
            if !self.opened {
                return Err(AlsaError::DeviceLost);
            }
            Ok(())
        }

        fn close(&mut self) {
            self.opened = false;
            self.started = false;
        }
    }

    fn alsa_request() -> DuplexRequest {
        DuplexRequest {
            sample_rate: 48_000,
            period_frames: 128,
            buffer_frames: 1024,
            min_input_channels: 20,
            min_output_channels: 22,
            format_priority: DuplexRequest::DEFAULT_FORMAT_PRIORITY,
            allow_file_fake_formats: false,
            capture_sanitize: true,
        }
    }

    fn sink_with(
        backend: RecordingBackend,
        mode: AlsaIoMode,
    ) -> (PcmPlaybackSink<RecordingBackend>, DuplexActual) {
        let mut session = DuplexSession::new(
            backend,
            alsa_request(),
            RecoveryPolicy::new(2, 3),
            HwOpenGate::denied(),
        );
        let actual = session
            .start(&DeviceSelector::by_id("UltraLitemk5", 0), CARDS_MOTU_LINE)
            .expect("mock duplex start");
        let config = HostRunConfig {
            backend: "alsa".to_string(),
            alsa_io_mode: mode.name().to_string(),
            ..HostRunConfig::default()
        };
        let sink =
            PcmPlaybackSink::started(&config, 2, mode, session, actual).expect("sink started");
        (sink, actual)
    }

    fn tone_output(frames: usize) -> Vec<f32> {
        let mut output = vec![0.0f32; 2 * frames];
        let mut n = 0usize;
        while n < frames {
            output[n] = ((n as f32) * 0.05).sin() * 0.5;
            output[frames + n] = -0.25;
            n += 1;
        }
        output
    }

    /// 参考字节：同一 T22 convert_playback（dither off）对 22ch 映射结果。
    fn expected_playback_bytes(actual: &DuplexActual, output: &[f32], frames: usize) -> Vec<u8> {
        let n_out = actual.hw.n_out as usize;
        let period = actual.hw.period_frames as usize;
        let mut planar = vec![0.0f32; n_out * period];
        planar[..frames].copy_from_slice(&output[..frames]);
        planar[period..period + frames].copy_from_slice(&output[frames..2 * frames]);
        let cfg = FormatConfig {
            hw: actual.hw,
            dither: DitherCfg::Off,
        };
        let mut bytes = vec![
            0u8;
            actual
                .period_bytes(khaen_rt_host::alsa::StreamDirection::Playback)
                .unwrap()
        ];
        format::convert_playback(&cfg, &planar, &mut bytes).expect("reference convert_playback");
        bytes
    }

    #[test]
    fn t71_alsa_sink_negotiates_motu_actual() {
        let (sink, actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Synth);
        assert_eq!(actual.hw.n_in, 20);
        assert_eq!(actual.hw.n_out, 22);
        assert_eq!(
            actual.hw.sample_format,
            khaen_core::abi::sample_format::S24_3LE_INTERLEAVED
        );
        assert_eq!(actual.hw.sample_rate, 48_000);
        assert_eq!(actual.hw.period_frames, 128);
        assert_eq!(actual.hw.buffer_frames, 1024);
        assert!(!actual.requires_new_epoch());
        assert_eq!(sink.counters.blocks, 0);
        assert_eq!(
            sink.device_bytes.len(),
            22 * 128 * 3,
            "S24_3LE 22ch x 128 帧 period 字节"
        );
    }

    #[test]
    fn t71_alsa_sink_writes_graph_block_as_s24_3le() {
        let (mut sink, actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Synth);
        let output = tone_output(128);
        let event = sink
            .write_block(&output, 128, 128, false)
            .expect("write_block");
        assert!(event.is_none());
        let expected = expected_playback_bytes(&actual, &output, 128);
        let backend = sink.session.backend();
        assert_eq!(backend.write_calls, 1);
        assert_eq!(backend.write_lengths, vec![22 * 128 * 3]);
        assert_eq!(backend.writes, expected, "写设备字节必须等于 T22 映射结果");
        for frame in [0usize, 64, 127] {
            for channel in 2..22usize {
                let base = frame * 22 * 3 + channel * 3;
                assert_eq!(&backend.writes[base..base + 3], &[0, 0, 0]);
            }
        }
        assert_eq!(sink.counters.blocks, 1);
        assert_eq!(sink.counters.bytes_written, (22 * 128 * 3) as u64);
    }

    #[test]
    fn t71_alsa_sink_mute_mode_writes_silence() {
        let (mut sink, _actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Synth);
        let output = tone_output(128);
        sink.write_block(&output, 128, 128, true)
            .expect("muted write_block");
        let backend = sink.session.backend();
        assert_eq!(backend.write_calls, 1);
        assert!(
            backend.writes.iter().all(|byte| *byte == 0),
            "安全静音必须全零"
        );
        assert_eq!(sink.counters.silence_blocks, 1);
    }

    #[test]
    fn t71_alsa_sink_prime_playback_fills_device_cushion_once() {
        // F-v5-2 根因修复：起播/恢复后必须先把 playback sw 缓冲预填到 cushion
        // （buffer_periods-2 = 1024/128-2 = 6 period 静音），否则块循环只有 1 period
        // 余量 → 写-恢复交替（v5 板窗 underruns == alsa.blocks-2，main:alsa blocks=2:1）。
        let (mut sink, actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Synth);
        assert_eq!(sink.counters.prime_blocks, 0);
        sink.prime_playback().expect("prime playback");
        let period_bytes = actual
            .period_bytes(StreamDirection::Playback)
            .expect("playback period bytes");
        assert_eq!(period_bytes, 22 * 128 * 3);
        {
            let backend = sink.session.backend();
            assert_eq!(backend.write_calls, 6, "预填 6 个 period 静音");
            assert_eq!(backend.write_lengths, vec![period_bytes; 6]);
            assert!(
                backend.writes.iter().all(|byte| *byte == 0),
                "预填必须是全零静音（不得写入未定义内容）"
            );
        }
        assert_eq!(sink.counters.prime_blocks, 6);
        assert_eq!(sink.counters.prime_bytes, 6 * period_bytes as u64);
        assert_eq!(sink.counters.blocks, 0, "预填不计入 blocks/bytes_written");
        assert_eq!(sink.counters.bytes_written, 0);
        // 幂等：已预填过 → 再调用不再写设备（起播 + 恢复路径不会重复灌 cushion）。
        sink.prime_playback().expect("prime idempotent");
        assert_eq!(sink.session.backend().write_calls, 6);
        // 稳态块写：1 block = 1 period（预填不改变块/period 映射语义）。
        let output = tone_output(128);
        sink.write_block(&output, 128, 128, false)
            .expect("write after priming");
        assert_eq!(sink.session.backend().write_calls, 7);
        assert_eq!(sink.counters.blocks, 1);
        let report = sink.report();
        assert!(report.contains("\"prime_blocks\":6"), "{report}");
        assert!(report.contains("\"prime_bytes\":50688"), "{report}");
        assert!(report.contains("\"blocks\":1"), "{report}");
        assert!(report.contains("\"xruns\":0"), "{report}");
    }

    #[test]
    fn t71_alsa_sink_recovery_reprimes_playback_cushion() {
        // F-v5-2：恢复 = Suspend/Prepare/Resume，playback sw 缓冲被清空；不重预填就会
        // 立刻回到 1 period 余量 → 每次恢复后又 underrun（v5 板窗写-恢复交替）。
        let (mut sink, _actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Synth);
        let output = tone_output(128);
        sink.prime_playback().expect("startup prime");
        assert_eq!(sink.counters.prime_blocks, 6);
        sink.session.backend_mut().next_write_error = Some(AlsaError::Xrun);
        let event = sink
            .write_block(&output, 128, 128, false)
            .expect("recoverable xrun")
            .expect("underrun event");
        assert_eq!(event.kind, khaen_core::events::EV_OUTPUT_UNDERRUN);
        assert_eq!(
            sink.session.state(),
            khaen_rt_host::alsa::DuplexState::Running
        );
        // 恢复后下一块：先重预填 6 period，再写 1 个真实 period。
        sink.write_block(&output, 128, 128, false)
            .expect("write after recovery");
        assert_eq!(sink.counters.prime_blocks, 12, "每次恢复都要重预填");
        assert_eq!(sink.session.backend().write_calls, 6 + 6 + 1);
        assert_eq!(sink.counters.blocks, 1, "预填/失败写都不计入 blocks");
        assert_eq!(sink.counters.xruns, 1);
    }

    #[test]
    fn t71_alsa_session_prime_playback_silence_requires_running() {
        let backend = RecordingBackend::motu();
        let mut session = DuplexSession::new(
            backend,
            alsa_request(),
            RecoveryPolicy::new(2, 3),
            HwOpenGate::denied(),
        );
        let error = session
            .prime_playback_silence()
            .expect_err("未 start 的 session 不得预填");
        assert_eq!(error.name(), "ERR_SWITCH_BUSY");
        assert_eq!(session.backend().write_calls, 0);
    }

    #[test]
    fn t71_alsa_sink_xrun_recovers_and_reports_events() {
        let (mut sink, _actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Synth);
        sink.session.backend_mut().next_write_error = Some(AlsaError::Xrun);
        let output = tone_output(128);
        let event = sink
            .write_block(&output, 128, 128, false)
            .expect("xrun recoverable")
            .expect("xrun event");
        assert_eq!(event.kind, khaen_core::events::EV_OUTPUT_UNDERRUN);
        // F2：§8.2 冻结表 severity=3 / value_a=consecutive / value_b=capacity。
        assert_eq!(event.severity, khaen_core::events::SEVERITY_ERROR);
        assert_eq!(event.value_a, 1, "连续 underrun 数");
        assert_eq!(event.value_b, 8, "设备缓冲容量（1024/128 period）");
        assert_eq!(sink.counters.xruns, 1);
        assert_eq!(sink.counters.underruns, 1);
        assert_eq!(sink.counters.recoveries, 1);
        assert_eq!(
            sink.session.state(),
            khaen_rt_host::alsa::DuplexState::Running,
            "xrun 恢复后必须回到 Running（新 epoch）"
        );
        assert_eq!(sink.session.backend().recover_calls, 3);
        // F-v5-2：恢复（prepare 清空 playback sw 缓冲）后，下一次写之前必须重新预填
        // cushion（1024/128-2=6 period 静音），否则又回到 1 period 余量 → 再 underrun。
        let next = sink
            .write_block(&output, 128, 128, false)
            .expect("write after recovery")
            .expect("epoch event");
        assert_eq!(next.kind, khaen_core::events::EV_EPOCH_START);
        let backend = sink.session.backend();
        assert_eq!(backend.write_calls, 7, "6 period 预填 + 1 个真实块");
        assert_eq!(sink.counters.prime_blocks, 6, "恢复后必须重预填 6 period");
        assert!(
            backend.writes[..6 * 8448].iter().all(|byte| *byte == 0),
            "预填必须是静音"
        );
        assert_eq!(sink.counters.blocks, 1, "预填不计入 blocks");
    }

    #[test]
    fn t71_alsa_sink_recovery_exhausted_enters_mute() {
        let (mut sink, _actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Synth);
        sink.session.backend_mut().next_write_error = Some(AlsaError::Xrun);
        sink.session.backend_mut().recover_failures = 255;
        let output = tone_output(128);
        let event = sink
            .write_block(&output, 128, 128, false)
            .expect("exhausted is not fatal")
            .expect("xrun event");
        assert_eq!(event.kind, khaen_core::events::EV_OUTPUT_UNDERRUN);
        assert_eq!(
            sink.session.state(),
            khaen_rt_host::alsa::DuplexState::Muted,
            "恢复重试耗尽必须进入 Muted"
        );
        assert!(sink.session.mute_required());
        // F3：进入 Muted 时写真实 safety_mute_entries（不再恒 0）。
        assert_eq!(
            sink.t21.snapshot().safety_mute_entries,
            1,
            "恢复耗尽进入 Muted 必须计 1 次 safety_mute_entries"
        );
        let second = sink
            .write_block(&output, 128, 128, false)
            .expect("muted silence write")
            .expect("safety mute event");
        assert_eq!(second.kind, khaen_core::events::EV_SAFETY_MUTE_ENTER);
        // F2：§8.2 冻结表 severity=4 / value_a=原因码 / value_b=epoch_id。
        assert_eq!(second.severity, khaen_core::events::SEVERITY_FATAL);
        assert_eq!(
            second.value_a,
            khaen_core::rt::RtStatus::RT_E_DEVICE_LOST.code(),
            "原因码 = T17 最后错误（设备丢失 407）"
        );
        assert_eq!(second.value_b, 1, "epoch_id（初始 epoch=1）");
        assert_eq!(sink.counters.muted_blocks, 1);
        assert_eq!(sink.counters.silence_blocks, 1);
        assert!(
            sink.session.backend().writes.iter().all(|byte| *byte == 0),
            "Muted 期间只能写静音"
        );
        sink.stop().expect("stop from muted");
    }

    #[test]
    fn t71_alsa_sink_nonrecoverable_fault_fails_closed() {
        let (mut sink, _actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Synth);
        sink.session.backend_mut().next_write_error = Some(AlsaError::Unsupported);
        let output = tone_output(128);
        let error = sink
            .write_block(&output, 128, 128, false)
            .expect_err("nonrecoverable must fail closed");
        assert!(error.contains("不可恢复"), "{error}");
        assert_eq!(sink.counters.faults, 1);
        assert_eq!(
            sink.session.state(),
            khaen_rt_host::alsa::DuplexState::Faulted
        );
    }

    #[test]
    fn t71_alsa_sink_muted_capture_is_safety_mute_not_fatal() {
        // F1：恢复耗尽进入 Muted 后，下一块 read_capture 必须返回 SafetyMute（不处理新块），
        // 而不是 ERR_SWITCH_BUSY → 被误判不可恢复 → run exit 1。
        let (mut sink, _actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Synth);
        sink.session.backend_mut().next_write_error = Some(AlsaError::Xrun);
        sink.session.backend_mut().recover_failures = 255;
        let output = tone_output(128);
        sink.write_block(&output, 128, 128, false)
            .expect("exhausted is not fatal");
        assert_eq!(sink.session.state(), khaen_rt_host::alsa::DuplexState::Muted);
        // 下一块：capture 阶段先遇到 Muted（旧实现在这里 Err）。
        let outcome = sink.read_capture().expect("Muted capture 不得失败");
        assert_eq!(
            outcome.safety_mute_reason(),
            Some(khaen_core::rt::RtStatus::RT_E_DEVICE_LOST.code()),
            "SafetyMute 必须带进入原因码"
        );
        let event = outcome.event().expect("进入静音的专用事件");
        assert_eq!(event.kind, khaen_core::events::EV_SAFETY_MUTE_ENTER);
        assert_eq!(event.severity, khaen_core::events::SEVERITY_FATAL);
        assert!(sink.capture_planar.iter().all(|sample| *sample == 0.0));
        // 再下一块：不重复刷进入事件，但仍不失败。
        let next = sink.read_capture().expect("持续 Muted 不得失败");
        assert!(next.event().is_none(), "进入事件每 epoch 恰一次");
        assert!(next.safety_mute_reason().is_some());
        // 本块写静音并计数（保持设备时钟）。
        sink.write_block(&output, 128, 128, true)
            .expect("muted silence write");
        assert_eq!(sink.counters.muted_blocks, 1);
        assert_eq!(sink.t21.snapshot().safety_mute_entries, 1);
        let report = sink.report();
        assert!(report.contains("\"safety_mute_entries\":1"), "{report}");
        assert!(report.contains("\"session_state\":\"Muted\""), "{report}");
    }

    #[test]
    fn t71_alsa_sink_capture_failure_clears_stale_loopback_input() {
        // F4：读失败后不得把上一 period 旧样本当新输入重放；本块 graph 输入必须静音。
        let (mut sink, _actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Loopback);
        let first = sink.read_capture().expect("first capture");
        assert!(first.safety_mute_reason().is_none());
        assert!(
            sink.capture_planar.iter().any(|sample| *sample != 0.0),
            "首块 loopback capture 应为非零测试图样"
        );
        sink.session.backend_mut().next_read_error = Some(AlsaError::Xrun);
        let outcome = sink.read_capture().expect("recoverable capture xrun");
        let event = outcome.event().expect("overrun event");
        assert_eq!(event.kind, khaen_core::events::EV_INPUT_OVERRUN);
        assert_eq!(
            sink.session.state(),
            khaen_rt_host::alsa::DuplexState::Running
        );
        assert!(
            sink.capture_planar.iter().all(|sample| *sample == 0.0),
            "读失败后 capture_planar 必须清零（F4）"
        );
        let mut input = vec![1.0f32; 2 * 128];
        sink.fill_loopback_input(&mut input, 128, 128)
            .expect("fill loopback");
        assert!(
            input.iter().all(|sample| *sample == 0.0),
            "失败块 graph 输入必须静音（不得重放旧音频）"
        );
    }

    #[test]
    fn t71_alsa_sink_xrun_event_reports_consecutive_count() {
        // F2：连续 xrun 的 value_a 递增；成功写后归零。事件一个 block 只推 1 个，
        // 恢复成功产生的 EV_EPOCH_START 会顺延到下一 block（本测试按顺序取事件）。
        let (mut sink, _actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Synth);
        let output = tone_output(128);
        // F-v5-2：先显式预填一次（清 prime_needed），之后每次恢复完成也显式重预填，
        // 避免自动重预填把后续注入的写故障吃掉（注入只针对真实块写）。
        sink.prime_playback().expect("startup prime");
        sink.session.backend_mut().next_write_error = Some(AlsaError::Xrun);
        let first = sink
            .write_block(&output, 128, 128, false)
            .expect("recoverable xrun")
            .expect("underrun event");
        assert_eq!(first.kind, khaen_core::events::EV_OUTPUT_UNDERRUN);
        assert_eq!(first.severity, khaen_core::events::SEVERITY_ERROR);
        assert_eq!(first.value_a, 1, "第 1 次连续 underrun");
        assert_eq!(first.value_b, 8, "设备缓冲容量");
        // 第 2 次 xrun：本块先返回上一块顺延的 EV_EPOCH_START。
        sink.prime_playback()
            .expect("re-prime after first recovery");
        sink.session.backend_mut().next_write_error = Some(AlsaError::Xrun);
        let deferred = sink
            .write_block(&output, 128, 128, false)
            .expect("recoverable xrun")
            .expect("epoch event");
        assert_eq!(deferred.kind, khaen_core::events::EV_EPOCH_START);
        // 下一块（正常写）取回第 2 次 underrun：value_a 必须递增为 2。
        sink.prime_playback()
            .expect("re-prime after second recovery");
        let second = sink
            .write_block(&output, 128, 128, false)
            .expect("clean write")
            .expect("underrun event");
        assert_eq!(second.kind, khaen_core::events::EV_OUTPUT_UNDERRUN);
        assert_eq!(second.severity, khaen_core::events::SEVERITY_ERROR);
        assert_eq!(second.value_a, 2, "连续第 2 次 underrun");
        // 成功写后归零：再触发一次 xrun，value_a 回到 1。
        sink.session.backend_mut().next_write_error = Some(AlsaError::Xrun);
        let after = sink
            .write_block(&output, 128, 128, false)
            .expect("recoverable xrun")
            .expect("underrun event");
        assert_eq!(after.value_a, 1, "成功写后 consecutive 归零");
        assert!(sink.counters.xruns >= 3, "三次注入的 xrun 都要计数");
    }

    #[test]
    fn t71_alsa_sink_capture_nonrecoverable_fails_closed() {
        // F1 边界：只有 Muted（SafetyMute）可继续；不可恢复 fault 仍 fail closed。
        let (mut sink, _actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Synth);
        sink.session.backend_mut().next_read_error = Some(AlsaError::Unsupported);
        let error = sink
            .read_capture()
            .expect_err("capture 不可恢复必须 fail closed");
        assert!(error.contains("不可恢复"), "{error}");
        assert_eq!(
            sink.session.state(),
            khaen_rt_host::alsa::DuplexState::Faulted
        );
        let error = sink
            .read_capture()
            .expect_err("Faulted 必须继续 fail closed");
        assert!(error.contains("Faulted"), "{error}");
    }

    #[test]
    fn t71_run_loop_with_mock_alsa_sink_survives_muted() {
        // F1 集成（O5）：真实 run 块循环 + mock ALSA sink。第 0 块写 xrun → 恢复耗尽进入
        // Muted；旧实现下一块 read_capture 得到 ERR_SWITCH_BUSY → run 返回 Err（exit 1）。
        // 修复后按 SafetyMute「不处理新块」继续写静音直到 max_blocks。
        let mut backend = RecordingBackend::motu();
        // F-v5-2：run loop 起播预填先写 6 个 period（buffer 8 - 2）；块 0 的真实写是第 7 次
        // → 用 write_error_after 精确把 xrun 落在第 0 块（不再被预填写吃掉）。
        backend.write_error_after = Some(6);
        backend.recover_failures = 255;
        let mut session = DuplexSession::new(
            backend,
            alsa_request(),
            RecoveryPolicy::new(2, 3),
            HwOpenGate::denied(),
        );
        let actual = session
            .start(&DeviceSelector::by_id("UltraLitemk5", 0), CARDS_MOTU_LINE)
            .expect("mock duplex start");
        let sink_config = HostRunConfig {
            backend: "alsa".to_string(),
            ..HostRunConfig::default()
        };
        let sink = PcmPlaybackSink::started(&sink_config, 2, AlsaIoMode::Synth, session, actual)
            .expect("sink started");
        let config_path = std::env::current_exe()
            .expect("current_exe")
            .parent()
            .expect("test exe dir")
            .join(format!(
                "khaen-rt-host-t71-muted-{}.toml",
                std::process::id()
            ));
        let config = concat!(
            "[host]\n",
            "block_size = 128\n",
            "sample_rate = 48000\n",
            "workers = 4\n",
            "graph = \"stereo_identity\"\n",
            "safe_mode = \"normal\"\n",
            "[alsa]\n",
            "backend = \"alsa\"\n",
            "io_mode = \"synth\"\n",
            "device = \"hw:UltraLitemk5,0\"\n",
            "capture_channels = 20\n",
            "playback_channels = 22\n",
            "period_frames = 128\n",
            "buffer_frames = 1024\n",
            "[rt]\n",
            "dsp_cpus = \"2-5\"\n",
            "tick_freq_hz = 1000000000\n",
            "[web]\n",
            "bind = \"127.0.0.1:1\"\n",
            "[run]\n",
            "max_blocks = 8\n",
        );
        std::fs::write(&config_path, config).expect("write temp rt-host config");
        let json = run_command_with_sink(
            config_path.to_str().expect("utf8 config path"),
            None,
            None,
            Some(Box::new(sink)),
        )
        .expect("run loop must survive Muted (no exit 1)");
        let _ = std::fs::remove_file(&config_path);
        assert!(json.contains("\"status\":\"ok\""), "run 失败: {json}");
        assert!(
            json.contains("\"blocks\":8"),
            "Muted 后必须继续跑满块数: {json}"
        );
        assert!(
            json.contains("\"muted_blocks\":7"),
            "第 1..=7 块必须写静音并计数: {json}"
        );
        assert!(
            json.contains("\"safety_mute_blocks\":7"),
            "loop 侧 SafetyMute 块计数: {json}"
        );
        assert!(
            json.contains("\"safety_mute_entries\":1"),
            "F3：safety_mute_entries 必须真实计数: {json}"
        );
        assert!(
            json.contains("\"session_state\":\"Muted\""),
            "sink 会话应停在 Muted: {json}"
        );
        assert!(
            json.contains("\"by_kind\":{") && json.contains("\"23\":1"),
            "T21 必须含且仅含 1 次 EV_SAFETY_MUTE_ENTER（不得由 loop 重复合成）: {json}"
        );
        assert!(json.contains("\"19\":1"), "第 0 块 underrun 事件: {json}");
        assert!(json.contains("\"graceful\":true"), "{json}");
    }

    /// F-v5-2 复现用 mock：按「块循环 1 块 = 1 个 capture period」建模只有 1 period
    /// 余量的 ALSA playback 设备（sw 缓冲离散模型）：
    /// - 每个块开始时设备先消费 1 period（时间推进 1 period）；
    /// - 消费后缓冲见底 = 设备在本次 write 之前已 XRUN → 本次写命中 EPIPE（计 1 underrun）；
    /// - XRUN 恢复 = prepare 清空缓冲、下一次写再自动启动（T17 恢复语义极简等价）；
    /// - priming_enabled=false = 修复前 host（只写 1 period 就进入稳态）→ 写/恢复交替；
    /// - priming_enabled=true = 起播预填 buffer_periods-2 个 period 静音 → 0 underrun。
    struct StarvingPlaybackSink {
        buffer_periods: usize,
        priming_enabled: bool,
        buffered_periods: usize,
        running: bool,
        pending_xrun: bool,
        blocks: u64,
        xruns: u64,
        prime_blocks: u64,
    }

    impl StarvingPlaybackSink {
        fn new(priming_enabled: bool) -> Self {
            Self {
                buffer_periods: 8,
                priming_enabled,
                buffered_periods: 0,
                running: false,
                pending_xrun: false,
                blocks: 0,
                xruns: 0,
                prime_blocks: 0,
            }
        }
    }

    impl PlaybackSink for StarvingPlaybackSink {
        fn read_capture(&mut self) -> Result<CaptureOutcome, String> {
            Ok(CaptureOutcome::Captured(None))
        }

        fn has_loopback_input(&self) -> bool {
            false
        }

        fn fill_loopback_input(
            &self,
            _dst: &mut [f32],
            _stride: usize,
            _samples: usize,
        ) -> Result<(), String> {
            Ok(())
        }

        fn prime_playback(&mut self) -> Result<(), String> {
            if !self.priming_enabled || self.running || self.prime_blocks > 0 {
                return Ok(());
            }
            let cushion = self.buffer_periods.saturating_sub(2).max(1);
            self.buffered_periods = cushion;
            self.running = true; // 首次预填 writei 自动启动（T17 D-07）
            self.prime_blocks = cushion as u64;
            Ok(())
        }

        fn write_block(
            &mut self,
            _output: &[f32],
            _stride: usize,
            _frames: usize,
            _mute: bool,
        ) -> Result<Option<SinkEvent>, String> {
            // 时间推进 1 个 period：运行中的设备消费 1 period；见底即设备在写前已跑空。
            if self.running {
                if self.buffered_periods == 0 {
                    self.running = false;
                    self.pending_xrun = true;
                } else {
                    self.buffered_periods -= 1;
                    if self.buffered_periods == 0 {
                        self.running = false;
                        self.pending_xrun = true;
                    }
                }
            }
            if self.pending_xrun {
                // XRUN（EPIPE）：本次写不落音频；恢复 prepare 清空缓冲，下一次写自动启动。
                self.pending_xrun = false;
                self.xruns += 1;
                self.buffered_periods = 0;
                return Ok(Some(SinkEvent {
                    kind: EV_OUTPUT_UNDERRUN,
                    lane: LANE_PLAYBACK,
                    severity: SEVERITY_ERROR,
                    value_a: 1,
                    value_b: self.buffer_periods as u32,
                }));
            }
            // 正常写：1 period 入缓冲（稳态留 2 period 头 room，写会阻塞等空间）。
            if self.buffered_periods >= self.buffer_periods {
                self.buffered_periods = self.buffer_periods - 1;
            }
            self.buffered_periods += 1;
            self.running = true;
            self.blocks += 1;
            Ok(None)
        }

        fn stop(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn report(&self) -> String {
            format!(
                "{{\"backend\":\"starving-mock\",\"period_frames\":128,\"buffer_frames\":1024,\"prime_blocks\":{},\"blocks\":{},\"xruns\":{},\"underruns\":{}}}",
                self.prime_blocks, self.blocks, self.xruns, self.xruns
            )
        }
    }

    /// 与 muted 集成测试同款 ALSA host 配置（真实 sink 由调用方注入）。
    fn t71_injected_sink_config_path(tag: &str, max_blocks: u64) -> std::path::PathBuf {
        let config_path = std::env::current_exe()
            .expect("current_exe")
            .parent()
            .expect("test exe dir")
            .join(format!(
                "khaen-rt-host-t71-{tag}-{}.toml",
                std::process::id()
            ));
        let config = format!(
            concat!(
                "[host]\n",
                "block_size = 128\n",
                "sample_rate = 48000\n",
                "workers = 4\n",
                "graph = \"stereo_identity\"\n",
                "safe_mode = \"normal\"\n",
                "[alsa]\n",
                "backend = \"alsa\"\n",
                "io_mode = \"synth\"\n",
                "device = \"hw:UltraLitemk5,0\"\n",
                "capture_channels = 20\n",
                "playback_channels = 22\n",
                "period_frames = 128\n",
                "buffer_frames = 1024\n",
                "[rt]\n",
                "dsp_cpus = \"2-5\"\n",
                "tick_freq_hz = 1000000000\n",
                "[web]\n",
                "bind = \"127.0.0.1:1\"\n",
                "[run]\n",
                "max_blocks = {}\n"
            ),
            max_blocks
        );
        std::fs::write(&config_path, config).expect("write temp rt-host config");
        config_path
    }

    #[test]
    fn t71_run_loop_starving_playback_without_priming_reproduces_2to1() {
        // F-v5-2 缺陷复现（修复前行为）：起播不预填 → 只有 1 period 余量 → 写-恢复交替，
        // main blocks : alsa blocks = 2:1（v5 板窗 13,198 : 6,600，xruns 6,598）。
        let config_path = t71_injected_sink_config_path("starving-unprimed", 8);
        let json = run_command_with_sink(
            config_path.to_str().expect("utf8 config path"),
            None,
            None,
            Some(Box::new(StarvingPlaybackSink::new(false))),
        )
        .expect("run loop with starving device");
        let _ = std::fs::remove_file(&config_path);
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        assert!(json.contains("\"blocks\":8"), "主块循环跑满 8 块: {json}");
        // 成功写 4 / xrun 4（8 块）：正好复现 main:alsa blocks=2:1。
        assert!(
            json.contains("\"prime_blocks\":0,\"blocks\":4,\"xruns\":4,\"underruns\":4"),
            "无预填必须复现 2:1 写-恢复交替: {json}"
        );
        assert!(json.contains("\"19\":4"), "4 次 EV_OUTPUT_UNDERRUN: {json}");
    }

    #[test]
    fn t71_run_loop_primed_playback_zero_xruns() {
        // F-v5-2 修复效果：起播预填 6 period cushion 后同一设备模型 0 underrun，
        // main blocks : alsa blocks = 1:1（验收要求 xrun=0 且写入音频≈墙钟）。
        let config_path = t71_injected_sink_config_path("starving-primed", 8);
        let json = run_command_with_sink(
            config_path.to_str().expect("utf8 config path"),
            None,
            None,
            Some(Box::new(StarvingPlaybackSink::new(true))),
        )
        .expect("run loop with primed device");
        let _ = std::fs::remove_file(&config_path);
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        assert!(json.contains("\"blocks\":8"), "主块循环跑满 8 块: {json}");
        assert!(
            json.contains("\"prime_blocks\":6,\"blocks\":8,\"xruns\":0,\"underruns\":0"),
            "预填后必须 1:1 且 xrun=0: {json}"
        );
        assert!(
            !json.contains("\"19\":"),
            "不得再出现 underrun 事件: {json}"
        );
    }

    #[test]
    fn t71_run_loop_primes_pcm_sink_before_first_block() {
        // 接线断言：run loop 在进入块循环前调用 sink.prime_playback()（真实
        // PcmPlaybackSink 实现 → 6 period 静音预填），稳态仍 1 block : 1 period。
        let mut backend = RecordingBackend::motu();
        backend.recover_failures = 0;
        let mut session = DuplexSession::new(
            backend,
            alsa_request(),
            RecoveryPolicy::new(2, 3),
            HwOpenGate::denied(),
        );
        let actual = session
            .start(&DeviceSelector::by_id("UltraLitemk5", 0), CARDS_MOTU_LINE)
            .expect("mock duplex start");
        let sink_config = HostRunConfig {
            backend: "alsa".to_string(),
            ..HostRunConfig::default()
        };
        let sink = PcmPlaybackSink::started(&sink_config, 2, AlsaIoMode::Synth, session, actual)
            .expect("sink started");
        let config_path = t71_injected_sink_config_path("prime-wiring", 2);
        let json = run_command_with_sink(
            config_path.to_str().expect("utf8 config path"),
            None,
            None,
            Some(Box::new(sink)),
        )
        .expect("run loop must prime injected pcm sink");
        let _ = std::fs::remove_file(&config_path);
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        assert!(
            json.contains("\"prime_blocks\":6"),
            "run loop 必须在块 0 前预填 6 period: {json}"
        );
        assert!(json.contains("\"blocks\":2"), "{json}");
        assert!(json.contains("\"xruns\":0"), "{json}");
    }

    #[test]
    fn t71_alsa_sink_loopback_uses_toslink_capture_channels() {
        let (mut sink, actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Loopback);
        assert!(sink.has_loopback_input());
        let outcome = sink.read_capture().expect("capture read");
        assert!(outcome.event().is_none());
        assert!(
            outcome.safety_mute_reason().is_none(),
            "Running 读不得是 SafetyMute"
        );
        let n_in = actual.hw.n_in as usize;
        let period = actual.hw.period_frames as usize;
        let cfg = FormatConfig {
            hw: actual.hw,
            dither: DitherCfg::Off,
        };
        let raw: Vec<u8> = (0..(n_in * period * 3)).map(|i| i as u8).collect();
        let mut reference = vec![0.0f32; n_in * period];
        format::convert_capture(&cfg, &raw, &mut reference).expect("reference capture");
        let mut input = vec![0.0f32; 2 * period];
        sink.fill_loopback_input(&mut input, period, period)
            .expect("fill loopback");
        assert_eq!(input[..period], reference[12 * period..13 * period]);
        assert_eq!(
            input[period..2 * period],
            reference[13 * period..14 * period]
        );
        assert!(input.iter().any(|sample| *sample != 0.0));
    }

    #[test]
    fn t71_alsa_sink_capture_xrun_reports_overrun_event() {
        let (mut sink, _actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Synth);
        sink.session.backend_mut().next_read_error = Some(AlsaError::Xrun);
        let outcome = sink.read_capture().expect("recoverable read error");
        assert!(
            outcome.safety_mute_reason().is_none(),
            "xrun 恢复成功不是 SafetyMute"
        );
        let event = outcome.event().expect("overrun event");
        assert_eq!(event.kind, khaen_core::events::EV_INPUT_OVERRUN);
        // F2：§8.2 冻结表 severity=3 / consecutive / capacity。
        assert_eq!(event.severity, khaen_core::events::SEVERITY_ERROR);
        assert_eq!(event.value_a, 1, "连续 overrun 数");
        assert_eq!(event.value_b, 8, "设备缓冲容量（1024/128 period）");
        assert_eq!(sink.counters.xruns, 1);
        assert_eq!(sink.counters.overruns, 1);
        assert_eq!(sink.counters.recoveries, 1);
        let next = sink
            .read_capture()
            .expect("read after recovery")
            .event()
            .expect("epoch event");
        assert_eq!(next.kind, khaen_core::events::EV_EPOCH_START);
    }

    #[test]
    fn t71_alsa_sink_short_read_pads_silence() {
        let (mut sink, actual) = sink_with(RecordingBackend::motu(), AlsaIoMode::Synth);
        let period_bytes = actual
            .period_bytes(khaen_rt_host::alsa::StreamDirection::Capture)
            .unwrap();
        sink.session.backend_mut().short_read = Some(period_bytes / 2);
        sink.read_capture().expect("short read is not fatal");
        assert_eq!(sink.counters.short_reads, 1);
        assert!(sink.capture_bytes[period_bytes / 2..]
            .iter()
            .all(|byte| *byte == 0));
    }

    #[test]
    fn t71_alsa_sink_rejects_container_overflow() {
        let backend = RecordingBackend::motu();
        let mut session = DuplexSession::new(
            backend,
            alsa_request(),
            RecoveryPolicy::new(2, 3),
            HwOpenGate::denied(),
        );
        let actual = session
            .start(&DeviceSelector::by_id("UltraLitemk5", 0), CARDS_MOTU_LINE)
            .expect("start");
        let config = HostRunConfig {
            backend: "alsa".to_string(),
            ..HostRunConfig::default()
        };
        let error = match PcmPlaybackSink::started(&config, 23, AlsaIoMode::Synth, session, actual)
        {
            Ok(_) => panic!("23 container 声道超出 22ch 设备"),
            Err(error) => error,
        };
        assert!(error.contains("超出设备 playback"), "{error}");
    }

    #[test]
    fn t71_alsa_config_rate_whitelist_rejects_192k() {
        let mut config = HostRunConfig {
            backend: "alsa".to_string(),
            ..HostRunConfig::default()
        };
        config.sample_rate = 192_000;
        let error = validate_backend_config(&config).expect_err("192k must be rejected");
        assert!(error.contains("192k 禁用"), "{error}");
        config.sample_rate = 44_100;
        assert!(validate_backend_config(&config).is_err());
        config.sample_rate = 96_000;
        assert!(validate_backend_config(&config).is_ok());
    }

    #[test]
    fn t71_alsa_config_period_buffer_channels_validated() {
        let mut config = HostRunConfig {
            backend: "alsa".to_string(),
            ..HostRunConfig::default()
        };
        config.period_frames = 256;
        assert!(validate_backend_config(&config).is_err());
        config.period_frames = 128;
        config.block_size = 64;
        assert!(
            validate_backend_config(&config).is_err(),
            "block_size != period"
        );
        config.block_size = 128;
        config.buffer_frames = 1000;
        assert!(
            validate_backend_config(&config).is_err(),
            "buffer 非 period 整数倍"
        );
        config.buffer_frames = 1024;
        config.capture_channels = 21;
        assert!(validate_backend_config(&config).is_err(), "capture > 20");
        config.capture_channels = 20;
        config.playback_channels = 23;
        assert!(validate_backend_config(&config).is_err(), "playback > 22");
        config.playback_channels = 22;
        config.device = "plughw:4,0".to_string();
        assert!(
            validate_backend_config(&config).is_err(),
            "只接受 hw: 设备串"
        );
        config.device = "hw:4,0".to_string();
        assert!(validate_backend_config(&config).is_ok());
    }

    #[test]
    fn t71_alsa_config_loopback_requires_toslink_capture_width() {
        let mut config = HostRunConfig {
            backend: "alsa".to_string(),
            alsa_io_mode: "loopback".to_string(),
            capture_channels: 13,
            ..HostRunConfig::default()
        };
        let error = validate_backend_config(&config).expect_err("loopback 需要 ch12/13");
        assert!(error.contains("loopback"), "{error}");
        config.capture_channels = 14;
        assert!(validate_backend_config(&config).is_ok());
    }

    #[test]
    fn t71_alsa_backend_unknown_rejected() {
        let config = HostRunConfig {
            backend: "file".to_string(),
            ..HostRunConfig::default()
        };
        let error = validate_backend_config(&config).expect_err("unknown backend");
        assert!(error.contains("allowed: fake|alsa"), "{error}");
    }

    #[test]
    fn t71_alsa_device_string_parsing() {
        assert_eq!(
            parse_alsa_device("hw:4,0").expect("index device"),
            AlsaDeviceSpec {
                card: AlsaCardKey::Index(4),
                device: 0
            }
        );
        assert_eq!(
            parse_alsa_device("hw:UltraLitemk5,0").expect("id device"),
            AlsaDeviceSpec {
                card: AlsaCardKey::Id("UltraLitemk5".to_string()),
                device: 0
            }
        );
        assert!(parse_alsa_device("4,0").is_err());
        assert!(parse_alsa_device("hw:4").is_err());
        assert!(parse_alsa_device("hw:,0").is_err());
        assert!(parse_alsa_device("hw:32,0").is_err());
        assert!(parse_alsa_device("hw:Ultra Lite,0").is_err());
    }

    #[test]
    fn t71_fake_alsa_contract_is_frozen() {
        // final-wiring 冻结证据（manifest.md / logs/host-run.log）中的 fake alsa 契约。
        let config = HostRunConfig::default();
        let json = fake_alsa_contract_json(&config).expect("fake contract");
        assert_eq!(
            json,
            "{\"backend\":\"fake\",\"device\":\"hw:UltraLitemk5,0\",\"owner\":\"host\",\"sample_format\":\"S24_3LE\",\"period_frames\":128,\"buffer_frames\":1024,\"capture_channels\":20,\"playback_channels\":22,\"hwparams_hash\":13741687812926930991}"
        );
        assert_eq!(prepare_backend(&config, 2).expect("fake prepare").0, json);
    }

    #[test]
    fn t71_alsa_io_mode_parsing() {
        assert_eq!(
            AlsaIoMode::parse("synth").expect("synth"),
            AlsaIoMode::Synth
        );
        assert_eq!(
            AlsaIoMode::parse("loopback").expect("loopback"),
            AlsaIoMode::Loopback
        );
        assert!(AlsaIoMode::parse("tolink").is_err());
        assert_eq!(AlsaIoMode::Synth.name(), "synth");
    }

    #[test]
    fn t71_alsa_config_parser_reads_io_mode() {
        let mut warnings = Vec::new();
        let config = parse_host_config(
            "[alsa]\nbackend = \"alsa\"\nio_mode = \"loopback\"\ndevice = \"hw:4,0\"\n",
            &mut warnings,
        )
        .expect("parse config");
        assert_eq!(config.backend, "alsa");
        assert_eq!(config.alsa_io_mode, "loopback");
        assert_eq!(config.device, "hw:4,0");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn t71_alsa_backend_non_linux_returns_clear_error() {
        let config = HostRunConfig {
            backend: "alsa".to_string(),
            ..HostRunConfig::default()
        };
        validate_backend_config(&config).expect("alsa config valid");
        let error = match prepare_backend(&config, 2) {
            Ok(_) => panic!("non-linux must not open PCM"),
            Err(error) => error,
        };
        assert!(error.contains("仅 Linux target"), "{error}");
        assert!(error.contains("NOT_RUN"), "{error}");
    }

    // -----------------------------------------------------------------------
    // T74 前置：capture source = file（WAV/raw 多声道 48k -> 既有 fake/plan-arena 链路）
    // -----------------------------------------------------------------------

    use crate::file_input_test_support::temp_file;

    /// FileCapture::open 的负向断言（FileCapture 不派生 Debug，避免误打印整文件字节）。
    fn file_open_error(config: &HostRunConfig, expected_channels: usize, context: &str) -> String {
        match FileCapture::open(config, expected_channels, None) {
            Ok(_) => panic!("{context}：不应接受"),
            Err(error) => error,
        }
    }

    fn file_capture_body(path: &std::path::Path, extra: &str) -> String {
        format!(
            "path = \"{}\"\nformat = \"wav\"\n{}",
            path.to_string_lossy(),
            extra
        )
    }

    fn run_file_capture_config(
        graph: &str,
        capture_body: &str,
        max_blocks: u64,
    ) -> Result<String, String> {
        let text = format!(
            concat!(
                "[host]\n",
                "block_size = 128\n",
                "sample_rate = 48000\n",
                "workers = 4\n",
                "graph = \"{}\"\n",
                "safe_mode = \"normal\"\n",
                "[alsa]\n",
                "backend = \"fake\"\n",
                "device = \"hw:UltraLitemk5,0\"\n",
                "capture_channels = 20\n",
                "playback_channels = 22\n",
                "period_frames = 128\n",
                "buffer_frames = 1024\n",
                "[rt]\n",
                "dsp_cpus = \"2-5\"\n",
                "tick_freq_hz = 1000000000\n",
                "[web]\n",
                "bind = \"127.0.0.1:1\"\n",
                "[capture]\n",
                "source = \"file\"\n",
                "{}\n",
                "[run]\n",
                "max_blocks = {}\n",
            ),
            graph, capture_body, max_blocks
        );
        let config_path = temp_file("t74-file-config", text.as_bytes());
        let result = run_command(config_path.to_str().expect("utf8 config path"), None, None);
        let _ = std::fs::remove_file(&config_path);
        result
    }

    #[test]
    fn t74_file_wav_2ch_plays_to_eof_and_exits_gracefully() {
        use file_input_test_support as fx;
        let wav = fx::wav_bytes(2, 48_000, fx::WavKind::Pcm24, 300, |frame, channel| {
            if channel == 0 {
                (frame as f32 * 0.01).sin() * 0.25
            } else {
                (frame as f32 * 0.02).cos() * 0.25
            }
        });
        let wav_path = fx::temp_file("t74-2ch-wav", &wav);
        let body = file_capture_body(&wav_path, "eof = \"exit\"\n");
        let result = run_file_capture_config("stereo_identity", &body, 10);
        let _ = std::fs::remove_file(&wav_path);
        let json = result.expect("2ch WAV file run");
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        assert!(json.contains("\"capture\":{\"source\":\"file\""), "{json}");
        assert!(json.contains("\"channels\":2"), "{json}");
        assert!(json.contains("\"layout\":\"2.0-strict\""), "{json}");
        assert!(json.contains("\"sample_format\":\"S24_3LE\""), "{json}");
        assert!(json.contains("\"sample_rate\":48000"), "{json}");
        assert!(json.contains("\"frames\":300"), "{json}");
        assert!(json.contains("\"duration_ms\":6"), "{json}");
        assert!(json.contains("\"eof_reached\":true"), "{json}");
        assert!(json.contains("\"exit_reason\":\"eof\""), "{json}");
        assert!(json.contains("\"blocks\":3"), "{json}");
        assert!(json.contains("\"frames\":384,\"safe_mode\""), "{json}");
        assert!(json.contains("\"tail_padding_frames\":84"), "{json}");
        assert!(json.contains("\"retire_ready\":true"), "{json}");
        assert!(json.contains("\"all_ack_blocks\":3"), "{json}");
        assert!(json.contains("\"graceful\":true"), "{json}");
        assert_eq!(json.matches("\"sha256\":\"").count(), 1, "{json}");
        assert_eq!(json.matches("\"source\":\"file\"").count(), 1, "{json}");
    }

    #[test]
    fn t74_file_eof_silence_pads_tail_and_keeps_running() {
        use file_input_test_support as fx;
        let wav = fx::wav_bytes(2, 48_000, fx::WavKind::Pcm24, 200, |_, _| 0.1);
        let wav_path = fx::temp_file("t74-silence-wav", &wav);
        let body = file_capture_body(&wav_path, "eof = \"silence\"\n");
        let result = run_file_capture_config("stereo_identity", &body, 4);
        let _ = std::fs::remove_file(&wav_path);
        let json = result.expect("eof=silence run");
        assert!(json.contains("\"blocks\":4"), "{json}");
        assert!(json.contains("\"eof_reached\":true"), "{json}");
        assert!(json.contains("\"tail_silence_blocks\":2"), "{json}");
        assert!(json.contains("\"tail_padding_frames\":56"), "{json}");
        assert!(json.contains("\"exit_reason\":\"max_blocks\""), "{json}");
        assert!(json.contains("\"graceful\":true"), "{json}");
    }

    #[test]
    fn t74_file_wav_192k_rejected_with_stable_code() {
        use file_input_test_support as fx;
        let wav = fx::wav_bytes(2, 192_000, fx::WavKind::Pcm24, 64, |_, _| 0.0);
        let wav_path = fx::temp_file("t74-192k-wav", &wav);
        let body = file_capture_body(&wav_path, "eof = \"exit\"\n");
        let error =
            run_file_capture_config("stereo_identity", &body, 2).expect_err("192k file 必须拒绝");
        let _ = std::fs::remove_file(&wav_path);
        assert!(error.starts_with("ERR_HOST_FILE_RATE"), "{error}");
        assert!(error.contains("192k"), "{error}");
        assert!(error.contains("48000"), "{error}");
    }

    #[test]
    fn t74_file_441k_rejected_and_config_192k_rejected() {
        use file_input_test_support as fx;
        let wav = fx::wav_bytes(2, 44_100, fx::WavKind::Pcm24, 32, |_, _| 0.0);
        let wav_path = fx::temp_file("t74-441k-wav", &wav);
        let error = run_file_capture_config(
            "stereo_identity",
            &file_capture_body(&wav_path, "eof = \"exit\"\n"),
            2,
        )
        .expect_err("44.1k file 必须拒绝（file 固定 48k）");
        let _ = std::fs::remove_file(&wav_path);
        assert!(error.starts_with("ERR_HOST_FILE_RATE"), "{error}");
        // 配置 sample_rate=192k 在 backend 白名单即拒绝（既有 192k 禁用口径不变）。
        let text = "[host]\nblock_size = 128\nsample_rate = 192000\nworkers = 4\ngraph = \"stereo_identity\"\n[capture]\nsource = \"file\"\npath = \"x.wav\"\n";
        let config_path = temp_file("t74-cfg-192k", text.as_bytes());
        let config_error = run_command(config_path.to_str().expect("utf8 config path"), None, None)
            .expect_err("sample_rate=192000 必须拒绝");
        let _ = std::fs::remove_file(&config_path);
        assert!(config_error.contains("192k"), "{config_error}");
    }

    #[test]
    fn t74_file_channels_must_match_graph_input_width() {
        use file_input_test_support as fx;
        // 默认 stereo_identity 图输入 2ch：8ch（4.1.2）文件必须拒绝，不做隐式下混。
        let wav = fx::wav_bytes(8, 48_000, fx::WavKind::Pcm24, 64, |_, channel| {
            if channel == 6 {
                0.0
            } else {
                0.1
            }
        });
        let wav_path = fx::temp_file("t74-8ch-mismatch", &wav);
        let error = run_file_capture_config(
            "stereo_identity",
            &file_capture_body(&wav_path, "eof = \"exit\"\n"),
            2,
        )
        .expect_err("8ch file + 2ch 图必须拒绝");
        let _ = std::fs::remove_file(&wav_path);
        assert!(error.starts_with("ERR_HOST_FILE_CHANNELS"), "{error}");
        assert!(error.contains("8"), "{error}");
        assert!(error.contains("2"), "{error}");
    }

    #[test]
    fn t74_file_412_strict_requires_empty_ch6() {
        use file_input_test_support as fx;
        let mut config = HostRunConfig {
            capture_source: "file".to_string(),
            capture_format: "wav".to_string(),
            capture_eof: "exit".to_string(),
            ..HostRunConfig::default()
        };
        // ch6 非零：4.1.2 strict 空槽必须为空，拒绝启动。
        let bad = fx::wav_bytes(8, 48_000, fx::WavKind::Pcm24, 32, |_, channel| {
            if channel == 6 {
                0.5
            } else {
                0.1
            }
        });
        let bad_path = fx::temp_file("t74-412-bad-ch6", &bad);
        config.capture_path = bad_path.to_string_lossy().into_owned();
        let error = file_open_error(&config, 8, "ch6 非零");
        let _ = std::fs::remove_file(&bad_path);
        assert!(error.starts_with("ERR_HOST_FILE_LAYOUT"), "{error}");
        assert!(error.contains("ch6"), "{error}");
        // ch6 全零：接受并报告 4.1.2 布局；8ch float32 也支持。
        let good = fx::wav_bytes(8, 48_000, fx::WavKind::F32, 32, |_, channel| {
            if channel == 6 {
                0.0
            } else {
                0.05
            }
        });
        let good_path = fx::temp_file("t74-412-good", &good);
        config.capture_path = good_path.to_string_lossy().into_owned();
        let file = FileCapture::open(&config, 8, None).expect("4.1.2 8ch float32 WAV 应接受");
        let _ = std::fs::remove_file(&good_path);
        assert_eq!(file.channels, 8);
        assert_eq!(file.layout, "4.1.2");
        assert_eq!(file.sample_kind.name(), "F32_LE");
        assert_eq!(file.frames, 32);
        // 图/plan 输入宽度不是 2/8（如 plan n_in=7）同样拒绝（strict 布局不可隐式转换）。
        let error = file_open_error(&config, 7, "n_in=7 无 strict 布局");
        assert!(error.starts_with("ERR_HOST_FILE_LAYOUT"), "{error}");
    }

    #[test]
    fn t74_file_bad_format_rejected() {
        use file_input_test_support as fx;
        let garbage = fx::temp_file("t74-garbage", b"not a wav file at all");
        let error = run_file_capture_config(
            "stereo_identity",
            &file_capture_body(&garbage, "eof = \"exit\"\n"),
            2,
        )
        .expect_err("garbage 必须拒绝");
        let _ = std::fs::remove_file(&garbage);
        assert!(error.starts_with("ERR_HOST_FILE_FORMAT"), "{error}");
        // 16-bit PCM 不在 T74 允许的 S24_3LE|f32 集合内。
        let pcm16 = fx::wav_bytes(2, 48_000, fx::WavKind::Pcm16, 32, |_, _| 0.0);
        let pcm16_path = fx::temp_file("t74-pcm16", &pcm16);
        let error = run_file_capture_config(
            "stereo_identity",
            &file_capture_body(&pcm16_path, "eof = \"exit\"\n"),
            2,
        )
        .expect_err("PCM16 必须拒绝");
        let _ = std::fs::remove_file(&pcm16_path);
        assert!(error.starts_with("ERR_HOST_FILE_FORMAT"), "{error}");
        assert!(error.contains("24-bit"), "{error}");
        // 截断 WAV（data 声称 1024 B 实际只有 10 B）必须拒绝，不 panic。
        let mut truncated = fx::wav_bytes(2, 48_000, fx::WavKind::Pcm24, 16, |_, _| 0.0);
        let keep = truncated.len() - 40;
        truncated.truncate(keep);
        let truncated_path = fx::temp_file("t74-truncated", &truncated);
        let error = run_file_capture_config(
            "stereo_identity",
            &file_capture_body(&truncated_path, "eof = \"exit\"\n"),
            2,
        )
        .expect_err("截断 WAV 必须拒绝");
        let _ = std::fs::remove_file(&truncated_path);
        assert!(error.starts_with("ERR_HOST_FILE_FORMAT"), "{error}");
    }

    #[test]
    fn t74_file_missing_path_rejected() {
        let missing = std::env::temp_dir().join("tspi-t74-does-not-exist-7f3a.wav");
        let _ = std::fs::remove_file(&missing);
        let error = run_file_capture_config(
            "stereo_identity",
            &file_capture_body(&missing, "eof = \"exit\"\n"),
            2,
        )
        .expect_err("缺失文件必须拒绝");
        assert!(error.starts_with("ERR_HOST_FILE_READ"), "{error}");
    }

    #[test]
    fn t74_file_raw_s24_and_f32_decode_strict() {
        use file_input_test_support as fx;
        let raw = fx::raw_bytes(2, FileSampleKind::S24Le, 4, |frame, channel| {
            match (frame, channel) {
                (0, 0) => 0.25,
                (0, 1) => -0.5,
                (1, 0) => 0.125,
                (1, 1) => -0.25,
                _ => 0.0,
            }
        });
        let raw_path = fx::temp_file("t74-raw-s24", &raw);
        let mut config = HostRunConfig {
            capture_source: "file".to_string(),
            capture_path: raw_path.to_string_lossy().into_owned(),
            capture_format: "raw".to_string(),
            capture_raw_format: "s24_3le".to_string(),
            capture_file_channels: 2,
            capture_eof: "exit".to_string(),
            ..HostRunConfig::default()
        };
        let mut file = FileCapture::open(&config, 2, None).expect("raw S24_3LE open");
        assert_eq!(file.frames, 4);
        assert_eq!(file.channels, 2);
        assert_eq!(file.layout, "2.0-strict");
        assert_eq!(file.sample_kind.name(), "S24_3LE");
        let mut dest = vec![0.0f32; 2 * 128];
        match file.fill_block(&mut dest, 128, 128) {
            FileFill::Data { last } => assert!(last),
            other => panic!("raw 4 帧应一块读完: {other:?}"),
        }
        // planar：ch0 = dest[0..128]、ch1 = dest[128..256]；帧优先交织读入。
        assert_eq!(dest[0].to_bits(), 0.25f32.to_bits());
        assert_eq!(dest[1].to_bits(), 0.125f32.to_bits());
        assert_eq!(dest[128].to_bits(), (-0.5f32).to_bits());
        assert_eq!(dest[129].to_bits(), (-0.25f32).to_bits());
        // 尾部补静音（不沿用上一块残留）。
        assert_eq!(dest[2].to_bits(), 0.0f32.to_bits());
        assert_eq!(dest[130].to_bits(), 0.0f32.to_bits());
        let _ = std::fs::remove_file(&raw_path);

        // raw f32：逐位还原（任意 f32 位型）。
        let f32_raw = fx::raw_bytes(2, FileSampleKind::F32Le, 3, |frame, channel| {
            match (frame, channel) {
                (0, 0) => 0.1,
                (1, 1) => -0.3,
                _ => 0.0,
            }
        });
        let f32_path = fx::temp_file("t74-raw-f32", &f32_raw);
        config.capture_path = f32_path.to_string_lossy().into_owned();
        config.capture_raw_format = "f32le".to_string();
        let mut file = FileCapture::open(&config, 2, None).expect("raw f32le open");
        assert_eq!(file.sample_kind.name(), "F32_LE");
        let mut dest = vec![0.0f32; 2 * 128];
        let _ = file.fill_block(&mut dest, 128, 128);
        assert_eq!(dest[0].to_bits(), 0.1f32.to_bits());
        assert_eq!(dest[129].to_bits(), (-0.3f32).to_bits());
        let _ = std::fs::remove_file(&f32_path);
    }

    #[test]
    fn t74_file_config_validation_codes() {
        let mut config = HostRunConfig {
            capture_source: "file".to_string(),
            capture_path: "x.raw".to_string(),
            capture_format: "raw".to_string(),
            capture_raw_format: "s24_3le".to_string(),
            capture_file_channels: 0,
            ..HostRunConfig::default()
        };
        let error = validate_capture_config(&config).expect_err("raw 缺 channels 必须拒绝");
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFIG"), "{error}");
        config.capture_format = "wav".to_string();
        config.capture_path = String::new();
        let error = validate_capture_config(&config).expect_err("file 缺 path 必须拒绝");
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFIG"), "{error}");
        let good = HostRunConfig::default();
        validate_capture_config(&good).expect("默认 synth 必须通过（向后兼容）");
        let bad_source = HostRunConfig {
            capture_source: "wasapi".to_string(),
            ..HostRunConfig::default()
        };
        let error = validate_capture_config(&bad_source).expect_err("非法 source");
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFIG"), "{error}");
        let bad_eof = HostRunConfig {
            capture_eof: "loop".to_string(),
            ..HostRunConfig::default()
        };
        let error = validate_capture_config(&bad_eof).expect_err("非法 eof");
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFIG"), "{error}");
        let bad_rate = HostRunConfig {
            capture_source: "file".to_string(),
            capture_path: "x.wav".to_string(),
            sample_rate: 192_000,
            ..HostRunConfig::default()
        };
        let error = validate_capture_config(&bad_rate).expect_err("file + 192k 必须拒绝");
        assert!(error.starts_with("ERR_HOST_FILE_RATE"), "{error}");
        assert!(error.contains("192k"), "{error}");
    }

    #[test]
    fn t74_file_loopback_mode_rejected_with_stable_code() {
        use file_input_test_support as fx;
        let wav = fx::wav_bytes(2, 48_000, fx::WavKind::Pcm24, 32, |_, _| 0.0);
        let wav_path = fx::temp_file("t74-loopback-wav", &wav);
        let text = format!(
            concat!(
                "[host]\n",
                "block_size = 128\n",
                "sample_rate = 48000\n",
                "workers = 4\n",
                "graph = \"stereo_identity\"\n",
                "[alsa]\n",
                "backend = \"alsa\"\n",
                "io_mode = \"loopback\"\n",
                "device = \"hw:4,0\"\n",
                "capture_channels = 20\n",
                "playback_channels = 22\n",
                "period_frames = 128\n",
                "buffer_frames = 1024\n",
                "[capture]\n",
                "source = \"file\"\n",
                "path = \"{}\"\n",
                "format = \"wav\"\n",
                "[run]\n",
                "max_blocks = 1\n"
            ),
            wav_path.to_string_lossy()
        );
        let config_path = temp_file("t74-loopback-config", text.as_bytes());
        let error = run_command(config_path.to_str().expect("utf8 config path"), None, None)
            .expect_err("file + loopback 必须拒绝");
        let _ = std::fs::remove_file(&wav_path);
        let _ = std::fs::remove_file(&config_path);
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFLICT"), "{error}");
        assert!(error.contains("loopback"), "{error}");
    }

    #[test]
    fn t74_file_config_parser_reads_capture_section() {
        let mut warnings = Vec::new();
        let config = parse_host_config(
            concat!(
                "[capture]\n",
                "source = \"file\"\n",
                "path = \"C:/tmp/render-4.1.2.wav\"\n",
                "format = \"raw\"\n",
                "raw_format = \"f32le\"\n",
                "channels = 8\n",
                "eof = \"silence\"\n",
                "[alsa]\n",
                "backend = \"fake\"\n",
            ),
            &mut warnings,
        )
        .expect("parse capture config");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(config.capture_source, "file");
        assert_eq!(config.capture_path, "C:/tmp/render-4.1.2.wav");
        assert_eq!(config.capture_format, "raw");
        assert_eq!(config.capture_raw_format, "f32le");
        assert_eq!(config.capture_file_channels, 8);
        assert_eq!(config.capture_eof, "silence");
        // 未配置 [capture] 时保持 synth（既有部署配置向后兼容）。
        let mut warnings = Vec::new();
        let config = parse_host_config("[alsa]\nbackend = \"fake\"\n", &mut warnings)
            .expect("parse legacy config");
        assert_eq!(config.capture_source, "synth");
        assert_eq!(config.capture_eof, "exit");
    }

    #[test]
    fn t74_file_fill_block_zero_alloc() {
        use file_input_test_support as fx;
        let raw = fx::raw_bytes(2, FileSampleKind::F32Le, 8, |_, _| 0.01);
        let path = fx::temp_file("t74-alloc", &raw);
        let config = HostRunConfig {
            capture_source: "file".to_string(),
            capture_path: path.to_string_lossy().into_owned(),
            capture_format: "raw".to_string(),
            capture_raw_format: "f32le".to_string(),
            capture_file_channels: 2,
            capture_eof: "silence".to_string(),
            ..HostRunConfig::default()
        };
        let mut file = FileCapture::open(&config, 2, None).expect("open raw for alloc probe");
        let mut dest = vec![0.0f32; 2 * 128];
        let _ = file.fill_block(&mut dest, 128, 128); // warmup
        alloc_probe::start_recording();
        let _ = file.fill_block(&mut dest, 128, 128); // Ended
        let _ = file.fill_block(&mut dest, 128, 128);
        let allocations = alloc_probe::stop_recording();
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            allocations, 0,
            "F-12：file 块循环必须零堆分配，实测 {allocations} 次"
        );
    }

    #[test]
    fn t74_synth_capture_keeps_default_path() {
        let text = concat!(
            "[host]\n",
            "block_size = 128\n",
            "sample_rate = 48000\n",
            "workers = 4\n",
            "graph = \"stereo_identity\"\n",
            "[alsa]\n",
            "backend = \"fake\"\n",
            "[web]\n",
            "bind = \"127.0.0.1:1\"\n",
            "[run]\n",
            "max_blocks = 2\n"
        );
        let config_path = temp_file("t74-synth-config", text.as_bytes());
        let json = run_command(config_path.to_str().expect("utf8 config path"), None, None)
            .expect("synth default run");
        let _ = std::fs::remove_file(&config_path);
        assert!(
            json.contains("\"capture\":{\"source\":\"synth\"}"),
            "{json}"
        );
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        assert!(json.contains("\"blocks\":2"), "{json}");
    }

    /// mock ALSA-like sink：只记每块写出的 frames/mute 次数（不打开设备）。
    /// 用于证明 file 输入确实进入「图输出 -> playback 写块」这一段现有输出链路。
    struct FileInputMockSink {
        blocks: std::sync::Arc<std::sync::atomic::AtomicU64>,
        frames: std::sync::Arc<std::sync::atomic::AtomicU64>,
        mute_blocks: std::sync::Arc<std::sync::atomic::AtomicU64>,
    }

    impl PlaybackSink for FileInputMockSink {
        fn read_capture(&mut self) -> Result<CaptureOutcome, String> {
            Ok(CaptureOutcome::Captured(None))
        }

        fn has_loopback_input(&self) -> bool {
            false
        }

        fn fill_loopback_input(
            &self,
            _dst: &mut [f32],
            _stride: usize,
            _samples: usize,
        ) -> Result<(), String> {
            Ok(())
        }

        fn write_block(
            &mut self,
            _output: &[f32],
            _stride: usize,
            frames: usize,
            mute: bool,
        ) -> Result<Option<SinkEvent>, String> {
            use std::sync::atomic::Ordering;
            self.blocks.fetch_add(1, Ordering::Relaxed);
            self.frames.fetch_add(frames as u64, Ordering::Relaxed);
            if mute {
                self.mute_blocks.fetch_add(1, Ordering::Relaxed);
            }
            Ok(None)
        }

        fn stop(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn report(&self) -> String {
            "{\"backend\":\"file-input-mock-sink\"}".to_string()
        }
    }

    #[test]
    fn t74_file_input_reaches_playback_sink_blocks() {
        use file_input_test_support as fx;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::Arc;
        let wav = fx::wav_bytes(2, 48_000, fx::WavKind::Pcm24, 300, |frame, channel| {
            if channel == 0 {
                (frame as f32 * 0.01).sin() * 0.25
            } else {
                0.0
            }
        });
        let wav_path = fx::temp_file("t74-sink-wav", &wav);
        let body = file_capture_body(&wav_path, "eof = \"exit\"\n");
        let text = format!(
            concat!(
                "[host]\n",
                "block_size = 128\n",
                "sample_rate = 48000\n",
                "workers = 4\n",
                "graph = \"stereo_identity\"\n",
                "[alsa]\n",
                "backend = \"fake\"\n",
                "[web]\n",
                "bind = \"127.0.0.1:1\"\n",
                "[capture]\n",
                "source = \"file\"\n",
                "{}\n",
                "[run]\n",
                "max_blocks = 0\n"
            ),
            body
        );
        let config_path = temp_file("t74-sink-config", text.as_bytes());
        let blocks = Arc::new(AtomicU64::new(0));
        let frames = Arc::new(AtomicU64::new(0));
        let mute_blocks = Arc::new(AtomicU64::new(0));
        let sink = Box::new(FileInputMockSink {
            blocks: Arc::clone(&blocks),
            frames: Arc::clone(&frames),
            mute_blocks: Arc::clone(&mute_blocks),
        });
        let result = run_command_with_sink(
            config_path.to_str().expect("utf8 config path"),
            None,
            None,
            Some(sink),
        );
        let _ = std::fs::remove_file(&wav_path);
        let _ = std::fs::remove_file(&config_path);
        let json = result.expect("file -> mock playback sink run");
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        assert!(json.contains("\"capture\":{\"source\":\"file\""), "{json}");
        assert!(json.contains("\"exit_reason\":\"eof\""), "{json}");
        assert_eq!(blocks.load(Ordering::Relaxed), 3, "{json}");
        assert_eq!(frames.load(Ordering::Relaxed), 3 * 128);
        assert_eq!(mute_blocks.load(Ordering::Relaxed), 0);
    }

    // -----------------------------------------------------------------------
    // T74 前置：capture source = motu / inferno / aes67（T43 AoIP 端点接线；fake 单测）
    // 真实 ALSA 端点/daemon/PTP/网络/板端出声全部 NOT_RUN（evidence/cursor-fix-capturesrc/）。
    // -----------------------------------------------------------------------

    /// AES67 稳定 card 解析用的 /proc/asound/cards fixture（RAVENNA + MOTU）。
    const CARDS_RAVENNA: &str = " 2 [RAVENNA         ]: MergingRavennaALSA - Merging RAVENNA\n 4 [UltraLitemk5    ]: USB-Audio - UltraLite-mk5\n";
    /// 缺 RAVENNA 的 cards fixture（= 缺端点负向）。
    const CARDS_NO_RAVENNA: &str = " 4 [UltraLitemk5    ]: USB-Audio - UltraLite-mk5\n";

    /// 单条 AES67 flow 的 SDP（H16 每 flow 一个会话；L24/48k/ts-refclk PTPv2）。
    fn aes67_sdp_session(port: u16, channels: u16, rate: u32, dest: &str, domain: u8) -> String {
        format!(
            concat!(
                "v=0\n",
                "o=- 0 0 IN IP4 127.0.0.1\n",
                "s=NuPrime-t74aoip-test\n",
                "c=IN IP4 {}/32\n",
                "t=0 0\n",
                "m=audio {} RTP/AVP 96\n",
                "a=rtpmap:96 L24/{}/{}\n",
                "a=ptime:1\n",
                "a=ts-refclk:ptp=IEEE1588-2008:00-11-22-33-44-55-66-77:{}\n",
                "a=mediaclk:direct=0\n"
            ),
            dest, port, rate, channels, domain
        )
    }

    /// host 配置（backend=alsa；AoIP 端点不会在单测里打开——只解析/校验）。
    fn aoip_host_config(rate: u32, capture_toml: &str) -> HostRunConfig {
        let text = format!(
            concat!(
                "[host]\n",
                "block_size = 128\n",
                "sample_rate = {}\n",
                "workers = 4\n",
                "graph = \"stereo_identity\"\n",
                "safe_mode = \"normal\"\n",
                "[alsa]\n",
                "backend = \"alsa\"\n",
                "device = \"hw:UltraLitemk5,0\"\n",
                "capture_channels = 20\n",
                "playback_channels = 22\n",
                "period_frames = 128\n",
                "buffer_frames = 1024\n",
                "io_mode = \"synth\"\n",
                "[rt]\n",
                "dsp_cpus = \"2-5\"\n",
                "tick_freq_hz = 1000000000\n",
                "[web]\n",
                "bind = \"127.0.0.1:1\"\n",
                "[run]\n",
                "max_blocks = 4\n",
                "[capture]\n",
                "{}"
            ),
            rate, capture_toml
        );
        let mut warnings = Vec::new();
        parse_host_config(&text, &mut warnings).expect("T74 AoIP 测试配置必须可解析")
    }

    const INFERNO_CAPTURE: &str =
        "source = \"inferno\"\nmode = \"dante\"\nptp_lock = \"locked\"\ninferno_iface = \"end0\"\n";
    const AES67_CAPTURE: &str =
        "source = \"aes67\"\nmode = \"aes67\"\nptp_lock = \"locked\"\naes67_daemon_ready = true\n";

    /// 一次 period 的脚本化结果（fake 读取器；不打开设备）。
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum AoipScriptStep {
        /// 写入确定性容器数据（通道 c = (c+1)/32）。
        Data,
        /// 本 period 无数据（EAGAIN/停滞）。
        Stalled,
        /// 端点硬错误（失流）。
        Error,
    }

    /// 脚本化 AoIP period 读取器（T43 失流语义的单测驱动）。
    struct ScriptedAoipReader {
        steps: Vec<AoipScriptStep>,
        next: usize,
        channels: usize,
    }

    impl ScriptedAoipReader {
        fn new(steps: &[AoipScriptStep], channels: usize) -> Self {
            Self {
                steps: steps.to_vec(),
                next: 0,
                channels,
            }
        }
    }

    impl AoipPeriodSource for ScriptedAoipReader {
        fn read_period(
            &mut self,
            dst: &mut [f32],
            stride: usize,
            block: usize,
        ) -> Result<AoipPeriodRead, AoipReadError> {
            let step = if self.next < self.steps.len() {
                self.steps[self.next]
            } else {
                AoipScriptStep::Data
            };
            self.next += 1;
            match step {
                AoipScriptStep::Data => {
                    let mut channel = 0usize;
                    while channel < self.channels {
                        let value = (channel + 1) as f32 / 32.0;
                        dst[channel * stride..channel * stride + block].fill(value);
                        channel += 1;
                    }
                    Ok(AoipPeriodRead::Data {
                        frames: block as u32,
                    })
                }
                AoipScriptStep::Stalled => Ok(AoipPeriodRead::Stalled),
                AoipScriptStep::Error => Err(AoipReadError {
                    error: aoip::AoipError::StreamLost,
                    detail: "scripted AoIP endpoint loss".to_string(),
                }),
            }
        }

        fn kind(&self) -> &'static str {
            "scripted-fake"
        }
    }

    /// 构造 AoIP 运行对象（计划 + 脚本读取器；不打开任何设备）。
    fn scripted_aoip_capture(
        config: &HostRunConfig,
        graph_input_channels: usize,
        steps: &[AoipScriptStep],
        cards_text: &str,
    ) -> AoipCapture {
        let plan = aoip_capture_plan(config, graph_input_channels, cards_text)
            .expect("AoIP 计划必须可用")
            .expect("source 必须解析出 AoIP 计划");
        let channels = plan.container.capture_channels() as usize;
        let reader = Box::new(ScriptedAoipReader::new(steps, channels));
        AoipCapture::start(plan, reader, config.block_size as usize, 0).expect("AoIP 端点启动")
    }

    /// 一个 block 的时长（ns ceil；用于脚本化停滞/失流时间轴）。
    fn aoip_block_ns(rate: u32, block: usize) -> u64 {
        (block as u64 * 1_000_000_000 + rate as u64 - 1) / rate as u64
    }

    #[test]
    fn t74aoip_capture_config_parser_and_mode_mutex() {
        // 旧配置（无 [capture] 段）= synth + mode=off + ptp_lock=unlocked（向后兼容）。
        let mut warnings = Vec::new();
        let legacy = parse_host_config(
            "[host]\nblock_size = 128\nsample_rate = 48000\n[alsa]\nbackend = \"fake\"\n[run]\nmax_blocks = 1\n",
            &mut warnings,
        )
        .expect("旧配置");
        assert_eq!(legacy.capture_source, "synth");
        assert_eq!(legacy.capture_mode, "off");
        assert_eq!(legacy.capture_ptp_lock, "unlocked");
        validate_capture_config(&legacy).expect("legacy synth 配置必须通过");

        // source/mode 一一对应（互斥）：所有错配必须被拒（打开设备前）。
        for (source, mode) in [
            ("inferno", "off"),
            ("inferno", "toslink"),
            ("aes67", "dante"),
            ("motu", "aes67"),
            ("synth", "dante"),
            ("file", "toslink"),
        ] {
            let config = aoip_host_config(
                48_000,
                &format!("source = \"{source}\"\nmode = \"{mode}\"\n"),
            );
            let error =
                validate_capture_config(&config).expect_err("source/mode 错配必须被拒（互斥）");
            assert!(
                error.starts_with("ERR_HOST_CAPTURE_CONFLICT"),
                "{source} x {mode}: {error}"
            );
            assert!(error.contains("mode"), "{error}");
        }
        // 非法 mode/source 值。
        let config = aoip_host_config(48_000, "source = \"synth\"\nmode = \"bogus\"\n");
        let error = validate_capture_config(&config).expect_err("非法 mode 必须被拒");
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFIG"), "{error}");
        let config = aoip_host_config(48_000, "source = \"wasapi\"\nmode = \"off\"\n");
        let error = validate_capture_config(&config).expect_err("非法 source 必须被拒");
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFIG"), "{error}");
        assert!(error.contains("synth|file|motu|inferno|aes67"), "{error}");
    }

    #[test]
    fn t74aoip_motu_requires_alsa_loopback_toslink() {
        // motu = 既有 T71 loopback 路径升格：必须 backend=alsa + io_mode=loopback。
        let mut config = aoip_host_config(48_000, "source = \"motu\"\nmode = \"toslink\"\n");
        config.backend = "fake".to_string();
        let error = validate_capture_config(&config).expect_err("motu + fake 必须被拒");
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFLICT"), "{error}");
        assert!(error.contains("backend=alsa"), "{error}");

        let config = aoip_host_config(48_000, "source = \"motu\"\nmode = \"toslink\"\n");
        let error = validate_capture_config(&config).expect_err("motu 缺 loopback 必须被拒");
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFLICT"), "{error}");
        assert!(error.contains("io_mode=loopback"), "{error}");

        let mut config = aoip_host_config(48_000, "source = \"motu\"\nmode = \"toslink\"\n");
        config.alsa_io_mode = "loopback".to_string();
        validate_capture_config(&config).expect("motu + alsa + loopback + toslink 必须通过");
    }

    #[test]
    fn t74aoip_network_sources_require_alsa_and_profile_rate() {
        // 网络端点需要真实 ALSA backend（fake 无设备输入）。
        let mut config = aoip_host_config(96_000, INFERNO_CAPTURE);
        config.backend = "fake".to_string();
        let error = validate_capture_config(&config).expect_err("inferno + fake 必须被拒");
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFLICT"), "{error}");
        // 协议 profile rate：Inferno 96k / AES67 48k（T43 A14/K42）。
        let config = aoip_host_config(48_000, INFERNO_CAPTURE);
        let error = validate_capture_config(&config).expect_err("inferno 48k 必须被拒");
        assert!(error.starts_with("ERR_RATE"), "{error}");
        assert!(error.contains("96000"), "{error}");
        let config = aoip_host_config(96_000, AES67_CAPTURE);
        let error = validate_capture_config(&config).expect_err("aes67 96k 必须被拒");
        assert!(error.starts_with("ERR_RATE"), "{error}");
        // loopback（MOTU capture ch12/13）与 AoIP 端点互斥。
        let mut config = aoip_host_config(96_000, INFERNO_CAPTURE);
        config.alsa_io_mode = "loopback".to_string();
        let error = validate_capture_config(&config).expect_err("inferno + loopback 必须被拒");
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFLICT"), "{error}");
        // 合法：inferno/aes67 + alsa + 正确 profile rate。
        validate_capture_config(&aoip_host_config(96_000, INFERNO_CAPTURE))
            .expect("inferno 合法配置");
        validate_capture_config(&aoip_host_config(48_000, AES67_CAPTURE)).expect("aes67 合法配置");
    }

    #[test]
    fn t74aoip_inferno_plan_valid_endpoint_and_roles() {
        let config = aoip_host_config(96_000, INFERNO_CAPTURE);
        let plan = aoip_capture_plan(&config, 2, "")
            .expect("inferno 计划")
            .expect("AoIP 计划");
        assert_eq!(plan.source, CaptureSource::Inferno);
        assert_eq!(plan.mode, CaptureMode::Dante);
        assert_eq!(plan.device_name, "inferno");
        assert_eq!(plan.container.capture_channels(), 16);
        assert_eq!(plan.spec.sample_rate_hz, 96_000);
        assert_eq!(plan.spec.device_format, sample_format::S32_LE_INTERLEAVED);
        assert_eq!(plan.spec.period_frames, 128);
        assert_eq!(plan.spec.buffer_frames, 1024);
        assert_eq!(
            plan.spec.ptp_source,
            aoip::PtpSource::StatimePtpV1VirtualClock
        );
        assert!(plan.spec.ptp_source_matches());
        assert_eq!(plan.ptp_lock, LockState::Locked);
        assert_eq!(plan.layout, CAPTURE_LAYOUT_STEREO);
        assert_eq!(plan.role_map.capture_index, vec![Some(0), Some(1)]);
        // 16ch 容器的 4 个非逻辑通道（H16 slot 8/9/12/13）显式忽略并计数。
        assert_eq!(plan.role_map.non_logical_ignored, 4);
        assert_eq!(
            plan.role_map.ignored,
            plan.container.logical_present_mask() & !0b11
        );
    }

    #[test]
    fn t74aoip_inferno_412_role_map_uses_h16_roles() {
        // 8ch 4.1.2：FL FR SL SR TFL TFR 空 LFE -> H16 容器 16 的 capture index [0,1,4,5,10,11,3]。
        let config = aoip_host_config(96_000, INFERNO_CAPTURE);
        let plan = aoip_capture_plan(&config, 8, "")
            .expect("inferno 4.1.2 计划")
            .expect("AoIP 计划");
        assert_eq!(plan.layout, CAPTURE_LAYOUT_412);
        assert_eq!(
            plan.role_map.capture_index,
            vec![
                Some(0),
                Some(1),
                Some(4),
                Some(5),
                Some(10),
                Some(11),
                None,
                Some(3)
            ]
        );
        // 12ch 逻辑容器（8+4 flow）：TFL/TFR 落到 flow2 的 capture 8/9。
        let config = aoip_host_config(
            96_000,
            "source = \"inferno\"\nmode = \"dante\"\nptp_lock = \"locked\"\ninferno_iface = \"end0\"\naoip_channels = 12\n",
        );
        let plan = aoip_capture_plan(&config, 8, "")
            .expect("inferno 12ch 4.1.2 计划")
            .expect("AoIP 计划");
        assert_eq!(plan.container.capture_channels(), 12);
        assert_eq!(
            plan.role_map.capture_index,
            vec![
                Some(0),
                Some(1),
                Some(4),
                Some(5),
                Some(8),
                Some(9),
                None,
                Some(3)
            ]
        );
        assert_eq!(plan.role_map.non_logical_ignored, 0);
        // 图输入宽度非 2/8 -> ERR_ROLES（不猜布局、不做隐式上下混）。
        let error = aoip_capture_plan(&config, 3, "").expect_err("3ch 图输入必须被拒");
        assert!(error.starts_with("ERR_ROLES"), "{error}");
    }

    #[test]
    fn t74aoip_inferno_missing_endpoint_and_unlocked_ptp_rejected() {
        // 缺 iface（InfernoEnv 缺绑定一律拒收）。
        let config = aoip_host_config(
            96_000,
            "source = \"inferno\"\nmode = \"dante\"\nptp_lock = \"locked\"\n",
        );
        let error = validate_capture_config(&config).expect_err("缺 iface 必须被拒");
        assert!(error.starts_with("ERR_DEVICE_PARAMS"), "{error}");
        // 非 inferno 插件 PCM 名。
        let config = aoip_host_config(
            96_000,
            &format!("{INFERNO_CAPTURE}inferno_pcm = \"hw:9,0\"\n"),
        );
        let error = validate_capture_config(&config).expect_err("非法 pcm 名必须被拒");
        assert!(error.starts_with("ERR_DEVICE_PARAMS"), "{error}");
        // 非法 device id。
        let config = aoip_host_config(
            96_000,
            &format!("{INFERNO_CAPTURE}inferno_device_id = \"not-hex\"\n"),
        );
        let error = validate_capture_config(&config).expect_err("非法 device id 必须被拒");
        assert!(error.starts_with("ERR_DEVICE_PARAMS"), "{error}");
        // PTP 未锁：T43 preflight 拒绝开始采集（不得进 RT 主模式）。
        let config = aoip_host_config(
            96_000,
            "source = \"inferno\"\nmode = \"dante\"\nptp_lock = \"unlocked\"\ninferno_iface = \"end0\"\n",
        );
        let error = aoip_capture_plan(&config, 2, "").expect_err("PTP 未锁必须被拒");
        assert!(error.starts_with("ERR_ADMISSION_CLOCK"), "{error}");
        // 非法 ptp_lock 值。
        let config = aoip_host_config(
            96_000,
            "source = \"inferno\"\nmode = \"dante\"\nptp_lock = \"maybe\"\ninferno_iface = \"end0\"\n",
        );
        let error = validate_capture_config(&config).expect_err("非法 ptp_lock 必须被拒");
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFIG"), "{error}");
    }

    #[test]
    fn t74aoip_aes67_plan_with_sdp_and_ravenna_card() {
        let sdp = format!(
            "{}{}",
            aes67_sdp_session(5_004, 8, 48_000, "239.69.1.1", 0),
            aes67_sdp_session(5_006, 8, 48_000, "239.69.1.2", 0)
        );
        let sdp_path = temp_file("t74aoip-aes67-sdp", sdp.as_bytes());
        let config = aoip_host_config(
            48_000,
            &format!(
                "{AES67_CAPTURE}aes67_sdp = \"{}\"\n",
                sdp_path.to_string_lossy()
            ),
        );
        validate_capture_config(&config).expect("AES67 配置合法");
        let plan = aoip_capture_plan(&config, 2, CARDS_RAVENNA)
            .expect("aes67 计划")
            .expect("AoIP 计划");
        let _ = std::fs::remove_file(&sdp_path);
        assert_eq!(plan.source, CaptureSource::Aes67);
        assert_eq!(plan.mode, CaptureMode::Aes67);
        assert_eq!(
            plan.device_name, "hw:2,0",
            "RAVENNA 稳定 card id -> hw:<card>,0"
        );
        assert_eq!(plan.container.capture_channels(), 16);
        assert_eq!(plan.spec.sample_rate_hz, 48_000);
        assert_eq!(
            plan.spec.ptp_source,
            aoip::PtpSource::Aes67DaemonBuiltinPtpV2
        );
        assert_eq!(plan.role_map.capture_index, vec![Some(0), Some(1)]);
        match &plan.detail {
            AoipPlanDetail::Aes67 {
                card,
                flow_count,
                encoding,
                daemon_ready,
                ..
            } => {
                assert_eq!(card, "RAVENNA");
                assert_eq!(*flow_count, 2);
                assert_eq!(*encoding, "L24");
                assert!(*daemon_ready);
            }
            _ => panic!("必须解析为 AES67 计划"),
        }
    }

    #[test]
    fn t74aoip_aes67_missing_endpoint_service_and_bad_sdp_rejected() {
        // 缺 RAVENNA 稳定 card -> ERR_DEVICE_PARAMS（不猜 hw 号）。
        let config = aoip_host_config(48_000, AES67_CAPTURE);
        let error =
            aoip_capture_plan(&config, 2, CARDS_NO_RAVENNA).expect_err("缺 RAVENNA card 必须被拒");
        assert!(error.starts_with("ERR_DEVICE_PARAMS"), "{error}");
        // daemon 未就绪 -> RT_E_DEVICE_LOST（端点不可用；不静默降级）。
        let mut config = aoip_host_config(48_000, AES67_CAPTURE);
        config.capture_aes67_daemon_ready = false;
        let error =
            aoip_capture_plan(&config, 2, CARDS_RAVENNA).expect_err("daemon 未就绪必须被拒");
        assert!(error.starts_with("RT_E_DEVICE_LOST"), "{error}");
        // SDP 44.1k -> ERR_RATE（AES67 媒体面固定 48k）。
        let bad_rate = aes67_sdp_session(5_004, 8, 44_100, "239.69.1.1", 0);
        let path = temp_file("t74aoip-aes67-441", bad_rate.as_bytes());
        let config = aoip_host_config(
            48_000,
            &format!(
                "{AES67_CAPTURE}aes67_sdp = \"{}\"\n",
                path.to_string_lossy()
            ),
        );
        let error = aoip_capture_plan(&config, 2, CARDS_RAVENNA).expect_err("44.1k SDP 必须被拒");
        let _ = std::fs::remove_file(&path);
        assert!(error.starts_with("ERR_RATE"), "{error}");
        // SDP 缺文件（启动前配置校验）-> ERR_HOST_CAPTURE_CONFIG。
        let config = aoip_host_config(
            48_000,
            &format!("{AES67_CAPTURE}aes67_sdp = \"C:/nonexistent/t74aoip.sdp\"\n"),
        );
        let error = validate_capture_config(&config).expect_err("SDP 缺文件必须被拒");
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFIG"), "{error}");
    }

    #[test]
    fn t74aoip_aes67_flow_config_cross_check() {
        // 12ch 逻辑容器 = 8+4 flows：SDP 必须同构（flow 数/每 flow 通道数）。
        let sdp = format!(
            "{}{}",
            aes67_sdp_session(5_004, 8, 48_000, "239.69.2.1", 0),
            aes67_sdp_session(5_006, 4, 48_000, "239.69.2.2", 0)
        );
        let path = temp_file("t74aoip-aes67-12ch", sdp.as_bytes());
        let config = aoip_host_config(
            48_000,
            &format!(
                "{AES67_CAPTURE}aes67_sdp = \"{}\"\naoip_channels = 12\n",
                path.to_string_lossy()
            ),
        );
        let plan = aoip_capture_plan(&config, 8, CARDS_RAVENNA)
            .expect("AES67 12ch 8+4 计划")
            .expect("AoIP 计划");
        let _ = std::fs::remove_file(&path);
        assert_eq!(plan.container.capture_channels(), 12);
        assert_eq!(
            plan.role_map.capture_index,
            vec![
                Some(0),
                Some(1),
                Some(4),
                Some(5),
                Some(8),
                Some(9),
                None,
                Some(3)
            ]
        );
        // SDP 16ch（8+8）与 12ch 配置不一致 -> ERR_DEVICE_PARAMS（flow 通道数交叉校验）。
        let sdp = format!(
            "{}{}",
            aes67_sdp_session(5_004, 8, 48_000, "239.69.3.1", 0),
            aes67_sdp_session(5_006, 8, 48_000, "239.69.3.2", 0)
        );
        let path = temp_file("t74aoip-aes67-16ch", sdp.as_bytes());
        let config = aoip_host_config(
            48_000,
            &format!(
                "{AES67_CAPTURE}aes67_sdp = \"{}\"\naoip_channels = 12\n",
                path.to_string_lossy()
            ),
        );
        let error =
            aoip_capture_plan(&config, 8, CARDS_RAVENNA).expect_err("8+8 SDP 与 12ch 必须被拒");
        let _ = std::fs::remove_file(&path);
        assert!(error.starts_with("ERR_DEVICE_PARAMS"), "{error}");
    }

    #[test]
    fn t74aoip_capture_feeds_graph_input_by_role() {
        let config = aoip_host_config(96_000, INFERNO_CAPTURE);
        let mut capture = scripted_aoip_capture(
            &config,
            2,
            &[AoipScriptStep::Data, AoipScriptStep::Data],
            "",
        );
        let stride = 128usize;
        let block = 128usize;
        let rate = 96_000u32;
        let mut input = vec![0.0f32; 2 * stride];
        let mut block_index = 0u64;
        while block_index < 2 {
            let now = block_index * aoip_block_ns(rate, block);
            aoip_input_phase(Some(&mut capture), &mut input, stride, block, false, now)
                .expect("AoIP period -> 图输入");
            // ScriptedAoipReader: ch0 = 1/32、ch1 = 2/32 -> 显式 FL/FR 映射。
            assert_eq!(input[0], 1.0 / 32.0);
            assert_eq!(input[stride], 2.0 / 32.0);
            block_index += 1;
        }
        assert_eq!(capture.blocks, 2);
        assert_eq!(capture.data_blocks, 2);
        assert_eq!(capture.silent_blocks, 0);
        let report = capture.report_json(2, "max_blocks");
        capture.stop();
        assert!(report.contains("\"source\":\"inferno\""), "{report}");
        assert!(report.contains("\"mode\":\"dante\""), "{report}");
        assert!(report.contains("\"transport\":\"alsa\""), "{report}");
        assert!(report.contains("\"reader\":\"scripted-fake\""), "{report}");
        assert!(report.contains("\"sample_format\":\"S32_LE\""), "{report}");
        assert!(report.contains("\"container_channels\":16"), "{report}");
        assert!(report.contains("\"graph_input_channels\":2"), "{report}");
        assert!(report.contains("\"layout\":\"2.0-strict\""), "{report}");
        assert!(report.contains("\"sample_rate\":96000"), "{report}");
        assert!(
            report.contains("\"ptp_source\":\"statime-ptpv1-virtual-clock\""),
            "{report}"
        );
        assert!(report.contains("\"ptp_lock\":\"locked\""), "{report}");
        assert!(report.contains("\"state\":\"streaming\""), "{report}");
        assert!(report.contains("\"non_logical_ignored\":4"), "{report}");
        assert!(
            report.contains("\"inferno\":{\"pcm\":\"inferno\""),
            "{report}"
        );
        assert!(report.contains("NOT_RUN"), "{report}");
    }

    #[test]
    fn t74aoip_capture_stall_mutes_then_resumes_same_epoch() {
        // T43：停滞 -> 整块静音；数据恢复 -> 同 epoch Resumed（不半块、不残留旧数据）。
        let config = aoip_host_config(96_000, INFERNO_CAPTURE);
        let steps = [
            AoipScriptStep::Stalled,
            AoipScriptStep::Stalled,
            AoipScriptStep::Stalled,
            AoipScriptStep::Stalled,
            AoipScriptStep::Stalled,
            AoipScriptStep::Data,
            AoipScriptStep::Data,
        ];
        let mut capture = scripted_aoip_capture(&config, 2, &steps, "");
        let stride = 128usize;
        let block = 128usize;
        let rate = 96_000u32;
        let t = aoip_block_ns(rate, block);
        let mut input = vec![7.0f32; 2 * stride];
        let mut index = 0u64;
        while index < 5 {
            let now = index * t;
            aoip_input_phase(Some(&mut capture), &mut input, stride, block, false, now)
                .expect("stall 块必须静音继续");
            assert_eq!(input[0], 0.0, "停滞块必须整块静音（不带旧数据）");
            assert_eq!(input[stride], 0.0);
            index += 1;
        }
        assert_eq!(capture.silent_blocks, 5);
        assert_eq!(capture.data_blocks, 0);
        assert_eq!(capture.stall_events, 1, "停滞事件只在迁移时计一次");
        // 数据恢复（第 6 块，now=5t）：同 epoch Resumed。
        let now = 5 * t;
        aoip_input_phase(Some(&mut capture), &mut input, stride, block, false, now)
            .expect("数据恢复必须继续同 epoch");
        assert_eq!(input[0], 1.0 / 32.0);
        assert_eq!(input[stride], 2.0 / 32.0);
        assert_eq!(capture.resume_events, 1);
        assert_eq!(capture.data_blocks, 1);
        let report = capture.report_json(6, "max_blocks");
        capture.stop();
        assert!(report.contains("\"stall_events\":1"), "{report}");
        assert!(report.contains("\"resume_events\":1"), "{report}");
        assert!(report.contains("\"silent_blocks\":5"), "{report}");
    }

    #[test]
    fn t74aoip_capture_sustained_stall_fails_closed() {
        // T43：持续失流（> 2x 停滞门限）必须新 epoch；host 无 epoch owner -> fail closed。
        let config = aoip_host_config(96_000, INFERNO_CAPTURE);
        let steps = [AoipScriptStep::Stalled; 10];
        let mut capture = scripted_aoip_capture(&config, 2, &steps, "");
        let stride = 128usize;
        let block = 128usize;
        let rate = 96_000u32;
        let t = aoip_block_ns(rate, block);
        let mut input = vec![0.0f32; 2 * stride];
        let mut error = None;
        let mut index = 0u64;
        while index < 10 {
            let now = index * t;
            match aoip_input_phase(Some(&mut capture), &mut input, stride, block, false, now) {
                Ok(()) => index += 1,
                Err(err) => {
                    error = Some(err);
                    break;
                }
            }
        }
        capture.stop();
        let error = error.expect("持续失流必须 fail closed（不得无限静音冒充恢复）");
        assert!(error.starts_with("RT_E_DEVICE_LOST"), "{error}");
        assert!(error.contains("NOT_WIRED"), "{error}");
    }

    #[test]
    fn t74aoip_capture_endpoint_error_fails_closed() {
        let config = aoip_host_config(96_000, INFERNO_CAPTURE);
        let steps = [AoipScriptStep::Data, AoipScriptStep::Error];
        let mut capture = scripted_aoip_capture(&config, 2, &steps, "");
        let stride = 128usize;
        let block = 128usize;
        let rate = 96_000u32;
        let t = aoip_block_ns(rate, block);
        let mut input = vec![0.0f32; 2 * stride];
        aoip_input_phase(Some(&mut capture), &mut input, stride, block, false, 0)
            .expect("首块数据正常");
        let error = aoip_input_phase(Some(&mut capture), &mut input, stride, block, false, t)
            .expect_err("端点错误必须 fail closed");
        capture.stop();
        assert!(error.starts_with("RT_E_DEVICE_LOST"), "{error}");
    }

    #[test]
    fn t74aoip_muted_blocks_do_not_consume_endpoint_data() {
        // SafetyMute/gate：不处理新块（不消费端点 period），输入清零，监视器时间轴仍推进。
        let config = aoip_host_config(96_000, INFERNO_CAPTURE);
        let steps = [AoipScriptStep::Data, AoipScriptStep::Data];
        let mut capture = scripted_aoip_capture(&config, 2, &steps, "");
        let stride = 128usize;
        let block = 128usize;
        let mut input = vec![3.0f32; 2 * stride];
        aoip_input_phase(Some(&mut capture), &mut input, stride, block, true, 0).expect("mute 块");
        assert_eq!(input[0], 0.0);
        assert_eq!(capture.muted_blocks, 1);
        assert_eq!(capture.data_blocks, 0, "mute 块不消费端点数据");
        capture.stop();
    }

    #[test]
    fn t74aoip_run_rejects_network_source_with_fake_backend() {
        // run 入口（不是只 plan）：网络 source + fake backend 必须在打开设备前拒绝。
        let text = format!(
            concat!(
                "[host]\nblock_size = 128\nsample_rate = 96000\nworkers = 4\n",
                "graph = \"stereo_identity\"\nsafe_mode = \"normal\"\n",
                "[alsa]\nbackend = \"fake\"\ndevice = \"hw:UltraLitemk5,0\"\n",
                "capture_channels = 20\nplayback_channels = 22\nperiod_frames = 128\n",
                "buffer_frames = 1024\n",
                "[rt]\ndsp_cpus = \"2-5\"\ntick_freq_hz = 1000000000\n",
                "[web]\nbind = \"127.0.0.1:1\"\n",
                "[capture]\n{}\n",
                "[run]\nmax_blocks = 1\n"
            ),
            INFERNO_CAPTURE
        );
        let config_path = temp_file("t74aoip-run-config", text.as_bytes());
        let result = run_command(config_path.to_str().expect("utf8 config path"), None, None);
        let _ = std::fs::remove_file(&config_path);
        let error = result.expect_err("inferno + fake backend 必须被拒");
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFLICT"), "{error}");
        assert!(error.contains("backend=alsa"), "{error}");
    }

    #[test]
    fn t74aoip_synth_and_file_capture_modes_stay_off_and_compatible() {
        // 既有 synth/file 输入与 mode=off 兼容（向后兼容：不配置 [capture] 也安全）。
        let config = aoip_host_config(48_000, "source = \"synth\"\nmode = \"off\"\n");
        validate_capture_config(&config).expect("synth + off");
        let config = aoip_host_config(
            48_000,
            "source = \"file\"\nmode = \"off\"\npath = \"C:/tmp/t74aoip-2ch.wav\"\n",
        );
        // 文件不存在在 FileCapture::open 才拒绝；配置层 synth/file + mode=off 必须通过。
        validate_capture_config(&config).expect("file + off 配置层合法");
        // synth + mode=toslink 必须被拒（一个生效模式只有一个输入 owner）。
        let config = aoip_host_config(48_000, "source = \"synth\"\nmode = \"toslink\"\n");
        let error = validate_capture_config(&config).expect_err("synth + toslink 必须被拒");
        assert!(error.starts_with("ERR_HOST_CAPTURE_CONFLICT"), "{error}");
    }

    // -----------------------------------------------------------------------
    // F6/task-216：RT 内核应用 + worker 自旋策略（本机 fake；真实 Linux 写内核 NOT_RUN）
    // -----------------------------------------------------------------------

    #[test]
    fn t45apply_scheduler_json_reports_not_applied_explicitly() {
        // apply_affinity=false：必须显式给 kernel_applied=false + reason，不允许"假装已 RT"。
        let config = HostRunConfig::default();
        let (sched, _topo) = build_sched_plan(&config).expect("sched plan");
        let json = scheduler_report_json(&sched, None);
        assert!(json.contains("\"kernel_applied\":false"), "{json}");
        assert!(json.contains("\"kernel_apply\":{"), "{json}");
        assert!(
            json.contains("\"reason\":\"rt.apply_affinity=false\""),
            "{json}"
        );
        assert!(json.contains("\"backend\":\"none\""), "{json}");
        assert!(json.contains("\"sched_policy\":\"none\""), "{json}");
        // 既有字段不回归（dsp_cpus/优先级/hash 仍在 scheduler 段）。
        assert!(json.contains("\"dsp_cpus\":[4,5,6,7]"), "{json}");
        assert!(json.contains("\"affinity_hash\":\""), "{json}");
    }

    #[test]
    fn t45apply_rt_not_applied_warning_only_for_alsa() {
        let mut config = HostRunConfig::default();
        config.backend = "alsa".to_string();
        let warning = rt_not_applied_warning(&config).expect("alsa 未应用 RT 必须显式告警");
        assert!(warning.contains("F6"), "{warning}");
        assert!(warning.contains("apply_affinity=true"), "{warning}");
        config.apply_affinity = true;
        assert!(
            rt_not_applied_warning(&config).is_none(),
            "已请求应用时不得告警"
        );
        let mut fake = HostRunConfig::default();
        fake.backend = "fake".to_string();
        assert!(
            rt_not_applied_warning(&fake).is_none(),
            "fake 后端无需 RT 告警"
        );
    }

    #[test]
    fn t45apply_parse_rt_io_cpu_accepts_auto_or_index() {
        assert_eq!(
            parse_rt_io_cpu("auto").expect("auto"),
            khaen_rt_host::scheduler::CPU_UNUSED
        );
        assert_eq!(parse_rt_io_cpu("0").expect("cpu0"), 0);
        assert_eq!(parse_rt_io_cpu("7").expect("cpu7"), 7);
        assert!(parse_rt_io_cpu("").is_ok(), "空值 = auto");
        assert!(parse_rt_io_cpu("99").is_err(), "越界 CPU 必须拒绝");
        assert!(parse_rt_io_cpu("x").is_err(), "非数字必须拒绝");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn t45apply_runtime_prepare_fails_closed_on_non_linux() {
        let config = HostRunConfig {
            apply_affinity: true,
            ..HostRunConfig::default()
        };
        let (sched, _topo) = build_sched_plan(&config).expect("plan");
        let error = RtRuntime::prepare(&config, &sched).expect_err("非 Linux 必须 fail closed");
        assert!(error.contains("不是 Linux target"), "{error}");
        assert!(error.contains("fail-closed"), "{error}");
        // apply_affinity=false：不产生 RT 上下文（只计划），也不尝试任何内核写入。
        let disabled = HostRunConfig::default();
        assert!(RtRuntime::prepare(&disabled, &sched)
            .expect("disabled 不报错")
            .is_none());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn t45apply_worker_start_rt_fails_closed_on_non_linux() {
        let graph = graph_stereo_identity();
        let plan = graph.plan().expect("plan");
        let coeffs = graph.coeffs();
        let worker_rt = [
            Some(rt_apply::ThreadRtTarget::new(2, 70)),
            Some(rt_apply::ThreadRtTarget::new(3, 70)),
            Some(rt_apply::ThreadRtTarget::new(4, 70)),
            Some(rt_apply::ThreadRtTarget::new(5, 70)),
        ];
        let error = match BlockWorkers::start_rt(&plan, &coeffs, worker_rt) {
            Ok(_) => panic!("非 Linux target 上 worker RT 写入必须 fail closed"),
            Err(error) => error,
        };
        assert!(error.contains("RT 应用失败"), "{error}");
        assert!(
            error.contains("unsupported") || error.contains("ENOSYS"),
            "错误必须带 stage/errno：{error}"
        );
    }

    #[test]
    fn t45apply_worker_gate_parks_between_blocks_and_stays_bit_exact() {
        // F6：块间等待从"纯自旋"改为"有界自旋 + park"；park/unpark 只改等待协议，
        // 输出必须与 T07 直算逐位相同（bit-exact 不受影响）。
        let graph = graph_stereo_identity();
        let plan = graph.plan().expect("plan");
        let coeffs = graph.coeffs();
        let layout = graph.layout().expect("layout");
        let block = BLOCK as usize;
        let stride = block;
        let containers = graph.topo.container_channels as usize;
        let planar = synth_input(0, 2 * block);
        let expected = direct_stream(&graph, &planar, 2).expect("direct reference");
        let mut state = vec![0.0f32; layout.state_samples()];
        let mut input = vec![0.0f32; 2 * stride];
        let mut output = vec![0.0f32; containers * stride];
        let mut workers = BlockWorkers::start(&plan, &coeffs).expect("start workers");
        synth_input_into(0, &mut input, stride, block);
        output.iter_mut().for_each(|sample| *sample = 0.0);
        workers
            .run_block_4thread(&mut state, &input, stride, &mut output, stride, block)
            .expect("block 0");
        // 块间空闲 20 ms：worker 自旋 4096 次后必须进入 park（不再 4 核无条件忙等）。
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(
            workers.gate_parks() > 0,
            "F6：块间空闲期 worker 必须 park 让出 CPU（gate_parks={}）",
            workers.gate_parks()
        );
        synth_input_into(1, &mut input, stride, block);
        output.iter_mut().for_each(|sample| *sample = 0.0);
        workers
            .run_block_4thread(&mut state, &input, stride, &mut output, stride, block)
            .expect("block 1");
        let mut channel = 0usize;
        while channel < containers {
            let got = &output[channel * stride..channel * stride + block];
            let want = &expected[channel][block..2 * block];
            assert_eq!(
                got, want,
                "park/unpark 只改等待协议，输出必须与 T07 直算 bit-exact（ch{channel}）"
            );
            channel += 1;
        }
    }

    // -----------------------------------------------------------------------
    // T239：capture source=aes67 的 userspace 传输（自研接收端 khaen-aoip-aes67 → host）
    // 离线：UDP 仅 127.0.0.1 单播；不打开 ALSA/设备/板端；PTP/PHC NOT_RUN。
    // -----------------------------------------------------------------------

    use std::net::{Ipv4Addr as T239Ipv4Addr, SocketAddr as T239SocketAddr};

    use khaen_aoip_aes67::clock::frames_to_ns_ceil as t239_frames_to_ns_ceil;
    use khaen_aoip_aes67::jitter::{self as t239_jitter, JitterConfig as T239JitterConfig};
    use khaen_aoip_aes67::rtp::Header as T239RtpHeader;
    use khaen_aoip_aes67::sender::{Aes67RtpSender as T239Sender, SenderConfig as T239SenderConfig};

    /// 单条 userspace test SDP（单播 127.0.0.1，L24/48k/<ch>，PTPv2 参考钟声明）。
    fn t239_sdp_unicast(port: u16, channels: u16) -> String {
        format!(
            "v=0\n\
o=- 0 0 IN IP4 127.0.0.1\n\
s=TSPI-T239-userspace-test\n\
c=IN IP4 127.0.0.1\n\
t=0 0\n\
m=audio {} RTP/AVP 96\n\
a=rtpmap:96 L24/48000/{}\n\
a=ptime:1\n\
a=ts-refclk:ptp=IEEE1588-2008:00-11-22-33-44-55-66-77:0\n\
a=mediaclk:direct=0\n",
            port, channels
        )
    }

    /// 单条 userspace test SDP（组播 group/port；H16 风格）。
    fn t239_sdp_multicast(port: u16, channels: u16, group: &str) -> String {
        format!(
            "v=0\n\
o=- 0 0 IN IP4 127.0.0.1\n\
s=TSPI-T239-userspace-mcast\n\
c=IN IP4 {}/32\n\
t=0 0\n\
m=audio {} RTP/AVP 97\n\
a=rtpmap:97 L24/48000/{}\n\
a=ptime:1\n\
a=ts-refclk:ptp=IEEE1588-2008:00-11-22-33-44-55-66-77:0\n\
a=mediaclk:direct=0\n",
            group, port, channels
        )
    }

    /// T239 userspace host 配置文本（fake/alsa 由参数；extra 追加到 [capture] 段）。
    fn t239_config_text(
        sdp_path: &std::path::Path,
        backend: &str,
        io_mode: &str,
        extra: &str,
    ) -> String {
        format!(
            concat!(
                "[host]\n",
                "block_size = 128\n",
                "sample_rate = 48000\n",
                "workers = 4\n",
                "graph = \"stereo_identity\"\n",
                "safe_mode = \"normal\"\n",
                "[alsa]\n",
                "backend = \"{}\"\n",
                "io_mode = \"{}\"\n",
                "[rt]\n",
                "dsp_cpus = \"2-5\"\n",
                "tick_freq_hz = 1000000000\n",
                "[web]\n",
                "bind = \"127.0.0.1:1\"\n",
                "[run]\n",
                "max_blocks = 8\n",
                "[capture]\n",
                "source = \"aes67\"\n",
                "mode = \"aes67\"\n",
                "aes67_transport = \"userspace\"\n",
                "aes67_sdp = \"{}\"\n",
                "aes67_iface = \"127.0.0.1\"\n",
                "ptp_lock = \"unlocked\"\n",
                "{}"
            ),
            backend,
            io_mode,
            sdp_path.to_string_lossy(),
            extra
        )
    }

    /// T239 userspace HostRunConfig（解析即校验输入；不打开设备）。
    fn t239_userspace_config(
        sdp_path: &std::path::Path,
        backend: &str,
        io_mode: &str,
        extra: &str,
    ) -> HostRunConfig {
        let text = t239_config_text(sdp_path, backend, io_mode, extra);
        let mut warnings = Vec::new();
        parse_host_config(&text, &mut warnings).expect("T239 userspace 测试配置必须可解析")
    }

    /// T239 测试的进程环境锁：env 覆盖用例会临时 set/remove TSPI_HOST_CAPTURE_AES67_*，
    /// 与并行解析配置的用例存在数据竞争；所有 T239 用例串行化该段（不影响非 T239 测试）。
    static T239_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn t239_env_guard() -> std::sync::MutexGuard<'static, ()> {
        T239_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// 从 JSON 文本取第一个 "key":<整数>（测试断言用；不依赖 serde）。
    fn t239_json_u64(json: &str, key: &str) -> u64 {
        let marker = format!("\"{key}\":");
        let start = json
            .find(&marker)
            .unwrap_or_else(|| panic!("JSON 缺 {key}: {json}"))
            + marker.len();
        let digits: String = json[start..]
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .collect();
        digits
            .parse()
            .unwrap_or_else(|_| panic!("JSON 字段 {key} 不是整数: {json}"))
    }

    /// 1 ms 包（48 帧）RTP 头（T239 测试载荷源）。
    fn t239_packet_header(sequence: u16, timestamp: u32, channels: u16) -> T239RtpHeader {
        T239RtpHeader {
            version: 2,
            padding: false,
            extension: false,
            csrc_count: 0,
            marker: false,
            payload_type: 96 + channels as u8,
            sequence,
            timestamp,
            ssrc: 0x2390_0001,
        }
    }

    #[test]
    fn t239_aes67_userspace_config_parser_transport_and_mutex() {
        let _env = t239_env_guard();
        let sdp = temp_file("t239-cfg.sdp", t239_sdp_unicast(5_604, 2).as_bytes());
        // 默认（未写 aes67_transport）= ravenna，向后兼容；写 userspace 后解析为新传输。
        let legacy = parse_host_config(
            "[host]\nblock_size = 128\nsample_rate = 48000\n[alsa]\nbackend = \"fake\"\n[capture]\nsource = \"aes67\"\n",
            &mut Vec::new(),
        )
        .expect("legacy config");
        assert_eq!(
            legacy.capture_aes67_transport,
            CAPTURE_AES67_TRANSPORT_RAVENNA
        );
        assert_eq!(legacy.capture_aes67_stall_blocks, 0);
        let cfg = t239_userspace_config(&sdp, "fake", "synth", "");
        assert_eq!(cfg.capture_aes67_transport, "userspace");
        assert_eq!(cfg.capture_aes67_iface, "127.0.0.1");
        validate_capture_config(&cfg).expect("userspace + fake backend 必须通过");
        // EnvironmentFile 覆盖（部署契约）。
        std::env::set_var("TSPI_HOST_CAPTURE_AES67_STALL_BLOCKS", "123");
        let with_env = parse_host_config(
            &t239_config_text(&sdp, "fake", "synth", ""),
            &mut Vec::new(),
        )
        .expect("env config");
        std::env::remove_var("TSPI_HOST_CAPTURE_AES67_STALL_BLOCKS");
        assert_eq!(with_env.capture_aes67_stall_blocks, 123);

        // mode 互斥（source/mode 一一对应先于传输分支）。
        let mut bad_mode = t239_userspace_config(&sdp, "fake", "synth", "");
        bad_mode.capture_mode = CAPTURE_MODE_DANTE.to_string();
        let error = validate_capture_config(&bad_mode).expect_err("userspace + dante 必须被拒");
        assert!(error.starts_with(ERR_HOST_CAPTURE_CONFLICT), "{error}");
        // 非法 transport 值。
        let mut bad = t239_userspace_config(&sdp, "fake", "synth", "");
        bad.capture_aes67_transport = "tunnel".to_string();
        let error = validate_capture_config(&bad).expect_err("非法 transport 必须被拒");
        assert!(error.starts_with(ERR_HOST_CAPTURE_CONFIG), "{error}");
        assert!(error.contains("ravenna|userspace"), "{error}");
        // ravenna 路径不接受 userspace-only 参数（不静默忽略）。
        let mut ravenna = t239_userspace_config(&sdp, "alsa", "synth", "");
        ravenna.capture_aes67_transport = CAPTURE_AES67_TRANSPORT_RAVENNA.to_string();
        ravenna.capture_aes67_stall_blocks = 10;
        let error = validate_capture_config(&ravenna).expect_err("ravenna + stall_blocks 必须被拒");
        assert!(error.starts_with(ERR_HOST_CAPTURE_CONFLICT), "{error}");
        // userspace 与 daemon_ready=true 互斥（不谎报 daemon 就绪）。
        let mut daemon = t239_userspace_config(&sdp, "fake", "synth", "");
        daemon.capture_aes67_daemon_ready = true;
        let error = validate_capture_config(&daemon).expect_err("userspace + daemon 必须被拒");
        assert!(error.starts_with(ERR_HOST_CAPTURE_CONFLICT), "{error}");
        assert!(error.contains("aes67_daemon_ready"), "{error}");
        // userspace 与 MOTU loopback 输入 owner 互斥（backend=alsa 时）。
        let loopback = t239_userspace_config(&sdp, "alsa", "loopback", "");
        let error = validate_capture_config(&loopback).expect_err("userspace + loopback 必须被拒");
        assert!(error.starts_with(ERR_HOST_CAPTURE_CONFLICT), "{error}");
        // 协议 profile：48k / 128 帧。
        let mut rate = t239_userspace_config(&sdp, "fake", "synth", "");
        rate.sample_rate = 96_000;
        let error = validate_capture_config(&rate).expect_err("userspace 96k 必须被拒");
        assert!(error.starts_with(aoip::AoipError::Rate.name()), "{error}");
        let mut block = t239_userspace_config(&sdp, "fake", "synth", "");
        block.block_size = 64;
        let error = validate_capture_config(&block).expect_err("userspace 64 帧必须被拒");
        assert!(
            error.starts_with(aoip::AoipError::BlockSize.name()),
            "{error}"
        );
        // SDP 必填 + 文件必须存在。
        let mut no_sdp = t239_userspace_config(&sdp, "fake", "synth", "");
        no_sdp.capture_aes67_sdp = String::new();
        let error = validate_capture_config(&no_sdp).expect_err("缺 SDP 必须被拒");
        assert!(error.starts_with(ERR_HOST_CAPTURE_CONFIG), "{error}");
        let mut bad_sdp = t239_userspace_config(&sdp, "fake", "synth", "");
        bad_sdp.capture_aes67_sdp = "C:/nonexistent/t239.sdp".to_string();
        let error = validate_capture_config(&bad_sdp).expect_err("SDP 缺文件必须被拒");
        assert!(error.starts_with(ERR_HOST_CAPTURE_CONFIG), "{error}");
        // 参数越界（iface/slots/io_priority/stall_blocks）。
        let mut iface = t239_userspace_config(&sdp, "fake", "synth", "");
        iface.capture_aes67_iface = "tspi-no-such-iface-xyz".to_string();
        assert!(validate_capture_config(&iface)
            .expect_err("不存在的网卡名必须被拒（fail closed，不猜）")
            .starts_with(ERR_HOST_CAPTURE_CONFIG));
        let mut slots = t239_userspace_config(&sdp, "fake", "synth", "");
        slots.capture_aes67_rx_slots = "0,0".to_string();
        assert!(validate_capture_config(&slots)
            .expect_err("重复 slot 必须被拒")
            .contains("重复"));
        let mut prio = t239_userspace_config(&sdp, "fake", "synth", "");
        prio.capture_aes67_io_priority = 200;
        assert!(validate_capture_config(&prio)
            .expect_err("io_priority 越界必须被拒")
            .starts_with(ERR_HOST_CAPTURE_CONFIG));
        let mut stall = t239_userspace_config(&sdp, "fake", "synth", "");
        stall.capture_aes67_stall_blocks = 1_001;
        assert!(validate_capture_config(&stall)
            .expect_err("stall_blocks 越界必须被拒")
            .starts_with(ERR_HOST_CAPTURE_CONFIG));
        let _ = std::fs::remove_file(&sdp);
    }

    #[test]
    fn task320_aes67_iface_list_and_name_parsing() {
        // 单个 IPv4 / 逗号列表 / 单行 TOML 数组。
        let single = parse_aes67_iface("192.0.2.222").expect("单 IPv4");
        assert_eq!(single.len(), 1);
        assert_eq!(single.primary(), Some(T239Ipv4Addr::new(192, 0, 2, 222)));
        let list = parse_aes67_iface("192.0.2.222, 10.0.0.5").expect("逗号列表");
        assert_eq!(list.len(), 2);
        let array = parse_aes67_iface("[\"192.0.2.222\", \"10.0.0.5\"]").expect("单行数组");
        assert_eq!(array.addresses(), list.addresses());
        // 空 = 旧行为 0.0.0.0；0.0.0.0 单独 = 旧行为；与具体接口混写 = 拒绝。
        assert!(parse_aes67_iface("").expect("空").is_empty());
        assert!(parse_aes67_iface("   ").expect("空白").is_empty());
        assert!(parse_aes67_iface("0.0.0.0").expect("显式 0.0.0.0").is_empty());
        assert!(parse_aes67_iface("0.0.0.0,192.0.2.222").is_err());
        // 网卡名解析（注入 resolver：确定性正/负例；真实系统解析在 iface.rs 单测）。
        let named = parse_aes67_iface_with("lan0", |name| {
            assert_eq!(name, "lan0");
            Ok(T239Ipv4Addr::new(10, 1, 2, 3))
        })
        .expect("网卡名");
        assert_eq!(named.primary(), Some(T239Ipv4Addr::new(10, 1, 2, 3)));
        let mixed = parse_aes67_iface_with("lan0,192.0.2.222", |name| {
            assert_eq!(name, "lan0");
            Ok(T239Ipv4Addr::new(10, 1, 2, 3))
        })
        .expect("网卡名 + IPv4");
        assert_eq!(mixed.len(), 2);
        let unknown = parse_aes67_iface_with("badnic", |name| {
            Err(khaen_aoip_aes67::iface::IfaceLookupError::NoSuchName {
                name: name.to_string(),
                available: vec!["eth0".to_string()],
            })
        })
        .expect_err("不存在的网卡名");
        assert!(unknown.starts_with(ERR_HOST_CAPTURE_CONFIG), "{unknown}");
        assert!(unknown.contains("badnic"), "{unknown}");
        // auto / 重复 / 组播地址 / 未闭合数组 / 超限。
        assert!(parse_aes67_iface("auto").is_err());
        assert!(parse_aes67_iface("192.0.2.222,192.0.2.222").is_err());
        assert!(parse_aes67_iface("239.69.1.1").is_err());
        assert!(parse_aes67_iface("[\"192.0.2.222\"").is_err());
        let too_many = (1..=9)
            .map(|last| format!("10.0.0.{last}"))
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse_aes67_iface(&too_many).is_err());
    }

    #[test]
    fn task320_aes67_allowlists_parse_and_fail_closed() {
        let _env = t239_env_guard();
        // 组/来源白名单解析（列表 + 单行数组 + 空 = 旧行为 + 非法条目）。
        let groups = parse_aes67_group_allow("239.69.16.146, 239.69.16.147").expect("组列表");
        assert_eq!(groups.len(), 2);
        assert!(groups.contains(T239Ipv4Addr::new(239, 69, 16, 146)));
        let groups_array = parse_aes67_group_allow("[\"239.69.16.146\"]").expect("组单行数组");
        assert_eq!(groups_array.len(), 1);
        assert!(parse_aes67_group_allow("").expect("空组白名单").is_empty());
        let bad_group = parse_aes67_group_allow("192.0.2.222").expect_err("非组播组");
        assert!(bad_group.contains("aes67_group_allow"), "{bad_group}");
        assert!(parse_aes67_group_allow("239.69.16.1,239.69.16.1").is_err());
        let sources =
            parse_aes67_source_allow("192.0.2.0/24, 192.0.2.222").expect("来源列表");
        assert_eq!(sources.len(), 2);
        assert!(sources.contains(T239Ipv4Addr::new(192, 0, 2, 10)));
        assert!(!sources.contains(T239Ipv4Addr::new(10, 0, 0, 1)));
        let bad_cidr = parse_aes67_source_allow("192.0.2.0/33").expect_err("前缀越界");
        assert!(bad_cidr.contains("aes67_source_allow"), "{bad_cidr}");
        assert!(parse_aes67_source_allow("").expect("空来源白名单").is_empty());

        // 组白名单命中 → 计划构造成功，字段落到 receiver_configs。
        let sdp = temp_file(
            "task320-mcast.sdp",
            t239_sdp_multicast(5_620, 2, "239.69.3.1").as_bytes(),
        );
        let cfg = t239_userspace_config(
            &sdp,
            "fake",
            "synth",
            "aes67_group_allow = \"239.69.3.1\"\naes67_source_allow = \"192.0.2.0/24\"\n",
        );
        let plan = plan_aes67_user_capture(&cfg, 2)
            .expect("组白名单命中计划")
            .expect("userspace 计划");
        assert!(plan.group_allow.contains(T239Ipv4Addr::new(239, 69, 3, 1)));
        assert_eq!(plan.source_allow.len(), 1);
        assert!(plan.receiver_configs[0]
            .group_allow
            .contains(T239Ipv4Addr::new(239, 69, 3, 1)));
        assert!(plan.receiver_configs[0]
            .source_allow
            .contains(T239Ipv4Addr::new(192, 0, 2, 10)));
        // 组白名单未命中 → 启动 fail-closed（错误信息含 SDP 组与允许列表）。
        let cfg_miss = t239_userspace_config(
            &sdp,
            "fake",
            "synth",
            "aes67_group_allow = \"239.69.9.9\"\n",
        );
        let error = plan_aes67_user_capture(&cfg_miss, 2).expect_err("未授权组必须 fail closed");
        assert!(error.starts_with(ERR_HOST_CAPTURE_CONFIG), "{error}");
        assert!(error.contains("239.69.3.1"), "{error}");
        assert!(error.contains("239.69.9.9"), "{error}");
        // 单播 + 组白名单 → 拒绝（白名单只对组播有语义）。
        let sdp_uni = temp_file("task320-uni.sdp", t239_sdp_unicast(5_621, 2).as_bytes());
        let cfg_uni = t239_userspace_config(
            &sdp_uni,
            "fake",
            "synth",
            "aes67_group_allow = \"239.69.9.9\"\n",
        );
        let error = plan_aes67_user_capture(&cfg_uni, 2).expect_err("单播 + 组白名单必须被拒");
        assert!(error.contains("aes67_group_allow"), "{error}");
        // 单播 + 多接口 → 拒绝（不静默只用主接口）。
        let cfg_multi = t239_userspace_config(
            &sdp_uni,
            "fake",
            "synth",
            "aes67_iface = \"127.0.0.1,127.0.0.2\"\n",
        );
        let error = plan_aes67_user_capture(&cfg_multi, 2).expect_err("单播 + 多接口必须被拒");
        assert!(error.contains("aes67_iface"), "{error}");
        // EnvironmentFile 覆盖（部署契约）。
        std::env::set_var("TSPI_HOST_CAPTURE_AES67_GROUP_ALLOW", "239.69.16.146");
        std::env::set_var("TSPI_HOST_CAPTURE_AES67_SOURCE_ALLOW", "192.0.2.0/24");
        let env_cfg = parse_host_config(
            &t239_config_text(&sdp, "fake", "synth", ""),
            &mut Vec::new(),
        )
        .expect("env 配置");
        std::env::remove_var("TSPI_HOST_CAPTURE_AES67_GROUP_ALLOW");
        std::env::remove_var("TSPI_HOST_CAPTURE_AES67_SOURCE_ALLOW");
        assert_eq!(env_cfg.capture_aes67_group_allow, "239.69.16.146");
        assert_eq!(env_cfg.capture_aes67_source_allow, "192.0.2.0/24");
        // 0.0.0.0 多网卡警告（注入网卡数：确定性；单网卡/显式接口/非 userspace 不警告）。
        let mut warn_cfg = t239_userspace_config(&sdp, "fake", "synth", "");
        warn_cfg.capture_aes67_iface = String::new();
        assert!(aes67_multicast_listen_warning_with(&warn_cfg, 0).is_none());
        assert!(aes67_multicast_listen_warning_with(&warn_cfg, 1).is_none());
        let warning = aes67_multicast_listen_warning_with(&warn_cfg, 3).expect("多网卡警告");
        assert!(warning.contains("0.0.0.0"), "{warning}");
        assert!(warning.contains('3'), "{warning}");
        assert!(warning.contains("aes67_iface"), "{warning}");
        warn_cfg.capture_aes67_iface = "127.0.0.1".to_string();
        assert!(aes67_multicast_listen_warning_with(&warn_cfg, 3).is_none());
        warn_cfg.capture_aes67_iface = String::new();
        warn_cfg.capture_source = "synth".to_string();
        assert!(aes67_multicast_listen_warning_with(&warn_cfg, 3).is_none());
        let _ = std::fs::remove_file(&sdp);
        let _ = std::fs::remove_file(&sdp_uni);
    }

    #[test]
    fn t239_aes67_userspace_sdp_plan_flow_info_and_roles() {
        let _env = t239_env_guard();
        // 单 flow 2ch：strict 2.0，容器 2ch（FL/FR），单播 flow 信息来自 SDP。
        let sdp = temp_file("t239-plan-2ch.sdp", t239_sdp_unicast(5_605, 2).as_bytes());
        let cfg = t239_userspace_config(&sdp, "fake", "synth", "");
        let plan = plan_aes67_user_capture(&cfg, 2)
            .expect("userspace 计划必须可构造")
            .expect("source/transport 必须解析为 userspace 计划");
        assert_eq!(plan.receiver_configs.len(), 1);
        let flow = &plan.flow_info[0];
        assert_eq!(flow.media, "127.0.0.1");
        assert_eq!(flow.port, 5_605);
        assert!(!flow.multicast);
        assert_eq!(flow.channels, 2);
        assert_eq!(plan.container.capture_channels(), 2);
        assert_eq!(plan.layout, CAPTURE_LAYOUT_STEREO);
        assert_eq!(plan.adapter.flow_count(), 1);
        assert_eq!(plan.interfaces.primary(), Some(T239Ipv4Addr::LOCALHOST));
        assert_eq!(plan.stall_blocks, aoip::DEFAULT_MAX_STALL_PERIODS);
        let _ = std::fs::remove_file(&sdp);

        // 组播 flow：multicast=true / iface 生效（不 bind，仅解析/校验）。
        let sdp = temp_file(
            "t239-plan-mcast.sdp",
            t239_sdp_multicast(5_004, 8, "239.69.1.1").as_bytes(),
        );
        let cfg = t239_userspace_config(&sdp, "fake", "synth", "");
        let plan = plan_aes67_user_capture(&cfg, 2)
            .expect("组播 userspace 计划")
            .expect("userspace 计划");
        assert!(plan.flow_info[0].multicast);
        assert_eq!(plan.flow_info[0].port, 5_004);
        assert_eq!(plan.flow_info[0].channels, 8);
        // 单 flow 8ch 未给 slots = H16 TX1..8（FL/FR 可满足 2ch 图；4.1.2 需要显式 slot 覆盖）。
        assert_eq!(plan.container.capture_channels(), 8);
        let error = plan_aes67_user_capture(&cfg, 8)
            .expect_err("单 flow 8ch -> 4.1.2 缺 TFL/TFR 必须拒（禁止静默零填）");
        assert!(error.starts_with(aoip::AoipError::Roles.name()), "{error}");
        let _ = std::fs::remove_file(&sdp);

        // 双 flow 8+4：H16 逻辑 12 容器 + 4.1.2 显式角色映射（TFL/TFR 在 flow2）。
        let sdp_text = format!(
            "{}{}",
            t239_sdp_multicast(5_006, 8, "239.69.2.1"),
            t239_sdp_multicast(5_008, 4, "239.69.2.2")
        );
        let sdp = temp_file("t239-plan-12ch.sdp", sdp_text.as_bytes());
        let cfg = t239_userspace_config(&sdp, "fake", "synth", "");
        let plan = plan_aes67_user_capture(&cfg, 8)
            .expect("8+4 userspace 计划")
            .expect("userspace 计划");
        assert_eq!(plan.container.capture_channels(), 12);
        assert_eq!(plan.adapter.flow_count(), 2);
        let readers = (0..2)
            .map(|index| {
                let channels = if index == 0 { 8u16 } else { 4u16 };
                let config =
                    T239JitterConfig::new(channels, 48_000, AES67_RX_BLOCK_FRAMES, 8_192, 4_000)
                        .expect("jitter config");
                t239_jitter::channel(config).expect("jitter channel").1
            })
            .collect::<Vec<_>>();
        let capture = Aes67RxCapture::from_readers(readers, plan.adapter).expect("capture 12ch");
        assert_eq!(
            capture.capture_index(),
            &[
                Some(0),
                Some(1),
                Some(4),
                Some(5),
                Some(8),
                Some(9),
                None,
                Some(3)
            ]
        );
        let _ = std::fs::remove_file(&sdp);

        // 单 flow 8ch + 显式 slots = 4.1.2 容器（禁止猜布局：slot 必须显式）。
        let sdp = temp_file("t239-plan-slots.sdp", t239_sdp_unicast(5_610, 8).as_bytes());
        let cfg = t239_userspace_config(
            &sdp,
            "fake",
            "synth",
            "aes67_rx_slots = \"0,1,4,5,10,11,3,6\"\n",
        );
        let plan = plan_aes67_user_capture(&cfg, 8)
            .expect("显式 slots 4.1.2 计划")
            .expect("userspace 计划");
        assert_eq!(plan.container.capture_channels(), 8);
        assert_eq!(plan.layout, CAPTURE_LAYOUT_412);
        let config =
            T239JitterConfig::new(8, 48_000, AES67_RX_BLOCK_FRAMES, 8_192, 4_000).expect("cfg");
        let (writer, reader) = t239_jitter::channel(config).expect("channel");
        let capture = Aes67RxCapture::from_readers(vec![reader], plan.adapter).expect("capture");
        assert_eq!(
            capture.capture_index(),
            &[
                Some(0),
                Some(1),
                Some(2),
                Some(3),
                Some(4),
                Some(5),
                None,
                Some(6)
            ]
        );
        let _ = writer;
        let _ = std::fs::remove_file(&sdp);

        // SDP 错误映射为 host 稳定错误码（与 T43 同码族）。
        let bad_rate = t239_sdp_unicast(5_611, 2).replace("L24/48000/2", "L24/44100/2");
        let sdp = temp_file("t239-plan-441.sdp", bad_rate.as_bytes());
        let cfg = t239_userspace_config(&sdp, "fake", "synth", "");
        let error = plan_aes67_user_capture(&cfg, 2).expect_err("44.1k SDP 必须被拒");
        assert!(error.starts_with(aoip::AoipError::Rate.name()), "{error}");
        assert!(error.contains("ERR_SDP_UNSUPPORTED_RATE"), "{error}");
        let _ = std::fs::remove_file(&sdp);
        let no_refclk = t239_sdp_unicast(5_612, 2)
            .lines()
            .filter(|line| !line.starts_with("a=ts-refclk"))
            .collect::<Vec<_>>()
            .join("\n");
        let sdp = temp_file("t239-plan-noref.sdp", no_refclk.as_bytes());
        let cfg = t239_userspace_config(&sdp, "fake", "synth", "");
        let error = plan_aes67_user_capture(&cfg, 2).expect_err("缺 ts-refclk 必须被拒");
        assert!(
            error.starts_with(aoip::AoipError::Unsupported.name()),
            "{error}"
        );
        let _ = std::fs::remove_file(&sdp);
        // 3 条 flow 无显式计划 -> 拒收（不猜容器组合）。
        let sdp_text = format!(
            "{}{}{}",
            t239_sdp_multicast(5_020, 2, "239.69.3.1"),
            t239_sdp_multicast(5_022, 2, "239.69.3.2"),
            t239_sdp_multicast(5_024, 2, "239.69.3.3")
        );
        let sdp = temp_file("t239-plan-3flow.sdp", sdp_text.as_bytes());
        let cfg = t239_userspace_config(&sdp, "fake", "synth", "");
        let error = plan_aes67_user_capture(&cfg, 2).expect_err("3 flow 必须被拒");
        assert!(
            error.starts_with(aoip::AoipError::DeviceParams.name()),
            "{error}"
        );
        let _ = std::fs::remove_file(&sdp);
    }

    /// T265-follow-up：AES67 userspace **单 flow 7ch 4.1.2**（SMPTE2110.(41,2)：
    /// L,R,Ls,Rs,Ltf,Rtf,LFE）→ 8ch strict 图输入（ch6 空、LFE 落 ch7）。
    #[test]
    fn t239_aes67_userspace_7ch_412_default_slot_map_and_lfe_position() {
        let _env = t239_env_guard();
        let sdp = temp_file("t239-7ch.sdp", t239_sdp_unicast(5_613, 7).as_bytes());
        let cfg = t239_userspace_config(&sdp, "fake", "synth", "");
        let plan = plan_aes67_user_capture(&cfg, 8)
            .expect("7ch 4.1.2 userspace 计划必须可构造")
            .expect("userspace 计划");
        // 源 flow 7ch → H16 容器 7 通道；图输入仍是 4.1.2 strict 8ch。
        assert_eq!(plan.container.capture_channels(), 7);
        assert_eq!(plan.layout, CAPTURE_LAYOUT_412);
        assert_eq!(plan.flow_layout, CAPTURE_FLOW_LAYOUT_412_7CH);
        assert_eq!(plan.graph_input_channels, 8);
        // 默认 slot 映射：FL FR SL SR TFL TFR LFE → graph ch0..5, ch6 空, ch7 LFE。
        let jitter_cfg =
            T239JitterConfig::new(7, 48_000, AES67_RX_BLOCK_FRAMES, 8_192, 4_000).expect("cfg");
        let (_writer, reader) = t239_jitter::channel(jitter_cfg).expect("channel");
        let capture = Aes67RxCapture::from_readers(vec![reader], plan.adapter).expect("capture");
        assert_eq!(
            capture.capture_index(),
            &[Some(0), Some(1), Some(2), Some(3), Some(4), Some(5), None, Some(6)]
        );
        // 显式给同一组 slot 也必须被接受（部署侧可写死映射）。
        let explicit = t239_userspace_config(
            &sdp,
            "fake",
            "synth",
            "aes67_rx_slots = \"0,1,4,5,10,11,3\"\n",
        );
        let plan = plan_aes67_user_capture(&explicit, 8)
            .expect("显式 7ch slots 计划")
            .expect("userspace 计划");
        let jitter_cfg =
            T239JitterConfig::new(7, 48_000, AES67_RX_BLOCK_FRAMES, 8_192, 4_000).expect("cfg");
        let (_writer2, reader2) = t239_jitter::channel(jitter_cfg).expect("channel");
        let capture2 = Aes67RxCapture::from_readers(vec![reader2], plan.adapter).expect("capture");
        assert_eq!(
            capture2.capture_index(),
            &[Some(0), Some(1), Some(2), Some(3), Some(4), Some(5), None, Some(6)]
        );
        // 7ch 只允许 4.1.2（8ch strict）图输入：2.0 图显式拒绝（不做隐式抽取/下混）。
        let error = plan_aes67_user_capture(&cfg, 2).expect_err("7ch + 2.0 图必须被拒");
        assert!(error.starts_with(aoip::AoipError::Roles.name()), "{error}");
        assert!(error.contains("7ch"), "{error}");
        // 显式错误 slot 顺序（把 LFE 放 ch6 / 忽略空槽）→ 角色缺失，拒收（禁止静默零填）。
        let wrong = t239_userspace_config(
            &sdp,
            "fake",
            "synth",
            "aes67_rx_slots = \"0,1,2,3,4,5,6\"\n",
        );
        let error = plan_aes67_user_capture(&wrong, 8)
            .expect_err("错误 slot 顺序（无 TFL/TFR/LFE 角色）必须被拒");
        assert!(error.starts_with(aoip::AoipError::Roles.name()), "{error}");
        // 非 7ch 的非 strict 宽度同样拒收（例如 5ch）：不猜布局。
        let sdp5 = temp_file("t239-5ch.sdp", t239_sdp_unicast(5_614, 5).as_bytes());
        let cfg5 = t239_userspace_config(&sdp5, "fake", "synth", "");
        let error = plan_aes67_user_capture(&cfg5, 8).expect_err("5ch + 4.1.2 必须被拒");
        assert!(error.starts_with(aoip::AoipError::Roles.name()), "{error}");
        let _ = std::fs::remove_file(&sdp5);
        let _ = std::fs::remove_file(&sdp);
    }

    /// T265-follow-up：NSP 生产风格 7ch SDP（SMPTE2110.(41,2) channel-order + localmac
    /// refclk + comment 布局行）必须能被 userspace 接收端解析并映射到 4.1.2。
    #[test]
    fn t239_aes67_userspace_7ch_412_smpte2110_sdp_parses() {
        let _env = t239_env_guard();
        let sdp_text = "v=0\n\
o=- 1314082870 1314082870 IN IP4 0.0.0.0\n\
s=NativeSpatialPipe AES67 output\n\
c=IN IP4 127.0.0.1\n\
t=0 0\n\
m=audio 5004 RTP/AVP 96\n\
a=rtpmap:96 L24/48000/7\n\
a=ptime:1\n\
a=sendonly\n\
a=ts-refclk:localmac=02-4E-53-50-00-01\n\
a=mediaclk:direct=0\n\
a=fmtp:96 channel-order=SMPTE2110.(41,2)\n\
a=comment:layout=4.1.2\n\
a=comment:channel-labels=L,R,Ls,Rs,Ltf,Rtf,LFE\n";
        let sdp = temp_file("t239-7ch-nsp.sdp", sdp_text.as_bytes());
        let cfg = t239_userspace_config(&sdp, "fake", "synth", "");
        let plan = plan_aes67_user_capture(&cfg, 8)
            .expect("NSP 风格 7ch SDP 必须被解析")
            .expect("userspace 计划");
        assert_eq!(plan.flow_info[0].channels, 7);
        assert_eq!(plan.flow_layout, CAPTURE_FLOW_LAYOUT_412_7CH);
        let jitter_cfg =
            T239JitterConfig::new(7, 48_000, AES67_RX_BLOCK_FRAMES, 8_192, 4_000).expect("cfg");
        let (_writer, reader) = t239_jitter::channel(jitter_cfg).expect("channel");
        let capture = Aes67RxCapture::from_readers(vec![reader], plan.adapter).expect("capture");
        assert_eq!(
            capture.capture_index(),
            &[Some(0), Some(1), Some(2), Some(3), Some(4), Some(5), None, Some(6)]
        );
        let _ = std::fs::remove_file(&sdp);
    }

    /// nsp-5.1.2：单流 8ch 按线序进图。没有该输入布局时，slot 0..7 仍按 4.1.2 角色校验并拒绝。
    #[test]
    fn nsp512_content_order_keeps_wire_order() {
        let _env = t239_env_guard();
        let sdp = temp_file("nsp512-wire.sdp", t239_sdp_unicast(5_617, 8).as_bytes());
        let mut cfg = t239_userspace_config(&sdp, "fake", "synth", "");
        let error = plan_aes67_user_capture(&cfg, 8).expect_err("裸 8ch 不能当成 4.1.2");
        assert!(
            error.contains("ERR_AES67RX_ROLES") || error.contains("角色"),
            "{error}"
        );
        cfg.content_input_layout = "nsp-5.1.2".to_string();
        let plan = plan_aes67_user_capture(&cfg, 8)
            .expect("内容顺序计划")
            .expect("userspace 计划");
        assert_eq!(plan.flow_layout, CAPTURE_FLOW_LAYOUT_512_8CH);
        assert_eq!(plan.layout, AES67_RX_LAYOUT_512_CONTENT);
        assert!(plan.adapter.content_order);
        let jitter_cfg =
            T239JitterConfig::new(8, 48_000, AES67_RX_BLOCK_FRAMES, 8_192, 4_000).expect("cfg");
        let (_writer, reader) = t239_jitter::channel(jitter_cfg).expect("channel");
        let capture = Aes67RxCapture::from_readers(vec![reader], plan.adapter).expect("capture");
        assert_eq!(
            capture.capture_index(),
            &[
                Some(0),
                Some(1),
                Some(2),
                Some(3),
                Some(4),
                Some(5),
                Some(6),
                Some(7)
            ]
        );
        let error = plan_aes67_user_capture(&cfg, 2).expect_err("2ch 图必须拒绝");
        assert!(error.contains("nsp-5.1.2"), "{error}");
        cfg.capture_aes67_rx_slots = "0,1,4,5,10,11,3,6".to_string();
        let error = plan_aes67_user_capture(&cfg, 8).expect_err("内容顺序不接受 slot 覆盖");
        assert!(error.contains("aes67_rx_slots"), "{error}");
        let _ = std::fs::remove_file(&sdp);
    }

    /// T265-followup：userspace 抖动缓冲 latency/ring 配置键（0 = 默认；非法拒收）。
    #[test]
    fn t239_aes67_userspace_jitter_latency_and_ring_config() {
        let _env = t239_env_guard();
        let sdp = temp_file("t239-jitter.sdp", t239_sdp_unicast(5_615, 7).as_bytes());
        // 默认：AES67 4 ms / 8192 帧。
        let cfg = t239_userspace_config(&sdp, "fake", "synth", "");
        assert_eq!(cfg.capture_aes67_jitter_latency_us, 0);
        assert_eq!(cfg.capture_aes67_jitter_ring_frames, 0);
        let plan = plan_aes67_user_capture(&cfg, 8)
            .expect("默认抖动配置计划")
            .expect("userspace 计划");
        assert_eq!(plan.receiver_configs[0].jitter.latency_us, 4_000);
        assert_eq!(plan.receiver_configs[0].jitter.capacity_frames, 8_192);
        // 显式 8 ms / 16384 帧。
        let cfg = t239_userspace_config(
            &sdp,
            "fake",
            "synth",
            "aes67_jitter_latency_us = 8000\naes67_jitter_ring_frames = 16384\n",
        );
        let plan = plan_aes67_user_capture(&cfg, 8)
            .expect("显式抖动配置计划")
            .expect("userspace 计划");
        assert_eq!(plan.receiver_configs[0].jitter.latency_us, 8_000);
        assert_eq!(plan.receiver_configs[0].jitter.capacity_frames, 16_384);
        // 非法值拒收（启动期 fail fast）。
        let mut bad = t239_userspace_config(&sdp, "fake", "synth", "");
        bad.capture_aes67_jitter_latency_us = 100;
        assert!(validate_aes67_userspace_config(&bad)
            .expect_err("latency 100us 必须被拒")
            .contains("aes67_jitter_latency_us"));
        let mut ok_ring = t239_userspace_config(&sdp, "fake", "synth", "");
        ok_ring.capture_aes67_jitter_ring_frames = 4_096;
        validate_aes67_userspace_config(&ok_ring).expect("ring 4096 可接受（>=512 且 2 的幂）");
        let mut bad = t239_userspace_config(&sdp, "fake", "synth", "");
        bad.capture_aes67_jitter_ring_frames = 100;
        assert!(validate_aes67_userspace_config(&bad)
            .expect_err("ring 100 必须被拒")
            .contains("aes67_jitter_ring_frames"));
        let _ = std::fs::remove_file(&sdp);
    }

    #[test]
    fn t239_aes67_userspace_block_injection_maps_planar_and_report() {
        let _env = t239_env_guard();
        let sdp = temp_file("t239-inject.sdp", t239_sdp_unicast(5_620, 2).as_bytes());
        let cfg = t239_userspace_config(&sdp, "fake", "synth", "");
        let plan = plan_aes67_user_capture(&cfg, 2)
            .expect("plan")
            .expect("userspace plan");
        let config = T239JitterConfig::new(2, 48_000, AES67_RX_BLOCK_FRAMES, 8_192, 4_000)
            .expect("jitter cfg");
        let (writer, reader) = t239_jitter::channel(config).expect("jitter channel");
        let capture = Aes67RxCapture::from_readers(vec![reader], plan.adapter).expect("capture");
        let mut runtime = Aes67UserCapture::from_capture(plan, capture, 1_000, false).expect("rt");
        // 8 包 × 48 帧 = 384 帧（> 4ms 目标延迟 256 帧，满足合成时钟预滚）；ch0 = +0.125 FS、ch1 = -0.0625 FS。
        let origin = 1_000u64;
        for packet in 0..8u16 {
            let mut samples = vec![0i32; 48 * 2];
            for frame in 0..48usize {
                samples[frame * 2] = 0x1000_0000;
                samples[frame * 2 + 1] = -0x0800_0000;
            }
            let result = writer.push_interleaved(
                origin,
                &t239_packet_header(packet, packet as u32 * 48, 2),
                &samples,
            );
            assert!(
                matches!(result, t239_jitter::PushResult::Accepted { .. }),
                "{result:?}"
            );
        }
        let stride = AES67_RX_BLOCK_FRAMES;
        let mut planar = vec![9.0f32; 2 * stride];
        let now = origin + t239_frames_to_ns_ceil(256, 48_000) + 1_000;
        runtime
            .fill_block(&mut planar, stride, AES67_RX_BLOCK_FRAMES, now)
            .expect("userspace capture 块必须可读");
        assert_eq!(planar[0], 0x1000_0000u32 as i32 as f32 / 2_147_483_648.0);
        assert_eq!(planar[stride], (-0x0800_0000i32) as f32 / 2_147_483_648.0);
        assert_eq!(runtime.data_blocks, 1);
        assert_eq!(runtime.silent_blocks, 0);
        assert_eq!(runtime.monitor.state(), aoip::MonitorState::Flowing);
        // 报告：transport/userspace/SDP/flow/adapter 计数可见 + NOT_RUN 明示。
        let report = runtime.report_json("max_blocks");
        assert!(report.contains("\"source\":\"aes67\""), "{report}");
        assert!(report.contains("\"transport\":\"userspace\""), "{report}");
        assert!(
            report.contains("\"reader\":\"khaen-aoip-aes67\""),
            "{report}"
        );
        assert!(report.contains("\"layout\":\"2.0-strict\""), "{report}");
        assert!(report.contains("\"clock\":\"usrvclock\""), "{report}");
        assert!(report.contains("\"ptp\":\"not-wired\""), "{report}");
        assert!(report.contains("\"data_blocks\":1"), "{report}");
        assert!(report.contains("\"frames\":128"), "{report}");
        assert!(report.contains("\"media\":\"127.0.0.1\""), "{report}");
        assert!(report.contains("\"adapter\":{"), "{report}");
        assert!(report.contains("NOT_RUN"), "{report}");
        // SafetyMute 块：不消费新块，输入清零并计数（与 T43 同语义）。
        let mut muted = vec![7.0f32; 2 * stride];
        runtime
            .muted_block(&mut muted, now + 1)
            .expect("mute 块不得报错");
        assert!(muted.iter().all(|sample| *sample == 0.0));
        assert_eq!(runtime.muted_blocks, 1);
        assert_eq!(runtime.data_blocks, 1, "mute 块不得消费 capture 数据");
        runtime.stop();
        let _ = std::fs::remove_file(&sdp);
    }

    #[test]
    fn t239_aes67_userspace_stall_silence_then_lost_fails_closed() {
        let _env = t239_env_guard();
        let sdp = temp_file("t239-stall.sdp", t239_sdp_unicast(5_621, 2).as_bytes());
        // 默认 stall_blocks = T43 默认 3（Lost = 2×3 块）。
        let cfg = t239_userspace_config(&sdp, "fake", "synth", "");
        let plan = plan_aes67_user_capture(&cfg, 2)
            .expect("plan")
            .expect("userspace plan");
        let config = T239JitterConfig::new(2, 48_000, AES67_RX_BLOCK_FRAMES, 8_192, 4_000)
            .expect("jitter cfg");
        let (_writer, reader) = t239_jitter::channel(config).expect("jitter channel");
        let capture = Aes67RxCapture::from_readers(vec![reader], plan.adapter).expect("capture");
        let mut runtime = Aes67UserCapture::from_capture(plan, capture, 0, false).expect("rt");
        let stride = AES67_RX_BLOCK_FRAMES;
        let t = t239_frames_to_ns_ceil(AES67_RX_BLOCK_FRAMES as u64, 48_000);
        let mut input = vec![5.0f32; 2 * stride];
        // 首包未到：Warmup 整块静音（不是错误）。
        runtime
            .fill_block(&mut input, stride, AES67_RX_BLOCK_FRAMES, 0)
            .expect("首包未到必须整块静音继续");
        assert!(input.iter().all(|sample| *sample == 0.0));
        assert_eq!(runtime.warmup_blocks, 1);
        assert_eq!(runtime.silent_blocks, 1);
        // 超过停滞门限（4t > 3t）：停滞 -> 整块静音，同 epoch 可恢复。
        runtime
            .fill_block(&mut input, stride, AES67_RX_BLOCK_FRAMES, 4 * t)
            .expect("停滞块必须静音继续");
        assert!(input.iter().all(|sample| *sample == 0.0));
        assert_eq!(runtime.stall_events, 1);
        // 超过持续失流门限（7t > 2×3t）：fail closed，稳定错误码 RT_E_DEVICE_LOST。
        let error = runtime
            .fill_block(&mut input, stride, AES67_RX_BLOCK_FRAMES, 7 * t)
            .expect_err("持续失流必须 fail closed");
        assert!(
            error.starts_with(aoip::AoipError::StreamLost.name()),
            "{error}"
        );
        assert!(error.contains("持续失流"), "{error}");
        assert!(error.contains("NOT_WIRED"), "{error}");
        assert_eq!(runtime.loss_events, 1);
        runtime.stop();
        let _ = std::fs::remove_file(&sdp);
    }


    #[test]
    fn t239_aes67_userspace_hold_stays_alive_reanchors_then_timeout_fails_closed() {
        let _env = t239_env_guard();
        let sdp = temp_file("t239-hold.sdp", t239_sdp_unicast(5_622, 2).as_bytes());
        // hold 语义：8ms（3 块）连续静音请求内部 reanchor；160ms（60 块）静音总时长才 fail closed。
        let extra = "aes67_stay_on_stream_end = true\n\
                     aes67_silence_hold_ms = 8\n\
                     aes67_stall_fail_close_ms = 160\n\
                     aes67_stall_blocks = 400\n\
                     aes67_reanchor_watchdog = true\n";
        let cfg = t239_userspace_config(&sdp, "fake", "synth", extra);
        let plan = plan_aes67_user_capture(&cfg, 2)
            .expect("plan")
            .expect("userspace plan");
        assert!(plan.stay_on_stream_end);
        assert_eq!(plan.silence_hold_ms, 8);
        assert_eq!(plan.silence_hold_blocks, 3, "8ms ≈ 3×2.667ms 块（向上取整）");
        assert_eq!(plan.stall_fail_close_blocks, 60, "160ms = 60 块");
        assert!(plan.reanchor_watchdog);
        let config = T239JitterConfig::new(2, 48_000, AES67_RX_BLOCK_FRAMES, 8_192, 4_000)
            .expect("jitter cfg");
        let (writer, reader) = t239_jitter::channel(config).expect("jitter channel");
        let capture = Aes67RxCapture::from_readers(vec![reader], plan.adapter).expect("capture");
        let mut runtime = Aes67UserCapture::from_capture(plan, capture, 0, false).expect("rt");
        let stride = AES67_RX_BLOCK_FRAMES;
        let t = t239_frames_to_ns_ceil(AES67_RX_BLOCK_FRAMES as u64, 48_000);
        let mut input = vec![5.0f32; 2 * stride];
        // 无流：hold 模式必须静音保持、不 fail closed，并在第 3 块请求一次内部 reanchor。
        for index in 0..3u64 {
            runtime
                .fill_block(&mut input, stride, AES67_RX_BLOCK_FRAMES, index * t)
                .expect("hold 期间不得 fail closed");
            assert!(input.iter().all(|sample| *sample == 0.0), "hold 块必须整块静音");
        }
        assert_eq!(runtime.hold_events, 1, "达到 silence_hold 必须请求一次内部 reanchor");
        assert_eq!(runtime.reanchor_requests, 1);
        // 数据恢复（同场进程内）：推 8 包（384 帧 > 预滚 256 帧）→ 下一块必须 Ok。
        let origin = 10_000u64;
        for packet in 0..8u16 {
            let mut samples = vec![0i32; 48 * 2];
            for frame in 0..48usize {
                samples[frame * 2] = 0x1000_0000;
                samples[frame * 2 + 1] = -0x0800_0000;
            }
            let result = writer.push_interleaved(
                origin,
                &t239_packet_header(packet, packet as u32 * 48, 2),
                &samples,
            );
            assert!(
                matches!(result, t239_jitter::PushResult::Accepted { .. }),
                "{result:?}"
            );
        }
        runtime
            .fill_block(&mut input, stride, AES67_RX_BLOCK_FRAMES, 3 * t)
            .expect("数据恢复必须保持进程内恢复");
        assert_eq!(runtime.data_blocks, 1);
        assert_eq!(runtime.silent_streak_blocks, 0, "Ok 块必须清零静音 streak");
        // 再次无流直到超过 stall_fail_close（15 块）→ fail closed 交 systemd。
        let mut error = None;
        for index in 4..(4 + 62u64) {
            match runtime.fill_block(&mut input, stride, AES67_RX_BLOCK_FRAMES, index * t) {
                Ok(()) => {}
                Err(err) => {
                    error = Some(err);
                    break;
                }
            }
        }
        let error = error.expect("静音超过 stall_fail_close 必须 fail closed");
        assert!(
            error.starts_with(aoip::AoipError::StreamLost.name()),
            "{error}"
        );
        assert!(error.contains("静音保持超时"), "{error}");
        runtime.stop();
        let _ = std::fs::remove_file(&sdp);
    }

    #[test]
    fn t239_aes67_userspace_hold_config_keys_and_bounds() {
        let _env = t239_env_guard();
        let sdp = temp_file("t239-holdcfg.sdp", t239_sdp_unicast(5_623, 2).as_bytes());
        let cfg = t239_userspace_config(
            &sdp,
            "fake",
            "synth",
            "aes67_stay_on_stream_end = true\n\
             aes67_silence_hold_ms = 80\n\
             aes67_stall_fail_close_ms = 10000\n\
             aes67_reanchor_watchdog = true\n",
        );
        assert!(cfg.capture_aes67_stay_on_stream_end);
        assert_eq!(cfg.capture_aes67_silence_hold_ms, 80);
        assert_eq!(cfg.capture_aes67_stall_fail_close_ms, 10_000);
        assert!(cfg.capture_aes67_reanchor_watchdog);
        validate_aes67_userspace_config(&cfg).expect("NSP7 hold 配置必须合法");
        let bad = t239_userspace_config(&sdp, "fake", "synth", "aes67_silence_hold_ms = 5001\n");
        assert!(
            validate_aes67_userspace_config(&bad)
                .expect_err("silence_hold_ms > 5000 必须被拒")
                .contains("aes67_silence_hold_ms")
        );
        let bad = t239_userspace_config(
            &sdp,
            "fake",
            "synth",
            "aes67_stall_fail_close_ms = 50\n",
        );
        assert!(
            validate_aes67_userspace_config(&bad)
                .expect_err("stall_fail_close_ms 50 必须被拒（0 或 100..=600000）")
                .contains("aes67_stall_fail_close_ms")
        );
        let _ = std::fs::remove_file(&sdp);
    }

    #[test]
    fn t239_aes67_userspace_hold_keys_rejected_for_ravenna_transport() {
        // ravenna 传输不得混入 userspace-only hold 参数（避免"配了没生效"）。
        let mut config = HostRunConfig::default();
        config.capture_source = "aes67".to_string();
        config.capture_mode = "aes67".to_string();
        config.capture_aes67_transport = "ravenna".to_string();
        config.backend = "alsa".to_string();
        config.capture_aes67_stay_on_stream_end = true;
        let error = validate_capture_config(&config).expect_err("ravenna + hold 必须被拒");
        assert!(error.starts_with(ERR_HOST_CAPTURE_CONFLICT), "{error}");
        assert!(error.contains("userspace 参数"), "{error}");
    }

    #[test]
    fn t239_aes67_userspace_run_udp_loopback_e2e() {
        let _env = t239_env_guard();
        use std::sync::atomic::{AtomicBool as T239AtomicBool, Ordering as T239Ordering};
        use std::time::{Duration as T239Duration, Instant as T239Instant};

        // 端口按进程号扰动，避免并行测试互相冲突（UDP 单播本机回环）。
        let port = 15_500u16 + (std::process::id() % 400) as u16;
        let sdp = temp_file("t239-e2e.sdp", t239_sdp_unicast(port, 2).as_bytes());
        // stall_blocks 放宽：只验证链路/帧注入/退出语义；PTP/网络失流结论 NOT_RUN。
        let text = t239_config_text(&sdp, "fake", "synth", "aes67_stall_blocks = 400\n");
        let config_path = temp_file("t239-e2e.toml", text.as_bytes());

        let stop = std::sync::Arc::new(T239AtomicBool::new(false));
        let sender_stop = std::sync::Arc::clone(&stop);
        let sender = std::thread::spawn(move || {
            let mut config =
                T239SenderConfig::unicast(T239SocketAddr::from((T239Ipv4Addr::LOCALHOST, port)), 2);
            config.payload_type = 96;
            let mut sender = T239Sender::bind(config).expect("userspace test sender bind");
            let start = T239Instant::now();
            let mut next = start;
            while !sender_stop.load(T239Ordering::Relaxed)
                && start.elapsed() < T239Duration::from_secs(5)
            {
                sender.send_sine(1_000.0).expect("send sine packet");
                next += T239Duration::from_millis(1);
                while T239Instant::now() < next {
                    std::hint::spin_loop();
                }
            }
            sender.packets_sent()
        });
        // 先起发送端，再起 host（host 绑定后即可收流；stall 门限 400 块容错启动抖动）。
        std::thread::sleep(T239Duration::from_millis(30));
        let result = run_command(config_path.to_str().expect("utf8 path"), Some(64), None);
        stop.store(true, T239Ordering::Relaxed);
        let packets_sent = sender.join().expect("sender thread");
        let _ = std::fs::remove_file(&sdp);
        let _ = std::fs::remove_file(&config_path);
        let json = result.expect("userspace AES67 UDP loopback run 必须成功");
        assert!(packets_sent > 0, "{json}");
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        assert!(json.contains("\"capture\":{\"source\":\"aes67\""), "{json}");
        assert!(json.contains("\"transport\":\"userspace\""), "{json}");
        assert!(json.contains("\"exit_reason\":\"max_blocks\""), "{json}");
        assert!(
            json.contains("\"pacing\":\"userspace-block-timer\""),
            "{json}"
        );
        assert_eq!(t239_json_u64(&json, "blocks"), 64, "{json}");
        assert!(
            t239_json_u64(&json, "accepted_packets") > 0,
            "实时 UDP loopback 必须真实收到包：{json}"
        );
        assert!(
            t239_json_u64(&json, "data_blocks") > 0,
            "至少一个块必须由 userspace capture 数据驱动图输入：{json}"
        );
        assert!(
            t239_json_u64(&json, "frames") > 0,
            "capture 必须向图交付帧：{json}"
        );
        assert!(
            t239_json_u64(&json, "loss_events") == 0,
            "本机回环不应误报失流：{json}"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn t45apply_run_with_apply_affinity_rejects_before_device() {
        // 端到端：apply_affinity=true 在非 Linux target 上必须在启动期报错退出（exit 1 路径），
        // 不得 warn 后继续跑（F6：不静默降级）。
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // 与既有 temp_file 同口径：写在测试 exe 同目录（%TEMP% 指向 R:\Temp，本机写权限不可靠）。
        let path = std::env::current_exe()
            .expect("current_exe")
            .parent()
            .expect("test exe dir")
            .join(format!(
                "tspi-t45apply-{}-{unique}.toml",
                std::process::id()
            ));
        let text = "[host]\nblock_size = 128\nsample_rate = 48000\nworkers = 4\ngraph = \"stereo_identity\"\n[alsa]\nbackend = \"fake\"\n[rt]\ndsp_cpus = \"2-5\"\ntick_freq_hz = 1000000000\napply_affinity = true\n[web]\nbind = \"127.0.0.1:1\"\n[run]\nmax_blocks = 1\n";
        std::fs::write(&path, text).expect("write config");
        let result = run_command(path.to_str().expect("utf8 path"), None, None);
        let _ = std::fs::remove_file(&path);
        let error = result.expect_err("非 Linux + apply_affinity=true 必须拒绝启动");
        assert!(error.contains("apply_affinity=true"), "{error}");
        assert!(
            error.contains("fail closed") || error.contains("fail-closed"),
            "{error}"
        );
    }
}
