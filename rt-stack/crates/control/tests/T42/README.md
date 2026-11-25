# tests/T42 —— 自有 REST 控制面私有测试

- 任务：WBS T42（deps T05/T20/T60）；写域 crates/control/src/rest.rs + tests/T42 + evidence/T42。
- 权威输入：rt-stack/spec/compat-matrix.md R01-R51/§2.4/§3.7/X01-X22、core-v1 §5.3/§9.2/§6.1、rt-stack/README.md §3；
  DSP-HOST-DECISION §2/§5/§6；WBS T42 行与 §1/§7。
- 测试文件：rest_contract.rs（T42 REST 契约）+ matrix_routes.rs（F-21 矩阵↔路由一致性，7 tests：1 正向 + 6 负向）。
- 运行：cargo test -p khaen-control rest matrix_routes -- --nocapture（F-21 覆盖表）；全量 cargo test -p khaen-control rest
  （或 --offline；全部离线，不打开设备/不启服务/不跑真实 host）

## 注册方式（不要改 public_registration.rs）

cargo 不自动发现 tests/<Txx>/*.rs，公共注册属 integrator 独占。因此本文件由
crates/control/src/rest.rs 文件末的 #[cfg(test)] 子模块通过 include! 编译执行：

    #[cfg(test)]
    mod rest_tests {
        use super::*;
        include!("../tests/T42/rest_contract.rs");
        include!("../tests/T42/matrix_routes.rs");
    }

本任务未改 Cargo.toml / lib.rs / public_registration.rs / CLI / README（公共根归 integrator）。

## 与 T24 的关系（rest 复用 ws 实际 ack 口径）

rest.rs 的 prepare/commit actual ack 复用 khaen_compat::ws 的 PreparedConfig / CommitAck /
validate_commit_ack / WsOpError::from_rt_status / rest_equivalent（T24 唯一口径，不另起第二套
错误码/ack 判据）；commit 只有在 ws::validate_commit_ack 通过（blocks_committed>=1、ack_mask=
ACK_MASK(expected)、plan_version/graph/config_sha256/expected_workers 一致、未 rolled_back）后
才返回 200；否则 500 internal_error（不假 ack）。REST 面补回 R49 的 118 -> 409 device_params
(ws 表把 118 归 invalid_argument) 与 123 -> 409 crossfade_budget 两个单点。

## 覆盖分组

| 组 | 覆盖 |
| --- | --- |
| R01/R50/X01/X14 | healthz 匿名字段、安全头、API 版本头（缺省 v1 / 显式不兼容 400）、JSON 404、已知路径 405、尾斜杠规范化、routes() 清单 |
| R02-R06/K03 | auth status/setup/login/session/logout、PBKDF2 密码、token 三种头、失败 5 次 429 锁定、会话 Cookie+CSRF、header token 免 CSRF、有界会话/过期 |
| R49/K15/K53 | runtime status/config/volume/mute/mode/target/coeff、prepare/commit/abort、actual version+graph hash+epoch、幂等 commit、stale/invalidate/409、坏 engine ack -> 500 |
| R10/R11/A06/R12 | /api/mode GET 字段全集、PUT 202+actual ack、未知值 400/未启用 403/在途 409、script-status 200 available:false |
| R13/R14 | audio/format GET 实际值+limits、PUT 经 prepare/commit 生效、chunksize 仅 64/128 显式拒绝、失败不半生效 |
| R47/C07/X15 | input-mode GET 200 not_supported / PUT 400 not_supported；注册退役端点 410 + replacement |
| R48/K14 | 校准 job list/start/get/cancel/apply/retest/rollback；无 port 503；MemoryCalibration 回环；ScriptedCalibration 坏 ack/坏状态 -> 500 不假成功 |
| X02/X03/X21/X22 | 严格字段白名单、未知/缺失/类型/重复键/NaN/超深嵌套/非法 JSON、payload 413、错误不泄 token |
| N-01/F-13（task-205，cursor v1.0 P1） | 模式切换后不得用 controller 切换时的旧 sample_rate/chunksize/epoch 覆盖 inner 真实 commit：新增 switch→format commit→format/status 组合用例（chunksize=64、epoch=commit ack、sample_rate 保持 96k、device_owner 仍 playback）；format GET 与 volume/mute 回执显式带 engine/synthetic/actual，synthetic 时 source 不写 "internal RT host actual (T26)"；web 音量/静音/格式文案按 ack 的 synthetic 分支显示 |
| F-21 矩阵↔路由（task-184） | compat-matrix §3.7 的 R01-R51 落地表 ↔ RestService::routes()：处置/落地状态一致、ROUTED/RETIRED_ROUTE 路由已注册、DEFERRED 不注册、无孤儿路由、R 号唯一；6 条负向用例（缺路由/孤儿/DEFERRED 带路由/处置状态不一致/重复 R 号/§3 与 §3.7 不一致） |

evidence/T42/ 为真实执行日志/manifest；每次运行写入 --nocapture 的 SAMPLE 响应样例见
evidence/T42/api-samples.txt（由 cargo-test-rest-nocapture.log 提取）。
