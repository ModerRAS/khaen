# T25 私有测试（输入内容检测 S1 兼容 + Aux1/Aux2 fader）

- 任务：WBS T25（rt-stack compat leaf content；deps T24/T23/T44）；写域 crates/compat/tests/T25/。
- 被测模块：crates/compat/src/content.rs（S1 契约兼容资料：content 检测/双路径 fader/状态机；
  P1 显式拒绝；**P1 不消费**，见 compat-matrix §6 C01–C08/R47、DEC §1/§6）。
- 运行（rt-stack/ 下）：
  ```
  cargo test -p khaen-compat content     # DoD 命令（测试名统一 content_ 前缀）
  cargo test -p khaen-compat             # 整 crate 回归（T13 + T24 + T25 + T57）
  ```
  Cargo 自动把 tests/T25/main.rs 发现为独立集成测试 target（与 T13/T24/T57 同机制），
  不需要 integrator 在 public_registration.rs 注册。

覆盖分组（92 tests）：
- A 契约常量/枚举/错误码；H16 role→index 与 overlay layout-presets 只读一致性；
- B detect_content：LFE 永不作判据、C/SL/SR/height 判多声道、进入阈值严格大于、
  迟滞带 unknown、置信度公式、active_channels 排序/阈值、未知角色不参与判定（D-2）；
- C W18 GetCaptureSignalRms 解析（长度/有限/-240 floor）与 role 映射；
  W19 SetFaderVolume [id,db] 形状/范围与命令 JSON；
- D Aux1/Aux2 0/-120 dB 绑定、双上混=0 不变量、切换计划先关后开、原子提交/失败安全；
- E override > detector > auto 合并视图（C03）与 input-mode override JSON/别名归一化；
- F S1 autodetect daemon 状态机：启动冻结 2500 ms、hold 2500 ms、迟滞 6 dB、manual override、
  fader 错误失败安全、WS error 合成 state、RMS 0.1 dB 取整；
- G khaen-input-content/v1 state JSON 字段顺序/往返/严格解析/前向未知键容忍；
- H P1 显式拒绝：R47 状态壳 NOT_SUPPORTED、PUT not_supported、S1 状态不得驱动播放、
  Aux 路径切换拒绝、S1 退役 unit 依赖证据（只读读取实际 unit 文件）；
- I 命令形状、全模式对切换计划安全性、P1 只读快照不变量。

测试纯内存、无网络/无板端/无临时文件（唯一文件读取是只读输入：layout-presets 与 S1 unit）。
