# 模型架构设计

本文档描述 Rust 功能模型（轨道 A）的内部设计：**为什么这样切分**、每一步执行的确切语义、以及当前模型的边界在哪里。阅读顺序建议：模块划分 → 单步执行 → 陷阱模型 → CSR 语义 → 内存模型 → 已知限制。

面向两类读者：修改模型的人，需要知道不变量在哪、哪里可以安全扩展；以及未来的 RTL 作者，这里是黄金参考的行为定义。

> 权威的架构定义以官方手册为准（见 [`RISC-V.md`](RISC-V.md)）。本文档只描述**本模型**的选择与简化，不替代规范。
> 里程碑计划见 [`ROADMAP.md`](ROADMAP.md)，工作项状态见 [`TODO.md`](TODO.md)。

## 1. 设计定位

模型是**可执行规范**，不是性能模拟器，也不追求 RTL 的时序结构。三个由此而来的定位：

1. **正确性优先于速度**。一条指令的执行就是一条语句，没有流水线、没有多周期时序。当前内存模型已有地址映射与平坦 RAM 后备；执行按指令推进，不模拟流水线时序。
2. **黄金参考**。RTL 差分测试（P3）以本模型为金标准，因此任何「为了跑通而放宽」的处理都是缺陷，不是权衡。
3. **安全 Rust**。`Cargo.toml` 声明 `unsafe_code = "forbid"`，CI 将警告视为失败。所有内存访问都经过建模的地址空间，硬件工程师或评审者无需推理未定义行为。

## 2. 模块划分

```
src/
  lib.rs      crate 文档与公开 API 再导出
  isa.rs      ISA 常量、ABI 寄存器名、立即数解码、32 位译码
  cpu.rs      CPU 状态与执行循环（取指 → 译码 → 执行 → 陷阱）
  csr.rs      CSR 容器：地址表、只读判定、WARL 字段语义
  pmp.rs      16 项物理内存保护：匹配、权限与锁定
  triggers.rs 双原生地址触发器：寄存器与地址匹配
  mem.rs      解码后的地址映射：区域、权限、访问故障、设备
  htif.rs     HTIF `tohost`/`fromhost` 测试完成协议
  platform.rs 预定义目标平台（`riscv-tests` 内存布局）
  main.rs     命令行驱动：加载镜像、运行、转储状态
tests/        集成测试（手工编码指令字 + 跨平台 CLI 测试）
```

依赖方向是单向的，`isa` / `csr` / `mem` / `htif` 都不依赖 `cpu`：

```
main ──▶ cpu ──▶ isa
             ├─▶ csr
             ├─▶ mem ──▶ clint (经 Device trait)
             ├─▶ trace
             └─▶ platform ──▶ htif
```

`cpu` 从不持有任何具体设备。它每步向地址空间索取两件事——设备当前置起的 `mip` 位，以及时间基——两者都经 `mem` 上的聚合方法（`Device::interrupt_pending`/`time_base`/`tick`），因此加一个新外设不需要改 CPU，而一个不带 CLINT 的地址空间也不需要特殊处理。

- **`isa.rs`** 纯函数、无状态。`decode()` 把一个 32 位字拆成 `Decoded`（含按格式符号扩展的 `imm`）。立即数按指令格式重排（I/S/B/U/J），因此必须分别解码而不是直接取位。指令测试也复用这里的常量。
- **`csr.rs`** 稀疏 CSR 文件，承载**字段**语义而非仅存储。计数器/时间类 CSR 在此登记为「存在」但不占存储，由 CPU 从自身状态提供。
- **`mem.rs`** 解码后的地址映射：小段 `Region` 列表，每段有基址、宽度、读写权限与后备（平坦 `Vec<u8>` 或稀疏 `BTreeMap`，或一个 `Device`）。未被任何段覆盖或被权限拒绝的访问产生 `AccessFault`。通用 API 允许非对齐访问。
- **`htif.rs`** `riscv-tests` 的测试完成协议。测试不 halt，而是把结果码写入 `tohost` 后自旋，因此设备需要把 `sw` 写入的两个 32 位半字拼成 64 位命令。
- **`platform.rs`** 把「链接脚本产出的内存布局」写成代码，使模型与套件按构造一致，而不是靠一条注释保持正确。
- **`cpu.rs`** 唯一有状态的执行核心，见下。
- **`clint.rs`** `mtime` 的唯一真实来源。`mtimecmp` 复位为全 1，因此复位后的 hart 不会立刻被中断；`MTIP` 是**电平**而非边沿：`mtime >= mtimecmp` 期间一直置起，漏掉的截止时间不会丢失。
- **`trace.rs`** 差分测试的线路格式，见 §11。

## 3. 单步执行

