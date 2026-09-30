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

## 接下来

- [ ] 补齐 `rv32mi-p-breakpoint` 所需的调试触发模块（`tcontrol`/`tselect`/`tdata1-3`）。当前 runner 将其列为 excluded：该测试的处理程序在 `tselect` 写入陷阱时直接跳向 `fail`，因此没有触发模块的实现无法通过它。
- [ ] 补齐 `rv32mi-p-pmpaddr` 所需的物理内存保护（`pmpcfg0`/`pmpaddr0-15`）。模型目前完全没有 PMP。两者同样列为 excluded 而非通过。
- [ ] 实现中断交付：`mip`/`mie` 当前无法置起任何中断，也没有 CLINT/PLIC 或内存映射的 `mtime`。
- [ ] 增加 S/U 特权级：`mstatus.MPP` 不再硬连线为 M，`sret`/`sfence.vma` 成为合法编码，`ecall` 报告其来源模式。
- [ ] 定义稳定的每指令 trace 格式（`PC`、指令、寄存器/CSR 变化、内存写入）以供差分测试使用。这是 ROADMAP P3 的前置条件：格式应先于 RTL 冻结，否则 RTL 侧会被迫适配未定的格式。
- [ ] 引入外部参考以支撑差分测试。注意 **`spike` 不在 Ubuntu apt 源中**，`qemu-system-misc`（8.2.2）可用作替代；Spike 需从源码构建（依赖 autoconf/gmp/mpc/dtc，构建耗时较长）。

## 之后

- [ ] 实现压缩 `C` 扩展以达成 RV32IMC 目标配置。`Trap::Unsupported("C")` 已能在遇到该编码时报告。
- [ ] 构建第一颗单周期 RTL 核并与本模型对比。
- [ ] 添加受限随机的指令生成与基于 Spike 的差分测试。
- [ ] 添加总线、boot ROM、RAM、UART 与中断控制器模型以支持 SoC 集成，并以 CLINT `mtime` 取代模型时间基。

## 本地环境说明

以下为本开发环境的实际情况：

- `cargo`/`rustc`（1.97.1）**已在 `PATH` 上**（`~/.cargo/bin`）。若在其他环境缺失，添加 `export PATH="$HOME/.cargo/bin:$PATH"`。
- RISC-V 交叉工具链（`riscv64-unknown-elf-gcc` 13.2.0）已安装，`scripts/riscv-smoke.sh --require` 与 `scripts/riscv-tests.sh --require` 均可在本地完整跑通。
- **未安装** Spike、QEMU、Verilator、yosys。`spike` 不在 Ubuntu apt 源中，需从源码构建。
- 75 个 Rust 测试不依赖任何外部工具，`cargo test --all-targets` 即可全部运行。但它们使用手工编码的指令字，**不**覆盖编译器产生的真实编码——那部分由 `riscv-smoke` 与 `riscv-tests` 验证。
- `scripts/riscv-tests.sh` 默认克隆到临时目录并在结束时删除。用 `RISCV_TESTS_DIR=/path` 复用已有克隆/构建树时，该目录**永远不会被脚本删除**（这是刻意的：一个脚本不应删除调用者指定的目录）。
