# khaen

A real-time audio DSP engine that plays a **compiled correction plan**, with AES67/PTP I/O, speaker calibration, and a web console.

> Status: early preview. The engine runs on Linux with ALSA, and every stage also runs against a fake backend for offline tests.

## Why another DSP engine

Most room-correction stacks recompile a graph at runtime and hope the swap is smooth. khaen treats a correction as an **artifact**: calibration measures, designs are solved offline, coefficients are packed into one plan file, and the plan is verified and then swapped in **at a block boundary** with explicit epoch semantics. If anything is inconsistent, the host refuses to start rather than playing something approximately right.

## What is in the box

| Component | Crate | What it does |
| --- | --- | --- |
| Real-time host | `khaen-rt-host` | Single ALSA owner, 128-frame blocks, worker DAG, lock-free ring, plan arena, fault containment, last-good fallback |
| Plan format | `khaen-core` | Application binary interface of the compiled plan (roles, delays, FIR, IIR, matrix), verified on parse |
| Calibration | `khaen-calibration`, `khaen-modal-design`, `khaen-mimo-design` | Sweep → microphone capture → Farina deconvolution → level/delay/crossover/FIR/modal designs |
| Filter design | `khaen-designer` | Linear-phase, minimum-phase, brickwall, apodizing, corrected designs |
| Network audio | `khaen-aoip-aes67` | AES67 receiver/sender, SDP generation and parsing, SAP, RTSP/HTTP SDP, mDNS discovery (RFC subsets) |
| Control plane | `khaen-control`, `khaen-audio-web` | REST/WebSocket daemon, session auth, calibration jobs, web console |
| Verification | `khaen-rt-check`, `khaen-compat`, `khaen-bench` | ABI layout checks, forbidden-call and drop audits, real-time checks, benchmarks |
| Room simulation | `khaen-room-sim`, `khaen-file-runner` | Offline simulation and file-driven runs |

## Naming

Crates are published as `khaen-*`. The executables they build keep the historical
`tspi-*` names (`tspi-rt-host`, `tspi-rt-daemon`, `tspi-calibrate`), because deployment
units and install paths reference those names; renaming the binaries is a separate,
coordinated change.

## Build

```sh
cd rt-stack
cargo test --workspace
cargo run -p khaen-rt-host -- --help
```

Linux needs `pkg-config` and `libasound2-dev` for the real ALSA backend; Windows and macOS build the offline paths only.

## Honest limitations

- AES67 support is a deliberate subset (SDP/SAP/RTSP/HTTP/mDNS basics), not full AES67 conformance.
- Clock drift is currently handled by a small write-side rate servo; an asynchronous sample rate converter is planned (we intend to use [rubato](https://github.com/HEnquist/rubato)).
- The web console and some design documents are still written in Chinese.
- Measured audio results are not included in this repository.

## License

GPL-3.0. See [LICENSE](LICENSE).
