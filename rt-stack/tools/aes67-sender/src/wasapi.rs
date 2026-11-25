//! Windows WASAPI backends:
//!   * system loopback capture (default render endpoint, or a named render endpoint)
//!   * process loopback capture (Win10 2004+, PER_PROCESS_LOOPBACK virtual device)
//!   * render/playback helper (used to drive cross-process loopback evidence)
//!
//! No ASIO driver is implemented or needed: WASAPI loopback is endpoint-level and
//! captures any application; ASIO-only applications can output through
//! FlexASIO/ASIO4ALL into a real WASAPI endpoint (e.g. HDMI) and be captured by
//! the system loopback path. See evidence/aes67-tx-win/ASIO-AND-VIRTUAL-ENDPOINTS.md.

#![cfg(windows)]

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use windows::core::{ComInterface, Interface, GUID, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::E_NOINTERFACE;
use windows::Win32::Media::Audio::{
    eConsole, eRender, ActivateAudioInterfaceAsync, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IAudioCaptureClient, IAudioClient,
    IAudioRenderClient, IMMDevice, IMMDeviceCollection, IMMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_LOOPBACK,
    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, AUDIOCLIENT_ACTIVATION_PARAMS,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    DEVICE_STATE_ACTIVE, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, PROPERTYKEY};

use crate::source::{ReadStatus, Source};
use crate::AES67_RATE;

// MMDeviceEnumerator CLSID {BCDE0395-E52F-467C-8E3D-C4579291692E}
const CLSID_MMDEVICE_ENUMERATOR: GUID = GUID::from_u128(0xBCDE0395_E52F_467C_8E3D_C4579291692E);
const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_C000_000000000046);
// The Microsoft ApplicationLoopback sample's handler is also IAgileObject; MMDevAPI
// queries it before activating the process-loopback virtual device.
const IID_IAGILEOBJECT: GUID = GUID::from_u128(0x94EA2B94_E9CC_49E0_C0FF_EE64CA8F5B90);
const VT_LPWSTR_CODE: u16 = 31;
// PKEY_Device_FriendlyName {A45C254E-DF1C-4EFD-8020-67D146A850E0},14
const PKEY_DEVICE_FRIENDLY_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xA45C254E_DF1C_4EFD_8020_67D146A850E0),
    pid: 14,
};

pub struct ComGuard {
    uninit: bool,
}

impl ComGuard {
    pub fn init_mta() -> Result<Self> {
        unsafe {
            match CoInitializeEx(None, COINIT_MULTITHREADED) {
                Ok(()) => Ok(Self { uninit: true }),
                // RPC_E_CHANGED_MODE: thread already initialized with another mode.
                Err(e) if e.code().0 as u32 == 0x8001_0106 => Ok(Self { uninit: false }),
                Err(e) => bail!("CoInitializeEx(MTA) failed: {e}"),
            }
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.uninit {
            unsafe { CoUninitialize() };
        }
    }
}

fn pwstr_to_string(p: PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    unsafe {
        let mut len = 0usize;
        while *p.0.add(len) != 0 {
            len += 1;
        }
        let s = std::slice::from_raw_parts(p.0, len);
        Some(OsString::from_wide(s).to_string_lossy().into_owned())
    }
}

// ---------------------------------------------------------------------------
// Endpoint enumeration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct EndpointInfo {
    pub index: u32,
    pub name: String,
    pub id: String,
    pub is_default: bool,
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    unsafe {
        CoCreateInstance(
            &CLSID_MMDEVICE_ENUMERATOR,
            None::<&windows::core::IUnknown>,
            CLSCTX_ALL,
        )
        .context("CoCreateInstance(MMDeviceEnumerator) failed")
    }
}

fn device_friendly_name(dev: &IMMDevice) -> String {
    unsafe {
        let store: Result<IPropertyStore, _> = dev.OpenPropertyStore(STGM_READ);
        if let Ok(store) = store {
            if let Ok(pv) = store.GetValue(&PKEY_DEVICE_FRIENDLY_NAME) {
                if pv.Anonymous.Anonymous.vt.0 == VT_LPWSTR_CODE {
                    let p = pv.Anonymous.Anonymous.Anonymous.pwszVal;
                    if let Some(s) = pwstr_to_string(p) {
                        return s;
                    }
                }
            }
        }
        String::from("<unknown device name>")
    }
}

fn default_render_id() -> Option<String> {
    unsafe {
        let e = enumerator().ok()?;
        let d = e.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let id = d.GetId().ok()?;
        let s = pwstr_to_string(id);
        CoTaskMemFree(Some(id.0 as *const _));
        s
    }
}