`Cpu::step()` 的一次调用严格按此顺序，任何一步失败都进入陷阱流程：

1. `cycle` 加一（在取指之前，因此陷阱步骤也计数），清除 `instret_written` 标志，清空本步的变化向量
2. 向地址空间索取设备置起的 `mip`（`& Csr::mip_mask()`）与时间基；并让它推进时间基。**没有 CLINT 的地址空间**退回模型自己的每步计数器，因此指令级测试读 `time` 不会看到 0
3. **中断检查**：合并设备信号与软件注入的 pending 位，按委派、源使能、当前特权和目标级全局使能交付；发生在取指前，目标级 EPC 指向没有执行的指令
4. **执行地址触发器检查**，命中即进入断点异常；未命中再做**取指对齐检查**：`pc & 0b11 != 0` → `InstructionAddressMisaligned`
5. 先按当前特权检查 PMP，再 `mem.fetch_u32(pc)` 取指（4 字节小端）；拒绝时 `InstructionAccessFault`，不读取后备设备
6. `isa::decode()` 译码
7. `execute()`：按 `opcode` 分派，产出 `StepOutcome`；数据访存先检查地址触发器，再检查 PMP（含 MPRV），最后访问内存，失败产生对应的 `LoadAccessFault`/`StoreAccessFault`
8. 若正常退休且本指令**未写入** `minstret` 且 `mcountinhibit.IR` 未置位，`instret` 加一
9. 若挂了 trace，构造一条 `Record` 并交给 sink

第 2 步的顺序有意义：`mip` 与 `mtime` 必须在任何东西能观察它们**之前**刷新，否则处理程序读到的 `mip` 不是导致它被进入的那个状态。

`Cpu::run()` 是 `step()` 的循环，直到 `ecall`/`ebreak`（无处理程序时）或指令预算用尽。**陷阱不终止循环**——若安装了处理程序，模型会继续执行处理程序。

### 计数器语义

`cycle` 计**尝试的**步骤数（含陷阱步骤），`instret` 计**成功退休**的指令数。`ebreak` 不退休。

**写入 `minstret` 抑制该指令自身的计数增量**——它刚写下的值就是下一个读者应看到的值，而不是再加一。这条规则由 `riscv-tests` 的 `instret_overflow` 检查，写 `minstreth` 同样抑制。注意 `mcycle` **不**有这条豁免。二者在 `architecture.rs` 中有专门测试。

### 对齐检查的分工

- 取指与**被采取的**控制流目标（`jal`/`jalr`/已采取的分支）要求 4 字节对齐 → `InstructionAddressMisaligned`。这在 IALIGN = 32 下是**架构强制**的，不是实现可选项；加入 `C` 扩展后 IALIGN 降为 16，规则随之改变。
- `jalr` 目标先按规范屏蔽最低位（`& !1`），再检查 4 字节对齐
- **未采取的分支不检查目标**——不检查就不会触发陷阱

### 非对齐数据访问：完成，而非陷阱

基础 ISA 把非对齐 load/store 的行为留给实现（「可以支持」）。本模型**选择完成它们**，因为 `riscv-tests` 的 `ma_data` 直接要求正确读出跨边界的数据，而真实硬件也这么做。作为黄金参考，模型必须与它所参考的对象一致。

**Spike 抛地址未对齐异常**（已用 `scripts/difftest.sh` 实测确认），QEMU 的选择依配置而定。也就是说这个行为是**架构未规定**的：两个选择都合规，差分测试不能要求两者一致，而一条分歧指令会使其后每一条都错位——所以差分程序刻意避开非对齐访问，模型的期望值由官方套件断言而非与 Spike 商定。

因此 `Trap::LoadAddressMisaligned`（`mcause` 4）与 `Trap::StoreAddressMisaligned`（`mcause` 6）在本模型中**不可达**。两个变体仍保留在 API 中，因为它们是架构的一部分，且未来实现可能选择陷阱。内存模型本身按字节小端访问，所以完成语义是自然结果，不需要额外代码。

## 4. 陷阱模型

### 路由与无处理程序约定

同步异常在当前特权为 S/U 且 `medeleg[cause] = 1` 时进入 S 级，否则进入 M 级；M 级异常始终留在 M。`medeleg` 可写原因 0–9，M 级 ECALL 位 11 与未支持原因读零。

先选目标，再检查该目标的 `stvec`/`mtvec`。非零表示有同步异常处理程序；零值时 `ecall`/`ebreak` 结束运行，其他异常（包括硬件 `Trigger`）返回 `Err(Trap)`。委派目标未安装处理程序时不回退到 M 级。此停机约定属于模型驱动，不是硬件复位语义。

### 进入序列

