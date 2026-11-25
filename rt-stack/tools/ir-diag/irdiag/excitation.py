"""Excitation reconstruction for TSPI G0 RAW baseline.

The waveform is rebuilt bit-exactly from the same code path that generated the
played composite, then verified against slices of the byte-identical
composite.raw captured on the target (see tools/ir-diag/README.md).

Sources of the formula (line numbers as of rt-stack HEAD 2026-11-21):

* build: C:/WorkSpace/Claw/projects/tspi-audio-dsp/calibrate/src/sweep.rs:28-55
      (SweepSpec::generate -- this is the binary that measure-multi used)
* same formula in the RT crate (for reference / future stages):
      rt-stack/crates/calibration/src/sweep.rs:1606-1630 (sample_at) and
      :1737-1759 (segment_envelope); level: :773-778 (peak_linear = 10^(dBFS/20))
* band limit: calibrate/src/main.rs:2381-2393 (src_for: end_hz = min(chan_hz, g_end),
    start_hz keeps the global --start-hz) -> "--band-limit 7:130" means
    *channel 7*, upper band edge 130 Hz, lower edge still the global 20 Hz.
* inverse filter: calibrate/src/dsp.rs:66-92 (farina_inverse_filter)
"""
import numpy as np
from scipy.signal import resample

FS = 48000

# Frozen excitation parameters of the t4b capture (2026-11-21 09:16..09:19).
SWEEP_SECONDS = 5.0
LEVEL_DBFS = -25.0
FADE_MS = 10.0
F0_HZ = 20.0
# channels 0..5 got the global 20 kHz end; SUB1 (ch7) was band limited to 130 Hz.
F1_FULL_HZ = 20000.0
F1_SUB_HZ = 130.0

CHANNELS = [
    # (dac_channel, name, slot_index, f1_hz)
    (0, "FL", 0, F1_FULL_HZ),
    (1, "FR", 1, F1_FULL_HZ),
    (2, "SL", 2, F1_FULL_HZ),
    (3, "SR", 3, F1_FULL_HZ),
    (4, "HFL", 4, F1_FULL_HZ),
    (5, "HFR", 5, F1_FULL_HZ),
    (7, "SUB1", 6, F1_SUB_HZ),  # band limited: 20..130 Hz excitation
]

ANALYSIS_BANDS_HZ = [40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0]


def gen_sweep(f0=F0_HZ, f1=F1_FULL_HZ, seconds=SWEEP_SECONDS, level_db=LEVEL_DBFS,
              fade_ms=FADE_MS, rate=FS):
    """Exponential (Farina) sweep, identical to calibrate/src/sweep.rs:28-55.

    x[i] = A * sin( 2*pi*f0*T/ln(f1/f0) * ((f1/f0)^(t/T) - 1) ) * w[i]
    A = 10^(level_db/20)      (level_db is the *peak* amplitude in dBFS)
    w  = half-cosine fade-in over fade samples, mirrored fade-out.
    """
    n = int(round(seconds * rate))
    t_len = n / rate
    k = np.log(f1 / f0)
    amp = 10.0 ** (level_db / 20.0)
    fade = int(round(fade_ms / 1000.0 * rate))
    i = np.arange(n, dtype=np.float64)
    t = i / rate
    phase = 2.0 * np.pi * f0 * t_len / k * (np.power(f1 / f0, t / t_len) - 1.0)
    w = np.ones(n)
    w[:fade] = 0.5 - 0.5 * np.cos(np.pi * i[:fade] / fade)
    tail = np.where(i + fade >= n)[0]
    if tail.size:
        w[tail] = np.minimum(w[tail], 0.5 - 0.5 * np.cos(np.pi * (n - 1 - i[tail]) / fade))
    return amp * np.sin(phase) * w


def stretched_source(f0, f1, length, seconds=SWEEP_SECONDS):
    """Rebuild the sweep at a non-nominal length (capture clock vs playback clock).

    The capture device (UMIK) runs ~42 ppm fast relative to the playback clock,
    so the sweep as recorded occupies (1 - 42e-6) * 240000 samples.  Building the
    inverse filter at that length removes the drift-induced phase mismatch.  FFT
    resampling keeps the 20 Hz..f1 trajectory exactly, it only changes the sample
    count / duration.
    """
    src = gen_sweep(f0=f0, f1=f1, seconds=seconds)
    if int(length) == len(src):
        return src
    return resample(src, int(length))


def farina_inverse_filter(src, rate=FS, f0=F0_HZ, f1=F1_FULL_HZ):
    """Farina inverse filter, identical to calibrate/src/dsp.rs:66-92.

    inv[i] = src[n-1-i] * exp(decay*(T - t)),  decay = ln(f1/f0)/T
    then normalised to unit energy.
    """
    n = len(src)
    t_len = n / rate
    decay = np.log(max(f1 / f0, 1.0001)) / max(t_len, 1e-9)
    i = np.arange(n, dtype=np.int64)
    t = i / rate
    inv = src[n - 1 - i] * np.exp(decay * (t_len - t))
    e = np.sqrt(np.sum(inv * inv))
    return inv / e if e > 0 else inv
