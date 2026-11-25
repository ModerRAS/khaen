# BREN7P 实验室 PTPv2 GM（在不等待板子的前提下验证 PTPv2 链路）

> 用户 2026-11-22 要求：PTP 主钟可以先跑在 BREN7P 上，不必等板子。本目录是可复现做法；
> 实测数据见 \`rt-stack/evidence/board-ptp-20261122/lab-bren7p/\`。**板端 GM（需求 15）仍是最终形态，板端运行 NOT_RUN。**

## 0. 方案选择（为什么不是 Windows statime）

| 方案 | 可行性 | 结论 |
| --- | --- | --- |
| A. BREN7P Windows 上跑 statime 构建作 GM | **不可行**：fork 只有 \`statime-linux\` daemon（依赖 libc/timestamped-socket/usrvclock；README 明确非 Linux 需自实现平台后端），工作区无 statime.exe | 放弃，不写 Windows 后端 |
| B. BREN7P 上的 Linux VM 跑 linuxptp \`ptp4l\` GM | **可行且已实测 PASS**（本次选择）：与板端发布同一份配置/wrapper/版本 4.2，只是二进制换成 x86_64 | 采用 |

Windows 若将来确实需要 PTP 主机：防火墙预案与限制见
\`rt-stack/evidence/board-ptp-20261122/lab-bren7p/statime-windows-evaluation-and-firewall.md\`（本次未修改防火墙）。

## 1. 拓扑（与生产 LAN 完全隔离）

\`\`\`
BREN7P (Windows host 192.0.2.94, 生产网卡 I226 不参与)
└── VirtualBox Host-Only Ethernet Adapter 192.168.56.1/24   ← 独立虚拟交换机
    ├── khaen-audio-build VM (Ubuntu 22.04)  enp0s8 192.168.56.2  = PTPv2 GM（linuxptp 4.2，-S 软件时间戳）
    └── inferno-dev VM     (Debian 13)       enp0s9 192.168.56.3  = PTPv2 slave（linuxptp 4.2，nullf 只测量）
\`\`\`

- PTP 组播只走 host-only 虚拟交换机：**不进 192.0.2.0/24**、不碰 dsp-host/板子/生产 AES67、不受 Windows 防火墙影响、不出声。
- GM 用**发布 bundle 的同一份** \`ptp4l-master.conf\` + \`ptp-master-start\` wrapper（x86_64 同源 linuxptp 4.2），
  因此验证的是将要上板的配置语义与启动包装；唯一差别是 x86_64 + 软件时间戳（无 PHC）。
- 板端的硬件时间戳/PHC/TAI-\`phc2sys -O 37\` 仍只能板端验证（NOT_RUN）；lab 用 \`tai-align-gm.py\` 做等价语义演练。

## 2. 一次性准备

\`\`\`powershell
# Windows：确认 host-only 适配器存在并处于启用状态（192.168.56.1/24）
Get-NetAdapter | Where-Object InterfaceDescription -like "*Host-Only*"
# 两台 VM 关机后加 host-only NIC（可回滚 --nic2 none / --nic3 none）
VBoxManage modifyvm khaen-audio-build --nic2 hostonly --hostonlyadapter2 "VirtualBox Host-Only Ethernet Adapter" --macaddress2 auto
VBoxManage modifyvm inferno-dev     --nic3 hostonly --hostonlyadapter3 "VirtualBox Host-Only Ethernet Adapter" --macaddress3 auto
VBoxManage startvm khaen-audio-build --type headless
VBoxManage startvm inferno-dev --type headless
\`\`\`

注意：给 inferno-dev 加第三块网卡后，Debian 桌面 NetworkManager 可能不会自动恢复原 bridged 静态 IP
（本次需要在 VM 控制台登录后手动 \`ip addr add 192.0.2.216/24 dev enp0s8\` 才恢复 SSH）；
这是 dev VM 的一次性网络扰动，不影响生产，收尾可按回滚命令移除 host-only NIC。

IP 配置（VM 内，root）：

\`\`\`bash
# builder（GM）
ip addr add 192.168.56.2/24 dev enp0s8 && ip link set enp0s8 up
# inferno-dev（slave）
ip addr add 192.168.56.3/24 dev enp0s9 && ip link set enp0s9 up
\`\`\`

## 3. 启动 GM（builder VM）

先按生产布局 staged（\`/opt/khaen-rt-stack/ptp/current/bin\` + \`/usr/local/lib/khaen/\` + \`/etc/khaen-ptp/\`），
可直接用 \`start-lab-gm.sh\`：

\`\`\`bash
sudo bash rt-stack/ptp/lab/bren7p/start-lab-gm.sh --iface enp0s8 --out ~/lab/bren7p-gm
# 日志应含：WARN no PHC ... software timestamp fallback；随后 portState MASTER
sudo /opt/khaen-rt-stack/ptp/current/bin/pmc -s /run/khaen-ptp/ptp4l.uds -u -b 0 'GET PORT_DATA_SET'
\`\`\`

## 4. 启动 slave + 采样（inferno-dev VM）

\`\`\`bash
# 把 x86_64 ptp4l/pmc（同源 linuxptp 4.2）与 slave.conf.example、tests/measure-slave-offset.sh 放到 ~/ptp-lab
sudo nohup ~/ptp-lab/ptp4l -f ~/ptp-lab/slave.conf -i enp0s9 -S -m -q > /tmp/ptp-slave/slave.log 2>&1 &
sudo nohup ~/ptp-lab/measure-slave-offset.sh --uds /tmp/ptp-slave/slave.uds --pmc ~/ptp-lab/pmc \
  --duration 900 --out /tmp/ptp-slave/soak-900 --tag bren7p-hostonly-udp > /tmp/ptp-slave/sampler.log 2>&1 &
\`\`\`

- slave 用 \`clock_servo nullf\` + \`step_threshold 1000\`，**不校正/不 step 宿主时钟**；offset 从 \`pmc CURRENT_DATA_SET\` 原始读取。
- \`--duration 900\`（15 min）在用户要求的 10–30 min 范围内；报告 mean/std/p95/max 与线性漂移斜率。
- GM 侧每 60 s 记录 \`portState\` 到 \`gm-states.csv\`，证明 GM 全程 MASTER。

## 5. TAI 语义演练（可选，第二段 10 min）

板端正式设计是 \`phc2sys -O 37\`（PHC=TAI）。lab 无 PHC，用 \`tai-align-gm.py\` 把 GM 的系统钟拨到
"slave UTC + 37 s"（等价于 GM 播 TAI），slave 配置 \`utc_offset 37\`，再做一轮采样验证 offset 收敛：

\`\`\`bash
# 1) 从第一段测得的 offset O（GM_UTC - slave_UTC）计算 GM 需要调整的步进：+37s - O
sudo python3 tai-align-gm.py --offset-ns <O_ns> --utc-offset 37 --dry-run   # 看计划
sudo timedatectl set-ntp false                                              # 防止 NTP 立即纠回
sudo python3 tai-align-gm.py --offset-ns <O_ns> --utc-offset 37            # 真步进
# 2) 重启 GM，slave 配置 utc_offset 37，重新采样 10 min
# 3) 收尾：sudo timedatectl set-ntp true（或把时钟拨回真实 UTC）
\`\`\`

**注意**：这是实验室等价演练，不是板端 phc2sys 的替代品；正式板端仍必须用 PHC/KA 硬件时间戳与 \`-O 37\`（NOT_RUN）。

## 6. 结果与证据

本次实测（2026-11-22，15 min + 10 min 两段）汇总见
\`rt-stack/evidence/board-ptp-20261122/REPORT.md\` §11 与 \`lab-bren7p/summary.json\`。
关键结论（详表见证据）：
- GM 软件回退日志、4 s 内 MASTER、GMT identity 正确；
- slave 锁 SLAVE、stepsRemoved=1、全程无失锁；offset 抖动与漂移量级见 summary（软件时间戳 + VirtualBox 虚拟链路，不代表板端 PHC 精度）；
- 第一段 absolute offset 含两台 VM 各自 NTP 时钟差（~0.66 s），不是 PTP 误差；TAI 对齐段消除该偏置后 offset 见证据；
- Windows 防火墙/生产 LAN：全程未改动、未触碰。

## 7. 回滚

\`\`\`powershell
VBoxManage modifyvm khaen-audio-build --nic2 none
VBoxManage modifyvm inferno-dev     --nic3 none   # 若还需保留原 bridged 生产网卡则不要动 nic2
\`\`\`
VM 内追加的 \`192.168.56.x\` 地址随重启消失；inferno-dev 的 bridged 192.0.2.216 恢复方式见 §2 注。
