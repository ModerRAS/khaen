# SPEC PROPOSAL - two-stage / segmented decay description (NON-STANDARD, opt-in)

> Status: **PROPOSAL, NOT RATIFIED, DEFAULT OFF.** Date 2026-11-21. Author: decay-fit
> (subagent), for the 125 / 200 Hz gate problem in the calib-chain-20261121 window.
> This document does not modify any existing spec; it proposes an *additional*
> description and an *opt-in* gate. Until the user/lead ratifies it, every number
> produced with it must be marked "NON-STANDARD".

## 0. Why

Frozen spec v1.2 P3 requires one T20 per band with fit R2 >= 0.90 and a repeat-range
gate. Two bands of the measured chain are reproducibly not single-slope:

* 125 Hz: T20 R2 = 0.841..0.863 (6/6 repeats); EDC is a systematic double slope
  (-5..-15 dB ~ 0.21 s, -15..-25 dB ~ 0.40 s);
* 200 Hz: the -25 dB end point lands on a ~40 ms EDC plateau; the -5..-15 dB segment
  is repeatable to 1.3% but the single-slope T20 spread is 15%.

Neither band is a bad measurement. A single slope is simply the wrong model for
them. The frozen T20 definition, its gate and its P6 certifiability are therefore
left untouched; the proposal adds a second, explicitly non-standard description.

## 1. Metric names (all explicitly non-standard)

Two layers are reported for every band/repeat:

A. **canonical fixed split** (deterministic, directly comparable across runs):

| name | definition |
| --- | --- |
| `T20_m5_m15_s` (early) | linear fit of EDC over -5..-15 dB -> -60/slope |
| `T20_m15_m25_s` (late) | linear fit of EDC over -15..-25 dB -> -60/slope |
| `r2_fixed_early` / `r2_fixed_late` | fit R2 of each fixed segment |
| `t20_ratio_late_over_early` | ratio; >= 1.5 means two-slope evidence |

B. **automatic piecewise model** (segment count selected by code):

| name | definition |
| --- | --- |
| `n_seg` | selected piecewise model: 1 / 2 / 3 |
| `T20_early_s` | -60 / slope of the first auto segment |
| `T20_late_s` | -60 / slope of the last auto segment |
| `knee_ms` (`knee1_ms`, `knee2_ms`) | knee time(s) relative to the broadband direct peak |
| `knee_level_db` | EDC level at the knee |
| `seg_r2` | per-segment fit R2 |
| `curv_knee_ms` | knee from max |curvature| of the EDC (independent of the model) |
| `decay_model` | SINGLE / TWO / THREE / AMBIGUOUS / NO_COVERAGE |
| `decay_gate` | gate status (see section 3) |
| `tau1_s` `tau2_s` `a1` `a2` | double-exponential description, only when identifiable |

Mandatory label in every report/JSON: **"non-standard segmented decay; NOT T20"**.
The same record keeps the standard `T20_s` and `r2` (frozen fit) side by side.

Window rule: the frozen single-slope control and the canonical fixed split use the
frozen -5..-25 dB window. The automatic model uses an **extended knee-detection
window** (-5 dB .. -30 dB, but never deeper than -10 dB above the noise floor),
because a knee can sit at the -25 dB T20 end point (200 Hz) and would be invisible
inside the T20 window.

## 2. Automatic model selection (as implemented)

Segment count chosen by: segment span >= 4 dB, duration >= 8 ms, BIC(effective n)
improvement >= 6 OR delta R2 >= 0.05, plus identifiability (adjacent T20 ratio
>= 1.5, slope-change t >= 3, knee profile-likelihood CI <= max(25 ms, 20% of the
window), knee not at the window edge). Otherwise SINGLE_SLOPE or AMBIGUOUS. Details:
`tools/decay-fit/README.md`.

## 3. Proposed opt-in gate "P3-SEG" (parallel to P3, never replacing it)

For the repeats of one condition (>= 3):

| status | meaning |
| --- | --- |
| `PASS_SEGMENTED` | same auto segment count in all repeats + every segment R2 >= 0.95 + segment T20 repeat stability + knee repeat stability |
| `PASS_FIXED_TWO` | canonical early AND late segments both pass R2/stability AND T20 ratio >= 1.5 (real two-slope) |
| `PASS_EARLY_ONLY` | canonical early segment passes, late segment does not, but the max-curvature knee is repeatable and the local slope drops (knee descriptor instead of a late T20) |
| `FAIL_SEGMENTED` / `FAIL_FIXED` | a required check fails |
| `AMBIGUOUS` | segment count not identical across repeats, or model not identifiable - never a pass |
| `SINGLE_SLOPE` | fixed segments are consistent with one slope; use the frozen T20 gate |
| `NO_COVERAGE` | -25 dB point not >= 10 dB above the noise floor |

Repeat-stability thresholds: per segment, median < 0.2 s -> absolute range <= 0.05 s;
else relative <= 10% and absolute <= 0.15 s. Knee time range <= max(25 ms, 15% of
the median knee). A `PASS_*` here must never be quoted as "T20 passed"; it only
says the *segmented description* is reproducible.

## 4. P6 impact

P6 may report `CERTIFIABLE_SEGMENTED` for a band when P3-SEG passes and the frozen
P2 (DR / noise margin) passes. Both conditions must be shown. The frozen P6
`CERTIFIABLE` (single T20) keeps its own unchanged definition. For a
`PASS_EARLY_ONLY` band, P6 may only say "early segment + knee position
certifiable; late T20 not certifiable".

## 5. Optional design weight (mimo-objective-v1 extension, default OFF)

`alpha_severity` (the requested variant):
`alpha_k = (a_k * s_k) * K / sum_j(a_j * s_j)` with
`s_k = clamp(T20_late_k / median(T20_late), 0.5, 2.0)` and `a_k` the v1.1 clamped
energy weight. `alpha_recommended` replaces `s_k` by an actionability factor
`clamp(early/late, 0.2, 1.0)` (or `1 - A2_biexp` when the double exponential is
identifiable) so that late / room-pole-dominated energy is not weighted up. Both
only re-allocate residual across bands:

* they do not change any measured decay, T20, or room physics;
* they must be reported as "energy x decay severity (non-standard weight)";
* the v1.1 energy-only weights remain the default and must be reported alongside.

Per-band executable output lives in `param-suggestions.json` (segment count,
target residual ratio, prefer-early-energy cut-only PEQ, give-up-late flag); it is
advisory input for a future merge/fit/experiment-G step.

## 6. Provenance requirements

Any P3-SEG number must carry: tool version, code sha256 (run-manifest.json), the
model-selection constants, the noise-floor estimator (plateau vs frozen), the fit
windows, and the segment count votes per repeat. Without those it is not reportable.

## 7. Freeze / default-off rule

Nothing here is active by default:

* ir-diag runs the frozen chain unless `--decay-fit` is given;
* decay-fit never edits the frozen `irdiag/metrics.py` constants;
* mimo-objective-v1 v1.1 weights stay energy-only unless a caller explicitly
  selects `alpha_severity` / `alpha_recommended`.
