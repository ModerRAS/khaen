# MOTU / CueMix 交付说明

日期：2026-11-16。范围：`projects/tspi-audio-dsp/web`。没有连真机，没有改 CamillaDSP / PTP / AES67，没有重启这些服务。

## 改了什么

- `src/motu/`：City/datastore 编解码、快照目录、`RealMotu`（发现 `enx*`、链路本地地址、`:1280` WebSocket、写后读回、失败回滚）、`MockMotu`（内存设备，不跑命令、不开套接字）。
- `src/routes/motu.rs`：`/api/motu/status|snapshot|monitor|direct-mute|restore`，都在原有认证后面。写操作没有 `confirm: true` 时返回 400。
- `src/config.rs`、`deploy/config.toml.example`：`[motu]`，旧配置缺这一节时用默认值。
- `assets/index.html`、`assets/app.js`：新页「MOTU / CueMix」，8 秒轮询。顺手修了首次设置密码后设置卡不消失：`showApp()` 现在会藏起 `#setup-view`。
- 测试：编解码往返、分帧、矩阵下标、直通计划、快照回滚、mock 读写；路由未认证 401、确认位、静音读回、失败注入回滚。
- 文档：`docs/API.md` 第 9.1 节，`docs/MOTU-CUEMIX.md`，`README.md`。

## 接口

| 方法 | 路径 | 成功 | 失败 |
| --- | --- | --- | --- |
| GET | `/api/motu/status` | 200，离线也是 200，`online: false` | 未认证 401 |
| GET | `/api/motu/snapshot` | 200；`?raw=1` 才带 datastore | 未认证 401 |
| POST | `/api/motu/monitor` | 200，`verified: true` | 无 confirm 400；离线 503；校验失败 500 且回滚 |
| POST | `/api/motu/direct-mute` | 同上。`mute: true` 是 final insert；`mute: false` 用最近快照恢复 280 格直通推子 | 同上 |
| POST | `/api/motu/restore` | 按 `MIXER_KEYS` 恢复，不含 `kMainTrim` | 无快照 400；指定 id 不存在 404 |

`trim_db` 是 0..=100 的整数，写入 `kMainTrim` 的字节 n，页面显示 −n dB。范围依据 CueMix 前端 `kMainVolCtrlInfo {min:100, max:0, u:"dB"}`。

## 和 client.py 的差别

协议以 `tools/motu/cuemix_client.py` 为准。下面这些是故意不一样，并写在 `docs/MOTU-CUEMIX.md`：

1. 不做 SysEx。mk5 走 WebSocket。
2. 不做 RSA-OAEP。代理子项表明已设密码时，返回「设备已设密码，暂不支持」。
3. 设备帧是一帧一条，和 `handle()` 一致。主机帧带长度并按 1400 字节分帧。
4. final insert 的候选集仍是 280 格，线上只写和当前值不同的格子。client.py `--mute-inputs` 会把已经是 0 的格子也放进计划；整份大写入会超时，所以这里只发差异。响应里 `candidates` 是 280，`written` 是实际条数。两边都只把推子写 0，不另写 `kiMixMute`。
5. 「恢复快照」只用 `MIXER_KEYS`，不恢复 `kMainTrim`。`mute: false` 只恢复 280 格直通推子，不动主机输入 18..29。
6. 字符串按 UTF-8 字符边界截到不超过 31 字节。client.py 按字节切 `[:31]`。ASCII 相同。
7. 服务留下链路本地地址 `169.254.111.200/16`。一次性探测脚本用完会删。
8. `kMainTrim` 写入上限 0..=100（依据 CueMix 前端 `kMainVolCtrlInfo`，2026-11-16 修正，原写 80）。
9. mock 比 2026-11-15 那份实测多了 Line 8 ← Line 8（in8→out8）。任务和校准记录需要这条，当时的 dump 没有。mock 是子集，不是 2498 条全量。实测记录是 6 条物理直通 + 12 条 USB；mock 是 7 条直通 + 12 条 USB = 19 个非零点。
10. mock 快照在内存里。真机写入 `[motu] snapshot_dir`，只留最近 20 份。
11. 未知 id、电平表、代理状态丢掉并计数，和 client.py 一样。
12. 没有后台重连。每次读、写各开一条连接。
13. 幻象电源只显示，不写。

## 命令和结果

在 `projects/tspi-audio-dsp/web`：

