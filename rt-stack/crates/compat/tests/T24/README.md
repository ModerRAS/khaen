# T24 私有测试（WS 控制面）

- 任务：WBS T24（P1.C-host / control-author，deps T05/T20）；写域 crates/compat/tests/T24/。
- 被测模块：crates/compat/src/ws.rs（W01–W22 必需子集、prepare/commit 实际 ack、
  GetCaptureSignalRms、rate/volume/mute/fader、未知命令拒收）。
- 运行：在 rt-stack/ 下 ```
  cargo test -p khaen-compat ws            # DoD 命令（测试名统一 ws_ 前缀）
  cargo test -p khaen-compat               # 整 crate 回归（T13 + T24 + T57）
  ```

Cargo 自动把 tests/T24/main.rs 发现为独立集成测试 target（与 T13/T57 同机制），
**不需要** integrator 在 public_registration.rs 注册（该文件归 integrator 独占）。

测试夹具：FakePlane 实现 khaen_compat::ws::WsControlPlane，模拟 T26 宿主控制面
（运行状态/音量/mute/actual hwparams/RMS/fader/prepare-commit-ack 全部可注入），
全部内存夹具、无临时文件、无网络/无板端。

覆盖分组：
- W01 帧规则（文本/Binary 报错/Ping-Pong/Close/非法 UTF-8/超帧上限）
- W02/W03/W04/W21 请求回复形态、顺序多命令、非法帧不 panic
- W05–W15 读命令（含 actual 语义、u64 精确、NaN/Inf 不上线）
- W16 prepare/commit 实际 ack（blocks<1、partial ack、版本/图/配置 hash 不一致、
  rolled_back、版本未推进、worker 数不一致全部 commit_not_acked；失败触发 abort）
- W17/W09 音量/静音（范围/越界/写后读回/互不覆盖）
- W18 GetCaptureSignalRms（长度=采集通道、有限、-240 dBFS floor）
- W19 fader（已知/未知 id、载荷形状、越界、读回）
- W20/W22 未知命令与 GetXruns 拒收
- 契约级：命令表完整性、错误码表、状态/停止原因枚举、RtStatus 映射、
  ack 判据公共函数（T42 复用）、JSON 编解码、限制/统计
