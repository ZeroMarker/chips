# 工作清单

Rust 黄金参考模型及其未来 RTL 对应实现的工作台账。条目按依赖关系与验证价值排序；项目里程碑见 [`ROADMAP.md`](ROADMAP.md)，模型设计见 [`ARCHITECTURE.md`](ARCHITECTURE.md)。

## 已完成

- [x] 执行 RV32I 基础整数指令集。
- [x] 执行 RV32M 乘除扩展（含除零与有符号最小值除 `-1` 的边界语义）。
- [x] 实现全部六条 Zicsr 读改写指令。
- [x] 拒绝写入架构上只读的 CSR。
- [x] 陷阱化未对齐的取指、加载与存储地址。
- [x] 拒绝保留的移位、`jalr`、`fence` 与 SYSTEM 编码。
- [x] 通过 RV32 低/高 CSR 对暴露可用的 64 位 `cycle` 与 `instret` 计数器。
- [x] 经 `mtvec`、`mepc`、`mcause`、`mtval` 将同步异常路由到机器级陷阱入口；已安装处理程序时 `ecall`/`ebreak` 同样进入。
- [x] 实现 `mret` 与本 hart 所需的 `mstatus` 机器级字段（`MIE`、`MPIE`、`MPP`）。
- [x] 为 `time`/`timeh` 提供每步推进一次的时间源。
- [x] 实现 CSR 字段语义：`mstatus`/`mtvec`/`mepc` 的 WARL 合法化，常量 `misa`/`mhartid`，仅 M 级的 `mie` 掩码，惰性的 `mip`，可写的机器级计数器，以及对未实现 CSR 地址的陷阱。
- [x] 将指令覆盖改造为表驱动测试，覆盖每一条 RV32I、RV32M、Zicsr 操作及架构边界情况（`tests/isa_coverage.rs`、`tests/traps.rs`）。
- [x] 改进 CLI：可配置指令上限、ABI 寄存器名、计数器输出、陷阱诊断。
- [x] 添加 CI（`.github/workflows/ci.yml`）：格式化、Clippy（警告视为失败）、rustdoc，Linux/macOS/Windows 双 profile 测试，以及 `riscv-smoke` job——用真实 RISC-V 交叉工具链汇编 `scripts/smoke.S` 并在模型上运行该镜像。
- [x] 补齐 [`README.md`](../README.md)：构建、测试、裸二进制生成、CLI 用法、CI 覆盖与不覆盖范围。
- [x] 补充 [`ARCHITECTURE.md`](ARCHITECTURE.md)：模块划分、单步执行、陷阱与 CSR 语义、内存模型、不变式与扩展指引。
- [x] 文档结构重整：文档统一为中文，路线图/清单/背景参考移入 `docs/`，交叉引用与代码注释同步更新。
- [x] 给内存模型加入解码后的地址映射与访问故障（`InstructionAccessFault`/`LoadAccessFault`/`StoreAccessFault`），并为 RAM 提供 O(1) 的平坦后备（`src/mem.rs`、`tests/memory.rs`）。
- [x] 实现 HTIF `tohost`/`fromhost` 协议与 `riscv-tests` 目标平台定义（`src/htif.rs`、`src/platform.rs`）。
- [x] 把工具链冒烟测试扩展为真正的 `riscv-tests` runner：`scripts/riscv-tests.sh` 克隆、配置（XLEN=32）、构建并运行 `rv32ui-p-*`/`rv32mi-p-*`，经 `--htif` 读取 `tohost` 判定 pass/fail。**58 个测试中 56 个通过，2 个 excluded。**
- [x] 将 `riscv-tests` 接入 CI（`riscv-tests` job）。
- [x] 修正两处由官方套件暴露的架构缺陷：写入 `minstret` 必须抑制该指令自身的计数增量（`instret_overflow`）；非对齐**数据**访问应当完成而非陷阱（`ma_data`）。
- [x] 实现中断交付：新增 CLINT（`msip`/`mtimecmp`/`mtime`，SiFive FE310 布局，`src/clint.rs`），`mip` 由设备驱动且对软件只读，`time`/`timeh` 跟随 CLINT；中断在步骤边界交付，`mcause` 为中断位本身，向量化 `mtvec` 首次真正生效（`tests/interrupts.rs`，19 个测试）。
- [x] 增加 S/U 特权级：`Privilege` 枚举（判别值即架构编码，`U < S < M`），`MPP` 改为合法化 WARL 字段，陷阱入口记录来源模式，`ecall` 依模式报告 8/9/11，`sret` 读 `SPP`（单比特，故不能返回 M），`sfence.vma` 在 S 及以上合法并空转；`sstatus`/`sie` 是 `mstatus`/`mie` 的**窄视图**而非独立存储；`satp` 存在但合法化为 Bare（`tests/privilege.rs`，22 个测试）。
- [x] 定义稳定的每指令 trace 格式（`src/trace.rs`）：每步一行，`i`/`t`/`x` 三种记录，语法在代码中显式规定而非由实现涌现；变化在已有的写入汇聚点采集，因此写入同值的寄存器也会出现；`--trace` 只输出 trace。刻意不含周期数与内存读（`tests/trace.rs`，23 个测试）。
- [x] 引入 Spike 作为外部黄金参考（`scripts/difftest.sh` + `scripts/difftest.S` + `scripts/difftest.ld`）：逐步比较 PC、指令字、目的寄存器与其值、以及内存写入的地址与值。**225 步程序，两侧完全一致。**

## 接下来

按依赖顺序：前三项是解锁 `rv32mi-p-illegal` 的条件，第四项扩大差分覆盖，最后一项面向 RTL。