M 级入口写 `mepc`/`mcause`/`mtval`，执行 `MPIE <- MIE`、`MIE <- 0`、`MPP <- 来源特权`；S 级入口写 `sepc`/`scause`/`stval`，执行 `SPIE <- SIE`、`SIE <- 0`、`SPP <- 来源特权`。S 级入口保留机器级陷阱寄存器、MPP、MPIE 与 MIE，因此可正确嵌套 M 级陷阱。

同步异常进入目标 `tvec` 基址，向量模式也不加偏移。入口与返回的状态写入均进入 trace；S 级陷阱记录使用 `scause`/`stval`。

### 中断交付

`mip` 的设备信号与软件 pending 位分开保存，读取时合并。CLINT 提供 MSIP/MTIP；M 可通过 CSR 写入 SSIP/STIP/SEIP。`mideleg` 仅支持位 1/5/9（SSI/STI/SEI），机器级源不可委派。

- 目标 M：`mip & mie & ~mideleg` 中有 pending 位，且当前特权低于 M 或 MIE=1。
- 目标 S：`mip & mie & mideleg` 中有 pending 位，且当前为 U，或当前为 S 且 SIE=1。M 级运行时屏蔽已委派中断。
- 目标 M 优先于目标 S；同目标按 MEI > MSI > MTI > SEI > SSI > STI 排序。

RV32 `xcause = 0x80000000 | 原因号`，`xtval = 0`。直接模式进入 `tvec & !3`；向量模式进入 `(tvec & !3) + 4 * 原因号`，偏移不使用 pending 位掩码。EPC 保存尚未执行的指令地址。WFI 立即退休，下一步重新检查中断。

### `mret` / `sret`

MRET 仅 M 可执行，返回 MPP 指定特权与 `mepc`；SRET 在 S/M 可执行，返回 SPP 指定特权与 `sepc`，TSR=1 时 S 级执行 SRET 陷阱，M 级不受此位限制。

两者均执行 `xIE <- xPIE`、`xPIE <- 1`，并将 `xPP` 清为最低支持特权 U；返回低于 M 的模式时同时清除 MPRV。测试覆盖 U→S→M 的嵌套进入与 MRET→SRET 返回路径。

### `Trap` 枚举

`Trap` 的 payload 承载 `mtval` 表达不了的信息：

| 变体 | `mcause` | `mtval` |
|------|---------|---------|
| `InstructionAddressMisaligned(a)` | 0 | 故障地址 |
| `InstructionAccessFault(a)` | 1 | 故障地址 |
| `IllegalInstruction(enc)` | 2 | **故障编码本身** |
| `Breakpoint(pc)` | 3 | `ebreak` 的 PC |
| `Trigger(a)` | 3 | 命中的访问地址；执行地址触发前尚未取指，trace 指令字为 0 |
| `LoadAddressMisaligned(a)` | 4 | 故障地址（本模型不产生，见上） |
| `LoadAccessFault(a)` | 5 | 故障地址 |
| `StoreAddressMisaligned(a)` | 6 | 故障地址（本模型不产生） |
| `StoreAccessFault(a)` | 7 | 故障地址 |
| `EnvironmentCall{mode, pc}` | 8 + mode | 0（ecall 不携带地址） |
| `Unsupported(ext)` | 2 | 0 |

`ecall` 的 `mcause` 取决于来源模式：U 是 8、S 是 9、M 是 11。这个偏移量由架构固定，所以偏移的是模式的**编码值**（0/1/3）而不是枚举下标。

`Unsupported(&'static str)` 在架构上等同于非法指令（`mcause` 2），但单独保留是为了让失败的运行**可诊断**——它告诉驱动缺的是哪个扩展，而不是丢出一个不认识的编码。`IllegalInstruction` 与 `Unsupported` 在进入处理程序后不可区分，这符合架构。

`Unsupported` 触发于：`LOAD_FP`/`STORE_FP`/`OP_FP`（`F/D`）、`AMO`（`A`）、`OP_V`（`V`），以及 `opcode` 低三位不为 `0b11` 的编码（`C`）。最后一条是压缩指令的启发式判定：16 位压缩指令的最低两位恒不为 `0b11`，所以全零字之类会被报为 `Unsupported("C")` 而非非法指令。

## 5. CSR 语义

CSR 的正确性集中在三件事，缺一不可。

### 存在性

`Csr::exists()` 白名单列出本模型支持的 CSR 地址，涵盖非特权计数器、监管级状态与陷阱寄存器、机器级状态与计数器。存在性只表示可访问，不表示相关设施已完整实现（例如 `satp` 仅支持 Bare）。访问白名单外的地址 → `IllegalInstruction`。这样未实现的 CSR 会明确报错，而不是静默读 0。

