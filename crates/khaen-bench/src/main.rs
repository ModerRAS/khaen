//! khaen-bench 命令行：跑本机性能基线并打印人类可读表或 JSON。
//!
//! 用法见 --help。人类可读表与 --json 是同一份数据（schema khaen-bench/v1）。

use std::process::ExitCode;

use khaen_bench::{run, BenchConfig, KernelMode};

/// 命令行选项。
struct Options {
    /// bench 配置。
    config: BenchConfig,
    /// 只输出 JSON。
    json: bool,
    /// 打印帮助。
    help: bool,
}

impl Options {
    fn parse(arguments: &[String]) -> Result<Self, String> {
        let mut config = BenchConfig::default();
        let mut json = false;
        let mut help = false;
        let mut index = 0;
        while index < arguments.len() {
            let flag = arguments[index].as_str();
            match flag {
                "--json" => json = true,
                "--quick" => {
                    config.warmup_blocks = 10;
                    config.measure_blocks = 60;
                    config.repeats = 2;
                }
                "--help" | "-h" => help = true,
                _ => {
                    let value = arguments
                        .get(index + 1)
                        .ok_or_else(|| format!("参数 {flag} 缺少值"))?;
                    match flag {
                        "--lanes" => config.lanes = parse_value(value, flag)?,
                        "--block-frames" => config.block_frames = parse_value(value, flag)?,
                        "--sample-rate" => config.sample_rate = parse_value(value, flag)?,
                        "--fir-taps" => config.crossover_taps = parse_value(value, flag)?,
                        "--matrix-taps" => config.matrix_taps = parse_value(value, flag)?,
                        "--factor" => config.matrix_factor = parse_value(value, flag)?,
                        "--active-lanes" => {
                            config.matrix_active_lanes = parse_value(value, flag)?;
                        }
                        "--kernel" => config.kernel_mode = KernelMode::parse(value)?,
                        "--warmup" => config.warmup_blocks = parse_value(value, flag)?,
                        "--measure" => config.measure_blocks = parse_value(value, flag)?,
                        "--repeats" => config.repeats = parse_value(value, flag)?,
                        other => return Err(format!("未知参数: {other}")),
                    }
                    index += 1;
                }
            }
            index += 1;
        }
        Ok(Self { config, json, help })
    }
}

fn parse_value<T: std::str::FromStr>(value: &str, flag: &str) -> Result<T, String> {
    value
        .parse::<T>()
        .map_err(|_| format!("参数 {flag} 的值 {value:?} 无法解析"))
}

fn print_usage() {
    println!(
        r#"khaen-bench —— khaen 实时图性能基线（T38）

用法: khaen-bench [选项]

  --json                  只输出机器可读 JSON（schema khaen-bench/v1）
  --quick                 快速模式：warmup=10 measure=60 repeats=2
  --lanes N               通道数 = lane 数（默认 22，最大 32）
  --block-frames N        每块帧数（默认 128）
  --sample-rate N         采样率 Hz（默认 48000；支持 48000 / 96000）
  --fir-taps N            crossover 每 lane 抽头数（默认 2048，最大 4096）
  --matrix-taps N         matrix 每格抽头数（默认 8）
  --factor N              matrix 抽取 / 插值因子（默认 16；48000/16 = 3 kHz）
  --active-lanes N        非恒等 matrix 子块边长（默认 16）
  --kernel MODE           kernel 级对照：auto（默认，标量 vs SIMD 两列）/ scalar / optimized
  --warmup N              每档预热块数（默认 50）
  --measure N             每档每次重复的测量块数（默认 300）
  --repeats N             每档重复次数（默认 3）
  -h, --help              显示本帮助

计时口径: PlanGraph::process 每块耗时（DSP 链本体）；不含后端 capture/playout、
线程调度与 daemon 开销。默认跑 A/B/B2/C/D/E/C1/E1 八个档位，见 lib.rs scenarios()。"#
    );
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let options = match Options::parse(&arguments) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("khaen-bench: {message}");
            eprintln!();
            print_usage();
            return ExitCode::from(2);
        }
    };
    if options.help {
        print_usage();
        return ExitCode::SUCCESS;
    }
    match run(&options.config) {
        Ok(report) => {
            if options.json {
                println!("{}", report.to_json());
            } else {
                print!("{}", report.human_report());
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("khaen-bench: {error}");
            ExitCode::FAILURE
        }
    }
}
