# KHAEN PTPv2 grandmaster (requirement 15) — board clock owner, offline-prepared

> Status: **offline implementation + lab validation done; board run NOT_RUN** (board 192.0.2.215 powered off,
> never contacted). See `docs/BOARD-PTP-MASTER.md` for the full solution/deployment/risk document and
> `rt-stack/evidence/board-ptp-20261122/` for evidence.

The KHAEN 3M (Rockchip RK3576, Debian 12 arm64) must act as the **network-audio PTP master** so that future
slave-only devices (e.g. NuPrime H16-AIP class endpoints) can lock. This directory is the single source of
truth for that clock owner: units, ptp4l profile templates, env knobs, launcher wrappers and the offline
validation harness. The aarch64 binaries are cross-built from pinned linuxptp source (not the distro package)
and shipped as a release add-on bundle next to `deploy/aarch64-20261122/`.

## 1. Decision: linuxptp ptp4l, not statime, for the GM

| criterion | linuxptp 4.2 (chosen) | statime fork (present on the box for Dante/Inferno) |
| --- | --- | --- |
| PTPv2 GM (Sync/Follow_Up/Announce) | mature, BMCA + `serverOnly`, battle-tested | PTPv2 master code exists (`statime/src/port/master.rs`) |
| HW timestamping | SO_TIMESTAMPING + PHC since 2010 | `hardware-clock = N` config exists; less exercised as GM |
| Clock chain tooling | ptp4l + phc2sys + pmc + phc_ctl (TAI offset `-O 37`, diagnostics) | no phc2sys equivalent; would need own TAI/PHC discipline |
| Board evidence | **board GM PASS 2026-11-17** (`evidence/b2-ptp-gm`, distro 3.1.1) | used as PTPv1/Inferno follower; GM path unproven |
| Build | plain C, aarch64-linux-gnu-gcc, no external libs | Rust + target toolchain, larger artifact |
| Ownership conflict | one daemon = one clock owner | statime already owns the Dante PTPv1 path |

statime stays the Inferno/Dante (PTPv1) follower path. The GM units **Conflict** with the slave-only owners
(`khaen-statime-inferno`, `khaen-aes67-daemon`, `khaen-ravenna-lkm`, legacy `khaen-ptp4l-master`/`khaen-phc2sys`),
so the box never runs two PTP clocks at once.

Pinned build: linuxptp **v4.2** (`17195fd9cd8f2aaebe76cbb914f161307f81cbf1`), cross-compiled in builder VM
`khaen-audio-build` with the Debian 12 arm64 sysroot; GLIBC symbol floor 2.34 <= board 2.36.

## 2. Clock architecture

```
  board system clock (UTC; NTP or manual)          <- only time source
        |  phc2sys -s CLOCK_REALTIME -c end0 -O 37
        v
  NIC PHC /dev/ptp0 (PTP/TAI timescale, UTC+37s)   <- hardware timestamps for every PTP event
        |  ptp4l -f ptp4l-master.conf -i end0 -H
        v
  PTPv2 multicast (domain 0, E2E, two-step)  --->  H16/AES67/RAVENNA slaves
```

* **Hardware timestamping first**: the wrapper detects PHC/HW-timestamp capability (`ethtool -T` ->
`PTP Hardware Clock`, then `/sys/class/net/<if>/device/ptp/ptp*`) and runs `ptp4l -H`.
* **Software fallback**: with `KHAEN_PTP_HW_TS=auto` (default) and no PHC, the wrapper runs `ptp4l -S` and logs a
  loud WARN. `on` refuses to start instead of silently degrading; `off` forces software mode for lab work.