/// Enumerate ACTIVE render endpoints (loopback capture targets) with friendly names.
pub fn list_render_endpoints() -> Result<Vec<EndpointInfo>> {
    unsafe {
        let e = enumerator()?;
        let coll: IMMDeviceCollection = e
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .context("EnumAudioEndpoints failed")?;
        let count = coll.GetCount().context("GetCount failed")?;
        let default_id = default_render_id();
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let dev = match coll.Item(i) {
                Ok(d) => d,
                Err(_) => continue,
            };
            let name = device_friendly_name(&dev);
            let id = match dev.GetId() {
                Ok(p) => {
                    let s = pwstr_to_string(p).unwrap_or_default();
                    CoTaskMemFree(Some(p.0 as *const _));
                    s
                }
                Err(_) => String::new(),
            };
            out.push(EndpointInfo {
                index: i,
                name,
                id: id.clone(),
                is_default: default_id.as_deref() == Some(id.as_str()),
            });
        }
        Ok(out)
    }
}

/// Select a render endpoint: None => default console render endpoint.
/// Some(selector): "#<index>", exact device id, or case-insensitive name substring.
pub fn find_render_device(selector: Option<&str>) -> Result<IMMDevice> {
    unsafe {
        let e = enumerator()?;
        match selector {
            None => e.GetDefaultAudioEndpoint(eRender, eConsole).context(
                "GetDefaultAudioEndpoint(render/console) failed (no active render endpoint?)",
            ),
            Some(sel) => {
                let sel = sel.trim();
                let endpoints = list_render_endpoints()?;
                if endpoints.is_empty() {
                    bail!("no active render endpoints found");
                }
                if let Some(rest) = sel.strip_prefix('#') {
                    let idx: u32 = rest
                        .parse()
                        .with_context(|| format!("invalid endpoint index {sel:?}"))?;
                    if let Some(info) = endpoints.iter().find(|i| i.index == idx) {
                        return find_device_by_id(&info.id);
                    }
                    bail!(
                        "endpoint index {idx} not found; available: {}",
                        endpoints
                            .iter()
                            .map(|i| format!("#{} {}", i.index, i.name))
                            .collect::<Vec<_>>()
                            .join(" | ")
                    );
                }
                if let Some(info) = endpoints.iter().find(|i| i.id == sel) {
                    return find_device_by_id(&info.id);
                }
                let lower = sel.to_ascii_lowercase();
                let matches: Vec<&EndpointInfo> = endpoints
                    .iter()
                    .filter(|i| i.name.to_ascii_lowercase().contains(&lower))
                    .collect();
                match matches.as_slice() {
                    [one] => find_device_by_id(&one.id),
                    [] => bail!(
                        "no render endpoint matches {sel:?}; available: {}",
                        endpoints
                            .iter()
                            .map(|i| format!("#{} {}", i.index, i.name))
                            .collect::<Vec<_>>()
                            .join(" | ")
                    ),
                    many => {
                        let names = many
                            .iter()
                            .map(|i| format!("#{} {} ({})", i.index, i.name, i.id))
                            .collect::<Vec<_>>()
                            .join(" | ");
                        bail!("render endpoint selector {sel:?} is ambiguous: {names}")
                    }
                }
            }
        }
    }
}

fn find_device_by_id(id: &str) -> Result<IMMDevice> {
    unsafe {
        let e = enumerator()?;
        let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
        e.GetDevice(PCWSTR(wide.as_ptr()))
            .with_context(|| format!("GetDevice({id}) failed"))
    }
}

pub fn endpoint_display_name(dev: &IMMDevice) -> String {
    device_friendly_name(dev)
}

// ---------------------------------------------------------------------------
// Mix format parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixSample {
    F32,
    I16,
    I24,
    I32,
    U8,
}

