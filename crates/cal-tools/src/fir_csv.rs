//! mimo FIR CSV 读取：跳过空行与 `#` 注释行，其余每行一个抽头。

use std::path::Path;

/// 输入内容错误（不是数字、逗号分隔行、没有抽头）。
pub const BAD_INPUT: &str = "E_FIR_CSV_INPUT";
/// 文件读取错误。
pub const IO: &str = "E_FIR_CSV_IO";

/// FIR CSV 读取错误（稳定码 + 人读消息）。
#[derive(Clone, Debug, PartialEq)]
pub struct FirCsvError {
    code: &'static str,
    message: String,
}

impl FirCsvError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// 稳定错误码。
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// 人读消息。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for FirCsvError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for FirCsvError {}

/// 读一个 FIR CSV：每行一个 f64 抽头；空行与 `#` 开头的注释行跳过。
pub fn read_fir_csv(path: &Path) -> Result<Vec<f64>, FirCsvError> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| FirCsvError::new(IO, format!("read {}: {err}", path.display())))?;
    let mut taps = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.contains(',') {
            return Err(FirCsvError::new(
                BAD_INPUT,
                format!(
                    "{}:{}: expected one tap per line, got a comma-separated row",
                    path.display(),
                    line_index + 1
                ),
            ));
        }
        let tap = line.parse::<f64>().map_err(|_| {
            FirCsvError::new(
                BAD_INPUT,
                format!(
                    "{}:{}: {:?} is not a number",
                    path.display(),
                    line_index + 1,
                    line
                ),
            )
        })?;
        taps.push(tap);
    }
    if taps.is_empty() {
        return Err(FirCsvError::new(
            BAD_INPUT,
            format!("{}: CSV has no taps", path.display()),
        ));
    }
    Ok(taps)
}
