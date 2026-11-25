# 热部署说明（板端，不重烧镜像）

## 1. 组装 dist（开发机）

```bash
cd projects/tspi-audio-dsp/upmix
python3 tspi-upmix all -o /tmp/upmix-examples --playback-channels 22
DIST=/tmp/upmix-dist
rm -rf $DIST && mkdir -p $DIST/presets
cp tspi-upmix $DIST/
cp presets/upmix-presets.yml $DIST/presets/
cp -r /tmp/upmix-examples/profiles $DIST/
cp -r /tmp/upmix-examples/camilladsp $DIST/
cp deploy/install-upmix.sh $DIST/
tar czf /tmp/upmix-dist.tar.gz -C /tmp upmix-dist
```

dist 目录布局（install-upmix.sh 依赖）：

```
upmix-dist/
├── tspi-upmix
├── presets/upmix-presets.yml
├── profiles/upmix-*.yml          # 12 个（6 预设 × 完整/safe）
├── camilladsp/upmix-*.yml        # 6 个 ALSA 示例（仅供 --check 与参考）
└── install-upmix.sh
```

## 2. 推送并安装（开发机 → 板端）

```bash
BOARD=root@192.0.2.215
KEY=projects/tspi-audio-dsp/deploy/ssh/tspi_audio_ed25519
scp -i $KEY /tmp/upmix-dist.tar.gz $BOARD:/tmp/
ssh -i $KEY $BOARD "rm -rf /tmp/upmix-dist && tar xzf /tmp/upmix-dist.tar.gz -C /tmp"
# 先 dry-run 看计划（不写文件）
ssh -i $KEY $BOARD "cd /tmp/upmix-dist && bash install-upmix.sh --dry-run"
# 正式安装（校验失败自动回滚）
ssh -i $KEY $BOARD "cd /tmp/upmix-dist && bash install-upmix.sh"
```

安装只写文件并做 validate/render/camilladsp --check，**不会** systemctl start/restart，
也**不会**打开 ALSA 设备，可与其他声卡调试并行。

## 3. 回滚 / 卸载

```bash
ssh -i $KEY $BOARD "bash /tmp/upmix-dist/install-upmix.sh --rollback"   # 回滚最近一次安装
ssh -i $KEY $BOARD "bash /tmp/upmix-dist/install-upmix.sh --uninstall" # 卸载（先备份）
ssh -i $KEY $BOARD "bash /tmp/upmix-dist/install-upmix.sh --src /path --no-validate"  # 跳过校验
```

备份目录：/var/lib/tspi/upmix/install-backups/<时间戳>/（files/ 下按绝对路径镜像 + 新增文件清单）。

## 4. 安装后怎么用

- WebUI：声道分配/校准页 → profile 列表 → upmix-4.1.2-music（或 movie/wide；旧渲染器用 *-safe）
  → 加载 → 校验 → 应用。
- 命令行独立路径：tspi-upmix apply upmix-4.1.2-music（模式 off 时只渲染，不启动服务）。
- 关闭上混：加载 upmix-off-restore（首次安装时对 active 的快照）。

## 5. 注意

1. 需要板端有 python3-yaml（tspi-channelmap 已依赖，OEM 镜像已含）与 camilladsp（OEM 已含）。
2. 渲染器需含 upmix 修复（sha256 91e85e18…，OEM v5 已含）；旧渲染器只建议用 *-safe profile 或
   独立配置路径，脚本会检测并警告。
3. 不要用本脚本改 /etc/spi-audio/channelmap.yml 或启动音频模式；启用上混由 WebUI/用户决定。


## 6. 输入内容检测（task-19）

dist 追加目录：

```
upmix-dist/
├── container/
│   ├── profiles/    # *-from-h16-7.1.4.yml（上混）+ passthrough-*.yml（直通）
│   └── camilladsp/  # autodetect-*.yml（双路径，Aux1=上混 / Aux2=直通）
└── autodetect/
    └── tspi-upmix-autodetect.service   # 安装为 /etc/tspi-audio/upmix/*.example，默认不启用
```

生成 dist 时额外执行：

```bash
python3 tspi-upmix container -o /tmp/upmix-examples/container \
  --input-layout h16-7.1.4 --playback-channels 22
cp -r /tmp/upmix-examples/container $DIST/
mkdir -p $DIST/autodetect && cp deploy/tspi-upmix-autodetect.service $DIST/autodetect/
```

安装后启用（用户确认后；本脚本不会启用）：

```bash
cp /etc/tspi-audio/upmix/tspi-upmix-autodetect.service.example \
   /etc/systemd/system/tspi-upmix-autodetect.service
systemctl daemon-reload
systemctl enable --now tspi-upmix-autodetect
```

建议 CamillaDSP 启动参数带 `--gain1=-120 --gain2=0`（默认直通，daemon 接管前无双路径叠加）。
判定规则/契约/限制见 docs/INPUT-MODE.md。