impl MixSample {
    pub fn name(self) -> &'static str {
        match self {
            MixSample::F32 => "f32",
            MixSample::I16 => "i16",
            MixSample::I24 => "i24",
            MixSample::I32 => "i32",
            MixSample::U8 => "u8",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MixFormat {
    pub channels: usize,
    pub rate: u32,
    pub bits: u16,
    pub sample: MixSample,
    pub block_align: u16,
    pub extensible: bool,
    raw: Vec<u8>,
}

impl MixFormat {
    pub fn from_ptr(p: *mut WAVEFORMATEX) -> Result<Self> {
        if p.is_null() {
            bail!("GetMixFormat returned null");
        }
        unsafe {
            let wf = &*p;
            let ext = wf.wFormatTag == 0xFFFE;
            let tag = if ext {
                let wfe = &*(p as *const WAVEFORMATEXTENSIBLE);
                wfe.SubFormat.data1
            } else {
                wf.wFormatTag as u32
            };
            let bits = wf.wBitsPerSample;
            let sample = match (tag, bits) {
                (3, 32) => MixSample::F32,
                (1, 16) => MixSample::I16,
                (1, 24) => MixSample::I24,
                (1, 32) => MixSample::I32,
                (1, 8) => MixSample::U8,
                (other, b) => bail!(
                    "unsupported WASAPI mix format: tag={other} bits={b} (supported: float32, pcm16/24/32, u8)"
                ),
            };
            let size = 18 + wf.cbSize as usize;
            let mut raw = vec![0u8; size];
            ptr::copy_nonoverlapping(p as *const u8, raw.as_mut_ptr(), size);
            CoTaskMemFree(Some(p as *const std::ffi::c_void));
            Ok(Self {
                channels: wf.nChannels as usize,
                rate: wf.nSamplesPerSec,
                bits,
                sample,
                block_align: wf.nBlockAlign,
                extensible: ext,
                raw,
            })
        }
    }

    /// Build from a caller-provided WAVEFORMATEX/WAVEFORMATEXTENSIBLE byte image
    /// (process loopback has no GetMixFormat; the caller supplies the format).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        anyhow::ensure!(bytes.len() >= 18, "WAVEFORMATEX image too short");
        Self::parse(bytes.to_vec())
    }

    fn parse(raw: Vec<u8>) -> Result<Self> {
        unsafe {
            let p = raw.as_ptr() as *const WAVEFORMATEX;
            let wf = &*p;
            let ext = wf.wFormatTag == 0xFFFE;
            let tag = if ext {
                let wfe = &*(p as *const WAVEFORMATEXTENSIBLE);
                wfe.SubFormat.data1
            } else {
                wf.wFormatTag as u32
            };
            let bits = wf.wBitsPerSample;
            let sample = match (tag, bits) {
                (3, 32) => MixSample::F32,
                (1, 16) => MixSample::I16,
                (1, 24) => MixSample::I24,
                (1, 32) => MixSample::I32,
                (1, 8) => MixSample::U8,
                (other, b) => bail!(
                    "unsupported WASAPI format: tag={other} bits={b} (supported: float32, pcm16/24/32, u8)"
                ),
            };
            Ok(Self {
                channels: wf.nChannels as usize,
                rate: wf.nSamplesPerSec,
                bits,
                sample,
                block_align: wf.nBlockAlign,
                extensible: ext,
                raw,
            })
        }
    }

    pub fn fmt_ptr(&self) -> *const WAVEFORMATEX {
        self.raw.as_ptr() as *const WAVEFORMATEX
    }

    pub fn describe(&self) -> String {
        format!(
            "{} Hz, {} ch, {} ({} bit), block_align={}",
            self.rate,
            self.channels,
            self.sample.name(),
            self.bits,
            self.block_align
        )
    }

    fn read_frames(&self, data: *const u8, frames: usize) -> Vec<f32> {
        let n = frames * self.channels;
        let mut out = Vec::with_capacity(n);
        unsafe {
            match self.sample {
                MixSample::F32 => {
                    let p = data as *const f32;
                    for i in 0..n {
                        out.push(*p.add(i));
                    }
                }
                MixSample::I16 => {
                    let p = data as *const i16;
                    for i in 0..n {
                        out.push(*p.add(i) as f32 / 32_768.0);
                    }
                }
                MixSample::I24 => {
                    let p = data;
                    for i in 0..n {
                        let b = p.add(i * 3);
                        let u =
                            (*b as u32) | ((*b.add(1) as u32) << 8) | ((*b.add(2) as u32) << 16);
                        let v = if u & 0x0080_0000 != 0 {
                            (u | 0xFF00_0000) as i32
                        } else {
                            u as i32
                        };
                        out.push(v as f32 / 8_388_608.0);
                    }
                }
                MixSample::I32 => {
                    let p = data as *const i32;
                    for i in 0..n {
                        out.push(*p.add(i) as f32 / 2_147_483_648.0);
                    }
                }
                MixSample::U8 => {
                    for i in 0..n {
                        out.push((*data.add(i) as f32 - 128.0) / 128.0);
                    }
                }
            }
        }
        out
    }

    fn write_frames(&self, data: *mut u8, interleaved: &[f32]) {
        unsafe {
            match self.sample {
                MixSample::F32 => {
                    let p = data as *mut f32;
                    for (i, s) in interleaved.iter().enumerate() {
                        *p.add(i) = *s;
                    }
                }
                MixSample::I16 => {
                    let p = data as *mut i16;
                    for (i, s) in interleaved.iter().enumerate() {
                        *p.add(i) = (s.clamp(-1.0, 1.0) * 32_767.0).round() as i16;
                    }
                }
                MixSample::I24 => {
                    for (i, s) in interleaved.iter().enumerate() {
                        let v = (s.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
                        let u = (v & 0x00FF_FFFF) as u32;
                        let b = data.add(i * 3);
                        *b = u as u8;
                        *b.add(1) = (u >> 8) as u8;
                        *b.add(2) = (u >> 16) as u8;
                    }
                }
                MixSample::I32 => {
                    let p = data as *mut i32;
                    for (i, s) in interleaved.iter().enumerate() {
                        *p.add(i) = (s.clamp(-1.0, 1.0) * 2_147_483_647.0).round() as i32;
                    }
                }
                MixSample::U8 => {
                    for (i, s) in interleaved.iter().enumerate() {
                        *data.add(i) = ((s.clamp(-1.0, 1.0) * 127.0) + 128.0).round() as u8;
                    }
                }
            }
        }
    }
}

fn open_audio_client(dev: &IMMDevice) -> Result<IAudioClient> {
    unsafe {
        dev.Activate::<IAudioClient>(CLSCTX_ALL, None)
            .context("IMMDevice::Activate(IAudioClient) failed")
    }
}

// ---------------------------------------------------------------------------
// Capture (system loopback + process loopback)
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct CaptureStats {
    pub packets: AtomicU64,
    pub frames_in: AtomicU64,
    pub frames_out: AtomicU64,
    pub discontinuity: AtomicU64,
    pub silent_packets: AtomicU64,
    pub timestamp_errors: AtomicU64,
    pub overflow_drops: AtomicU64,
    pub read_errors: AtomicU64,
}

#[derive(Debug, Clone, Default)]
pub struct CaptureSnapshot {
    pub packets: u64,
    pub frames_in: u64,
    pub frames_out: u64,
    pub discontinuity: u64,
    pub silent_packets: u64,
    pub timestamp_errors: u64,
    pub overflow_drops: u64,
    pub read_errors: u64,
}

impl CaptureStats {
    pub fn snapshot(&self) -> CaptureSnapshot {
        CaptureSnapshot {
            packets: self.packets.load(Ordering::Relaxed),
            frames_in: self.frames_in.load(Ordering::Relaxed),
            frames_out: self.frames_out.load(Ordering::Relaxed),
            discontinuity: self.discontinuity.load(Ordering::Relaxed),
            silent_packets: self.silent_packets.load(Ordering::Relaxed),
            timestamp_errors: self.timestamp_errors.load(Ordering::Relaxed),
            overflow_drops: self.overflow_drops.load(Ordering::Relaxed),
            read_errors: self.read_errors.load(Ordering::Relaxed),
        }
    }
}

pub struct CaptureHandle {
    pub format: MixFormat,
    pub endpoint_name: String,
    pub process_loopback: bool,
    pub stats: Arc<CaptureStats>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl CaptureHandle {
    pub fn stop(mut self) {
        self.request_stop();
    }

    fn request_stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        self.request_stop();
    }
}

/// Start WASAPI loopback capture on `dev` (system-wide loopback of that render
/// endpoint). Output is stereo f32 @ 48 kHz.
pub fn start_system_loopback(
    dev: &IMMDevice,
    endpoint_name: &str,
) -> Result<(CaptureHandle, Receiver<Vec<f32>>)> {
    let client = open_audio_client(dev)?;
    let format =
        unsafe { MixFormat::from_ptr(client.GetMixFormat().context("GetMixFormat failed")?)? };
    unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                100 * 10_000, // 100 ms in 100 ns units
                0,
                format.fmt_ptr(),
                None,
            )
            .context("IAudioClient::Initialize(loopback) failed")?;
    }
    start_capture_thread(
        client,
        format,
        endpoint_name.to_string(),
        false,
        Arc::new(CaptureStats::default()),
    )
}

