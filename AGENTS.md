# AGENTS.md —— khaen 仓库内的工作协议

> 本文件是**任何 agent 在本仓库动手前必读**的统一规则。lead 派活时只需要给「任务 + 写范围 + 验收判据」，
> 通用纪律不再逐条复述。与上级目录 `C:\WorkSpace\Audio\AGENTS.md` 冲突时，以本文件为准（更具体）。

## 1. 仓库与分支

- 仓库：`C:\WorkSpace\Audio\khaen`，远端 `https://github.com/ModerRAS/khaen`，工作分支 **main**。
- **只有一个工作树**：禁止 `git worktree`、禁止克隆第二份、禁止复制仓库到 tmp 里改。
- **禁止 push**。push 只由 lead 执行，且需用户明确许可。
- 旧实现只读：`git show legacy/pre-rewrite:<path>` 读文件；**绝不 checkout 该分支**、不切换到别的工作树。

## 2. 写范围（write scope）

- 每个任务由 lead 指定写范围（文件或目录）。**只改自己范围内的文件**；越界先问。
- 公共文件（根 `Cargo.toml`、`Cargo.lock`、`docs/`、`.github/`、`README.md`、`deploy/`、各 crate 的 `lib.rs`）默认不归 worker；需要时 lead 提前把模块注册好（stub + `lib.rs` 一行），worker 只填自己模块。
- 发现别人未提交的改动：**不要覆盖、不要 stash、不要提交**；先报告。

## 3. 提交纪律（硬性）

工作树是多 agent 共享的，提交必须只含自己的文件：

```
git add <显式路径 1> <显式路径 2> ...
git commit -m "<前缀>: <一句话>" -- <同样路径 1> <同样路径 2> ...
```

- **禁止**：`git add -A`、`git add .`、`git commit -a`、`git commit -am`、不带 pathspec 的 `git commit`。
- **禁止**：`git checkout -- .`、`git restore`、`git reset --hard`、`git clean`、`git stash`（会毁别人的在途改动）。
- 不提交 `Cargo.lock`（由 lead 在集成时统一提交），除非任务明确要求。
- 不提交证据/临时文件：`evidence/`、`tmp/`、`*.pcap`、`*.wav`、`target/`、`dist/` 已被 `.gitignore` 挡住，也不要强行 `-f`。
- 提交前 `git status --short` 自检：只出现自己的路径；`git show --name-only HEAD` 复核。
- `index.lock` 冲突（多 agent 并发）：等 1 秒重试，**不要删锁文件**。
- 提交信息前缀用任务号：`T21:`、`T25b:`、`T70a:` …… 一句话说清做了什么。

## 4. 门禁（提交前必须全绿）

在仓库根执行（PowerShell 会把 cargo 正常 stderr 显示成 `NativeCommandError`，**以 `$LASTEXITCODE` 和 `test result: ok` 为准**）：

```
cargo fmt -p <你的 crate>                 # 只格式化自己的 crate；禁止 cargo fmt --all 应用
cargo clippy -p <你的 crate> --offline --all-targets -- -D warnings
cargo test -p <你的 crate> --offline
```

- 全仓门禁（`--workspace`、`fmt --all --check`）由 lead 在集成时跑；worker 不要跑（会碰到别人在写的文件）。
- 依赖离线：`--offline`；本机 cargo 缓存里没有的 crate 不要擅自加，先问 lead。
- **Windows 本机必须能跑的测试才算完成**；平台相关代码用 `#[cfg(target_os = "linux")]` 隔离，逻辑做成平台无关 + 可注入假实现，并在报告里标注 Linux-only 部分为 NOT_RUN（由 CI 覆盖）。

## 5. 架构约束（写代码前读 docs/）

- 三份契约：`docs/CONTRACTS.md`（方案文件 ABI v3 / 主机控制协议 / 校准产物 manifest）。
- 设计：`docs/DESIGN.md`（数据流、延迟、采样率所有权、设备生命周期）。
- 实时路径（`BlockEngine::run_block`、`Processor::process`、`AudioBackend::capture/playout`、任何标了 RT 的函数）：
  **零分配、零锁、无 IO、不 panic、无日志**；所有缓冲在构造期分配；错误通过返回值/状态上报。
- 能力上报必须真实：没实现的 op 返回 `unsupported` 并进 `not_wired`，**不许假成功、不许编数值**；没有数据来源的字段给 `null` + 原因。
- 迁移代码不改 v1 的数值行为；发现的旧缺陷写 `// NOTE(legacy-defect): ...` 并报告，不擅自"修"。

## 6. 设备与环境操作

本仓库只约束**代码与提交**。设备（N150 / MOTU / Zidoo / 泰山派）与环境操作规范属于**工作区级规则**，写在仓库外的 `C:\WorkSpace\Audio\AGENTS.md`，不随本仓库发布；在那里动手前先读它。

## 7. 交接

- 不写 `HANDOFF/`（仓库外，由 lead 统一写）、不改根 `README`/`docs`（除非任务就是文档）。
- 完成后报告四件事：① 文件清单；② 测试数与覆盖点；③ 三条门禁的退出码/结果；④ 未验证项（明确写 NOT_RUN 及原因）。

## 8. 方案写作与决策变更守卫（2026-11-27 立）

写方案、设计、实现时，必须让“为什么”可追溯。

### 8.1 方案必须包含理由

每个方案/设计决策至少写：

- 背景/前因：为什么现在做；
- 备选方案：至少写出被否掉的选项；
- 选择与理由：为什么选这个、不选那个；
- 代价/边界：增加什么复杂度、什么情况下不成立；
- 违背信号：后续出现哪些做法说明偏离了原目的；
- 实测 / 预测 / NOT_RUN 分清。

不能只写“做什么”。已确认决策记录在工作区
HANDOFF/20261127-khaen-requirements-alignment/DECISIONS.md；新决策追加，不删旧理由。

### 8.2 发现会违背已确认目的时

worker 必须停下并报告 lead，不得自行改方向，也不得先按新方案做完再说。lead 向用户完整复述该决策块：

1. 决策编号 + 标题；
2. 当初的背景/原始目的；
3. 当时的选择和理由；
4. 当前方案具体哪一步会违背；
5. 后果；
6. 替代方案 A/B；
7. 请用户裁决：保持原目的，还是明确改变它。

不能只报编号让用户自己回忆。用户确认改变后，更新 DECISIONS.md（保留旧理由，追加新决策/原因），再实施。

禁止：只报编号；发现冲突仍先实现；为通过方案删掉/弱化原决策理由；worker 自行改方向。

