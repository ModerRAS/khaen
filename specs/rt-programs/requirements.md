# 需求文档

状态：**已定稿（2026-11-23）**。技术方案见 `design.md`。任务见 `tasks.md`，已实施。

## 介绍

把皇帝位校准收进 `tspi-calibrate`。`tspi-rt-daemon` 的网页负责下发这次任务、显示进度和复测结论。正在播的实时链仍是一个 `tspi-rt-host` 进程。板上现在以 `tspi-rt-host-nsp16` 运行的那次本地构建，有用的部署差异收回这份 host；下次安装时服务名用 `tspi-rt-host`。按已确认的任务 11，安装不看 soak，但不跑可闻扫频。

皇帝位的求解、通过线和抽头规则仍以 `specs/emperor-focus/requirements.md` 为准。本需求只规定这三个程序怎么接。

## 沿用，不重开

1. 求解留在现有 `emperor` 库。校准程序调用它，不另写一套代价函数。
2. 通过线仍是同一麦克风、同一刺激、同一组喇叭下的复测衰减变短。预测卷积、系数 sha、数字对拍不算通过。
3. 主喇叭固定为该声道自己，低音效果声道的主喇叭是 SUB1。从喇叭由用户按声道选择，缺省为空，未选中的路径为零。
4. 支援频段上沿可调，缺省 150 Hz，高于 150 Hz 拒绝。滤波器长度按噪声底以上的可测尾巴。全部音箱里最高的正增益 G dB 配同一个预衰减 −G dB，各音箱一起衰减，相对声压不变；G 超过 20 则整套拒绝，不削顶。+20 dB 对应的预衰减就是 −20 dB。
5. 侧路系数是加在延迟直通上的修正。导出的对角只有 `g_pp`。幅度填谷、补谷、电声吸声、第一种模态均衡不进这条任务。
6. 服务端只有一直运行的 `tspi-rt-host`。daemon 不开声卡，也不并进 host。网页和 `tspi-cal` 都是它的客户端：命令行做完一条就退出，网页经 daemon 转发同一次请求。校准的播放、录音和求解都在 host 里。
7. 二进制名保持 `tspi-rt-host`。Dante 不另起进程。

## 已核对的程序现状

| 项 | 现状 |
| --- | --- |
| 求解 | `tspi-mimo-design` 的 `emperor` 已实现。现场编排还在 `tools/analysis/emperor_focus_field.py`，默认声道图是 8 宽容器，不是 MOTU 22 声道。 |
| 校准程序 | `tspi-calibrate` 已有扫频、PEQ、相位、编译 `plan.bin`、RT60，以及用 `--g-paths` 塞入现成系数的实验路径。皇帝位求解不在其中。走 host 的复测只做了立体声 2.0，7 声道 / 4.1.2 标着 `NOT_IMPLEMENTED`。真实采集默认是 MOTU `hw:UltraLitemk5,0` 22 声道、UMIK `hw:0,0` 第 0 路、周期 128、缓冲 512、电平 −12 dBFS。 |
| 网页后台 | `tspi-rt-daemon` 在跑，校准接口在。默认后端是合成测量和离线应答，回执 `synthetic=true`。`TSPI_CAL_JOB_BACKEND=real` 返回 503，不调用 `tspi-calibrate`，也不打开 MOTU。页面不能管理正在播的 host。 |
| nsp16 | 仓库里没有 `nsp16` 的 unit 或配置。板上的服务名是 `tspi-rt-host-nsp16`，进程仍是本地构建后推上去的 `tspi-rt-host`，配置在 `/etc/tspi-rt-stack/rt-host-nsp16.toml`。仓库里的通用 unit 仍叫 `tspi-rt-host.service`。7 声道那份是另一份部署文件 `tspi-rt-host-nsp7.service`，保留不动。 |
| soak | 2026-11-23 下午验收窗口结束前，不把这次改动安装到 sender-host、N150 或 MOTU，不重启正在播的 host。 |

## 需求

### 需求 1 - 校准程序收口

**用户故事：** 作为维护者，我在 `tspi-calibrate` 里完成一次皇帝位校准，不再靠旁边的脚本自己开关声卡。

#### 验收标准

