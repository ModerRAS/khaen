//! T66v3：单点校准产物的确定性 artifact 包。
//!
//! 目标（对应任务目标 ①/④）：
//! - 把"设计 → 方案字节"的产物组装成**可复现**的 artifact 目录：同一设计 + 同一选项，
//!   每次写出的文件字节、sha256、manifest 都完全相同（不含时间戳/主机名/随机数）；
//! - 每个文件带 sha256 + 字节数，外加整包摘要 `bundle_sha256`（路径+摘要的确定性序列再 sha256），
//!   供证据链与 job artifact 登记直接消费；
//! - 写入是**不可变**的：同路径已有不同内容一律拒绝，同内容重复写幂等（与 job.rs 的 artifact
//!   语义一致）。
//!
//! 边界（如实声明，不越界承诺）：
//! - 本模块**不做**测量质量门、权威加权、安全钳位与 FULL/PARTIAL/NONE 三态——那些在
//!   `khaen-mimo-design`（quality/authority/limits）与校准 job 编排里；这里只负责"把已经
//!   确定的设计编译成方案并冻结成证据包"，report.json 的 limitations 字段写明这一点；
//! - 不读取仓库外路径、不打开声卡、不依赖 host/daemon（host 只在 dev 测试里验证"包里的方案
//!   能被准入与图编译接受"）。

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use khaen_core::sha256::sha256;
use khaen_designer::modal_peq::PeqDesign;
use khaen_plan_builder::PlanConfig;

use crate::job::sha256_hex;
use crate::plan_compile::{compile_peq_design, CompiledPlan, PlanCompileError, PlanCompileOptions};

/// 包 schema（params.json / report.json 共用前缀）。
pub const BUNDLE_SCHEMA: &str = "khaen-single-point-bundle/v1";
/// 报告 schema。
pub const REPORT_SCHEMA: &str = "khaen-single-point-report/v1";
/// manifest schema。
pub const MANIFEST_SCHEMA: &str = "khaen-single-point-artifacts/v1";
/// params 文件名。
pub const PARAMS_FILE_NAME: &str = "params.json";
/// report 文件名。
pub const REPORT_FILE_NAME: &str = "report.json";
/// manifest 文件名。
pub const MANIFEST_FILE_NAME: &str = "single-point.manifest.json";

/// 产物来源的诚实标注（预测不得写成实测）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// 真实测量得到的设计输入。
    Measured,
    /// 仿真/合成输入（例如 room-sim、合成 H(f)）。
    Synthetic,
    /// 来源未知（调用方没给证据）。
    Unknown,
}

impl Provenance {
    /// 稳定机器名（写进 JSON）。
    pub fn as_str(self) -> &'static str {
        match self {
            Provenance::Measured => "MEASURED",
            Provenance::Synthetic => "SYNTHETIC",
            Provenance::Unknown => "UNKNOWN",
        }
    }
}

/// 包内单个文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleFile {
    /// 相对包目录的路径（固定集合，不接受调用方拼路径）。
    pub path: &'static str,
    /// 文件内容。
    pub bytes: Vec<u8>,
    /// 内容 sha256（64 位小写十六进制）。
    pub sha256: String,
}

/// 一次构建得到的完整包（内存形态；`write_bundle` 落盘）。
#[derive(Debug, Clone, PartialEq)]
pub struct PlanBundle {
    /// 编译结果（方案 + 字节 + sha256 + graph_id）。
    pub compiled: CompiledPlan,
    /// 包内文件（固定顺序，含 manifest 之外的三个产物）。
    pub files: Vec<BundleFile>,
    /// manifest 文件字节（也参与整包摘要）。
    pub manifest: Vec<u8>,
    /// 整包摘要：sha256("path\tsha256\n" 按路径排序拼接，不含 manifest 自身)。
    pub bundle_sha256: String,
}

impl PlanBundle {
    /// 按相对路径取文件（未找到返回 None）。
    pub fn file(&self, path: &str) -> Option<&BundleFile> {
        self.files.iter().find(|file| file.path == path)
    }
}

/// 包写入/校验错误：稳定码 + 说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleError {
    code: &'static str,
    message: String,
}

impl BundleError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 稳定错误码（`write_failed` / `artifact_conflict` / `verify_missing` / `verify_mismatch`）。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 给人看的说明。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for BundleError {}