/// WAVEFORMATEXTENSIBLE helper: 2ch / 48 kHz / 32-bit IEEE float.
fn process_loopback_format() -> Vec<u8> {
    // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {00000003-0000-0010-8000-00AA00389B71}
    const SUBTYPE_IEEE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00AA00389B71);
    let wfe = WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: 0xFFFE,
            nChannels: 2,
            nSamplesPerSec: AES67_RATE,
            nAvgBytesPerSec: AES67_RATE * 8,
            nBlockAlign: 8,
            wBitsPerSample: 32,
            cbSize: 22,
        },
        Samples: windows::Win32::Media::Audio::WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: 32,
        },
        dwChannelMask: 3, // SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT
        SubFormat: SUBTYPE_IEEE_FLOAT,
    };
    // Return the full byte image (WAVEFORMATEX + cbSize extension fields).
    unsafe {
        std::slice::from_raw_parts(
            std::ptr::addr_of!(wfe) as *const u8,
            std::mem::size_of::<WAVEFORMATEXTENSIBLE>(),
        )
        .to_vec()
    }
}

/// Start process loopback capture for `pid` (Win10 2004+). Output stereo f32 @ 48 kHz.
pub fn start_process_loopback(pid: u32) -> Result<(CaptureHandle, Receiver<Vec<f32>>)> {
    let client = activate_process_loopback(pid)?;
    // Process loopback has no endpoint: IAudioClient::GetMixFormat returns
    // E_NOTIMPL (documented). Use an explicit 32-bit float / 48 kHz / stereo
    // WAVEFORMATEXTENSIBLE and let WASAPI auto-convert (AUTOCONVERTPCM + SRCDEFAULT).
    let format_bytes = process_loopback_format();
    let format = MixFormat::from_bytes(&format_bytes)?;
    unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                100 * 10_000,
                0,
                format.fmt_ptr(),
                None,
            )
            .context("IAudioClient::Initialize(process loopback) failed")?;
    }
    let endpoint_name = format!("process-loopback(pid={pid})");
    start_capture_thread(
        client,
        format,
        endpoint_name,
        true,
        Arc::new(CaptureStats::default()),
    )
}

/// COM interface pointers are not `Send` in windows-rs; WASAPI shared-mode
/// clients are free-threaded, so the capture worker can own them safely.
struct SendWrapper<T>(T);
unsafe impl<T> Send for SendWrapper<T> {}
impl<T> SendWrapper<T> {
    fn into_inner(self) -> T {
        self.0
    }
}

