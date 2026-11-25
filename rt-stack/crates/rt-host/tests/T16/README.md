# tests/T16 —— T16（spsc）私有模型测试

- 归属：WBS T16 / team task-73，owner rt-spsc；写域仅
  `crates/rt-host/src/spsc.rs` + `crates/rt-host/tests/T16/` + `rt-stack/evidence/T16/`。
- 测试文件：`spsc_model.rs`（38 个 #[test]，函数名统一 `t16_*`；含内部 `alloc_probe` 计数分配器）。
- 运行（在 `rt-stack/` 下）：

```
cargo test -p khaen-rt-host spsc
```

- 期望：`running 38 tests ... 38 passed; 0 failed`（executed test_count = 38 > 0）。

## 为什么用 include! 而不是 public_registration.rs

`rt-stack/README.md` §3.1/§5 规定：`tests/public_registration.rs` 与公共注册**只由 integrator 写**；
cargo 只把 `tests/*.rs`（顶层文件）当集成测试目标，**不会自动发现** `tests/T16/` 子目录。

因此 T16 采用不碰公共注册的合法方式：`crates/rt-host/src/spsc.rs` 末尾

```rust
#[cfg(test)]
mod spsc_tests {
    include!("../tests/T16/spsc_model.rs");
}
```

本文件因此参与 lib 的单测构建，真实被 `cargo test -p khaen-rt-host spsc` 执行
（executed = 38，测试名过滤路径含 `spsc`）；**不需要 integrator 注册，也不要再把它加到
public_registration.rs**（会重复注册/编译失败：文件按模块内联语义编写，依赖 `super::*` 作用域）。

## 测试覆盖（对照 core-v1 §6.3 / §11.7 / §12.3）

| 组 | 覆盖 |
|---|---|
| A 构造/视图 | 固定容量 4、magic/ABI/entry/容量校验、`DescSlot::slice_from_raw` 空指针/未对齐/长度 |
| B 发布消费 | 单块 roundtrip 逐字段一致、FIFO、空环 None/`RT_E_QUEUE_UNDERRUN`、frame_index = block_seq×B（64/128）、DRAIN_TAIL 部分帧 |
| C overrun/槽回收 | 满时丢**新**块且不覆盖未读块、`dropped` 计数、消费后复用槽、非法 desc 不计 overrun、P-09 `next_slot` 预检查（满时不得写 payload）、queued/free_slots |
| D 发布前校验 | 未知 flag 位、VALID=0 占位块、valid_frames < B 仅 DRAIN_TAIL、slot_index 跨环、epoch 倒退、frame_index 不一致、block_size/通道越界、`validate_block_desc` 13 例错误码表 |
| E 无半块 | 已写 desc 槽但未 Release 发布 → 消费者不可见（P-05）；只读已发布槽 |
| F wrap | write_pos=u64::MAX→0 边界回绕（wrapping，无 panic）、1000 块槽位轮转 |
| G cap→DSP→pb | 双层环 lock-step 交付（payload marker/epoch/frame 连续）、DSP 停顿时 capture overrun→恢复出现 block_seq gap、playback 满时 DSP 输出丢弃→恢复 |
| H 多线程 | 20k 块单写单读真实线程压测：Release/Acquire 下 desc 逐字段完整、无撕裂、无丢失（有 30s 无进展保护） |
| I 无锁/无分配/RT 类型 | 计数分配器证明 push/pop 期间 0 分配；源码 token 扫描（锁/owned/分配/syscall/panic/SeqCst 等）；RT 可见类型 needs_drop=false + 64B 布局 |

## 限制（NOT_RUN）

- 未做 Miri/ThreadSanitizer 数据竞争验证（工具链未安装；建议 integrator 在 CI 增加，见 evidence/T16/MANIFEST.md §8）。
- 未接真实 ALSA/板端；payload 内存与 deadline/gate 归 T17/T19/T21。
