# 实施计划

状态：**已实施（2026-11-23）**。1–11 已落地。现场衰减结论仍是 `NOT_RUN`，这次没有出声扫频。

需求见 `requirements.md`。方案见 `design.md`。皇帝位求解、抽头上限和通过线仍以 `specs/emperor-focus/` 为准，本清单不重开。

- [x] 1. host 增加校准 socket 和虚拟输入
- 新增 `rt-stack/crates/rt-host/src/cal_control.rs`，并在 `lib.rs` 登记
- 缺省监听 `/run/tspi-rt-stack/host.sock`，可用 `TSPI_HOST_SOCK` 改路径
- 一行 JSON 请求：`status`、`enter`、`measure`、`solve`、`remeasure`、`leave`
- `run_command` 启动时绑定 socket。绑定失败只记警告，播放继续
- 在 plan-arena 块循环和旧 2 声道块循环里，`capture_input_phase` 之后覆盖输入。播放模式不改缓冲
- 校准模式清掉这块节目样本，扫频写进同一块。时钟仍是 MOTU 播放时钟。不添加内核回环声卡
- _需求: 1.1, 3.1, 3.2, 5.3, 5.4_

- [x] 2. enter 与 leave
- `enter` 没有出声确认时保持原来的 capture 源，不打开 UMIK，回执里 `capture_source_changed` 为 false
- `enter` 带出声确认时记下进入前的源，把 capture 源改成 `cal`，打开 UMIK `hw:0,0` 第 0 路。进程不退出，MOTU 仍由本进程播放
- 校准模式期间，AES67 和其他节目样本不到 DSP
- `leave` 关闭 UMIK，capture 源改回进入前的值。改不回去时回执 `playback_restored=false`
- 不调用 `systemctl`，不写 `90-safe-mute.env`，不走 `calibration_cli_stage.rs` 的 `CliSafety`
- _需求: 1.6, 3.1, 3.2, 3.3_

- [x] 3. 同一次校准模式扫两遍
- `measure` 只在已经进入校准模式、并且请求带出声确认时开始
- 第一遍 20–500 Hz、5 秒，只留下低频脉冲响应。第二遍 20–20000 Hz、5 秒，只留下全频脉冲响应。两份不混用
- 播放设备 `hw:UltraLitemk5,0` 22 声道，采集 UMIK `hw:0,0` 第 0 路，48 kHz，S24_3LE，周期 128，缓冲 512，电平 −12 dBFS。请求里的覆盖值优先
- 4.1.2 的播放通道 0、1、2、3、4、5、7 对应 FL、FR、SL、SR、HFL、HFR、SUB1
- 反卷积调用现有 `ir_extract::deconvolve`
- 进度写入任务目录的 `status.json`
- _需求: 1.2, 1.3_

- [x] 4. 在非实时线程里求解
- `solve` 不放在音频线程上
- 低频脉冲响应交给现有 `emperor::design_channel`。`supports` 缺省为空，未选中的从路径不导出。`f_hi` 缺省 150，大于 150 由该库拒绝
- 对角导出只有 `g_pp`
- 全频脉冲响应交给现有 `fit_peq`，只保留线性相位目标 FIR。缺省 flat，`target=tilt10` 时用 tilt10。模态段和宽带段丢掉，不导出、不播放、不写 plan
- 侧路系数不进入目标 FIR，目标 FIR 不进入现有 SolveG
- `summary.json` 写出 `verdict`、`synthetic`、`f_hi`、`supports`、`metric`、`engine_playable`、`target`、`target_fir_taps`。没有真实录音时 `synthetic=true`，`verdict=PREDICTED_ONLY`
- 不改 `emperor.rs` 的代价函数、抽头上限和通过线
- _需求: 1.1, 1.4, 1.7, 5.1, 5.2_

- [x] 5. 复测走同一条 DSP 链
- `remeasure` 带出声确认，扫频仍从 `cal` 虚拟输入进链
- 低频一对保持恒等 plan：先空侧路，再装上这次的 `g`，扫 20–500 Hz
- 全频一对关掉侧路：先让 `P_post` 为恒等，再换成目标 FIR，扫 20–20000 Hz
- 侧路打开时 `P_post` 仍须为恒等，沿用 `prepare_mimo12k_branch` 的 fail closed
- 衰减结论只比较低频一对，取 `FIELD_SHORTER`、`FIELD_NOT_SHORTER`、`FIELD_UNFALSIFIABLE` 之一。全频一对只留频响对照。预测衰减比不写入 `verdict`
- 两次复测结束前不自动 `leave`。切换发生在进程内
- _需求: 1.5, 5.2_

- [x] 6. 把 tspi-cal 改成短客户端
- 在 `rt-stack/crates/calibration/src/bin/tspi-cal.rs` 增加 `status`、`enter`、`measure`、`solve`、`remeasure`、`leave`
- 每条命令连接 socket，打印 host 的一行结果，然后退出
- 这些命令不打开 PCM，不调用 `aplay`，不调用 `tools/analysis/emperor_focus_field.py`
- 现有 `run` 和 `rt60` 保持原样
- _需求: 1.1, 5.4_