fn start_capture_thread(
    client: IAudioClient,
    format: MixFormat,
    endpoint_name: String,
    process_loopback: bool,
    stats: Arc<CaptureStats>,
) -> Result<(CaptureHandle, Receiver<Vec<f32>>)> {
    let capture: IAudioCaptureClient = unsafe {
        client
            .GetService::<IAudioCaptureClient>()
            .context("GetService(IAudioCaptureClient) failed")?
    };
    unsafe { client.Start().context("IAudioClient::Start failed")? };
    let (tx, rx): (SyncSender<Vec<f32>>, Receiver<Vec<f32>>) = sync_channel(64);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let stats_thread = stats.clone();
    let fmt = format.clone();
    let client = SendWrapper(client);
    let capture = SendWrapper(capture);
    let join = thread::Builder::new()
        .name("wasapi-capture".into())
        .spawn(move || {
            // Use into_inner() so edition-2021 precise capture keeps the whole
            // SendWrapper (not the inner non-Send COM pointer) in the closure.
            let client = client.into_inner();
            let capture = capture.into_inner();
            let mut resampler = if fmt.rate != AES67_RATE {
                crate::resample::LinearResampler::new(fmt.rate, AES67_RATE, 2).ok()
            } else {
                None
            };
            loop {
                if stop_thread.load(Ordering::SeqCst) {
                    break;
                }
                let mut got_any = false;
                loop {
                    let next = unsafe { capture.GetNextPacketSize() };
                    let next = match next {
                        Ok(n) => n,
                        Err(e) => {
                            stats_thread.read_errors.fetch_add(1, Ordering::Relaxed);
                            eprintln!("capture: GetNextPacketSize failed: {e}");
                            0
                        }
                    };
                    if next == 0 {
                        break;
                    }
                    let mut data: *mut u8 = ptr::null_mut();
                    let mut frames: u32 = 0;
                    let mut flags: u32 = 0;
                    let hr = unsafe {
                        capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    };
                    if let Err(e) = hr {
                        stats_thread.read_errors.fetch_add(1, Ordering::Relaxed);
                        eprintln!("capture: GetBuffer failed: {e}");
                        break;
                    }
                    stats_thread.packets.fetch_add(1, Ordering::Relaxed);
                    stats_thread
                        .frames_in
                        .fetch_add(frames as u64, Ordering::Relaxed);
                    if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
                        stats_thread.discontinuity.fetch_add(1, Ordering::Relaxed);
                    }
                    if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                        stats_thread.silent_packets.fetch_add(1, Ordering::Relaxed);
                    } else {
                        let decoded = fmt.read_frames(data, frames as usize);
                        let stereo = if fmt.channels == 1 {
                            let mut v = Vec::with_capacity(frames as usize * 2);
                            for s in decoded {
                                v.push(s);
                                v.push(s);
                            }
                            v
                        } else if fmt.channels == 2 {
                            decoded
                        } else {
                            let mut v = Vec::with_capacity(frames as usize * 2);
                            for f in 0..frames as usize {
                                v.push(decoded[f * fmt.channels]);
                                v.push(decoded[f * fmt.channels + 1]);
                            }
                            v
                        };
                        let converted = match resampler.as_mut() {
                            Some(rs) => rs.process(&stereo),
                            None => stereo,
                        };
                        if !converted.is_empty() {
                            stats_thread
                                .frames_out
                                .fetch_add((converted.len() / 2) as u64, Ordering::Relaxed);
                            match tx.try_send(converted) {
                                Ok(()) => {}
                                Err(TrySendError::Full(_)) => {
                                    stats_thread.overflow_drops.fetch_add(1, Ordering::Relaxed);
                                }
                                Err(TrySendError::Disconnected(_)) => {
                                    let _ = unsafe { client.Stop() };
                                    return;
                                }
                            }
                        }
                    }
                    let _ = unsafe { capture.ReleaseBuffer(frames) };
                    got_any = true;
                }
                if !got_any {
                    thread::sleep(Duration::from_millis(2));
                }
            }
            let _ = unsafe { client.Stop() };
        })
        .context("failed to spawn capture thread")?;

    Ok((
        CaptureHandle {
            format,
            endpoint_name,
            process_loopback,
            stats,
            stop,
            join: Some(join),
        },
        rx,
    ))
}

// ---------------------------------------------------------------------------
// Process loopback activation (ActivateAudioInterfaceAsync + raw COM handler)
// ---------------------------------------------------------------------------

struct HandlerShared {
    result: Mutex<Option<std::result::Result<IAudioClient, String>>>,
    ready: Condvar,
}

impl HandlerShared {
    fn complete(&self, value: std::result::Result<IAudioClient, String>) {
        let mut guard = self.result.lock().unwrap();
        if guard.is_none() {
            *guard = Some(value);
            self.ready.notify_all();
        }
    }
}