/// 稳定错误码。
pub mod error {
    /// 写文件失败（IO/权限）。
    pub const WRITE_FAILED: &str = "write_failed";
    /// 目标路径已有不同内容（artifact 不可变）。
    pub const ARTIFACT_CONFLICT: &str = "artifact_conflict";
    /// 校验时文件缺失。
    pub const VERIFY_MISSING: &str = "verify_missing";
    /// 校验时内容不一致。
    pub const VERIFY_MISMATCH: &str = "verify_mismatch";
}

/// 一份可复现的 f64 文本表示：17 位有效数字的科学计数法（f64 round-trip 精确）。
fn canonical_f64(value: f64) -> String {
    format!("{value:.16e}")
}

/// 设计里"影响方案字节"的部分的确定性摘要。
///
/// 只取 `fs` 与每通道的 lane index + biquad 系数（b0,b1,b2,a1,a2）；`created_unix`、
/// tool version、通道名、fit 报告 sha 等**不影响方案字节**的字段不进摘要，
/// 因此同一组系数换时间/换名字得到同一摘要（测试锁定）。
pub fn design_digest(design: &PeqDesign) -> String {
    let mut text = String::with_capacity(256 + design.channels.len() * 128);
    text.push_str("khaen-single-point-design/v1\n");
    text.push_str(&format!("fs={}\n", design.fs));
    text.push_str(&format!("channels={}\n", design.channels.len()));
    for channel in &design.channels {
        text.push_str(&format!("channel={}\n", channel.index));
        text.push_str(&format!("bands={}\n", channel.bands.len()));
        for band in &channel.bands {
            text.push_str(&canonical_f64(band.b[0]));
            text.push(' ');
            text.push_str(&canonical_f64(band.b[1]));
            text.push(' ');
            text.push_str(&canonical_f64(band.b[2]));
            text.push(' ');
            text.push_str(&canonical_f64(band.a[1]));
            text.push(' ');
            text.push_str(&canonical_f64(band.a[2]));
            text.push('\n');
        }
    }
    sha256_hex(text.as_bytes())
}

/// 构建参数快照（params.json）——确定性序列化，字段顺序固定。
fn params_json(
    design: &PeqDesign,
    options: &PlanCompileOptions,
    provenance: Provenance,
    input_sha256: Option<&str>,
    digest: &str,
) -> Vec<u8> {
    let mut text = String::with_capacity(512 + design.channels.len() * 128);
    text.push('{');
    text.push_str(&format!("\"schema\":\"{BUNDLE_SCHEMA}\","));
    text.push_str(&format!("\"provenance\":\"{}\",", provenance.as_str()));
    text.push_str(&format!("\"design_digest\":\"{digest}\","));
    match input_sha256 {
        Some(sha) => text.push_str(&format!("\"input_artifact_sha256\":\"{sha}\",")),
        None => text.push_str("\"input_artifact_sha256\":null,"),
    }
    text.push_str("\"options\":{");
    text.push_str(&format!("\"sample_rate\":{},", options.sample_rate));
    text.push_str(&format!("\"block_size\":{},", options.block_size));
    text.push_str(&format!("\"channels\":{},", options.channels));
    text.push_str("\"lane_gain_db\":[");
    for (index, gain) in options.lane_gain_db.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        text.push_str(&format!("{}", f64::from(*gain)));
    }
    text.push_str("],\"lane_delay\":[");
    for (index, delay) in options.lane_delay.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        text.push_str(&format!("{delay}"));
    }
    text.push_str("]},");
    text.push_str("\"design\":{");
    text.push_str(&format!("\"fs\":{},\"channels\":[", design.fs));
    for (position, channel) in design.channels.iter().enumerate() {
        if position > 0 {
            text.push(',');
        }
        text.push_str(&format!("{{\"index\":{},\"bands\":[", channel.index));
        for (band_position, band) in channel.bands.iter().enumerate() {
            if band_position > 0 {
                text.push(',');
            }
            text.push_str("{\"b\":[");
            text.push_str(&format!(
                "{},{},{}],\"a\":[1.0,{},{}]}}",
                canonical_f64(band.b[0]),
                canonical_f64(band.b[1]),
                canonical_f64(band.b[2]),
                canonical_f64(band.a[1]),
                canonical_f64(band.a[2])
            ));
        }
        text.push_str("]}");
    }
    text.push_str("]}}");
    text.into_bytes()
}

