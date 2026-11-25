# tspi-calibrate 离线验证报告（2026-11-15）

环境：Windows x64，Rust 1.99.1；未接触板端音频（24h soak 期间只做代码/离线）。

## cargo test
- **7/7 PASS**（0 failed）：
  - sweep 长度/电平/边缘 fade
  - Farina 反卷积：已知 37 样本延迟 + 反相 → 恢复误差 <0.75 样本、极性 -1
  - 分数延迟/相位斜率：合成通道可恢复
  - PEQ 拟合：3 个已知峰谷 → 8 段拟合 RMS <1 dB
  - 合并抵消检查：24 样本差在 1 kHz 处无校正时 margin < -20 dB；加校正后 > +3 dB（负/正例均验证）

## 离线端到端（无硬件）
命令：
```bash
tspi-calibrate simulate --dir examples/offline --channels 4 --delays 0,3.5,10,20 --polarities 1,1,1,1 --names FL,FR,SL,SR --pre-seconds 0.5
tspi-calibrate merge --dir examples/offline --out examples/offline/calibration.yml --names FL,FR,SL,SR --layout synthetic-4 --crossovers 80,2000
```
结果：
- 4 通道 delay 恢复：0 / 3.500 / 10.000 / 20.000 样本；极性均 normal；
- 归一化修正：20 / 16 / 10 / 0 样本，reference_index=3（修正=0）；
- **相位抵消检查 PASS，worst margin = 9.03 dB @ 19897 Hz**（80–20k + 80/2000 分频点）；
- 产物：`examples/offline/calibration.yml`、`.camilladsp.yml`、`.validation.json`、每通道 `*.analysis.json` / `*.fr.csv` / `cap-ch*.raw`。

反相示例：`examples/offline-invert/`（两通道一正一反）→ 校准极性修正后 merge 仍 PASS（6.02 dB @ 87.2 Hz），
说明 polarity 字段生效；未修正的反相场景由单元测试覆盖（预期 FAIL）。

## 已知限制 / 下一步
- 离线 simulate 只验证算法与 YAML/校验流程，不代替真实声学测量；
- PEQ 拟合目前只 Peaking、最多 32 段（契约 v1），架式/全参数化后续扩展；
- 板端（soak 结束后）：MOTU 1:1 → UMIK 实测 4 通道 + 合并验证；64/128 帧各一次播放/测量；H16 到货后 16ch。

## P0 修复后回归（2026-11-15）
- `cargo test`：**11/11 PASS**，新增 N1/N2/N3/N5 负向用例全部检测 FAIL；merge 正/负例仍 PASS/FAIL 符合预期。
- 离线示例重新生成并带 pair 检查：
  ```bash
  tspi-calibrate simulate --dir examples/offline --channels 4 --delays 0,3.5,10,20 --polarities 1,1,1,1 --names FL,FR,SL,SR --pre-seconds 0.5
  tspi-calibrate merge --dir examples/offline --out examples/offline/calibration.yml --names FL,FR,SL,SR --layout synthetic-4 --crossovers 80,2000 --pairs FL:FR:2000
  ```
  - 全局抵消检查 PASS（worst 9.03 dB @ 19897 Hz）；
  - pair FL:FR@2000 PASS（Δφ=-15.4°、CD(fc)=-0.08dB、minCD=-0.30dB、notch=5.45dB、ripple=0.51dB）；
  - `examples/offline/calibration.yml.pairs.json` 为 pair 报告。
- 注意：离线 simulate 仅算法验证；`measure-multi` 的公共时间基准/N1–N5 板端 T1/T4 需 soak 结束后实测。

## Round-2 P0 修复验证（2026-11-15）
- `cargo test`：**11/11 PASS**，N1 改为真实数据形态（FR phase=180° + polarity=-1）：校准前 pair FAIL、校准 polarity=invert 后 PASS。
- 用 upmix-dev 的 `scratch/calib-verify` 数据复现（release 二进制）：
  - `hand`（phase=0、polarity=-1）→ pair **FAIL**（Δφ=180°，符合“测量自相矛盾”）；
  - `hand180`（phase=180°、polarity=-1，真实反相形态）→ pair **PASS**（Δφ=0°、CD=0dB）；
  - 两者 calibration 的 FR polarity 都为 invert → 结论不再依赖相位编码方式。
- release：`target/release/tspi-calibrate.exe` 1,682,432 B，sha256 `e364fee99bb8189046c5054e80300fb848d11fea4005ca0ed6cb5ff285bdc6e7`。
- 离线示例仍 PASS：全局 worst 9.03dB、pair FL:FR@2000 PASS（Δφ=-15.4°、CD=-0.08dB）。