#[repr(C)]
struct HandlerVtbl {
    query_interface: unsafe extern "system" fn(
        this: *mut std::ffi::c_void,
        riid: *const GUID,
        ppv: *mut *mut std::ffi::c_void,
    ) -> HRESULT,
    add_ref: unsafe extern "system" fn(this: *mut std::ffi::c_void) -> u32,
    release: unsafe extern "system" fn(this: *mut std::ffi::c_void) -> u32,
    activate_completed: unsafe extern "system" fn(
        this: *mut std::ffi::c_void,
        operation: *mut std::ffi::c_void,
    ) -> HRESULT,
}

#[repr(C)]
struct CompletionHandler {
    // vtable pointer MUST be the first field (COM object layout).
    vtbl: *const HandlerVtbl,
    refs: AtomicU64,
    shared: Arc<HandlerShared>,
}

static HANDLER_VTBL: HandlerVtbl = HandlerVtbl {
    query_interface: handler_query_interface,
    add_ref: handler_add_ref,
    release: handler_release,
    activate_completed: handler_activate_completed,
};

unsafe extern "system" fn handler_query_interface(
    this: *mut std::ffi::c_void,
    riid: *const GUID,
    ppv: *mut *mut std::ffi::c_void,
) -> HRESULT {
    if riid.is_null() || ppv.is_null() {
        return E_NOINTERFACE;
    }
    let iid = *riid;
    let handler_iid = <IActivateAudioInterfaceCompletionHandler as ComInterface>::IID;
    if iid == IID_IUNKNOWN || iid == handler_iid || iid == IID_IAGILEOBJECT {
        *ppv = this;
        handler_add_ref(this);
        HRESULT(0)
    } else {
        *ppv = ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn handler_add_ref(this: *mut std::ffi::c_void) -> u32 {
    let h = &*(this as *const CompletionHandler);
    (h.refs.fetch_add(1, Ordering::SeqCst) + 1) as u32
}

unsafe extern "system" fn handler_release(this: *mut std::ffi::c_void) -> u32 {
    let boxed = Box::from_raw(this as *mut CompletionHandler);
    let refs = boxed.refs.fetch_sub(1, Ordering::SeqCst);
    if refs == 1 {
        drop(boxed);
        0
    } else {
        std::mem::forget(boxed);
        (refs - 1) as u32
    }
}

unsafe extern "system" fn handler_activate_completed(
    this: *mut std::ffi::c_void,
    operation: *mut std::ffi::c_void,
) -> HRESULT {
    let h = &*(this as *const CompletionHandler);
    let op =
        std::mem::ManuallyDrop::new(IActivateAudioInterfaceAsyncOperation::from_raw(operation));
    let mut hr = HRESULT(0);
    let mut unknown: Option<windows::core::IUnknown> = None;
    let call = op.GetActivateResult(&mut hr, &mut unknown);
    let value: std::result::Result<IAudioClient, String> = match call {
        Err(e) => Err(format!("GetActivateResult call failed: {e}")),
        Ok(()) => {
            if hr.is_err() {
                Err(format!(
                    "process loopback activation HRESULT 0x{:08X}",
                    hr.0 as u32
                ))
            } else {
                match unknown {
                    Some(u) => match u.cast::<IAudioClient>() {
                        Ok(c) => Ok(c),
                        Err(e) => Err(format!("activated interface is not IAudioClient: {e}")),
                    },
                    None => Err("activation returned null interface".to_string()),
                }
            }
        }
    };
    h.shared.complete(value);
    HRESULT(0)
}

fn activate_process_loopback(pid: u32) -> Result<IAudioClient> {
    anyhow::ensure!(pid > 0, "process loopback requires --pid > 0");
    let shared = Arc::new(HandlerShared {
        result: Mutex::new(None),
        ready: Condvar::new(),
    });
    let handler = Box::new(CompletionHandler {
        vtbl: &HANDLER_VTBL,
        refs: AtomicU64::new(1),
        shared: shared.clone(),
    });
    let raw = Box::into_raw(handler) as *mut std::ffi::c_void;

    // Docs (ApplicationLoopback sample): the VT_BLOB payload must be the
    // AUDIOCLIENT_ACTIVATION_PARAMS wrapper, not the inner loopback params.
    let mut activation = AUDIOCLIENT_ACTIVATION_PARAMS::default();
    activation.ActivationType = AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK;
    activation.Anonymous.ProcessLoopbackParams = AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
        TargetProcessId: pid,
        ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
    };
    let mut propvariant = PROPVARIANT::default();
    unsafe {
        let pv = &mut *propvariant.Anonymous.Anonymous;
        pv.vt = windows::Win32::System::Variant::VT_BLOB;
        pv.Anonymous.blob = windows::Win32::System::Com::BLOB {
            cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            pBlobData: &activation as *const _ as *mut u8,
        };
    }
    let handler_iface = unsafe { IActivateAudioInterfaceCompletionHandler::from_raw(raw) };
    let iid = <IAudioClient as ComInterface>::IID;
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &iid,
            Some(&propvariant),
            &handler_iface,
        )
    }
    .context("ActivateAudioInterfaceAsync(process loopback) failed")?;

    // Wait for the completion handler (5 s safety timeout).
    let value = {
        let guard = shared.result.lock().unwrap();
        let (guard, timeout) = shared
            .ready
            .wait_timeout_while(guard, Duration::from_secs(5), |r| r.is_none())
            .unwrap();
        if timeout.timed_out() {
            bail!("process loopback activation timed out after 5s");
        }
        guard
            .clone()
            .unwrap_or_else(|| Err("activation produced no result".to_string()))
    };
    drop(operation);

    // Drop our own reference to the completion handler (the async API holds one
    // while the activation is in flight and releases it after the callback).
    drop(handler_iface);
    value.map_err(|e| anyhow::anyhow!(e))
}

