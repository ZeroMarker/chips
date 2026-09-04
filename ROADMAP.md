# RISC-V 芯片研发路线图

> 配套文档：`RISC-V.md`（ISA 技术参考）
> 本路线图为「软硬件结合」路线：**Rust 功能模型**（黄金参考模型 + 验证骨架）与**硬件 RTL 实现**两条并行轨道，以**交叉验证**为耦合主线。
>
> 当前实现状态和按优先级排列的工作项见 [`TODO.md`](TODO.md)。路线图描述长期里程碑，TODO 清单记录可执行任务及其完成状态。

## 0. 目标与总体策略

- **目标**：实现一颗 RISC-V 处理器（软核 → SoC），软件层能跑真实裸机/`no_std` 程序，硬件层通过相同测试程序；两条轨道共享同一套验收标准。
- **目标配置**（可调）：`RV32IMC` 起步，进阶 `RV64GC`。
- **双轨道策略**：
  - **轨道 A（Rust 软件模型）**：作为可执行规范与黄金参考（golden reference），同时是测试激励生成、差分验证的裁判。
  - **轨道 B（RTL 硬件实现）**：Verilog/SystemVerilog 描述，走 RTL → 仿真 → 验证 → SoC 集成。
- **交叉验证原则**：两条轨道必须跑**同一套程序**并产生**一致结果**；任何不一致都是缺陷（而非只怪一方）。
- **技术选型**：Rust（`cargo`）、Spike/QEMU（外部参考）、Verilator（RTL 仿真）、`riscv-tests` / `riscv-dv`（测试集）、yosys/OpenLane（开源综合/实现）。

---

## 1. 阶段总览

| 阶段 | 时长(参考) | 轨道 | 里程碑 |
|------|-----------|------|--------|
| P0 基线 | 1 周 | 双 | 锁定 ISA 配置与工具链，验证框架可用 |
| P1 RV32I 模型 | 2 周 | A | Rust 模型通过 `riscv-tests`，与 Spike 一致 |
| P2 RTL 单周期核 | 2 周 | B | 单周期 RV32I 核可综合、仿真通过基线测试 |
| P3 交叉验证 | 并行 | 双 | 差分测试框架建立，RTL vs 模型状态一致 |
| P4 扩展(IMC/A/F/D) | 4–8 周 | A→B | Rust 与 RTL 同步补齐扩展并一致 |
| P5 流水线微架构 | 4 周 | B | RTL 5 级流水线，功能等价 |
| P6 SoC 集成 | 4 周 | B | 总线+内存+外设，裸机/`no_std` 程序 boot |
| P7 验证工程化 | 持续 | 双 | 随机/定向回归、覆盖率 | 
| P8 物理实现(可选) | 长期 | B | 综合/FPGA/OpenLane 流程 |

> 时长仅为量级参考，按人手/目标配置伸缩。

---

## 2. P0 基线

**要点**：先定规范与工具，避免后期返工。

- 锁定 ISA 配置（如 `RV32IMC`）与预期特权级（M，或 M+U/S）。
- 确立工具链：
  - Rust toolchain + `riscv32imac-unknown-none-elf` / `riscv64gc-unknown-none-elf` target。
  - Spike / QEMU 作为**外部黄金参考**。
  - `riscv-tests`（riscv-isa-sim 的测试集）与 `riscv-dv` 清单。
  - Verilator 用于 RTL 仿真；`pysim`/Cocotb 可选作 testbench。
- 定义**验收基准**：目标是要能跑通哪一组程序（如 `rv32ui-p-*`、`rv32mi-p-*`）。
- 确认本仓库的 `Cargo` 骨架与模块划分（`isa/`、`cpu/`、`mem/`、`csr/`、`tests/`）。

**验收**：`cargo build` 通过；能调用 Spike/QEMU 跑一条最小程序（如 `_start` + `wfi`）。

---

## 3. P1：Rust 功能模型（轨道 A）

模型作为**可执行规范**，优先实现、按规范精确。

**模块**：
- `fetch`/`decode`：取指、32/16 位（C）与 32 位指令解码。
- `execute`：RV32I 算术/逻辑/分支/跳转/load-store。
- `csr` 与陷阱：`csrrw`/`ecall`/`ebreak`/`mret`，`mstatus`/`mepc`/`mcause`/`mtval`。
- `mem`：地址空间与内存模型（含对齐检查）。
- RV64 分支：寄存器/`PC` 宽度、`*w` 词操作。

**测试**：
- `riscv-tests`（`rv32ui-*`，`rv32mi-*`，`rv32si-*`）逐组通过。
- Rust 全量单测（指令级表驱动：每条指令的 RFC/样例）。

**验收**：`rv32ui-p-*`、`rv32mi-p-*` 全绿；随机指令流下与 Spike 结果一致。

---

## 4. P2：RTL 单周期核（轨道 B）

**先正确后性能**：单周期实现，打通正确性闭环后再流水线化。

- RV32I 单周期核：`IF/ID/EX/MEM/WB` 在同拍完成。
- 指令译码器、ALU、寄存器堆、memory 接口。
- 分支/跳转、CSR、陷阱路径。
- 用 Verilator 编译并跑 `riscv-tests`（与 P1 相同的测试程序）。

**验收**：单周期核通过 `rv32ui-*`/`rv32mi-*`；RTL 综合可用（yosys 不报严重时序/占用错误即可）。

---

## 5. P3：交叉验证框架（核心耦合主线）

两条轨道在此融合，是本路线的**关键创新点**。

