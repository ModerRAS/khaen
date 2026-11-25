# decay-fit - offline segmented / double-exponential EDC analysis

Pure offline analysis tool. **No playback, no sound card, no apply.** It answers one
question that the frozen single-slope T20 cannot answer: *is this band's decay a
single slope, or two (or three) stages with a knee, and if so, how stable is each
stage across repeats?*

It exists because two bands of the 2026-11-21 calibration chain are reproducibly
non-single-slope:

* **125 Hz** - T20 R2 = 0.841..0.863 < 0.90 in 6/6 repeats, because the 1/3-oct EDC
  is a systematic double slope (-5..-15 dB ~ 0.21 s, -15..-25 dB ~ 0.40 s);
* **200 Hz** - the -25 dB end point lands on a ~40 ms EDC plateau/knee, so the
  single-slope T20 spread across repeats is 15% even though the -5..-15 dB segment
  is repeatable to 1.3%.

**This tool does NOT redefine T20.** The standard T20 (frozen -5..-25 dB fit) is
always computed and reported as the control. T20_early / T20_late / knee_ms are
*non-standard segment descriptors*: T20_seg = -60/slope, i.e. the same dB/s rule as
T20 but applied to one piecewise segment.

## Models

All fits use the same -5..-25 dB EDC window as T20 (coverage and >= 10 dB noise
margin rules identical to the frozen chain). Continuity at the knee(s) is enforced,
so a 2-segment fit has only 4 free parameters (level, slope1, slope2, knee time).

| model | form | params |
| --- | --- | --- |
| SINGLE | y = b + s1*u | 2 |
| TWO | y = b + s1*u + d2*max(u-tb1,0) | 3 + knee |
| THREE | y = b + s1*u + d2*max(u-tb1,0) + d3*max(u-tb2,0) | 4 + 2 knees |
| BIEXP | E(t) = C + 10*log10(A1*exp(-t/tau1) + A2*exp(-t/tau2)) | 4 |

(u = t - window start; breaks are grid-searched with an exact linear solve per
candidate, then locally refined.)

### Automatic model selection (pre-registered, no cherry picking)

The segment count is chosen by code, not by the analyst:

1. every segment must span >= 4 dB, last >= 8 ms and have >= 12 samples;
2. the richer model needs either
   * BIC(effective n) improvement >= 6, or
   * delta R2 >= 0.05 (EDC residuals are strongly correlated, so BIC alone is
     conservative - the effective sample size is estimated from the residual
     autocorrelation time);
3. **identifiability** (all required):
   * adjacent T20 ratio >= 1.5,
   * slope-change t-statistic >= 3 (formal, from the least-squares covariance),
   * knee-time profile-likelihood 95% CI width <= max(25 ms, 20% of the window),
   * knee not at the window edge;
4. if the evidence is weak, or a knee is suggested but not identifiable, the
   result is `AMBIGUOUS` (reported, never counted as a pass);
5. the double exponential is always reported, but never used to force a segment
   count; it is `UNIDENTIFIABLE` when tau2/tau1 < 1.6, a weight < 0.05 or a tau
   relative standard error > 60%.

## Opt-in segmented acceptance gate

`decayfit.gates.segmented_gate` implements a gate *parallel* to the frozen P3 gate
for multi-stage bands. It requires, over the repeats of one condition:

* the SAME segment count selected in every repeat (else `AMBIGUOUS`);
* per segment: R2_seg >= 0.95 in every repeat;
* per segment: T20_seg repeat range <= 0.05 s (median < 0.2 s) or relative <= 10%
  and absolute <= 0.15 s (longer segments);
* knee time repeat range <= max(25 ms, 15% of the median knee time).

Statuses: `PASS_SEGMENTED`, `FAIL_SEGMENTED`, `AMBIGUOUS`, `SINGLE_SLOPE`,
`NO_COVERAGE`. The standard T20 gate (`standard_t20_gate`) is always reported
alongside as the control. **`PASS_SEGMENTED` is not a T20 pass and must never be
quoted as one.**

## Opt-in design-weight / target proposal

`decayfit.objective` produces three alpha_k variants for the mimo-objective-v1
band weights (all normalised so sum(alpha) = K, same convention as the spec):

