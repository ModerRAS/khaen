# PTP grandmaster add-on bundle — install / verify / rollback (board steps are NOT_RUN)

> **Status declaration: every board command below is NOT_RUN.** They were statically checked and dry-run
> tested on Linux (builder VM staging root), but never executed on TSPI 3M 192.0.2.215 (board powered off,
> not contacted). The lab netns validation in `rt-stack/evidence/board-ptp-20261122/` does **not** count as
> board verification.

## 0. Bundle contents

| path | content |
| --- | --- |
| `bin/` | `ptp4l`, `phc2sys`, `pmc`, `phc_ctl` — ELF64 aarch64, dynamically linked against glibc (max 2.34) / libm |
| `lib/` | `ptp-lib.sh` (0644), `ptp-master-start` / `ptp-phc2sys-start` (0755) |
| `units/` | `tspi-ptp-master.service`, `tspi-ptp-phc2sys.service`, `tspi-ptp.target` |
| `etc/` | `ptp-master.env.example`, `ptp4l-master-udp.conf.example`, `ptp4l-master-l2.conf.example` |
| `manifest.json` | per-file size/sha256 (schema `tspi-rt-stack/release/v1`) |
| `SHA256SUMS` | checksums (includes `manifest.json` itself) |
| `dist/*.tgz` | same tree as a tarball (explicit member modes: bin/lib 0755) |

Installed layout: `/opt/tspi-rt-stack/ptp/releases/<id>/{bin,lib,units,etc}` + `/opt/tspi-rt-stack/ptp/current` symlink,
wrappers in `/usr/local/lib/tspi/`, site config in `/etc/tspi-ptp/`, units in `/etc/systemd/system/`.

## 1. Preflight on the board (read-only, NOT_RUN)

```bash
uname -a; cat /etc/debian_version                 # expect RK3576 aarch64 / Debian 12
ip -4 route show default                          # expect dev end0 (or set TSPI_PTP_IFACE)
ethtool -T end0 | sed -n '1,12p'                  # expect hardware-transmit / hardware-receive
ls -l /dev/ptp*; readlink -f /sys/class/net/end0  # PHC /dev/ptp0 expected on RK3576 GMAC
timedatectl | sed -n '1,6p'                       # system clock must be sane (NTP or manual UTC)
systemctl is-active tspi-ptp4l-master.service tspi-phc2sys.service   tspi-statime-inferno.service tspi-aes67-daemon.service tspi-ravenna-lkm.service
grep -E '^(ExecStart|ConditionPathExists)' /etc/systemd/system/tspi-ptp4l-master.service 2>/dev/null
ldd --version | head -1                            # glibc >= 2.34
```

The new units `Conflict` with the legacy PTP owners above, but they must also be **disabled/stopped**
before enabling the GM, otherwise the first `systemctl start` stops them (correct but noisy) or the
legacy marker `/run/tspi/active-aes67-master` re-starts them. Keep exactly one PTP clock owner.

## 2. Transfer & verify (NOT_RUN)

```bash
sudo mkdir -p /var/tmp/tspi-ptp-release
sudo tar -xzf tspi-ptp-aarch64-20261122.tgz -C /var/tmp/tspi-ptp-release
cd /var/tmp/tspi-ptp-release
sha256sum -c SHA256SUMS
python3 <repo>/rt-stack/deploy/lib/release_manifest.py verify . --require-arch aarch64
file bin/*; ldd bin/ptp4l | grep -E 'libc|libm'
```

## 3. Install (dry-run first; live needs a window; NOT_RUN)

```bash
# dry-run (default): prints every change, touches nothing
sudo bash <repo>/rt-stack/deploy/bin/09-install-ptp-gm.sh --bundle /var/tmp/tspi-ptp-release

# live: requires an authorized board window with scope install-ptp-master
sudo bash <repo>/rt-stack/deploy/bin/09-install-ptp-gm.sh --live   --authorize /var/tmp/WINDOW.json --bundle /var/tmp/tspi-ptp-release
```

