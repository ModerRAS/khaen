// khaen-mimo-fir: ART-like 全矩阵 MIMO FIR 引擎原型（离线 benchmark + 系数导入）
// v2: 持久 worker 线程（channel 派发，无每块 spawn）；系数 manifest（raw f32 / wav）；稀疏路径跳过；band_limit 元数据。
use realfft::RealFftPlanner;
use rustfft::num_complex::Complex;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Mode { Direct, Fft }

struct Args {
    n: usize, m: usize, taps: usize, fs: usize, seconds: f64,
    block: usize, threads: usize, mode: Mode, part: usize,
    manifest: Option<String>,
    demo_coeff_dir: Option<String>,
    lowrate: usize,
    lowtaps: usize,
    decim: usize,
}

fn parse_args() -> Args {
    let mut a = Args { n: 12, m: 10, taps: 512, fs: 48000, seconds: 10.0,
                       block: 128, threads: 4, mode: Mode::Fft, part: 128,
                       manifest: None, demo_coeff_dir: None,
                       lowrate: 0, lowtaps: 0, decim: 0 };
    let v: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < v.len() {
        let k = v[i].as_str();
        let val = if i + 1 < v.len() { v[i + 1].clone() } else { String::new() };
        match k {
            "--n" => a.n = val.parse().unwrap_or(a.n),
            "--m" => a.m = val.parse().unwrap_or(a.m),
            "--taps" => a.taps = val.parse().unwrap_or(a.taps),
            "--fs" => a.fs = val.parse().unwrap_or(a.fs),
            "--seconds" => a.seconds = val.parse().unwrap_or(a.seconds),
            "--block" => a.block = val.parse().unwrap_or(a.block),
            "--threads" => a.threads = val.parse().unwrap_or(a.threads),
            "--part" => a.part = val.parse().unwrap_or(a.part),
            "--mode" => a.mode = if val == "direct" { Mode::Direct } else { Mode::Fft },
            "--manifest" => a.manifest = Some(val),
            "--demo-coeff-dir" => a.demo_coeff_dir = Some(val),
            "--lowrate" => a.lowrate = val.parse().unwrap_or(a.lowrate),
            "--lowtaps" => a.lowtaps = val.parse().unwrap_or(a.lowtaps),
            "--decim" => a.decim = val.parse().unwrap_or(a.decim),
            _ => {}
        }
        i += 1;
    }
    if a.block < 16 { a.block = 16; }
    if a.part < a.block { a.part = a.block; }
    if a.threads == 0 { a.threads = 1; }
    if a.lowrate > 0 {
        if a.lowtaps == 0 { a.lowtaps = a.taps; }
        if a.decim == 0 { a.decim = a.fs / a.lowrate; }
        if a.decim < 1 { a.decim = 1; }
    } else if a.mode == Mode::Fft { a.block = a.part; }
    a
}

// ---------------- 系数导入 ----------------
fn synth_coeff(taps: usize, route: usize) -> Vec<f32> {
    let fc = 0.06 + 0.02 * (route % 4) as f64;
    let mut tmp = vec![0.0f64; taps];
    let mut sum = 0.0f64;
    for t in 0..taps {
        let x = t as f64 - (taps as f64 - 1.0) / 2.0;
        let s = if x.abs() < 1e-9 { 2.0 * fc } else { (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x) };
        let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * t as f64 / taps.max(2) as f64).cos();
        tmp[t] = s * w; sum += tmp[t];
    }
    let g = if sum.abs() > 1e-12 { 1.0 / sum } else { 1.0 };
    tmp.iter().map(|v| (v * g) as f32).collect()
}

