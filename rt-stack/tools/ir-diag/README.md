# ir-diag - G0 RAW time-domain baseline (IR / EDC / T20 / waterfall)

Pure offline analysis of a **single-point** KHAEN measurement.  No playback, no device I/O,
no apply.  It answers one question only: *what does the RAW (P_post bypass) low-frequency
decay look like* - and it is the frozen reference every later G1/G2/G3 measurement must be
compared against with the **same code + same parameter hash**.

## What it does

1. rebuilds the excitation **bit-exactly** from `calibrate/src/sweep.rs:28-55` and proves it
   against the bytes actually played (`composite.raw`, error < 1 LSB S24);
2. deconvolves the whole capture with a Farina inverse filter
   (`calibrate/src/dsp.rs:66-92`), with the inverse filter built at the measured stretched
   length to remove the ~42 ppm capture/playback clock offset;
3. locates all 21 IR pulses (7 channels x 3 repeats) inside the deconvolved stream and cuts
   each IR from its direct-sound peak, window `min(5.0 s, next pulse - 0.05 s)`;
4. band-splits the IR (1/3 oct, 40..200 Hz), computes a noise-subtracted Schroeder EDC and a
   T20 fit (-5..-25 dB, R2 + dynamic range + coverage reported, never extrapolated);
5. applies the reliability gates of `rt-stack/spec/mimo-single-point-diag-v1.md` (v1.2):
   per-repeat T20 / EDC@100ms / IR cross-correlation, low-frequency dynamic-range and
   noise-margin gates;
6. writes `raw-baseline.json`, CSVs, figures, `REPORT.md` and a frozen
   `ANALYSIS_MANIFEST.json` (parameter hash + code hash + capture hash).

## Usage

```
py -3 run_ir_diag.py \
    --capture <capture.raw> \
    --meta    <dir with *.diag.json / *.repeats.csv / session.diag.json> \
    --out     <evidence dir> \
    --composite-dir <dir with composite slices, optional>
```

The composite slices used to prove the excitation (they are only 1 s each, generated on the
target with `dd` - see the evidence README):

```
dd if=composite.raw of=fl_ch0.bin    bs=66 skip=96000   count=48000
dd if=composite.raw of=sub_ch7.bin   bs=66 skip=4819200 count=48000
dd if=composite.raw of=fl_ch0_s6.bin bs=66 skip=4819200 count=48000
```

(66 bytes = 22 ch x 3 bytes S24_3LE; `skip` counts frames.)

## Frozen analysis parameters

See `ANALYSIS_MANIFEST.json` in the output directory.  Changing anything in
`irdiag/metrics.py` invalidates every previous baseline (spec v1.2 section 1.3); the manifest stores
the SHA-256 of every code file so a mismatch is detectable.

## Known limits of this capture (read before trusting any number)

* repeat gap is only 0.2 s and the slot gap 0.8 s, so an IR can only be followed for ~5 s
  (the Farina IR sits at the end of the 5 s sweep; the next repeat's pulse arrives 5.2 s later);
* the usable EDC dynamic range of this RAW capture is **~20-40 dB** (limited by the residual
  of the *next* sweep in the same recording + ambient noise), not the ~90 dB the
  deconvolution kernel itself could deliver;
* **SUB1 was driven 20-130 Hz**, not 7-130 Hz: `--band-limit 7:130` means *channel 7, upper
  edge 130 Hz* (calibrate/src/main.rs:2360-2393), the sweep start stayed at the global 20 Hz;
* IR pulse positions cannot be converted into absolute acoustic delays: there is no loopback
  reference in this run, so the constant playback-path latency is unknown.  Only the
  *relative* delays between channels are acoustically meaningful.

## Results / evidence

```
rt-stack/evidence/ir-diag-20261121/
```

G1 (PEQ-only), G2 (MIMO-only), G3 (MIMO+Reg) and the S0-S3 apply ladder are **NOT_RUN**:
they need an apply, which this task explicitly excludes (and the host-side P_post consumer is
still not wired - spec section 0).