* **TAI, not UTC**: IEEE 1588 uses the TAI timescale. `phc2sys -O 37` keeps the PHC 37 s ahead of UTC
(the board B2 run proved that a UTC PHC makes linuxptp slaves see a fake ~36.35 s offset and never lock).
Update `KHAEN_PTP_UTC_OFFSET` on a leap second.
* **Two profiles**: `ptp4l-master.conf` = UDP/IPv4 (default, lab-validated); `ptp4l-master-l2.conf` =
IEEE 802.3 L2 (lab-validated). The transport must match the slaves' listener (AES67/RAVENNA deployments
often use L2); it is a deployment-time choice, not a rebuild.

## 3. Files

| path | purpose |
| --- | --- |
| `units/khaen-ptp-master.service` | ptp4l GM (wrapper ExecStart, sandbox, capabilities, Conflicts) |
| `units/khaen-ptp-phc2sys.service` | phc2sys system->PHC with `-O 37`; exits 0 when no PHC |
| `units/khaen-ptp.target` | Wants both; `enable` this (not the units directly) |
| `etc/ptp4l-master-udp.conf.example` | ptp4l profile, UDP/IPv4, domain 0, GM dataset |
| `etc/ptp4l-master-l2.conf.example` | same dataset, IEEE 802.3 transport |
| `etc/ptp-master.env.example` | runtime knobs (iface, conf, hw-ts policy, TAI offset) |
| `lib/ptp-lib.sh` | shared interface/PHC detection helpers (POSIX sh) |
| `lib/ptp-master-start` | ExecStart wrapper: resolve iface, HW/SW selection, exec ptp4l |
| `lib/ptp-phc2sys-start` | ExecStart wrapper: skip without PHC, else exec phc2sys |
| `tests/check_ptp_bundle.py` | static unit/config/wrapper validator (used by the bundle packer) |
| `tests/lab-gm-slave.sh` | reproducible netns GM+slave offline lock/offset test |
| `INSTALL-ROLLBACK.md` | board installation / verification / rollback steps (all NOT_RUN) |

Bundle build/install/rollback entry points live in `rt-stack/deploy/bin/`:
`make-ptp-bundle.sh`, `09-install-ptp-gm.sh`, `10-rollback-ptp-gm.sh`.

## 4. Offline validation (reproducible)

```sh
# build (builder VM, see evidence/board-ptp-20261122/PROVENANCE.md)
make -j8 CROSS_COMPILE=aarch64-linux-gnu-      EXTRA_CFLAGS="--sysroot=$ROOTFS -O2" EXTRA_LDFLAGS="--sysroot=$ROOTFS"

# protocol/lock/offset (root; veth netns, no production network, no PHC)
sudo tests/lab-gm-slave.sh --ptp4l <native>/ptp4l --pmc <native>/pmc      --gm-conf etc/ptp4l-master-udp.conf.example --transport udp --duration 120 --out /tmp/udp

# same with the shipped wrapper (proves the ExecStart path + software fallback)
sudo tests/lab-gm-slave.sh --gm-wrapper /usr/local/lib/khaen/ptp-master-start      --ptp4l <native>/ptp4l --pmc <native>/pmc --gm-conf /etc/khaen-ptp/ptp4l-master.conf      --transport udp --duration 120 --out /tmp/wrapper
```

Measured results (2026-11-22, builder VM) are recorded in
`rt-stack/evidence/board-ptp-20261122/REPORT.md` — software timestamps on veth only, therefore an
order-of-magnitude worse than the board PHC path; they prove protocol/config/lock/failover, not board
accuracy. **Board PHC accuracy remains NOT_RUN.**

## 5. What must still happen on the board (NOT_RUN)

1. Board window authorization + aarch64 add-on bundle install (`09-install-ptp-gm.sh`);
2. `ethtool -T end0` shows `hardware-transmit/receive` and `/dev/ptp0` exists (expected from RK3576 GMAC,
   previously observed in B2 evidence);
3. real lock test against a slave (lab VM first, then H16/AES67 endpoint) and offset stability over hours;
4. decide UDPv4 vs L2 per the deployed slave ecosystem and record it.
