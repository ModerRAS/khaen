# TSPI RT-first DSP stack release（aarch64-20261122）

> 由 deploy/bin/make-release-bundle.sh 生成；部署/切换/回退见 rt-stack/deploy/README.md 与
> rt-stack/evidence/migration-plan/PLAN.md。P1 G-no-C：禁止把 CamillaDSP/旧 C 任何 unit 作为回退。

- target: aarch64-unknown-linux-gnu
- built_at: 2026-11-22T04:42:22+0800
- bundle: bin/（3 个二进制）、units/、etc/、manifest.json
- 安装：deploy/bin/04-install-release.sh --live --authorize <window.json> --bundle <本目录>
- 切换：deploy/bin/05-atomic-switch.sh --live --authorize <window.json> --bundle <本目录>
- 回退：deploy/bin/08-rollback.sh --live --authorize <window.json>（只回自研 last-good 或 mute）