- [ ] 实现**陷阱委派**（`medeleg`/`mideleg`），并按委派把陷阱路由到 `stvec` 而非 `mtvec`。这是 `rv32mi-p-illegal` 被列为 excluded 的直接原因：模型现在有 S/U 特权级，该测试的探测不再短路，于是会继续走 `mideleg`、向量化的监管级中断、以及 `TVM`/`TSR`/`SUM`/`MXR` 的门控路径。相关联地还需要：
  - [ ] `mstatus.TVM`/`TSR`——在 M 级屏蔽 `sfence.vma` 与 `satp` 访问、在 S 级屏蔽 `sret`。
  - [ ] `sstatus.SUM`/`MXR`——S 级是否可访问 U 页面的内存、以及是否可取指令页。两者都可报告为不支持，但必须**显式**报告而不是让 `sret`/`sfence.vma` 静默非法。
  - [ ] S 级中断交付：被委派的 MSI/SEI 经 `stvec` 进入，含向量模式。
- [ ] 补齐 `rv32mi-p-pmpaddr` 所需的物理内存保护（`pmpcfg0`/`pmpaddr0-15`）与 `rv32mi-p-breakpoint` 所需的调试触发模块（`tcontrol`/`tselect`/`tdata1-3`）。两者当前是 excluded 而非通过，理由写在 `scripts/riscv-tests.sh` 的注释里。
- [ ] 扩充差分比较的范围。当前 `scripts/difftest.sh` 只比较**已退休**的指令——这是 Spike 提交日志唯一记录的东西——因此 CSR 写入完全不在交集内。补充比较最终架构状态（全部寄存器 + 可读 CSR）与 CSR 写入序列，才能覆盖 trap 入口的 `mepc`/`mcause`/`mtval` 写入。
- [ ] 收敛差分测试的程序集。`scripts/difftest.S` 刻意避开了两类行为：非对齐数据访问（Spike 陷阱、模型完成，**两者都合规**，而一条分歧指令会使其后每一条都错位）与取值由实现决定的寄存器（`misa` 报告扩展位，`cycle`/`time`/`instret` 的值只在各自机器上有意义）。每排除一类都缩小了覆盖，因此需要更多程序来补上——这也是「受限随机的指令生成」的前提。
- [ ] 为差分 harness 固定 Spike 的构建方式。Spike **不在 Ubuntu apt 源中**，本环境从源码构建（`riscv-isa-sim`，依赖 autoconf/gmp/mpc/dtc）；注意**代理内核 `pk` 已从 riscv-isa-sim 中移除**，因此 Spike 内建 HTIF 与 `riscv-tests` 的 `tohost` 约定并不相同——这正是 `scripts/difftest.S` 用自旋地址而非 `tohost` 报告结果的原因。

## 之后

- [ ] 实现压缩 `C` 扩展以达成 RV32IMC 目标配置。`Trap::Unsupported("C")` 已能在遇到该编码时报告。
- [ ] 构建第一颗单周期 RTL 核并与本模型对比。
- [ ] 添加受限随机的指令生成与基于 Spike 的差分测试。
- [ ] 添加总线、boot ROM、RAM、UART 与中断控制器模型以支持 SoC 集成，并以 CLINT `mtime` 取代模型时间基。

## 本地环境说明

以下为本开发环境的实际情况：

- `cargo`/`rustc`（1.97.1）**已在 `PATH` 上**（`~/.cargo/bin`）。若在其他环境缺失，添加 `export PATH="$HOME/.cargo/bin:$PATH"`。
- RISC-V 交叉工具链（`riscv64-unknown-elf-gcc` 13.2.0）已安装，`scripts/riscv-smoke.sh --require` 与 `scripts/riscv-tests.sh --require` 均可在本地完整跑通。
- **Spike 已从源码构建并可用**，位于 `/tmp/opencode/spike-install/bin/spike`（`riscv-isa-sim`，`--prefix=/tmp/opencode/spike-install`）。`spike` 不在 Ubuntu apt 源中，**QEMU、Verilator、yosys 仍未安装**。运行差分测试：`export PATH="/tmp/opencode/spike-install/bin:$PATH" && bash scripts/difftest.sh`。
- 注意默认 `make` **不构建代理内核 `pk`**，且 `pk` 已被移出 riscv-isa-sim，因此 Spike 内建 HTIF 与 `riscv-tests` 的 `tohost` 约定不同：Spike 会把 `tohost` 的高字节当作 syscall 号。差分测试因此不使用 `tohost` 报告结果。
- 139 个 Rust 测试不依赖任何外部工具，`cargo test --all-targets` 即可全部运行。但它们使用手工编码的指令字，**不**覆盖编译器产生的真实编码——那部分由 `riscv-smoke` 与 `riscv-tests` 验证。
- `scripts/riscv-tests.sh` 默认克隆到临时目录并在结束时删除。用 `RISCV_TESTS_DIR=/path` 复用已有克隆/构建树时，该目录**永远不会被脚本删除**（这是刻意的：一个脚本不应删除调用者指定的目录）。本环境的复用目录是 `/tmp/opencode/rt`。
- `scripts/difftest.sh` **不属于门禁**：Spike 不在 apt 源中，把它设为 CI 必需项意味着每次运行都要从源码构建。缺 Spike 时该脚本打印 `SKIP` 并以 0 退出；`--require` 可让缺失变成失败。
- 当前 `riscv-tests` 成绩：**55 通过、0 失败、3 excluded**（`rv32mi-p-breakpoint`、`rv32mi-p-illegal`、`rv32mi-p-pmpaddr`）。排除理由写在 `scripts/riscv-tests.sh` 的注释中。
