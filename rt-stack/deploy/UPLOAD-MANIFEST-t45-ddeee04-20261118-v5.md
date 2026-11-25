# UPLOAD-MANIFEST — 板端上传清单（release-v5 / t45-ddeee04-20261118-v5，task-218）

> **现行 T71 板端上传依据（2026-11-18 release-v5：T45 真写内核 + F1–F5 部署修复）**；取代 release-v4 的
> `m4-15cf411-20261118-v4` 上传包。板端动作全部 **NOT_RUN**，必须在授权窗口内、按 `deploy/README.md` 流程执行。
> 构建/重打包证据（逐文件 sha、qemu、解包校验）：`rt-stack/evidence/release-v5/`；F1–F5 修复证据：
> `rt-stack/evidence/cursor-fix-deploy-f1f5/`；T45/F6 实现证据：`rt-stack/evidence/cursor-fix-t45-apply/`。
>
> 上传包：`rt-stack/evidence/release-v5/artifact/khaen-deploy-t45-ddeee04-20261118-v5.tgz`。
> **说明**：本清单在包内，包自身 sha256 记在包外（`artifact/*.sha256` 与 ARTIFACTS.md），避免自引用；校验以
> 包外 sha256 + 包内 `SHA256SUMS`（sha256sum -c）+ 板端 `test -x` 为准。
>
> 构建源：**git ddeee04**（T45 真写内核 + F1–F5 deploy 修复）。bundle `t45-ddeee04-20261118-v5`（14 files；tgz
> 1,050,866 B sha256 `6ea0f326354af98cd3eda1359981a027c07bca7547172f5e7637667da6a587c6`）。

## 1. 上传包 → 板端目标（由板上脚本安装，不手工拷）

| 包内目录/文件 | 内容 | 板端去向（04/05 安装） |
| --- | --- | --- |
| `deploy/` | `bin/00..08` + `make-release-bundle.sh` + `make-deploy-package.sh`、`lib/`（含 `release_tar.py`）、`config/`、`tools/`、`tests/`、README、本 UPLOAD-MANIFEST | `/var/tmp/khaen-rt-stack-deploy/`（root 执行） |
| `service/` | host/daemon/target/irq-affinity units + host.service.d/90-safe-mute.{conf,env}.example + README | `/etc/systemd/system/`（04 安装；target Wants/After host+daemon） |
| `release-bundle/`（16 文件） | `bin/khaen-rt-host`（**T45 新**）、`bin/khaen-rt-daemon`、`units/`（6）、`etc/`（5 个 `*.example`）、manifest.json、README.deploy.md、SHA256SUMS | `/opt/khaen-rt-stack/releases/t45-ddeee04-20261118-v5/{bin,units,manifest.json,README.deploy.md}` + `current`（symlink） |
| `README-UPLOAD.md`、`SHA256SUMS` | 上传说明 + 包级逐文件校验和 | `/var/tmp/` 保留（校验用） |

## 2. 关键文件 sha256（完整清单见包内 `SHA256SUMS`；bundle 14 项见 `rt-stack/evidence/release-v5/ARTIFACTS.md` §2）

| 文件 | 大小(B) | sha256 |
| --- | ---: | --- |
| bin/khaen-rt-host（bundle，**T45 重编译**） | 1198400 | `e6f07776f58c34492b05c934bd04f9b6c00e8c99b8f59f41a6b1655783486522` |
| bin/khaen-rt-daemon（bundle，与 M4 字节相同） | 1286384 | `804a8bcd94bc73cf8aff29bc459633abbee0cf55d50c512ad4215c410c577c18` |
| units/khaen-rt-host.service（F3/F4） | 3594 | `1ddda0fb07e2b60e326b0a430336238f8ca65b7d58373b448923ccb8464be708` |
| units/khaen-rt-daemon.service（F3） | 2522 | `7d3028ab244fe86c2fcf54bd43fbede882a51c1e3cd0c6ea2d766dcebdde9182` |
| etc/rt-host.env.example（**T45 开关示例**） | 1327 | `fa8428c7cf1e12dbc75ead5a51bee03e2ccb9487db8adc7aa48524545882fd37` |
| etc/rt-host.toml.example（T45 注释） | 1927 | `884d6c84f9438549c8e6f5cae9c536c4fac14d46fe60bf39e22c77756e9eb591` |
| bundle tgz（可单独上传） | 1050866 | `6ea0f326354af98cd3eda1359981a027c07bca7547172f5e7637667da6a587c6` |

