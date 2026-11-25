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
| Control plane | `khaen-control` | REST/WebSocket daemon, session auth, calibration jobs; serves the Vue console from `webui/dist` embedded at build time |
| Verification | `khaen-rt-check`, `khaen-compat`, `khaen-bench` | ABI layout checks, forbidden-call and drop audits, real-time checks, benchmarks |
| Room simulation | `khaen-room-sim`, `khaen-file-runner` | Offline simulation and file-driven runs |

## Naming

The project is named `khaen` throughout. Everything uses that one name:

- crates are published as `khaen-*`;
- the executables they build are `khaen-*` (`khaen-rt-host`, `khaen-rt-daemon`, `khaen-cal`);
- install paths are `/opt/khaen-rt-stack`, `/etc/khaen-rt-stack` and `/var/lib/khaen-rt-stack`;
- systemd units are `khaen-*.service`, `khaen-*.timer` and `khaen-*.target`;
- environment variables use the `KHAEN_` prefix;
- on-the-wire identifiers (schema tags such as `khaen-plan/v1` and
  `khaen-layout-presets/v2`, hash domain separators, and the plan sha256 / graph_id
  values derived from them) all carry the same name.

Renaming those identifiers changes the size of the plan hash domain by one byte, so it
changes every recorded `graph_id` and plan `sha256`. That was done deliberately in one
commit; older plans hashed under the previous name are not byte-compatible.

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
- `webui/dist` is a committed build artifact (the daemon embeds it at compile time); rebuild it
  with `pnpm build` and commit it together with any `webui/src` change (see `webui/README.md`).
- Measured audio results are not included in this repository.

## License

GPL-3.0. See [LICENSE](LICENSE).