- [x] 7. daemon 转发同一组请求
- `TSPI_CAL_JOB_BACKEND=real` 改为连接 `TSPI_HOST_SOCK`，缺省路径与 host 相同
- `CAL_JOB_KINDS` 增加 `emperor`
- real 模式下 `kind=full` 和 `kind=retest` 返回 400
- socket 连不上时返回 503，不造一条测量记录
- `POST /api/calibration/jobs` 增加 `stage`，取 `measure`、`solve`、`remeasure`，并接受 `f_hi`、`supports`、`target`。这些字段原样送进 socket
- real 模式下 `measure` 和 `remeasure` 缺少 `audible_sweep_authorized=true` 时返回 400，host 不切换输入
- `synthetic` 模式的回执保持 `synthetic=true`，verdict 不写成 `FIELD_SHORTER`
- daemon 不打开声卡，不执行 `systemctl restart`，不再按 `TSPI_CAL_BIN` 拉起会自己播声的校准程序
- 进度读 `status.json`，结束读 `summary.json` 的 `verdict`
- _需求: 2.1, 2.2, 2.3, 2.4, 2.5, 5.3_

- [x] 8. 把 Dirac 向导接到这些请求
- 页面仍用 `web/demo/dirac-style-flow`，由 daemon 提供。双击打开时仍可读 `pilot.json`
- 从 daemon 打开时，第 5 步提交 `stage=measure`，第 7 步提交 `stage=solve`，第 8 步提交 `stage=remeasure`。回滚提交 `leave`
- 第 4 步只点亮皇帝位也可以进入测量。多出来的点和 ART 的 3 点要求留在页面上
- 第 6 步的瀑布和衰减画低频脉冲响应，频响画 20–20000 Hz 那一遍。模态表留在页面上，这次不填
- 第 7 步的目标曲线只有 flat 和 tilt10。低切只保留滚降警告。ART 滑杆和 0…−24 dB 的示意不出现在求解请求里。MIMO 区改成支援频段上沿和从喇叭勾选，缺省不勾选
- `web.rs` 里现有的 full / retest 按钮保持原来的请求
- 向导改完后在浏览器里走第 5、7、8 步，确认提交的是上述请求
- _需求: 2.1, 2.4, 5.2_

- [x] 9. 只读对照 nsp16
- 读取板上 `/etc/systemd/system/tspi-rt-host-nsp16.service` 和 `/etc/tspi-rt-stack/rt-host-nsp16.toml`
- 对照 `rt-stack/service/tspi-rt-host.service` 和现有 host 配置模板
- 当前 16 声道播放仍依赖、通用模板里没有的项，写进 `tspi-rt-host` 的部署文件
- 对照结果写入 `rt-stack/deploy/NSP16-FOLD.md`，列出收进来的项和故意不收的项。只改了服务名的行不收。`tspi-rt-host-nsp7.service` 保持原样
- 板上的文件读不到就停在这一步，不凭记忆写 toml
- 下次安装的服务名用 `tspi-rt-host`。二进制名保持 `tspi-rt-host`
- _需求: 4.1, 4.2, 4.3_

- [x] 10. 单测
- `TEMP` 和 `TMP` 指向 `C:/WorkSpace/Claw/scratch/lead/testtmp`
- 假 socket 上跑 `tspi-cal solve`：写出 `g_pp` 和 `target_fir_taps`，不写 IIR 段，`verdict=PREDICTED_ONLY`，`synthetic=true`，进程随后退出
- 真实设备请求缺出声确认时，capture 源不变，UMIK 不打开
- daemon 的 real 模式对着假 socket：`kind=full` 返回 400；socket 可用时 `kind=emperor` 不再是 503；`stage=measure` 和 `stage=remeasure` 不调用 `systemctl`。已授权的回执带 host 写出的 verdict，真实录音完成后 `synthetic=false`
- daemon 的 synthetic 模式里，`kind=emperor` 保持 `synthetic=true`，verdict 不是 `FIELD_SHORTER`
- host 在校准模式中不把 AES67 块送进 DSP，`leave` 之后恢复进入前的源
- 低频复测的块计数表明扫频经过侧路所在的同一条链。全频复测的侧路为关，第二次 `P_post` 不是恒等
- 这些测试不宣称房间里的衰减变短。现场仍写 `NOT_RUN`
- _需求: 1.5, 1.6, 2.2, 2.3, 2.4, 3.2, 3.3_

- [x] 11. 安装并做无声检查
- 按 2026-11-23 的口头口径，soak 不作为停手条件。这一条覆盖需求 3.4 和 4.4 里的安装拒绝，以本清单确认结果为准
- 先完成任务 9。替换二进制时沿用板上正在使用的 16 声道配置
- 装上之后只查询 socket 状态，并发送一次没有出声确认的 `enter`，确认 capture 源没有切换
- 不发送 `measure`，不发送 `remeasure`
- 不提交，不 push
- _需求: 3.4, 4.4_