/// 构建报告（report.json）——只写已知事实；三态/质量门明确标为不在本包断言范围。
fn report_json(bundle: &PlanBundle, digest: &str, provenance: Provenance) -> Vec<u8> {
    let plan_file = bundle
        .file(crate::plan_compile::PLAN_ARTIFACT_PATH)
        .expect("plan file is always present");
    let mut text = String::with_capacity(512);
    text.push('{');
    text.push_str(&format!("\"schema\":\"{REPORT_SCHEMA}\","));
    text.push_str(&format!("\"provenance\":\"{}\",", provenance.as_str()));
    text.push_str(&format!("\"design_digest\":\"{digest}\","));
    text.push_str(&format!(
        "\"plan\":{{\"path\":\"{}\",\"sha256\":\"{}\",\"graph_id\":\"{}\",\"bytes\":{}}},",
        plan_file.path,
        bundle.compiled.sha256,
        bundle.compiled.graph_id,
        plan_file.bytes.len()
    ));
    text.push_str("\"limits\":[");
    text.push_str(
        "\"quality gate / authority weighting / safety clamp and FULL/PARTIAL/NONE tri-state are produced by the calibration job pipeline (khaen-mimo-design + job), not asserted by this bundle\",",
    );
    text.push_str("\"this bundle only freezes an already-decided design into an ABI v3 plan\"");
    text.push_str("]}");
    text.into_bytes()
}

/// manifest 字节：文件清单 + 整包摘要。
fn manifest_json(files: &[BundleFile], bundle_sha256: &str) -> Vec<u8> {
    let mut text = String::with_capacity(256 + files.len() * 96);
    text.push('{');
    text.push_str(&format!("\"schema\":\"{MANIFEST_SCHEMA}\","));
    text.push_str(&format!("\"bundle_sha256\":\"{bundle_sha256}\","));
    text.push_str("\"files\":[");
    for (index, file) in files.iter().enumerate() {
        if index > 0 {
            text.push(',');
        }
        text.push_str(&format!(
            "{{\"path\":\"{}\",\"sha256\":\"{}\",\"bytes\":{}}}",
            file.path,
            file.sha256,
            file.bytes.len()
        ));
    }
    text.push_str("]}");
    text.into_bytes()
}

/// 整包摘要：按路径排序的 "path\tsha256\n" 拼接后 sha256（不含 manifest 自身）。
fn bundle_digest(files: &[BundleFile]) -> String {
    let mut entries: Vec<(&str, &str)> = files
        .iter()
        .map(|file| (file.path, file.sha256.as_str()))
        .collect();
    entries.sort_by(|left, right| left.0.cmp(right.0));
    let mut text = String::new();
    for (path, sha) in entries {
        text.push_str(path);
        text.push('\t');
        text.push_str(sha);
        text.push('\n');
    }
    sha256_hex(text.as_bytes())
}