fn read_coeff_file(path: &str) -> Result<Vec<f32>, String> {
    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ext == "wav" {
        let data = std::fs::read(path).map_err(|e| format!("read {}: {}", path, e))?;
        if data.len() < 44 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" { return Err(format!("{} 不是 WAV", path)); }
        let mut pos = 12usize;
        let mut fmt_tag = 0u16; let mut ch = 1u16; let mut bits = 32u16; let mut data_off = 0usize; let mut data_len = 0usize;
        while pos + 8 <= data.len() {
            let id = &data[pos..pos + 4]; let sz = u32::from_le_bytes([data[pos+4],data[pos+5],data[pos+6],data[pos+7]]) as usize;
            let body = pos + 8;
            if id == b"fmt " && body + 16 <= data.len() {
                fmt_tag = u16::from_le_bytes([data[body],data[body+1]]);
                ch = u16::from_le_bytes([data[body+2],data[body+3]]);
                bits = u16::from_le_bytes([data[body+14],data[body+15]]);
            } else if id == b"data" { data_off = body; data_len = sz.min(data.len().saturating_sub(body)); }
            pos = body + sz + (sz & 1);
            if data_off > 0 { break; }
        }
        if data_off == 0 { return Err(format!("{} 缺 data", path)); }
        if ch != 1 { return Err(format!("{} 需单声道（当前 {}）", path, ch)); }
        let bps = (bits / 8) as usize;
        if bps == 0 { return Err(format!("{} 位深非法", path)); }
        let frames = data_len / bps;
        let mut out = Vec::with_capacity(frames);
        for f in 0..frames {
            let o = data_off + f * bps;
            if o + bps > data.len() { break; }
            let v = if fmt_tag == 3 && bits == 32 {
                f32::from_le_bytes([data[o],data[o+1],data[o+2],data[o+3]])
            } else {
                match bps {
                    2 => i16::from_le_bytes([data[o], data[o+1]]) as f32 / 32768.0,
                    4 => i32::from_le_bytes([data[o],data[o+1],data[o+2],data[o+3]]) as f32 / 2147483648.0,
                    3 => {
                        let raw = (data[o] as i32) | ((data[o+1] as i32) << 8) | ((data[o+2] as i32) << 16);
                        let s = if raw & 0x800000 != 0 { raw | !0xFFFFFF } else { raw };
                        s as f32 / 8388608.0
                    }
                    _ => return Err(format!("{} 不支持 {} 字节样本", path, bps)),
                }
            };
            out.push(v);
        }
        Ok(out)
    } else {
        let raw = std::fs::read(path).map_err(|e| format!("read {}: {}", path, e))?;
        if raw.len() % 4 != 0 { return Err(format!("{} 不是 f32le", path)); }
        Ok(raw.chunks_exact(4).map(|c| f32::from_le_bytes([c[0],c[1],c[2],c[3]])).collect())
    }
}

struct CoeffStats { declared: usize, total: usize, skipped: usize, max_taps: usize }