- **差分测试（differential testing）**：
  - 同一程序分别由 Rust 模型与 RTL 仿真执行。
  - 每个指令/周期后对比：`PC`、`x0–x31`（含 ABI 语义）、CSR、内存写入序列。
  - 不一致 → 定位到具体指令与状态，归因轨道 A 或 B。
- **激励生成**：
  - 定向：`riscv-dv`、`riscv-tests`。
  - 随机：自研指令流生成器（随机寄存器/立即数/地址/对齐边界），经受限 CSR 状态注入。
- **一致性保证**：
  - 以 Rust 模型为金标准，RTL 为被测（DUT）。
  - 覆盖：指令全覆盖、陷阱路径、对齐异常、边界立即数。
- **工具**：cocotb / C++ testbench 绑定 Verilator，Rust 模型作参照；产出统一测试报告。

**验收**：随机生成的数千指令流下，两轨道状态 100% 一致；至少一处真实缺陷（如对不齐、符号扩展）被差分测试捕获。

---

## 6. P4：扩展实现（M/A/F/D/C 等）

按优先级扩展到目标配置：

- **M**（乘除）：Rust 模型先实现，RTL 随后（可用加/移位迭代或乘法器）。
- **A**（原子）：`lr`/`sc`/`amo*`，RTL 需支持内存顺序（`aq`/`rl`）。
- **F/D**（浮点）：Rust 用 `f32`/`f64` 语义 + 舍入/标志（`fcsr.frm`/`fflags`）；RTL 用软核浮点单元或库。
- **C**（压缩）：模型解码 16 位；RTL 需 16/32 位取指对齐与译码适配。

每条扩展都遵循：**模型先行 → 差分验证 → RTL 实现 → 再差分**。

**验收**：目标配置（如 RV32IMC）下，`riscv-tests` 全部通过且与 Spike 一致；浮点符合 IEEE 754（含舍入与 NaN 语义）。

---

## 7. P5：流水线微架构（轨道 B）

从单周期演进到经典 5 级流水线（IF/ID/EX/MEM/WB），保证**功能等价**于 P1 模型：

- 前递（forwarding）、load-use 冒险、分支预测（先简单 BHT，再 BTB）。
- 分支/跳转的取指延迟与冲刷。
- 实现 `P3` 差分框架在 RTL 上持续回归，确保流水化不破坏正确性。

**验收**：流水线核在差分测试中与单周期/模型结果等价；性能用 `CoreMark`/循环计数初步度量。

---

## 8. P6：SoC 集成（轨道 B）

把核接入更完整的系统，跑真实程序：

- **总线**：简单 Wishbone → TILELink/AXI4（按需）。
- **内存**：指令/数据存储器、boot ROM（上电入口地址，如 `0x80000000`）。
- **外设**：UART/CLINT/PLIC 等最小集合，支持中断。
- **裸机运行时**：链接脚本、`_start`、栈/全局指针初始化，跑 C / Rust `no_std` 程序。
- **目标**：在 Rust 模型、QEMU、自家 RTL 三处运行同一程序结果一致。

**验收**：能 boot 到 Rust `no_std` `main`，通过 `semi-hosting`/UART 输出，且与 QEMU 输出一致。

---

## 9. P7：验证工程化（持续）

- **回归**：每日全量 `riscv-tests` + 差分测试 + 覆盖率（功能/行/分支）。
- **随机与定向结合**：`riscv-dv` + 自研生成器；`rvv`/浮点/原子定向 stress。
- **形式验证**（可选）：指令译码、CSR 属性不变量。
- **报告**：CI（GitHub Actions）跑模型与 RTL 双轨测试，失败即门槛。

**验收**：CI 全绿；覆盖率达到约定阈值；缺陷闭环率可追踪。

---

## 10. P8：物理实现（可选，长期）

- **综合**：yosys + 目标工艺库；时序/面积报告。
- **FPGA 原型**：上板运行，验证真实时钟/引脚/外设。
- **开源流程**：OpenLane（SKY130）走 GDSII 流片；评估成本/周期。
- **性能/功耗优化**：分支预测、缓存、乱序（若必要）为后续课题。

---

## 11. 风险与对策

| 风险 | 影响 | 对策 |
|------|------|------|
| ISA 配置/规范频繁变更 | 返工 | 锁定基线版本（如 unpriv/priv spec 冻结版本），记录 `misa` 组合 |
| 交叉验证框架初期不稳定 | 误报/漏报 | 先用 `riscv-tests`+Spike 打底，再上随机差分；先定向后随机 |
| RTL 时间难以收敛 | 进度 | 先单周期正确性，再流水线；扩展按优先级取舍 |
| 浮点/原子细节复杂 | 缺陷多 | 以 Rust 模型为金标准逐条对照 IEEE/规范，定向 stress |
| 验证投入大 | 成本 | 借助开源测试集与 CI 自动化，避免手写海量定向用例 |

---

## 12. 里程碑验收清单（勾选即「完成」）

- [ ] RTL 与 Rust 模型跑通 `rv32ui-*`/`rv32mi-*`
- [ ] 差分测试捕获至少一个真实缺陷并修复
- [ ] 目标配置全部扩展与 Spike 一致
- [ ] 流水线核与模型功能等价
- [ ] SoC boot 真实 `no_std` 程序，与 QEMU 输出一致
- [ ] CI 全量回归全绿

---

## 13. 相关规范与工具

- ISA 手册：https://riscv.org/technical/specifications/
- `riscv-tests` / `riscv-dv`：https://github.com/riscv/riscv-tests / https://github.com/google/riscv-dv
- Spike：https://github.com/riscv/riscv-isa-sim
- Verilator：https://www.veripool.org/verilator/
- OpenLane：https://github.com/The-OpenROAD-Project/OpenLane