### 特权

每个 CSR 地址从 `csr[9:8]` 隐含一个最低特权级：`0b00` 无特权（`fcsr` 0x003、`cycle` 0xC00），`0b01` 监管级（`sstatus` 0x100、`satp` 0x180），`0b10` 保留，`0b11` 机器级（`mstatus` 0x300、`mcycle` 0xB00、`mhartid` 0xF14）。保留值按机器级处理——方向是安全的那一侧：一个本模型未实现的寄存器不应因为地址看起来如何就变得可达。

低于所需等级的访问是**非法指令**而非读到别人的状态。`Privilege` 的判别值就是架构编码（U = 0、S = 1、M = 3），且派生的序关系有意义（`U < S < M`），所以「至少同等特权」是一次 `<=` 比较而不是遍历一个集合。

`sstatus`、`sie`、`sip` 分别是 `mstatus`、`mie`、`mip` 的**窄视图**，共享存储。`sie`/`sip` 仅显示 `mideleg` 中已委派的位；S 可写这些使能位，但 pending 位只允许写 SSIP。经别名写入只更新该别名可见的字段，因此监管级无法通过写 `sstatus` 触及 `MIE`。

### 只读

`Csr::is_read_only()` 按架构读取地址的 `csr[11:10] == 0b11`。写入只读 CSR **产生非法指令异常**（在 `cpu::execute_csr` 中判定），而 `Csr::write()` 内部则静默丢弃——两层防御，使得无论谁调用，存储都保持一致。

`mvendorid`/`marchid`/`mimpid`/`mhartid`/`misa` 属只读/常量，写入被忽略（这是合法的 WARL 结果）。

### WARL 字段

「写任意值、读回合法值」的字段在写入时**合法化**而非陷阱：

| CSR | 合法化规则 |
|-----|-----------|
| `mstatus` | 保留 `SIE`/`MIE`/`SPIE`/`MPIE`/`SPP`/`MPP`/`TSR`/`MPRV`；`MPP` 支持 M/S/U，保留编码合法化为 U，其余位读 0 |
| `mtvec` | mode 0/1 保留，保留值 2/3 合法化为 0；基址天然 4 字节对齐 |
| `mepc` | 屏蔽最低两位（IALIGN = 32） |
| `mie` | 保留 M/S 的外部、软件、定时器使能位（11/9/7/5/3/1） |
| `mip` | M/S 设备信号与软件 pending 逻辑或；仅 S 级 pending 位可由 M 写入，读改写不会锁存设备信号 |
| `medeleg` / `mideleg` | 分别保留 `0x3ff` / `0x222` |
| `satp`、TVM、SUM、MXR | 仅 Bare，无 ASID/页表根，全部读零；`SFENCE.VMA` 非法 |
| `misa` | 常量 `0x40001100`（MXL=1，扩展 I 与 M；Zicsr 由基础隐含，无 `misa` 位） |

### 计数器与时间

`cycle`/`mcycle`/`instret`/`minstret`（及 `*H` 高半、`cycle`/`time`/`instret` 非特权只读别名）在 `csr.rs` 中登记为存在但**不占存储**，由 `Cpu::read_csr`/`write_csr` 从 CPU 状态提供：机器级计数器可写，非特权别名不可写（`cpu::execute_csr` 先拒绝）。

`time`/`timeh` 在带 CLINT 的地址空间中读取 CLINT `mtime`，CLINT 同时提供内存映射访问；CPU 每步采样后推进设备时间基。无 CLINT 时才退回模型自己的每步计数器。

## 6. Zicsr 指令语义

`execute_csr()` 实现六条读改写指令，两处细节容易实现错：

- **`rd = x0` 的 `csrrw`/`csrrwi` 不读取 CSR**。读可能有副作用，架构在此明确禁止。其余情况都需要旧值。
- **`csrrs`/`csrrc` 在 `rs1 = x0`（或 `zimm = 0`）时是纯读，不发生写入**——即使 `rd` 非零。

写入发生后才检查只读性，因此「写只读 CSR」总是非法指令异常，而不是被静默吞掉。

## 7. 内存模型

`Memory` 是一组 `Region`，按基址排序，每个区域有基址、宽度（`u64`，因为整个 32 位地址空间需要 2^32 字节，装不进 `u32`）、读写权限，以及平坦或稀疏后备。地址被解析到**第一个覆盖它**的区域，因此设备页必须与 RAM 区域**不重叠**，否则永远不会被访问到。

- `Memory::permissive()`：整个地址空间、一段稀疏、读写全开。没有任何访问会故障——指令级测试关心的是算术而非地址布局，不该为「我的地址落在哪」分心。
- `Memory::from_regions(...)`：装入真实映射，未映射区域与权限不足都会产生 `AccessFault`。