The installer: verifies the manifest, copies the release under `/opt/tspi-rt-stack/ptp/releases/<id>`,
atomically moves `current`, installs the two wrappers + `ptp-lib.sh`, seeds
`/etc/tspi-ptp/{ptp-master.env,ptp4l-master.conf,ptp4l-master-l2.conf}` **only if absent**, installs the
units, runs `systemctl daemon-reload` + `systemctl enable tspi-ptp.target` (enable only, does not start),
and writes `/var/lib/tspi-rt-stack/ptp-install.json` with the previous release id for rollback.

## 4. Start & verify (in the window; NOT_RUN)

```bash
systemctl start tspi-ptp.target
systemctl status tspi-ptp-phc2sys.service tspi-ptp-master.service --no-pager
journalctl -u tspi-ptp-master -u tspi-ptp-phc2sys -b --no-pager | tail -n 40   # look for "timestamping=hardware"
systemctl show tspi-ptp-master.service -p MainPID -p ActiveState

# GM dataset through the bundled pmc (UDS from the config: /run/tspi-ptp/ptp4l.uds)
/opt/tspi-rt-stack/ptp/current/bin/pmc -s /run/tspi-ptp/ptp4l.uds -u -b 0 'GET PORT_DATA_SET'
#   expect: portState MASTER
/opt/tspi-rt-stack/ptp/current/bin/pmc -s /run/tspi-ptp/ptp4l.uds -u -b 0 'GET PARENT_DATA_SET'
#   expect: grandmasterIdentity <own clock id>, clockClass 248, priority1 0, stepsRemoved 0
/opt/tspi-rt-stack/ptp/current/bin/pmc -s /run/tspi-ptp/ptp4l.uds -u -b 0 'GET CURRENT_DATA_SET'
#   expect: stepsRemoved 0

# PHC runs TAI: ~37 s ahead of UTC system time, offset stable within tens of ns
/opt/tspi-rt-stack/ptp/current/bin/phc_ctl /dev/ptp0 get
date -u +%s.%N
journalctl -u tspi-ptp-phc2sys -b --no-pager | tail -n 20   # "s0 sys offset ... freq ..."

# slave side: run any PTPv2 slave on another box (linuxptp ptp4l -S / statime follower) and expect
#   portState SLAVE, stepsRemoved 1, offsetFromMaster in the microseconds range (software slave)
```

Acceptance hints: `portState MASTER` within ~10 s, `phc2sys` offset <= 1 us after the first step,
slave lock with `stepsRemoved 1` and no UNCALIBRATED flapping for >= 10 min; a GM restart must return to
MASTER without manual intervention.

## 5. Rollback (NOT_RUN)

```bash
# stop + disable, keep files and previous release pointer (safe default)
sudo bash <repo>/rt-stack/deploy/bin/10-rollback-ptp-gm.sh --live   --authorize /var/tmp/WINDOW.json

# full removal (units, wrappers, /etc/tspi-ptp, /opt release); configs are backed up first
sudo bash <repo>/rt-stack/deploy/bin/10-rollback-ptp-gm.sh --live   --authorize /var/tmp/WINDOW.json --remove
```

Manual equivalent:

```bash
systemctl disable --now tspi-ptp.target
systemctl stop tspi-ptp-master.service tspi-ptp-phc2sys.service
rm -f /etc/systemd/system/tspi-ptp-master.service /etc/systemd/system/tspi-ptp-phc2sys.service       /etc/systemd/system/tspi-ptp.target
systemctl daemon-reload
cp -a /etc/tspi-ptp /var/tmp/tspi-ptp-config-backup-$(date +%Y%m%dT%H%M%S)
rm -rf /etc/tspi-ptp /usr/local/lib/tspi/ptp-master-start /usr/local/lib/tspi/ptp-phc2sys-start        /usr/local/lib/tspi/ptp-lib.sh /opt/tspi-rt-stack/ptp
# legacy PTP units are NOT touched by this rollback; bring the required owner back explicitly.
```

## 6. Boundaries

* The GM is a **clock-domain owner**: only install/start it in an authorized board window, with audio off.
* Never run it together with `tspi-statime-inferno` / `tspi-aes67-daemon` / `tspi-ravenna-lkm`
  (slave-only paths; the LKM also intercepts PTP frames).
* Do not change `TSPI_PTP_UTC_OFFSET` without a leap-second event; wrong offset breaks slave lock.
* Board installation, service start, PHC accuracy, long soak and real H16/AES67 interop: **NOT_RUN**.