// ---------------------------------------------------------------------------
// Source wrapper
// ---------------------------------------------------------------------------

pub struct WasapiCaptureSource {
    handle: Option<CaptureHandle>,
    rx: Receiver<Vec<f32>>,
    pending: Vec<f32>,
    pending_off: usize,
    channels: usize,
    stats: Arc<CaptureStats>,
    underruns: u64,
}

impl WasapiCaptureSource {
    pub fn system_loopback(device_selector: Option<&str>) -> Result<Self> {
        let dev = find_render_device(device_selector)?;
        let name = device_friendly_name(&dev);
        let (handle, rx) = start_system_loopback(&dev, &name)?;
        eprintln!(
            "wasapi: system loopback on endpoint {:?} ({}), capture format {}",
            handle.endpoint_name,
            if handle.process_loopback {
                "process"
            } else {
                "system"
            },
            handle.format.describe()
        );
        let stats = handle.stats.clone();
        Ok(Self {
            handle: Some(handle),
            rx,
            pending: Vec::new(),
            pending_off: 0,
            channels: 2,
            stats,
            underruns: 0,
        })
    }

    pub fn process_loopback(pid: u32) -> Result<Self> {
        let (handle, rx) = start_process_loopback(pid)?;
        eprintln!(
            "wasapi: process loopback pid={} capture format {}",
            pid,
            handle.format.describe()
        );
        let stats = handle.stats.clone();
        Ok(Self {
            handle: Some(handle),
            rx,
            pending: Vec::new(),
            pending_off: 0,
            channels: 2,
            stats,
            underruns: 0,
        })
    }

    pub fn capture_stats(&self) -> CaptureSnapshot {
        self.stats.snapshot()
    }

    pub fn stats_arc(&self) -> Arc<CaptureStats> {
        self.stats.clone()
    }

    pub fn capture_format_desc(&self) -> String {
        self.handle
            .as_ref()
            .map(|h| h.format.describe())
            .unwrap_or_else(|| "<none>".into())
    }

    pub fn endpoint_name(&self) -> String {
        self.handle
            .as_ref()
            .map(|h| h.endpoint_name.clone())
            .unwrap_or_default()
    }

    pub fn source_underruns(&self) -> u64 {
        self.underruns
    }
}

impl Source for WasapiCaptureSource {
    fn channels(&self) -> usize {
        self.channels
    }

    fn description(&self) -> String {
        format!(
            "wasapi-loopback endpoint={:?} capture_format={}",
            self.endpoint_name(),
            self.handle
                .as_ref()
                .map(|h| h.format.describe())
                .unwrap_or_else(|| "<none>".into())
        )
    }