/// 构建 artifact 包（编译 + 参数快照 + 报告 + manifest），不落盘。
pub fn build_bundle(
    design: &PeqDesign,
    options: &PlanCompileOptions,
    provenance: Provenance,
    input_sha256: Option<&str>,
) -> Result<PlanBundle, PlanCompileError> {
    // 先做一次与 plan_compile 同口径的选项/设计校验（compile_peq_design 会重复校验，
    // 但那是有意的 fail-closed 冗余；这里不复制校验逻辑）。
    let compiled = compile_peq_design(design, options)?;
    let digest = design_digest(design);
    let mut bundle = PlanBundle {
        compiled,
        files: Vec::new(),
        manifest: Vec::new(),
        bundle_sha256: String::new(),
    };
    let params = params_json(design, options, provenance, input_sha256, &digest);
    let plan_bytes = bundle.compiled.bytes.clone();
    let files = vec![
        BundleFile {
            path: PARAMS_FILE_NAME,
            sha256: sha256_hex(&params),
            bytes: params,
        },
        BundleFile {
            path: crate::plan_compile::PLAN_ARTIFACT_PATH,
            sha256: sha256_hex(&plan_bytes),
            bytes: plan_bytes,
        },
    ];
    bundle.files = files;
    let report = report_json(&bundle, &digest, provenance);
    let files = {
        let mut files = bundle.files.clone();
        files.push(BundleFile {
            path: REPORT_FILE_NAME,
            sha256: sha256_hex(&report),
            bytes: report,
        });
        files
    };
    bundle.files = files;
    bundle.bundle_sha256 = bundle_digest(&bundle.files);
    bundle.manifest = manifest_json(&bundle.files, &bundle.bundle_sha256);
    Ok(bundle)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), BundleError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            BundleError::new(
                error::WRITE_FAILED,
                format!("cannot create directory {}: {err}", parent.display()),
            )
        })?;
    }
    match fs::read(path) {
        Ok(existing) => {
            if existing == bytes {
                return Ok(());
            }
            Err(BundleError::new(
                error::ARTIFACT_CONFLICT,
                format!(
                    "{} already exists with different content ({} vs {} bytes); artifacts are immutable",
                    path.display(),
                    existing.len(),
                    bytes.len()
                ),
            ))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::write(path, bytes).map_err(|err| {
                BundleError::new(
                    error::WRITE_FAILED,
                    format!("cannot write {}: {err}", path.display()),
                )
            })
        }
        Err(err) => Err(BundleError::new(
            error::WRITE_FAILED,
            format!("cannot read {}: {err}", path.display()),
        )),
    }
}

/// 把包写到目录（不可变语义：同内容幂等，不同内容拒绝；manifest 最后写）。
pub fn write_bundle(dir: &Path, bundle: &PlanBundle) -> Result<(), BundleError> {
    for file in &bundle.files {
        write_file(&dir.join(file.path), &file.bytes)?;
    }
    write_file(&dir.join(MANIFEST_FILE_NAME), &bundle.manifest)
}

/// 按期望包校验磁盘上的目录（文件缺失/内容不一致都报稳定码）。
pub fn verify_bundle(dir: &Path, bundle: &PlanBundle) -> Result<(), BundleError> {
    for file in &bundle.files {
        let path = dir.join(file.path);
        let bytes = fs::read(&path).map_err(|err| {
            BundleError::new(
                error::VERIFY_MISSING,
                format!("cannot read {}: {err}", path.display()),
            )
        })?;
        let actual = sha256_hex(&bytes);
        if actual != file.sha256 {
            return Err(BundleError::new(
                error::VERIFY_MISMATCH,
                format!(
                    "{} sha256 mismatch: manifest {}, file {}",
                    path.display(),
                    file.sha256,
                    actual
                ),
            ));
        }
        if bytes != file.bytes {
            return Err(BundleError::new(
                error::VERIFY_MISMATCH,
                format!("{} content differs from expected", path.display()),
            ));
        }
    }
    let manifest_path = dir.join(MANIFEST_FILE_NAME);
    let manifest = fs::read(&manifest_path).map_err(|err| {
        BundleError::new(
            error::VERIFY_MISSING,
            format!("cannot read {}: {err}", manifest_path.display()),
        )
    })?;
    if manifest != bundle.manifest {
        return Err(BundleError::new(
            error::VERIFY_MISMATCH,
            format!("{} content differs from expected", manifest_path.display()),
        ));
    }
    Ok(())
}

/// 包内文件路径列表（诊断/日志用）。
pub fn bundle_paths() -> Vec<PathBuf> {
    vec![
        PathBuf::from(PARAMS_FILE_NAME),
        PathBuf::from(crate::plan_compile::PLAN_ARTIFACT_PATH),
        PathBuf::from(REPORT_FILE_NAME),
        PathBuf::from(MANIFEST_FILE_NAME),
    ]
}

/// 生成方案名称占位用的 build config（供调用方参考；本模块不修改 plan-builder 配置）。
pub fn plan_config(options: &PlanCompileOptions) -> PlanConfig {
    PlanConfig {
        block_size: options.block_size,
        sample_rate: options.sample_rate,
        channels: options.channels,
    }
}

// 未直接使用、但契约要求"整包摘要必须基于 sha256"：这里显式引用一次以避免死代码告警，
// 同时给调用方一个"摘要就是 sha256"的断言点。
const _: fn(&[u8]) -> [u8; 32] = sha256;
