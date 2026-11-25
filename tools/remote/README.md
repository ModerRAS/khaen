# 泰山派远程诊断/烧录工具（Python）

只用 Python 标准库 + Windows OpenSSH（`ssh.exe`/`scp.exe`）。远端命令一律用
PowerShell `-EncodedCommand` 执行，避免 cmd/PowerShell 的引号问题。

## 用法

在本目录执行：

```bash
# 状态：板端 UDC、远端 PnP(VID_2207)、upgrade_tool 路径、本地镜像 hash
python remote.py status

# 在 sender-host 上执行一段 PowerShell
python remote.py ps --script "Get-Date"

# 复制文件到远端
python remote.py scp --local .\foo.ps1 --remote "D:/WorkSpace/Claw/scratch/lead/remote-flash/foo.ps1"

# 重启板子进 Loader（会中断正在跑的音频/soak）
python remote.py loader

# 烧写（会重启板子；必须 --force）
python remote.py flash --force

# 把 remote.py/README 复制到远端工作区
python remote.py copy-scripts
```

## 默认目标

- 远端：`adqew@192.0.2.63`，工作区 `D:\WorkSpace\Claw`
- 板端：`root@192.0.2.215`
- 镜像：`projects/tspi-audio-dsp/firmware/rt-oem-v14/update.img`
- 工具：远端 `vendor/rockchip-tools/.../upgrade_tool.exe`（递归查找）

可用 `-c config.json` 覆盖默认值；字段见 `remote.py` 顶部 `DEFAULTS`。

## 状态判据

- 板端 `/sys/class/udc/*/state` 应为 `configured`，不是 `not attached`；
- 远端 PnP 应出现 `USB\VID_2207&PID_0006\...`（rk3xxx，正常 gadget）；
- 烧写前必须进 Loader：`upgrade_tool LD` 显示 `Mode=Loader` / `rockusb connected(1)`。