> 与 release-v4 比：**host 二进制因 T45 变化**（1,176,128→1,198,400 B，`fb247696…`→`e6f07776…`）；daemon 与 6 个 units 同字节；
> rt-host.env.example / rt-host.toml.example 因 T45 开关示例/注释变化；其余 etc 不变。逐项对比见 release-v5/ARTIFACTS.md。

## 3. 上传与板端只读校验

```bash
scp khaen-deploy-t45-ddeee04-20261118-v5.tgz board:/var/tmp/
ssh board 'cd /var/tmp && tar xzf khaen-deploy-t45-ddeee04-20261118-v5.tgz'
cd /var/tmp && sha256sum -c SHA256SUMS
test -x deploy/bin/05-atomic-switch.sh && test -x deploy/bin/03-retire-c.sh && echo SCRIPT_EXEC_OK
chmod +x deploy/bin/*.sh deploy/tools/*.sh deploy/tests/*.sh \
         deploy/config/irq-affinity-pin deploy/config/motu-48k-keeper   # 仅中转丢位时兜底
python3 deploy/lib/release_manifest.py verify release-bundle --require-arch aarch64   # files_checked=14, ok
python3 deploy/lib/check_elf.py release-bundle/bin/khaen-rt-host --require-arch aarch64
python3 deploy/lib/check_elf.py release-bundle/bin/khaen-rt-daemon --require-arch aarch64
```

## 4. 安装/切换（授权窗口内，root；唯一入口 05）

```bash
sudo ./deploy/bin/05-atomic-switch.sh --live --authorize /var/tmp/window.json \
     --bundle /var/tmp/release-bundle --allow-first-switch
```

> 内部顺序：00 snapshot → 01 pre-install（失败=零变更）→ 04 install（release/current + units + env/toml + disable irqbalance/IRQ pin + keeper，不 start）
> → 01 完整 A–J → 03 retire-c（F2：普通 unit 文件先备份移走再 mask）→ 02 设备独占 → start keeper/irq-affinity
> → enable/start target（等 host+daemon active；F3/F4 新 unit 生效）→ 06 verify（V1–V10）→ 07 last-good。
> 失败只回自研 last-good 或 mute（G-no-C；F5 的 mute 核对已改为运行时环境）。

## 5. T45/F6 板窗必做（真实播放验收）

1. **开启 RT 写内核**（已装板的 /etc/khaen-rt-stack/rt-host.env 不会被 04 覆盖，需手动追加）：
   ```bash
   grep -q '^KHAEN_HOST_APPLY_AFFINITY=true' /etc/khaen-rt-stack/rt-host.env \
     || echo 'KHAEN_HOST_APPLY_AFFINITY=true' >> /etc/khaen-rt-stack/rt-host.env
   systemctl daemon-reload
   ```
2. host 启动后核对 run JSON：`scheduler.kernel_applied=true`、`kernel_apply.sched_policy="SCHED_FIFO"`、`mlockall=true`、
   `main_cpu=0`、`workers_applied=4`。失败时 host exit 1 且 error 含 `stage=...`（fail-closed，不会带病播放）。
3. 再跑 T71 §4.3 真实播放 no-xrun 对照（buffer1024/2048 × workers4/2）。

## 6. 依赖与边界

- 板上需 `python3`、`systemctl`、`tar`、`sha256sum`；上传包不联网。
- **所有板端动作 NOT_RUN**：安装/systemd/真实 ALSA/MOTU 播放/RT/IRQ 持久化/浏览器/回滚演练均未执行
  （见 `rt-stack/evidence/release-v5/NOT_RUN.md`）。
- 禁止 CamillaDSP/旧 C unit 作任何回退；授权窗口 JSON 与 `audio_off/S1/soak` 口径见 `deploy/README.md` §1/§3。
- web dist 不在本包：板端若仍需替换，按 `web/scripts/install-device.sh` 单独上传
  （release-v5 重打 `d458ae7b…`，与 M4 字节相同；见 `rt-stack/evidence/release-v5/ARTIFACTS.md`）。