**访存（guest access）与装载（host setup）是两回事。** 访存可故障，因为那是架构可观察的行为。把程序装进内存不是 guest 做的事，也不该能故障，所以 `poke`/`peek` 系列不做检查，遇到未映射地址直接 panic——那是调用方的 bug，不是 guest 能观察到的行为。

**访问不得跨区域**：一个访问必须完全落在单个区域内。起点在内但越过了区域末尾 → 故障，而不是去读下一个区域。

`load_image` 是唯一的例外：镜像本身可以跨区域（`riscv-tests` 的二进制同时覆盖 `.text.init` 页与 `tohost` 页），因为它是逐字节装载的，这与「单次 guest 访问不得跨界」是两个不同的问题。

### PMP 与有效特权

`Csr` 持有独立的 `Pmp` 状态：16 项、G=0、32 位物理地址，`pmpaddr` 仅保存位 29:0；RV32 四个 packed 配置 CSR 位于 `0x3a0..0x3a3`，地址 CSR 位于 `0x3b0..0x3bf`。未实现 Smepmp。

支持 OFF/TOR/NA4/NAPOT，按最低索引的相交项判断整次访存。未完全覆盖就拒绝；匹配项未锁定时 M 可绕过 R/W/X，锁定后 M 也受限；无匹配项时 M 允许、S/U 拒绝。复位所有项 OFF，因此固件进入 S/U 前必须显式放行。L 锁定本项配置与地址，锁定 TOR 还冻结前一项地址，直至 hart 复位；保留配置位读零，R=0/W=1 清除 W。

CPU 在取指和每次 load/store 调用后备内存之前检查 PMP。MPRV=1 时数据访问使用 MPP 特权，取指始终使用当前特权。拒绝返回对应访问故障，保留原始指令/数据地址作为 tval，并复用 M/S 陷阱路由。模型将非对齐数据指令视作一次完整操作，跨项拒绝时不产生部分写入或 MMIO 调用。区域权限仍须通过，PMP 放行不会绕过地址映射。

`tests/pmp.rs` 覆盖权限、边界、锁定和 MPRV。官方 `pmpaddr` 在 G=0 时只走寄存器探测路径；额外 `scripts/pmp.S` 通过执行与故障处理检查隔离，103 步程序与 Spike 一致（含逐步寄存器与 CSR 状态）。

### 原生地址触发器

`Csr` 持有两个独立触发器。`tselect` 只选择配置窗口，不影响其他触发器是否激活；越界选择保持原值以便枚举结束。`tdata1` 固定 type=2，支持 M/S/U 与执行/读/写位；`tdata2` 保存完整 32 位地址；`tdata3` 固定零（无上下文过滤）；`tinfo` 为 `0x01000004`（version=1，type=2）。零写或未支持类型禁用触发器且保留地址。其余字段合法化为零，读回可发现仅支持等值地址、任意宽度、before timing、action=0。无 `mcontrol6`、外部 Debug Mode、dmode、hit 位、数据/链式匹配。