* `alpha_energy` - the v1.1 default (energy only); included as the control;
* `alpha_severity` - energy x clamp(T20_late/median(T20_late), 0.5, 2.0), as
  requested ("longer late T20 -> higher weight"); affects residual allocation only,
  not room physics;
* `alpha_recommended` - energy x actionability (actionability = clamp(1 - A2_biexp,
  0.2, 1.0)), which avoids spending effort on late / room-pole-dominated energy that
  an open-loop FIR cannot change (spec v1.2 section 11).

The per-band `param-suggestions.json` also carries `target_residual_ratio`,
`prefer_early_energy_cut` (cut-only PEQ for the initial energy) and `give_up_late`
(measured with the one-condition S2 PEQ experiment in the same data set). It is
advisory input for a future merge/fit/experiment-G step. Nothing in
mimo-objective-v1 v1.1 changes by default.

## Usage

    py -3 -m decayfit.selftest          # synthetic ground-truth self-test (19 checks)

    py -3 run_decay_fit.py \
        --run-dir <dir with RAW_r1.raw ... S2_r3.raw> \
        --tags RAW_r1,S2_r1,RAW_r2,S2_r2,RAW_r3,S2_r3 \
        --out <evidence dir> \
        --validate <prior analysis.json>      # optional frozen-T20 control check

Outputs: `results.json` (machine readable), `csv/per_repeat.csv`,
`csv/bands.csv`, `csv/model_selection.csv`, `csv/objective_weights.csv`,
`param-suggestions.json`, `figures/*.png`, `run-manifest.json` (code + capture
sha256, parameters).

The capture format is the same as ir-diag: 2ch interleaved S24_3LE @ 48 kHz UMIK,
and the excitation is the 30 s exponential sweep of the dr-upgrade window. The
deconvolution reuses `tools/ir-diag/irdiag` read-only with the single common clock
stretch measured for that device pair (eps = -60.4 ppm).

## Optional integration with the frozen ir-diag pipeline (default OFF)

`tools/ir-diag/irdiag/pipeline.py` gained an optional `decay_fit=None` keyword and
`run_ir_diag.py` a `--decay-fit` flag. When (and only when) enabled, each band's
result JSON gains a `channels[*].bands[*].decay_fit` block with the segmented
description and segmented gate. **Default behaviour is unchanged**: no import, no
extra computation, identical JSON apart from the deliberately added block.

## Honest boundaries

* offline only; the 6 captures were measured earlier in the 2026-11-21 window;
* single point, single microphone (UMIK-1 listening position);
* segmented metrics describe the measured decay; they are not T20;
* a double exponential is only a description of the EDC shape - poles in the room
  are not changed by this tool or by an open-loop FIR;
* no claim about whether an FIR can shorten the late segment (it cannot change the
  room pole; it can lower the initial energy).

## Evidence

`rt-stack/evidence/calib-chain-20261121/decay-fit-20261121/` (REPORT.md,
NOT_RUN.md, results/CSVs/figures, run-manifest.json).


---

## Update 2026-11-21 (after the first fit round)

Two corrections were needed once real data was fitted; both are implemented:

1. **Knee-detection window is extended.** The first version fitted the piecewise
   models on the same -5..-25 dB window as T20. That cannot see the 200 Hz knee,
   which sits at (or just beyond) the -25 dB end point. The model-selection window
   is now -5 dB .. -30 dB, capped by the same >= 10 dB noise-margin rule. The frozen
   -5..-25 dB single-slope control (and the canonical fixed split) are unchanged and
   still reproduce `dr-upgrade-20261121/analysis.json` exactly (max |dT20| = 0).
2. **Canonical fixed split is reported next to the auto model.** -5..-15 dB (early)
   and -15..-25 dB (late) linear segments are always reported with their own R2 and
   repeat statistics; these are the T20_early/T20_late numbers quoted in the report
   when comparability with the earlier p36 analysis matters. The auto model's
   segments (free knee) remain the model-selection layer.

Gate statuses therefore include `PASS_FIXED_TWO` and `PASS_EARLY_ONLY` in
addition to `PASS_SEGMENTED`; see `decayfit/gates.py`.