1. When 用户选择皇帝位校准时，tspi-rt-host shall 在本进程内调用现有 `emperor` 库完成求解，并沿用该库已定稿的频段、抽头、增益和拒绝规则。`tspi-cal` 只作为发出这次请求的短命令，发完即退出。
2. When 这次任务采集时，tspi-rt-host shall 继续占用 MOTU `hw:UltraLitemk5,0` 的 22 声道，并打开 UMIK `hw:0,0` 的第 0 路。格式为 48 kHz、S24_3LE、周期 128、缓冲 512、电平 −12 dBFS。用户显式覆盖时用覆盖值。同一次校准模式里扫两遍：20–500 Hz 只交给皇帝位求解，20–20000 Hz 只交给目标 FIR。两份脉冲响应不混用。
3. When 布局为 4.1.2 时，tspi-rt-host shall 把播放通道 0、1、2、3、4、5、7 对应为 FL、FR、SL、SR、HFL、HFR、SUB1。
4. When 求解完成时，tspi-rt-host shall 导出侧路修正系数。对角只含该声道的 `g_pp`。从喇叭未选中时不导出该路径。
5. When 同一次任务做复测时，tspi-rt-host shall 用校准入口把扫频送进与 AES67 相同的 DSP 链，并用同一只麦、同一组喇叭录回来。低频比较走恒等 plan 加皇帝位侧路，先空侧路再带系数。全频比较关掉侧路，把 `P_post` 换成目标 FIR。衰减结论只取变短、未变短或不可证伪。预测衰减比不得写成复测结论。这两次比较都在同一进程内切换，不重启进程。
6. While 用户没有同时给出允许出声的确认，when 收到进入校准模式的请求时，tspi-rt-host shall 拒绝切换输入，并保持原来的播放。
7. When 这次任务运行时，tspi-rt-host shall 让皇帝位侧路系数和目标 FIR 各算各的。侧路系数不作为目标 FIR 的输入，目标 FIR 的结果也不进入幅度填谷的 SolveG。这两条原有命令保持原样。

### 需求 2 - 网页下发真实任务

**用户故事：** 作为听音者，我在网页上开始一次皇帝位校准，并看到这次真实复测的结论。

#### 验收标准

1. When 网页发起皇帝位校准时，tspi-rt-daemon shall 通过 Unix socket 让 `tspi-rt-host` 进入校准模式，并在页面上给出进度和需求 1 的复测结论。
2. When 这次任务真正采过音并完成复测时，tspi-rt-daemon shall 把回执标成该次任务自己的结论。合成后端的回执继续标 `synthetic=true`。
3. When 配置为真实校准后端时，tspi-rt-daemon shall 执行上述 socket 请求。该设置不得再以未接线为由返回 503。
4. While 后端仍是合成模式，when 页面显示结果时，tspi-rt-daemon shall 保持 `synthetic=true`，不得显示成已经在 MOTU 上生效。
5. The tspi-rt-daemon shall 自己不打开声卡，也不重启 `tspi-rt-host`。

### 需求 3 - 声卡交接

**用户故事：** 作为维护者，校准开始时节目和 AES67 让出输入，进程继续跑，结束后原来的输入再接回来。

#### 验收标准

1. When 真实皇帝位任务开始时，tspi-rt-host shall 停掉 AES67 和其他节目输入，改从校准入口取块，并打开 UMIK。MOTU 仍由本进程播放，进程不退出。
2. While 校准模式开着，when 有节目或 AES67 样本到达时，tspi-rt-host shall 不把它们送进 DSP 链，直到收到退出校准模式的请求。
3. When 任务结束、失败或被取消时，tspi-rt-host shall 关闭 UMIK，退出校准入口，恢复进入前的输入。恢复失败时页面标明播放未恢复。
4. While 生产 soak 未结束，when 这次改动被要求安装到 sender-host、N150 或 MOTU，或被要求重启正在播的 host 时，这三个程序 shall 拒绝。

### 需求 4 - 收回 nsp16 部署

**用户故事：** 作为维护者，我以后只维护一份 `tspi-rt-host`。16 声道这次推上去的有用配置留在这份里。

#### 验收标准

1. When 整理 host 部署文件时，tspi-rt-host shall 先对照板上 `/etc/systemd/system/tspi-rt-host-nsp16.service` 和 `/etc/tspi-rt-stack/rt-host-nsp16.toml`。当前 16 声道播放仍依赖、且仓库通用 host 部署里还没有的项，写入 `tspi-rt-host` 的部署文件。
2. When 下次安装这份 host 时，服务名 shall 为 `tspi-rt-host`。二进制名保持 `tspi-rt-host`。
3. When 对照完成时，tspi-rt-host 的说明 shall 列出收进来的项和故意不收的项。只改服务名、或只属于 7 声道 `nsp7` 的项不收进 16 声道这份。
4. While 板上的 `tspi-rt-host-nsp16` 仍在 soak，when 本需求落地时，tspi-rt-host shall 只改仓库里的部署文件，不替换、不重启该进程。

### 需求 5 - 这次不收的范围

**用户故事：** 作为维护者，我能看出哪些旧缺口这次不动。

#### 验收标准

1. When 本需求实现时，这三个程序 shall 不修改皇帝位代价函数、抽头上限和通过线。
2. When 同一次测量完成时，tspi-rt-host shall 只取出目标 FIR。目标曲线缺省 flat，向导选了 tilt10 时用 tilt10。模态段、宽带段和其他 IIR 双二阶不导出、不播放、不写入 plan。分频点和分频滤波器这次不设置、不生成。相位拟合和 RT60 的网页闭环仍不进入这次验收。
3. When 本需求实现时，tspi-rt-daemon shall 继续作为独立控制进程，不并进 `tspi-rt-host`。它只做 Unix socket 的客户端。
4. When 网页或命令行需要现场编排时，tspi-rt-daemon 或 `tspi-cal` shall 向 host 的 Unix socket 发请求，不调用 `emperor_focus_field.py`，也不再另开进程用 aplay 播放扫频。