使用 [Sdtrig v1.0](https://docs.riscv.org/reference/debug/v1.0/Sdtrig.html) 的 MIE/SIE 防重入方案：M 且 MIE=0 时抑制；S 且断点已委派、SIE=0 时抑制。U 不受这些门控限制，S 断点未委派时可向 M 陷阱。此方案保留 `medeleg[3]`，可选 `tcontrol` 不存在。MPRV 只影响内存保护，触发器模式位依据当前特权。

执行地址在取指/PMP 前比较；数据地址在解码合法性确认后、PMP 与内存副作用前比较。每个访问字节都可匹配，最低触发器索引决定 tval；匹配不绕回零地址。命中产生 `Trap::Trigger`（原因 3），EPC 为未完成指令 PC，tval 为命中的字节地址，不退休、不写目的寄存器或内存。现有 M/S 陷阱入口和 xRET 自然关闭/恢复门控。未安装目标处理程序时返回错误，不复用 EBREAK 停机约定。

`tests/triggers.rs` 的 12 个回归验证能力、优先级、权限和副作用；额外 `scripts/triggers.S` 的 105 步程序与 Spike 一致，覆盖执行/读/写、双触发器、处理程序内抑制与恢复。

## 8. 测试完成协议（HTIF）

`riscv-tests` 不 halt。`RVTEST_PASS` 写 `1`，`RVTEST_FAIL` 写 `(check << 1) | 1`，然后自旋；遇到无法处理的异常则把 `1337` 或进测试号后**原样**写入（不走那个编码）。因此：

- `tohost` 是 64 位，但测试用 `sw` 逐字写：先低半字，再向高半字写 0。设备必须**拼装**两个半字，而不是把一次写当成整寄存器写。
- `1` 为通过；大于 1 的奇数是失败并指明第几项检查；`1337` 标记表示「发生了意外异常」。
- 编码本身有歧义：`(668 << 1) | 1 == 1337`，所以「第 668 项检查失败」与「意外异常」不可区分。模型报告后者（更可能是真实原因，也更有指导意义）。现有套件没有编号 668 的检查，实践上无损失。

驱动在 `--htif` 下逐条 `step` 并在每条之后检查设备，因此结果一落定就停止，而不是烧完剩余预算。

## 9. 不变式与安全网

以下性质由测试固定，是修改模型时的回归依据：

- `x0` 恒为 0，写入被丢弃
- 采取的跳转若触发陷阱，**不得**写链接寄存器（`architecture.rs`）
- 陷阱的访存无副作用（`architecture.rs` 检查对齐失败的跳转未改状态）
- 未采取的分支不检查目标对齐
- 保留编码（`slli`/`srli` 的保留 `funct7`、`jalr` 的保留 `funct3`、`fence` 的保留 `fm`、`ecall` 的非零 `rd`）产生非法指令，且 `mtval` 为其自身编码
- `ebreak` 不退休
- 写入 `minstret` 抑制该指令自身的计数增量（`architecture.rs`）
- 非对齐数据访问完成而非陷阱，且只写自己宽度覆盖的字节（`architecture.rs`）
- hart 复位在 M 级；`mret`/`sret` 恢复保存的特权级；`MPP` 记录陷阱来源模式（`privilege.rs`）
- 低于所需等级的 CSR 访问是**非法指令**而非读到别人的状态（`privilege.rs`）
- 经 `sstatus`/`sie` 别名写入不触及机器级字段（`privilege.rs`）
- 中断在步骤边界交付，`xcause` 含最高位中断标记和原因号，`mtval` 为 0（`interrupts.rs`）
- vectored `mtvec` 只对中断偏移，异常仍进基址（`interrupts.rs`）
- `mip` 的 M 级源由设备驱动，S 级源支持软件注入；`mcountinhibit.IR` 抑制 `instret`（`interrupts.rs`）
- `mtime` 跟随 CLINT，无 CLINT 时退回模型计数器（`interrupts.rs`）
- 挂 trace 不改变执行：寄存器、计数器、内存逐一相同（`trace.rs`）
- 每条 trace 行的确切文本（`trace.rs`）——这是「格式已冻结」的实际含义
- 访问不得跨区域；设备页与 RAM 不重叠（`memory.rs`、`htif.rs`）
- 未映射/无权限访问产生正确的 `mcause`（1/5/7）并经处理程序正确路由（`memory.rs`）
- `tohost` 的拼装与解码，包括 `1337` 标记与 `check 668` 的编码歧义（`htif.rs`）
- debug 与 release 结果一致（模型依赖环绕运算，release 关闭溢出检查）——CI 两个 profile 都跑

### 官方套件

上述不变量之外，`scripts/riscv-tests.sh` 跑官方 `rv32ui-p-*` 与 `rv32mi-p-*`，**58 通过、0 失败、0 excluded**。它比任何手写测试都更容易发现语义偏差——本项目中被它抓出的四处（`minstret` 抑制、非对齐数据访问、`csr[9:8]` 特权表方向、陷阱入口的 `MPP` 写入）都不是靠推理想到的。修改执行语义后应先跑它。

`scripts/riscv-tests.sh` 已删除排除清单，当前选定的 58 个程序全部执行；任一失败都会令脚本以非零码退出。

`scripts/difftest.sh` 是第三重安全网：比较退休效果及逐步真实架构状态。CI 的 `spike-difftest` job 从锁定源码构建或校验缓存，并以 `--require` 运行四组探针。本地缺 Spike 时仍可跳过；CI 中缺失或构建失败会令 job 失败。

## 10. 逐指令 trace

P3 要逐步比较模型与 RTL，两者必须能对每一步「说出同一件事」。格式因此在 RTL 存在**之前**冻结（`src/trace.rs`），并且是**规定**出来的而非由实现涌现：每步一行，一个字母标识记录类型，一个步号，然后是 `key=value` 字段。没有续行——读一条记录不需要知道前面任何一条。

```
v1
i <step> pc=<8 hex> inst=<8 hex> [x<reg>=<8 hex>]... [c<csr>=<8 hex>]... [m<addr>=<hex>]...
t <step> pc=<8 hex> inst=<8 hex> cause=<dec> tval=<8 hex> [...]
x <step> pc=<8 hex> inst=<8 hex> [...]
```

- `i` 正常退休。`x` 字段是它写入的寄存器，`c` 是 CSR，`m` 是内存；三组都可省略。
- `t` 陷阱被交付。`cause`/`tval` 是架构值，该行**也**带上进入序列写入的 CSR——否则一条 trace 会显示一个「什么都没变的陷阱」，无法说明处理程序被送到了哪里。
- `x` 该步没有退休且运行停止：`ecall`/`ebreak` 且未安装处理程序。

寄存器按**编号**（`x0`–`x31`）而非 ABI 名称记录，因为 ABI 名称是工具链的约定，而这是机器对机器的比较。

**刻意缺席**，且在代码中写明理由：

- **周期数**。模型一步一条指令，RTL 不是。写进去会让每一行都因一个与正确性无关的原因不同。
- **内存读**。读不改状态，未命中已经表现为取访问故障；记录它们会让 trace 体积翻三倍而不增加诊断价值。
- **未触及的状态**。写入同值的寄存器**会**记录，因为硬件也执行了这次写；省略它会把写行为的差异藏在相同的状态背后。

变化在已有的写入汇聚点（`write_rd`、`write_csr`、存储路径、陷阱入口）采集，而不是事后对拍快照——这正是「写入同值也出现」得以成立的原因。`--trace` 只把 trace 写到 stdout，不与状态转储交织；未开启 trace 的运行完全不构造记录。

## 11. 与 Spike 的差分

`scripts/difftest.sh` 调用 Python 标准库 runner `scripts/difftest.py`，默认构建并运行通用、PMP、触发器及委派四组探针。它同时比较退休指令效果与独立读取的真实状态，不从提交日志推算快照。

模型 `--state-log=FILE` 写入初始状态及每次 `Cpu::step` 后的状态，包括未退休的陷阱步骤。`Cpu::state_line()` 只观察 CPU，不推进计数器，不执行客体 CSR 指令，也不触发特权检查。格式为一行一个快照：

```text
s1 pc=<8 hex> priv=<0|1|3> x0=<8 hex> ... x31=<8 hex> c100=<8 hex> ...
```

`c` 后是三位十六进制 CSR 地址；按地址递增，列出 `Csr::exists()` 登记的所有 CSR，计数器/时间/pending 通过 CPU 的实时读取路径取得。触发器窗口反映当前 `tselect`，不是所有槽位的展开。日志独立于 `v1` 写入 trace，二者均开启时 stdout 只含 `v1` trace。

Spike 通过 `-d --debug-cmd` 执行相同步数的 `rs 1`，每步分别查询 PC、特权级、32 个整数寄存器和 40 个 CSR。包括陷阱进入后的快照都必须对应；最终快照也参与比较。退休记录另外核对 PC、指令字、寄存器写入和内存写入。Spike 提交日志**包含显式 CSR 写入**，旧版文档称其不记录 CSR 是错误的；但陷阱入口没有对应退休记录，且同值写入及别名的日志形式可不同，因此 harness 解析 CSR token，比较其逐步真实状态效果，不要求文本写入序列一致。

共有字段由 `scripts/difftest.py::CSRS` 固定，掩码之外不宣称一致：

| CSR | 比较掩码 / 范围 |
|---|---|
| `sstatus` / `sie` | `0x122` / `0x222` |
| `mstatus` | `0x4219aa`：已实现的中断栈、MPP、MPRV、TSR |
| `medeleg` / `mideleg` / `mie` | `0x3ff` / `0x222` / `0xaaa` |
| `stvec`、`sscratch`、`sepc`、`scause`、`stval`、`satp` | 全 32 位 |
| `mtvec`、`mscratch`、`mepc`、`mcause`、`mtval` | 全 32 位 |
| `pmpcfg0..3` / `pmpaddr0..15` | 每项合法配置位 `0x9f9f9f9f` / 32 位物理地址域 `0x3fffffff` |
| `tselect` / `tdata2` / `tdata1` | 全 32 位 / 全 32 位 / 基础 M/S/U 与 R/W/X 位 `0x5f` |

不比较计数器、时间、设备 pending、标识/能力 CSR、触发器 type/hit 等实现差异。`tdata1/2` 只读当前选中槽，探针通过切换 `tselect` 检查两个槽。非对齐数据访问在模型完成、Spike 陷阱，故探针避开此项；模型的结果由官方套件断言。

Spike reset 默认把 PMP 第零项设为全放行，模型默认全 OFF。四组探针先执行 `csrw pmpcfg0, zero` 和 `csrw pmpaddr0, zero`，再从显式符号 `difftest_start` 开始比较全部状态。这两条初始化指令仍参与退休记录比较；没有该符号的自定义 ELF 从复位状态开始比对，不隐藏默认 PMP 差异。

模型 `--stop-pc` 在执行成功地址前停止；未到达目标则失败。Spike 严格执行相同步数再正常退出，程序不执行末尾自旋。任意非零退出、超时、状态读取缺失、多余数据、退休记录数量不同或状态分歧均失败，不再将可选 CSR 分歧归类为 gap。`--timeout` 默认每个命令 30 秒；`--budget` 默认 2000 步。13 个 Python 回归在 CI 验证解析、长度检查及故障路径；另有 7 个构建缓存回归。实际 Spike 差分也已接入独立 CI job。

当前四组分别执行 225 / 103 / 105 / 53 步，退休 224 / 100 / 101 / 51 条，比较 224 / 102 / 104 / 52 个状态（从两条 PMP 初始化后的状态开始，含最终状态）。新增 `delegation.S` 验证 U→S 委派 ECALL、嵌套 S→M ECALL，以及 MRET/SRET 返回 U。

参考 Spike 由 `scripts/spike.lock.json` 锁定提交 `0bff12123b1fd510e19e19634dd997dbade70e54` 与配置。构建器 `scripts/build_spike.py` 按完整 SHA 获取源码，只生成 `spike` 可执行文件；构建成功且确认支持调试命令和提交日志后才发布缓存。缓存键含 lock、脚本摘要、系统 / 架构 / libc 及编译器 / 链接器身份；`build.json` 保存这些信息与二进制 SHA-256，命中时重新校验。失败或中断不发布半成品；损坏条目直接失败，不静默使用。CI 缓存不使用模糊回退，构建日志失败时上传为 artifact。此流程固定源码与配置，不承诺跨宿主机逐字节可复现。

## 12. 已知限制

按对验证工作的阻塞程度排列，与 [`TODO.md`](TODO.md) 的优先级一致：

1. **原生触发器仅实现基础地址匹配**。`breakpoint`、`pmpaddr`、`illegal` 已全部纳入门禁，但外部调试、type-6 与高级触发功能未实现；PMP 的 Smepmp 增强也未实现。
2. **无地址转换**。仅 Bare，`satp`/TVM/SUM/MXR 读零；选择不实现 `SFENCE.VMA` 并产生非法指令异常。引入 Sv32 时需要配套页表遍历、权限和 TVM 门控。
3. **无 PLIC**。平台目前只有 CLINT，尚无外部中断控制器；S 级外部/定时器中断可由 M 软件注入。
4. **差分尚无随机流及全部状态覆盖**。四组定向探针已核对每步完整整数寄存器与 40 个 CSR 的共有字段；计数器、设备 pending、能力字段及所有触发器槽的同时快照尚未纳入。参考工具的源码和配置已锁定，系统依赖仍由宿主机提供。

本次特权语义依据固定版本的官方手册：[Machine-Level ISA v1.13](https://docs.riscv.org/reference/isa/v20260120/priv/machine.html)（委派、中断、TSR、PMP 与 MPRV）及 [Supervisor-Level ISA v1.13](https://docs.riscv.org/reference/isa/v20260120/priv/supervisor.html)（CSR 视图、Bare 与 SFENCE.VMA）。

## 13. 扩展模型的方式

按以下顺序，每步都应先有测试：

1. 在 `isa.rs` 加常量（`opcode`/`funct3`/`funct12`）
2. 在 `csr.rs` 若涉及 CSR：加入 `exists()`，按需处理只读与 WARL
3. 在 `cpu.rs` 的 `execute()` 加分派分支
4. 若新扩展的编码当前落在 `Unsupported` 分支，**替换掉**该分支——否则新指令永远到不了你的分派
5. 若涉及新设备：在 `mem.rs` 实现 `Device`（注意访问以**整块**形式传入，设备才能看到宽度），并在 `platform.rs` 中安排区域**不重叠**。若设备能置起中断或提供时间基，实现 `Device::interrupt_pending`/`time_base`/`tick`——**并确保 `impl Device for Rc<D>` 转发它们**：漏掉一个转发不会编译失败，只会让该方法退回默认返回值，`mip` 恒读 0 且中断永不交付，且无任何报错。
6. 若编码的合法性依赖特权级：加入 `execute_csr` 的特权检查或 `execute_system` 的模式门控，并注意未安装处理程序时异常是**返回**给调用者而非被交付——因此测试断言的是 trap CSR 而非 `Err`
7. 在 `tests/common/mod.rs` 加编码器，写表驱动测试
8. 跑 `scripts/riscv-tests.sh`——官方套件常能发现手写测试想不到的语义偏差
9. 跑 `scripts/difftest.sh`——Spike 差分；注意差分程序必须避开「架构未规定」的行为
10. 更新本文档与 `misa`（若扩展有 `misa` 位）
