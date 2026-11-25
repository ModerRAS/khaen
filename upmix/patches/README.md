# tspi-channelmap 上混相关修复（存档，已合入）

`tspi-channelmap-upmix-fixes.patch` 是 pinned 基线（overlay 文件，sha256 1e7dc232…）上的
统一 diff，修复 3 个改变渲染结果的 bug：

| # | 问题 | 影响 | 修复 |
| --- | --- | --- | --- |
| A | Filter step 的 channels/names 被当成配对使用 | CamillaDSP 语义是 names 全部串到 channels 每个通道 → 角色滤波互相叠加；4.1.2 的 FL 在 1 kHz 被衰减 -42 dB | add_step 改为每 filter 一步（同 name 多通道才合并）；SUB LP 同理 |
| B | roles[].sources[].inverted 被 dest_sources() 丢弃 | (L−R) 差信号变 (L+R) mid | 与 role polarity 异或后写入 mixer source inverted |
| C | bass_extract mixer 未直通主声道 | CamillaDSP 未映射输出=静音 → 开 bass_management 后主声道全静音 | 先显式直通 0..pb-1，再追加 extract 通道 |

状态：**audio-stack 已 review 并合入**（新增 3 条回归；新 renderer sha256 91e85e18…，
OEM v5 镜像已包含）。本 patch 作为历史记录与旧镜像应急使用：

```bash
cd / && patch -p1 --dry-run < tspi-channelmap-upmix-fixes.patch
cd / && patch -p1 < tspi-channelmap-upmix-fixes.patch
# 音频模式在跑时：systemctl restart tspi-camilladsp
```

验证：用打了 patch 的渲染器重渲染 4.1.2/5.3.2 profile，与独立生成的正确配置逐电平一致；
本工具测试套件的 renderer 变体要求修复后的渲染器（OEM v5 或本 patch）。