fn load_coeffs(a: &Args) -> Result<(Vec<Vec<Option<Vec<f32>>>>, CoeffStats), String> {
    let mut coeffs: Vec<Vec<Option<Vec<f32>>>> = (0..a.n).map(|_| (0..a.m).map(|_| None).collect()).collect();
    let mut stats = CoeffStats { declared: 0, total: a.n * a.m, skipped: 0, max_taps: a.taps };
    if let Some(dir) = &a.demo_coeff_dir {
        std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {}", dir, e))?;
        let manifest = format!("{}/manifest.txt", dir);
        let mut lines = String::new();
        for i in 0..a.n {
            for j in 0..a.m {
                let c = synth_coeff(a.taps, i * a.m + j);
                let p = format!("{}/r_{}_{}.f32", dir, i, j);
                let bytes: Vec<u8> = c.iter().flat_map(|v| v.to_le_bytes()).collect();
                std::fs::write(&p, bytes).map_err(|e| format!("write {}: {}", p, e))?;
                lines.push_str(&format!("{} {} {} {} {}\n", i, j, p, a.fs, a.taps));
                coeffs[i][j] = Some(c);
                stats.declared += 1;
            }
        }
        std::fs::write(&manifest, lines).map_err(|e| format!("write manifest: {}", e))?;
        return Ok((coeffs, stats));
    }
    if let Some(mpath) = &a.manifest {
        let text = std::fs::read_to_string(mpath).map_err(|e| format!("read {}: {}", mpath, e))?;
        let base = std::path::Path::new(mpath).parent().map(|p| p.to_path_buf()).unwrap_or_default();
        for (ln, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') { continue; }
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() < 4 { return Err(format!("manifest:{} 需 src dst file [fs] [taps]", ln + 1)); }
            let i: usize = f[0].parse().map_err(|_| format!("manifest:{} src", ln + 1))?;
            let j: usize = f[1].parse().map_err(|_| format!("manifest:{} dst", ln + 1))?;
            if i >= a.n || j >= a.m { return Err(format!("manifest:{} src/dst 越界", ln + 1)); }
            let p = if std::path::Path::new(f[2]).is_absolute() { std::path::PathBuf::from(f[2]) } else { base.join(f[2]) };
            let fs: usize = if f.len() > 3 { f[3].parse().map_err(|_| format!("manifest:{} fs", ln + 1))? } else { a.fs };
            if fs != a.fs { return Err(format!("manifest:{} fs={} != 当前 {}", ln + 1, fs, a.fs)); }
            let taps: usize = if f.len() > 4 { f[4].parse().map_err(|_| format!("manifest:{} taps", ln + 1))? } else { a.taps };
            if taps > a.taps { return Err(format!("manifest:{} taps={} > 运行 taps={}", ln + 1, taps, a.taps)); }
            let mut c = read_coeff_file(p.to_str().unwrap_or(""))?;
            if c.len() < taps { return Err(format!("manifest:{} 系数太短 {} < {}", ln + 1, c.len(), taps)); }
            c.truncate(taps);
            // 补零到运行 taps（长 FIR 可按运行长度截断；短系数允许）
            c.resize(a.taps, 0.0);
            let nonzero = c.iter().any(|v| v.abs() > 1e-12);
            if nonzero { stats.declared += 1; coeffs[i][j] = Some(c); stats.max_taps = stats.max_taps.max(taps); } else { stats.skipped += 1; }
        }
    } else {
        for i in 0..a.n {
            for j in 0..a.m {
                coeffs[i][j] = Some(synth_coeff(a.taps, i * a.m + j));
                stats.declared += 1;
            }
        }
    }
    stats.skipped = stats.total - stats.declared;
    Ok((coeffs, stats))
}

fn limiter(x: f32, th: f32) -> f32 { x.max(-th).min(th) }

fn proc_cpu_s() -> f64 {
    if let Ok(s) = std::fs::read_to_string("/proc/self/stat") {
        if let Some(close) = s.rfind(')') {
            let parts: Vec<&str> = s[close + 2..].split_whitespace().collect();
            if parts.len() > 12 {
                let ut: f64 = parts[11].parse().unwrap_or(0.0);
                let st: f64 = parts[12].parse().unwrap_or(0.0);
                return (ut + st) / 100.0;
            }
        }
    }
    0.0
}

fn make_signal(a: &Args) -> Vec<Vec<f32>> {
    let total = (a.fs as f64 * a.seconds) as usize;
    let mut sig = vec![vec![0.0f32; total]; a.n];
    for i in 0..a.n {
        let f = 1000.0 + 37.0 * i as f64;
        for t in 0..total {
            sig[i][t] = (0.1 * (2.0 * std::f64::consts::PI * f * t as f64 / a.fs as f64).sin()) as f32;
        }
    }
    sig
}