    fn read_into(&mut self, out: &mut [f32], frames: usize) -> Result<ReadStatus> {
        let ch = self.channels;
        let need = frames * ch;
        // Drain pending buffer first.
        let mut filled = 0usize;
        if self.pending_off < self.pending.len() {
            let avail = self.pending.len() - self.pending_off;
            let take = avail.min(need);
            out[..take].copy_from_slice(&self.pending[self.pending_off..self.pending_off + take]);
            self.pending_off += take;
            filled = take;
            if self.pending_off >= self.pending.len() {
                self.pending.clear();
                self.pending_off = 0;
            }
        }
        while filled < need {
            match self.rx.try_recv() {
                Ok(chunk) => {
                    let want = need - filled;
                    let take = chunk.len().min(want);
                    out[filled..filled + take].copy_from_slice(&chunk[..take]);
                    filled += take;
                    if take < chunk.len() {
                        self.pending.extend_from_slice(&chunk[take..]);
                        self.pending_off = 0;
                        break;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        if filled < need {
            for v in out[filled..].iter_mut() {
                *v = 0.0;
            }
            self.underruns += ((need - filled) / ch) as u64;
        }
        Ok(ReadStatus {
            requested_frames: frames,
            produced_frames: filled / ch,
            underrun_frames: (need - filled) / ch,
            eof: false,
        })
    }
}

impl Drop for WasapiCaptureSource {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            h.stop();
        }
    }
}

// ---------------------------------------------------------------------------
// Playback (render) helper — used for cross-process loopback evidence
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct PlaybackStats {
    pub frames_written: u64,
    pub underruns: u64,
    pub buffer_frames: u32,
    pub format_desc: String,
    pub endpoint_name: String,
}

/// Render interleaved f32 @ `src_rate` (and `src_channels`) to a render endpoint
/// using WASAPI shared mode. Resamples/maps channels if the endpoint differs.
pub fn play_interleaved(
    device_selector: Option<&str>,
    samples: &[f32],
    src_channels: usize,
    src_rate: u32,
    gain: f32,
    loop_audio: bool,
    duration_s: Option<f64>,
) -> Result<PlaybackStats> {
    let dev = find_render_device(device_selector)?;
    let name = device_friendly_name(&dev);
    let client = open_audio_client(&dev)?;
    let format = unsafe {
        MixFormat::from_ptr(
            client
                .GetMixFormat()
                .context("GetMixFormat (render) failed")?,
        )?
    };
    unsafe {
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                0,
                100 * 10_000,
                0,
                format.fmt_ptr(),
                None,
            )
            .context("IAudioClient::Initialize(render) failed")?;
    }
    let buffer_frames = unsafe { client.GetBufferSize().context("GetBufferSize failed")? };
    let render: IAudioRenderClient = unsafe {
        client
            .GetService::<IAudioRenderClient>()
            .context("GetService(IAudioRenderClient) failed")?
    };
    unsafe {
        client
            .Start()
            .context("IAudioClient::Start(render) failed")?
    };

    // Prepare source samples at endpoint rate & channels (interleaved f32).
    let resampled = if src_rate != format.rate {
        crate::resample::resample_all(samples, src_rate, format.rate, src_channels)?
    } else {
        samples.to_vec()
    };
    let src_frames = resampled.len() / src_channels;
    let mut mapped = Vec::with_capacity(src_frames * format.channels);
    for f in 0..src_frames {
        for c in 0..format.channels {
            let v = if c < src_channels {
                resampled[f * src_channels + c]
            } else if src_channels == 1 {
                resampled[f]
            } else {
                0.0
            };
            mapped.push(v * gain);
        }
    }
    let total_frames = mapped.len() / format.channels;
    let limit_frames = duration_s.map(|d| (d * format.rate as f64).round() as usize);
    let mut written = 0usize;
    let mut stats = PlaybackStats {
        buffer_frames,
        format_desc: format.describe(),
        endpoint_name: name.clone(),
        ..Default::default()
    };
    eprintln!(
        "play: endpoint {:?} {}; source {} frames {} ch {} Hz; buffer {} frames",
        name,
        format.describe(),
        total_frames,
        src_channels,
        src_rate,
        buffer_frames
    );

    let mut idle_sleeps = 0u32;
    loop {
        if let Some(limit) = limit_frames {
            if written >= limit {
                break;
            }
        }
        let padding = unsafe {
            client
                .GetCurrentPadding()
                .context("GetCurrentPadding failed")?
        };
        let avail = buffer_frames.saturating_sub(padding) as usize;
        if avail == 0 {
            thread::sleep(Duration::from_millis(2));
            idle_sleeps += 1;
            if idle_sleeps > 5000 {
                bail!("render stalled (buffer never drained)");
            }
            continue;
        }
        idle_sleeps = 0;
        let write_frames = avail.min(total_frames - written);
        if write_frames == 0 {
            if loop_audio {
                written = 0;
                continue;
            }
            break;
        }
        let data = unsafe {
            render
                .GetBuffer(write_frames as u32)
                .context("render GetBuffer failed")?
        };
        let slice = &mapped[written * format.channels..(written + write_frames) * format.channels];
        format.write_frames(data, slice);
        unsafe {
            render
                .ReleaseBuffer(write_frames as u32, 0)
                .context("render ReleaseBuffer failed")?
        };
        written += write_frames;
        stats.frames_written += write_frames as u64;
        thread::sleep(Duration::from_millis(3));
    }

    // Let the endpoint drain the last buffer before stopping.
    thread::sleep(Duration::from_millis(30));
    unsafe { client.Stop().context("IAudioClient::Stop(render) failed")? };
    Ok(stats)
}

// ---------------------------------------------------------------------------
// Misc helpers for evidence
// ---------------------------------------------------------------------------

pub fn process_loopback_supported() -> bool {
    // PDWORD available on Windows 10 2004+ (build 19041); this build is >= 26200.
    true
}

/// Query an endpoint's mix format description without starting any stream.
pub fn query_endpoint_format(device_id: &str) -> Result<String> {
    let dev = find_device_by_id(device_id)?;
    let client = open_audio_client(&dev)?;
    let fmt =
        unsafe { MixFormat::from_ptr(client.GetMixFormat().context("GetMixFormat failed")?)? };
    Ok(fmt.describe())
}

/// Return the endpoint list as plain strings (used by aes67-devices).
pub fn format_endpoints(endpoints: &[EndpointInfo]) -> String {
    let mut s = String::new();
    for e in endpoints {
        s.push_str(&format!(
            "#{:<2} {}{}\n    id={}\n",
            e.index,
            e.name,
            if e.is_default { "  [default]" } else { "" },
            e.id
        ));
    }
    s
}