```text
cargo fmt --check          # 无差异
cargo check --offline      # Finished dev
cargo test --offline       # 121 单元 + 36 路由集成，全部通过
cargo build --release --offline
# Finished release profile [optimized] in 1m 09s
```

`TSPI_MOCK=1 cargo run --offline -- --mock` 现正在 `http://127.0.0.1/`（默认 80；非 root 或 80 被占用时加 `--listen 127.0.0.1:8088` 回退）。这是重启后的干净进程：首次打开会要求设置管理密码，mock 不写 `auth.json`。下面的 curl 读回是同一次演练里、重启前那次进程的记录。token 是 `mock-token`。

未认证：

```text
curl.exe -s -w "\n%{http_code}\n" http://127.0.0.1/api/motu/status
```

```json
{ "error": { "code": "unauthorized", "message": "需要登录或提供访问 token" } }
```

HTTP 401。

状态（节选，HTTP 200）：`online true`，`mock true`，型号 `UL5L`，固件 `2.1.2+3352`，UID `UL5LFE7E6E`，IP `169.254.111.126`，网口 `enx0001f2fe7e6f`，`main_trim 28`，`main_db -28.0`，`direct_count 7`，其中 `in7→out7` 推子 0.935、`in8→out8` 推子 1.0。幻象两路都是关。

没有 confirm（HTTP 400）：

```text
curl.exe -s -H "x-auth-token: mock-token" -H "Content-Type: application/json" -d "{\"mute\":true}" http://127.0.0.1/api/motu/direct-mute
```

```json
{ "error": { "code": "bad_request", "message": "写操作需要请求体 confirm: true" } }
```

final insert（HTTP 200）：

```json
{ "ok": true, "changed": 7, "written": 7, "verified": true, "rollback": false,
  "snapshot_id": "20261116T033300415Z-0000", "candidates": 280, "host_inputs_untouched": true }
```

读回：`direct_count 0`，`main_trim` 仍是 28。快照里非零点剩 12 条，全是 USB（`host: true`），`direct` 为 0。14 条总线、32 路输入。

恢复快照（HTTP 200）：`written 7`，`verified true`。读回 `direct_count 7`，别名回到 `in7→out7` 和 `in8→out8`，`main_trim` 仍是 28。

Main 音量（HTTP 200）：`{"trim_db":10,"confirm":true}` → `written 1`。读回 `main_trim 10`，`main_db -10.0`，直通仍是 7。

失败注入不在这个进程里做（会把正在看的页面写坏）。`tests/api.rs` 的 `motu_apply_fail_returns_rollback` 用 `TSPI_MOCK_MOTU_APPLY_FAILS=1`：HTTP 500，`ok false`，`written 0`，`rollback true`，`rollback_verified true`，`error.code` 为 `internal_error`，直通条数不变。

页面（浏览器，mock）：首次设置密码后进入「MOTU / CueMix」。设备卡是 UL5L / −28.0 dB / 直通 7 条，表里有 in7→out7 和 in8→out8。点「全部静音（会关闭 CueMix 输入直通）」并确认后，直通变成「暂无数据」，提示「已关闭 CueMix 输入直通（读回校验通过，written=7）」。点「恢复快照」后 7 条回来。把 Main 滑到 10 并确认后，设备卡是 `10（-10.0 dB）`。展开矩阵仍能看到 12 条 USB（USB Main L/R、Line 3–10、Phones）推子 1.000。

## 已知限制

- 不编辑 EQ、门、压缩、混响、单格矩阵、固件。
- 不实现设备密码登录。
- 幻象电源、原始 JSON、矩阵都是只读。
- 没有快照列表、下载、删除接口。页面只恢复最近一份。
- 没有后台重连，也没有把 8 秒轮询再节流。缓存默认 12 秒，写操作会绕过缓存。
- mock 的 in8→out8 是按任务补上的，不是 2026-11-15 dump 里的实测点。

## 留给真机核对

1. `kMainTrim` 的合法范围：已按 CueMix 前端源码确认 0..=100；写入卡在 0..=100。
2. `k48V` 的 index 是否就是 Mic/Line 1、2。页面只读，不写。
3. 设备已设网络密码时，现在直接拒绝。若要支持，再做 RSA-OAEP，不要把口令写进日志。
4. 真机 `enx*` 口没有地址时，服务会留下 `169.254.111.200/16`。确认不和现有地址冲突。
5. final insert 之后用独立读回确认 USB 18..29 的推子没变，并且 CueMix 输入直通已关。