fn run_threads(a: &Args, job: impl Fn(usize) -> f64 + Sync) -> Vec<f64> {
    let workers = a.threads.min(a.m).max(1);
    std::thread::scope(|s| {
        let job_ref = &job;
        let mut hs = Vec::new();
        for w in 0..workers {
            hs.push(s.spawn(move || {
                let mut acc = 0.0f64;
                let mut j = w;
                while j < a.m {
                    acc += job_ref(j);
                    j += workers;
                }
                acc
            }));
        }
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

// ---------------- FFT worker pool（持久线程） ----------------
type HMatrix = Arc<Vec<Vec<Option<Vec<Vec<Complex<f32>>>>>>>;

struct FftPool {
    txs: Vec<Sender<Arc<Vec<Vec<Complex<f32>>>>>>,
    rxs: Vec<Receiver<f64>>,
}

fn make_pool(
    a: &Args,
    h: HMatrix,
    inv: Arc<dyn realfft::ComplexToReal<f32>>,
    bins: usize,
    part: usize,
    passes: usize,
) -> FftPool {
    let workers = a.threads.min(a.m).max(1);
    let mut txs = Vec::with_capacity(workers);
    let mut rxs = Vec::with_capacity(workers);
    for w in 0..workers {
        let (tx_job, rx_job) = channel::<Arc<Vec<Vec<Complex<f32>>>>>();
        let (tx_res, rx_res) = channel::<f64>();
        let h = h.clone();
        let inv = inv.clone();
        let n = a.n;
        let m = a.m;
        let outputs: Vec<usize> = (w..m).step_by(workers).collect();
        std::thread::spawn(move || {
            let fftlen = 2 * part;
            let mut acc = vec![Complex::new(0.0f32, 0.0); bins];
            let mut time = vec![0.0f32; fftlen];
            while let Ok(spectra) = rx_job.recv() {
                let mut total = 0.0f64;
                for &j in &outputs {
                    for v in acc.iter_mut() { *v = Complex::new(0.0, 0.0); }
                    for i in 0..n {
                        let Some(hp_list) = h[i][j].as_ref() else { continue };
                        let xs = &spectra[i];
                        for p in 0..passes {
                            let hp = &hp_list[p];
                            for f in 0..bins { acc[f] += xs[f] * hp[f]; }
                        }
                    }
                    inv.process(&mut acc, &mut time).unwrap();
                    let scale = 1.0 / fftlen as f32;
                    let mut ss = 0.0f64;
                    for t in 0..part {
                        let v = limiter(time[part + t] * scale, 0.89);
                        ss += (v as f64) * (v as f64);
                    }
                    total += (ss / part as f64).sqrt();
                }
                let _ = tx_res.send(total);
            }
        });
        txs.push(tx_job);
        rxs.push(rx_res);
    }
    FftPool { txs, rxs }
}

fn process_fft(a: &Args, coeffs: &[Vec<Option<Vec<f32>>>]) -> Result<(f64, usize, f64), String> {
    let part = a.part;
    let fftlen = 2 * part;
    let bins = fftlen / 2 + 1;
    let passes = (a.taps + part - 1) / part;
    let mut planner = RealFftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(fftlen);
    let inv = planner.plan_fft_inverse(fftlen);
    let mut h: Vec<Vec<Option<Vec<Vec<Complex<f32>>>>>> = Vec::with_capacity(a.n);
    for i in 0..a.n {
        let mut mi: Vec<Option<Vec<Vec<Complex<f32>>>>> = Vec::with_capacity(a.m);
        for j in 0..a.m {
            match coeffs[i][j].as_ref() {
                None => mi.push(None),
                Some(c) => {
                    let mut parts = Vec::with_capacity(passes);
                    for p in 0..passes {
                        let mut buf = vec![0.0f32; fftlen];
                        for k in 0..part {
                            let idx = p * part + k;
                            if idx < a.taps && idx < c.len() { buf[k] = c[idx]; }
                        }
                        let mut spec = vec![Complex::new(0.0f32, 0.0); bins];
                        fwd.process(&mut buf, &mut spec).unwrap();
                        parts.push(spec);
                    }
                    mi.push(Some(parts));
                }
            }
        }
        h.push(mi);
    }
    let h = Arc::new(h);
    let inv: Arc<dyn realfft::ComplexToReal<f32>> = inv;
    let pool = make_pool(a, h, inv, bins, part, passes);
    let sig = make_signal(a);
    let total = sig[0].len();
    let blocks = total / part;
    let start = Instant::now();
    let cpu0 = proc_cpu_s();
    let mut rms = 0.0f64;
    let mut prev: Vec<Vec<f32>> = vec![vec![0.0f32; part]; a.n];
    for b in 0..blocks {
        let mut spectra: Vec<Vec<Complex<f32>>> = Vec::with_capacity(a.n);
        for i in 0..a.n {
            let mut buf = vec![0.0f32; fftlen];
            buf[..part].copy_from_slice(&prev[i]);
            buf[part..].copy_from_slice(&sig[i][b * part..(b + 1) * part]);
            let mut spec = vec![Complex::new(0.0f32, 0.0); bins];
            fwd.process(&mut buf, &mut spec).unwrap();
            spectra.push(spec);
        }
        let spectra = Arc::new(spectra);
        for tx in &pool.txs { tx.send(spectra.clone()).map_err(|e| format!("worker send: {}", e))?; }
        for rx in &pool.rxs { rms += rx.recv().map_err(|e| format!("worker recv: {}", e))?; }
        for i in 0..a.n { prev[i].copy_from_slice(&sig[i][b * part..(b + 1) * part]); }
    }
    let wall = start.elapsed().as_secs_f64();
    let cpu = proc_cpu_s() - cpu0;
    let _ = rms;
    Ok((wall, blocks, cpu / wall * 100.0))
}

fn process_direct(a: &Args, coeffs: &[Vec<Option<Vec<f32>>>], sig: &[Vec<f32>]) -> (f64, usize, f64) {
    let total = sig[0].len();
    let blocks = total / a.block;
    let start = Instant::now();
    let cpu0 = proc_cpu_s();
    let mut rms = 0.0f64;
    for b in 0..blocks {
        let g0 = b * a.block;
        let vals = run_threads(a, |j| {
            let mut ss = 0.0f64;
            for t in 0..a.block {
                let g = g0 + t;
                let mut y = 0.0f32;
                for i in 0..a.n {
                    let Some(c) = coeffs[i][j].as_ref() else { continue };
                    let s = &sig[i];
                    let mut k = 0;
                    while k < a.taps && k <= g {
                        y += c[k] * s[g - k];
                        k += 1;
                    }
                }
                let v = limiter(y, 0.89);
                ss += (v as f64) * (v as f64);
            }
            (ss / a.block as f64).sqrt()
        });
        rms += vals.iter().sum::<f64>();
    }
    let wall = start.elapsed().as_secs_f64();
    let cpu = proc_cpu_s() - cpu0;
    let _ = rms;
    (wall, blocks, cpu / wall * 100.0)
}


fn make_lp(taps: usize, cutoff: f64) -> Vec<f32> {
    let mut h = vec![0.0f64; taps];
    let mut sum = 0.0f64;
    for t in 0..taps {
        let x = t as f64 - (taps as f64 - 1.0) / 2.0;
        let s = if x.abs() < 1e-9 { 2.0 * cutoff } else { (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x) };
        let w = if taps > 1 { 0.5 - 0.5 * (2.0 * std::f64::consts::PI * t as f64 / (taps as f64 - 1.0)).cos() } else { 1.0 };
        h[t] = s * w;
        sum += h[t];
    }
    let g = if sum.abs() > 1e-12 { 1.0 / sum } else { 1.0 };
    h.iter().map(|v| (v * g) as f32).collect()
}

// 低频多速率原型：抗混叠抽取 -> 低速率 MIMO FIR（合成系数）-> 插值回全速
fn process_multirate(a: &Args) -> (f64, usize, f64) {
    let d = a.decim.max(1);
    let lr = a.fs / d;
    let lowblock = a.block;
    let fullblock = lowblock * d;
    let dec_fir = make_lp(63, 0.45 * lr as f64 / a.fs as f64);
    let int_fir: Vec<f32> = { let mut v = dec_fir.clone(); for x in v.iter_mut() { *x *= d as f32; } v };
    let sig = make_signal(a);
    let total = sig[0].len();
    let nblocks = total / fullblock;
    if nblocks == 0 { return (0.0, 0, 0.0); }
    let start = Instant::now();
    let cpu0 = proc_cpu_s();
    // 1) 抽取（按输入并行）
    let mut low_sig: Vec<Vec<f32>> = vec![Vec::new(); a.n];
    let workers_in = a.n.min(a.threads.max(1));
    std::thread::scope(|s| {
        let mut hs = Vec::new();
        for w in 0..workers_in {
            let sig_ref = &sig;
            let dec_ref = &dec_fir;
            hs.push(s.spawn(move || {
                let mut out: Vec<(usize, Vec<f32>)> = Vec::new();
                let mut i = w;
                while i < a.n {
                    let x = &sig_ref[i];
                    let mut y = vec![0.0f32; nblocks * lowblock];
                    for m in 0..(nblocks * lowblock) {
                        let n = m * d;
                        let mut acc = 0.0f32;
                        for k in 0..dec_ref.len() { if n >= k { acc += dec_ref[k] * x[n - k]; } }
                        y[m] = acc;
                    }
                    out.push((i, y));
                    i += workers_in;
                }
                out
            }));
        }
        for h in hs { for (i, y) in h.join().unwrap() { low_sig[i] = y; } }
    });
    // 2) 低速率系数 + 频谱
    let part = lowblock;
    let fftlen = 2 * part;
    let bins = part + 1;
    let passes = (a.lowtaps + part - 1) / part;
    let mut planner = RealFftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(fftlen);
    let inv = planner.plan_fft_inverse(fftlen);
    let mut h: Vec<Vec<Option<Vec<Vec<Complex<f32>>>>>> = Vec::with_capacity(a.n);
    for i in 0..a.n {
        let mut mi: Vec<Option<Vec<Vec<Complex<f32>>>>> = Vec::with_capacity(a.m);
        for j in 0..a.m {
            let c = synth_coeff(a.lowtaps, i * a.m + j);
            let mut parts = Vec::with_capacity(passes);
            for p in 0..passes {
                let mut buf = vec![0.0f32; fftlen];
                for k in 0..part {
                    let idx = p * part + k;
                    if idx < a.lowtaps { buf[k] = c[idx]; }
                }
                let mut spec = vec![Complex::new(0.0f32, 0.0); bins];
                fwd.process(&mut buf, &mut spec).unwrap();
                parts.push(spec);
            }
            mi.push(Some(parts));
        }
        h.push(mi);
    }
    let mut spectra: Vec<Vec<Complex<f32>>> = Vec::with_capacity(nblocks * a.n);
    for b in 0..nblocks {
        for i in 0..a.n {
            let mut buf = vec![0.0f32; fftlen];
            if b > 0 { buf[..part].copy_from_slice(&low_sig[i][(b - 1) * part..b * part]); }
            buf[part..].copy_from_slice(&low_sig[i][b * part..(b + 1) * part]);
            let mut spec = vec![Complex::new(0.0f32, 0.0); bins];
            fwd.process(&mut buf, &mut spec).unwrap();
            spectra.push(spec);
        }
    }
    let workers = a.threads.min(a.m).max(1);
    let mut rms = 0.0f64;
    std::thread::scope(|s| {
        let mut hs = Vec::new();
        for w in 0..workers {
            let h_ref = &h;
            let spectra_ref = &spectra;
            let int_ref = &int_fir;
            let inv = Arc::clone(&inv);
            hs.push(s.spawn(move || {
                let mut acc_rms = 0.0f64;
                let mut j = w;
                while j < a.m {
                    let mut low_out = vec![0.0f32; nblocks * part];
                    for b in 0..nblocks {
                        let mut acc = vec![Complex::new(0.0f32, 0.0); bins];
                        for i in 0..a.n {
                            let Some(hp_list) = h_ref[i][j].as_ref() else { continue };
                            let xs = &spectra_ref[b * a.n + i];
                            for p in 0..passes {
                                let hp = &hp_list[p];
                                for f in 0..bins { acc[f] += xs[f] * hp[f]; }
                            }
                        }
                        let mut time = vec![0.0f32; fftlen];
                        inv.process(&mut acc, &mut time).unwrap();
                        let scale = 1.0 / fftlen as f32;
                        for t in 0..part { low_out[b * part + t] = time[part + t] * scale; }
                    }
                    let nfull = nblocks * fullblock;
                    for n in 0..nfull {
                        let r = n % d;
                        let mut k = r;
                        let mut y = 0.0f32;
                        while k < int_ref.len() {
                            if n >= k {
                                let m = (n - k) / d;
                                if m < low_out.len() { y += int_ref[k] * low_out[m]; }
                            }
                            k += d;
                        }
                        let v = limiter(y, 0.89);
                        acc_rms += (v as f64) * (v as f64);
                    }
                    j += workers;
                }
                acc_rms
            }));
        }
        for hh in hs { rms += hh.join().unwrap(); }
    });
    let wall = start.elapsed().as_secs_f64();
    let cpu = proc_cpu_s() - cpu0;
    let _ = rms;
    let lat_ms = ((dec_fir.len() / 2) as f64 / a.fs as f64
        + (a.lowtaps as f64 / 2.0) / lr as f64
        + (int_fir.len() / 2) as f64 / a.fs as f64) * 1000.0;
    eprintln!("multirate: lowrate={} decim={} lowtaps={} lowblock={} fullblock={} est_latency={:.1}ms",
        lr, d, a.lowtaps, part, fullblock, lat_ms);
    (wall, nblocks, cpu / wall * 100.0)
}

fn main() {
    let a = parse_args();
    let (coeffs, stats) = match load_coeffs(&a) {
        Ok(v) => v,
        Err(e) => { eprintln!("系数加载失败：{}", e); std::process::exit(2); }
    };
    let result = if a.lowrate > 0 {
        process_multirate(&a)
    } else {
        match a.mode {
            Mode::Direct => process_direct(&a, &coeffs, &make_signal(&a)),
            Mode::Fft => match process_fft(&a, &coeffs) {
                Ok(v) => v,
                Err(e) => { eprintln!("FFT 处理失败：{}", e); std::process::exit(3); }
            },
        }
    };
    let (wall, blocks, cpu_pct) = result;
    let audio = if a.lowrate > 0 {
        (blocks * a.block * a.decim) as f64 / a.fs as f64
    } else {
        (blocks * a.block) as f64 / a.fs as f64
    };
    let rtf = audio / wall;
    if a.lowrate > 0 {
        println!("mode=Multirate n={} m={} taps={} lowrate={} lowtaps={} decim={} fs={} lowblock={} threads={} audio={:.2}s wall={:.3}s rtf={:.3} cpu_pct={:.0}",
            a.n, a.m, a.taps, a.lowrate, a.lowtaps, a.decim, a.fs, a.block, a.threads, audio, wall, rtf, cpu_pct);
    } else {
        println!("mode={:?} n={} m={} taps={} fs={} block={} part={} threads={} paths={}/{} audio={:.2}s wall={:.3}s rtf={:.3} cpu_pct={:.0}",
            a.mode, a.n, a.m, a.taps, a.fs, a.block, a.part, a.threads, stats.declared, stats.total, audio, wall, rtf, cpu_pct);
    }
}
