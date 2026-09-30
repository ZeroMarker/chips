# 模型架构设计

本文档描述 Rust 功能模型（轨道 A）的内部设计：**为什么这样切分**、每一步执行的确切语义、以及当前模型的边界在哪里。阅读顺序建议：模块划分 → 单步执行 → 陷阱模型 → CSR 语义 → 内存模型 → 已知限制。

面向两类读者：修改模型的人，需要知道不变量在哪、哪里可以安全扩展；以及未来的 RTL 作者，这里是黄金参考的行为定义。

> 权威的架构定义以官方手册为准（见 [`RISC-V.md`](RISC-V.md)）。本文档只描述**本模型**的选择与简化，不替代规范。
> 里程碑计划见 [`ROADMAP.md`](ROADMAP.md)，工作项状态见 [`TODO.md`](TODO.md)。

## 1. 设计定位

模型是**可执行规范**，不是性能模拟器，也不追求 RTL 的时序结构。三个由此而来的定位：

1. **正确性优先于速度**。一条指令的执行就是一条语句，没有流水线、没有多周期时序。代价是全量测试套件会很慢，这正是 TODO 中「给内存模型加地址映射」的原因。
2. **黄金参考**。RTL 差分测试（P3）以本模型为金标准，因此任何「为了跑通而放宽」的处理都是缺陷，不是权衡。
3. **安全 Rust**。`Cargo.toml` 声明 `unsafe_code = "forbid"`，CI 将警告视为失败。所有内存访问都经过建模的地址空间，硬件工程师或评审者无需推理未定义行为。

## 2. 模块划分

```
src/
  lib.rs    crate 文档与公开 API 再导出
  isa.rs    ISA 常量、ABI 寄存器名、立即数解码、32 位译码
  cpu.rs    CPU 状态与执行循环（取指 → 译码 → 执行 → 陷阱）
  csr.rs    CSR 容器：地址表、只读判定、WARL 字段语义
  mem.rs    稀疏字节可寻址内存
  main.rs   命令行驱动：加载镜像、运行、转储状态
tests/      集成测试（手工编码指令字 + 跨平台 CLI 测试）
```

依赖方向是单向的，`isa` / `csr` / `mem` 都不依赖 `cpu`：

```
main ──▶ cpu ──▶ isa
             ├─▶ csr
             └─▶ mem
```

- **`isa.rs`** 纯函数、无状态。`decode()` 把一个 32 位字拆成 `Decoded`（含按格式符号扩展的 `imm`）。立即数按指令格式重排（I/S/B/U/J），因此必须分别解码而不是直接取位。指令测试也复用这里的常量。
- **`csr.rs`** 稀疏 CSR 文件，承载**字段**语义而非仅存储。计数器/时间类 CSR 在此登记为「存在」但不占存储，由 CPU 从自身状态提供。
- **`mem.rs`** 稀疏 `BTreeMap<u32, u8>`，小端。通用 API 允许非对齐访问；**对齐规则由 CPU 在访存前强制**。
- **`cpu.rs`** 唯一有状态的执行核心，见下。

## 3. 单步执行

`Cpu::step()` 的一次调用严格按此顺序，任何一步失败都进入陷阱流程：

1. `cycle` 与 `mtime` 各加一（在取指之前，因此陷阱步骤也计数）
2. **取指对齐检查**：`pc & 0b11 != 0` → `InstructionAddressMisaligned`
3. `mem.load_u32(pc)` 取指（4 字节小端）
4. `isa::decode()` 译码
5. `execute()`：按 `opcode` 分派，产出 `StepOutcome`
6. 若正常退休，`instret` 加一

`Cpu::run()` 是 `step()` 的循环，直到 `ecall`/`ebreak`（无处理程序时）或指令预算用尽。**陷阱不终止循环**——若安装了处理程序，模型会继续执行处理程序。

### 计数器语义

`cycle` 计**尝试的**步骤数（含陷阱步骤），`instret` 计**成功退休**的指令数。`ebreak` 不退休。二者在 `architecture.rs` 中有专门测试。

### 对齐检查的分工

- 取指与**被采取的**控制流目标（`jal`/`jalr`/已采取的分支）要求 4 字节对齐 → `InstructionAddressMisaligned`
- `jalr` 目标先按规范屏蔽最低位（`& !1`），再检查 4 字节对齐
- **未采取的分支不检查目标**——不检查就不会触发陷阱
- `lh`/`lw`/`sh`/`sw` 按访问宽度检查 → `LoadAddressMisaligned` / `StoreAddressMisaligned`
- `lb`/`lbu`/`sb` 宽度为 1，天然对齐

## 4. 陷阱模型

### 无处理程序 vs 有处理程序

模型以 `mtvec != 0` 判定「处理程序已安装」。这个分岔是驱动裸机程序停机的机制：

| 陷阱 | `mtvec == 0` | `mtvec != 0` |
|------|-------------|--------------|
| `ecall` / `ebreak` | 结束 `run` 循环（`StopReason::Ecall` / `Ebreak`） | 正常陷阱进入 |
| 其他所有异常 | 作为 `Err(Trap)` 返回给调用方 | 正常陷阱进入 |

未安装处理程序时，模型**没有架构上有意义的目标可跳转**，因此 `ecall`/`ebreak` 用来停机，其他异常上抛给调用方（CLI 转储状态并以退出码 1 退出）。

### 进入序列

`handle_trap()` 写入 `mepc`（故障 PC）、`mcause`、`mtval`，然后压栈中断使能位：

```
MPIE <- MIE
MIE  <- 0
MPP  =  M        (硬连线)
PC   <- mtvec & !0b11
```

**同步异常总是进入 `mtvec` 基址**，即使 `mtvec` 选择了 vectored 模式——vectored 只对中断有意义，而模型尚不交付中断。

### `mret`

```
MIE  <- MPIE
MPIE <- 1
PC   <- mepc
```

### `Trap` 枚举

`Trap` 的 payload 承载 `mtval` 表达不了的信息：

| 变体 | `mcause` | `mtval` |
|------|---------|---------|
| `InstructionAddressMisaligned(a)` | 0 | 故障地址 |
| `IllegalInstruction(enc)` | 2 | **故障编码本身** |
| `Breakpoint(pc)` | 3 | `ebreak` 的 PC |
| `LoadAddressMisaligned(a)` | 4 | 故障地址 |
| `StoreAddressMisaligned(a)` | 6 | 故障地址 |
| `EnvironmentCallFromM(pc)` | 11 | 0（ecall 不携带地址） |
| `Unsupported(ext)` | 2 | 0 |

`Unsupported(&'static str)` 在架构上等同于非法指令（`mcause` 2），但单独保留是为了让失败的运行**可诊断**——它告诉驱动缺的是哪个扩展，而不是丢出一个不认识的编码。`IllegalInstruction` 与 `Unsupported` 在进入处理程序后不可区分，这符合架构。

`Unsupported` 触发于：`LOAD_FP`/`STORE_FP`/`OP_FP`（`F/D`）、`AMO`（`A`）、`OP_V`（`V`），以及 `opcode` 低三位不为 `0b11` 的编码（`C`）。最后一条是压缩指令的启发式判定：16 位压缩指令的最低两位恒不为 `0b11`，所以全零字之类会被报为 `Unsupported("C")` 而非非法指令。

## 5. CSR 语义

CSR 的正确性集中在三件事，缺一不可。

### 存在性

`Csr::exists()` 白名单列出本模型实现的 22 个地址。访问白名单外的地址 → `IllegalInstruction`。这样未实现的 CSR 会明确报错，而不是静默读 0。

### 只读

`Csr::is_read_only()` 按架构读取地址的 `csr[11:10] == 0b11`。写入只读 CSR **产生非法指令异常**（在 `cpu::execute_csr` 中判定），而 `Csr::write()` 内部则静默丢弃——两层防御，使得无论谁调用，存储都保持一致。

`mvendorid`/`marchid`/`mimpid`/`mhartid`/`misa`/`mip` 属只读/常量，写入被忽略（这是合法的 WARL 结果）。

### WARL 字段

「写任意值、读回合法值」的字段在写入时**合法化**而非陷阱：

| CSR | 合法化规则 |
|-----|-----------|
| `mstatus` | 仅保留 `MIE`/`MPIE`/`MPP`；`MPP` 恒为 M（唯一实现的模式），其余位读 0 |
| `mtvec` | mode 0/1 保留，保留值 2/3 合法化为 0；基址天然 4 字节对齐 |
| `mepc` | 屏蔽最低两位（IALIGN = 32） |
| `mie` | 仅保留 MEIE(11)/MTIE(7)/MSIE(3)——S/U 模式不存在 |
| `misa` | 常量 `0x40001100`（MXL=1，扩展 I 与 M；Zicsr 由基础隐含，无 `misa` 位） |

### 计数器与时间

`cycle`/`mcycle`/`instret`/`minstret`（及 `*H` 高半、`cycle`/`time`/`instret` 非特权只读别名）在 `csr.rs` 中登记为存在但**不占存储**，由 `Cpu::read_csr`/`write_csr` 从 CPU 状态提供：机器级计数器可写，非特权别名不可写（`cpu::execute_csr` 先拒绝）。

`time`/`timeh` 读模型时间基（每步加一）。真实 hart 从中断控制器的内存映射 `mtime` 读取，那属于 SoC 阶段。

## 6. Zicsr 指令语义

`execute_csr()` 实现六条读改写指令，两处细节容易实现错：

- **`rd = x0` 的 `csrrw`/`csrrwi` 不读取 CSR**。读可能有副作用，架构在此明确禁止。其余情况都需要旧值。
- **`csrrs`/`csrrc` 在 `rs1 = x0`（或 `zimm = 0`）时是纯读，不发生写入**——即使 `rd` 非零。

写入发生后才检查只读性，因此「写只读 CSR」总是非法指令异常，而不是被静默吞掉。

## 7. 内存模型

`Memory` 是稀疏 `BTreeMap<u32, u8>`，小端，**任意地址都可写**，未写过的地址读 0。

这个选择让算术测试无需操心地址空间，对 ISA 语义验证是正确的；但它**无法建模未映射区域**，因此也没有访问故障（`LoadAccessFault`/`StoreAccessFault`）。同时逐字节 `BTreeMap` 访问对全量测试套件偏慢。TODO 中已记录：需要给内存模型加解码后的地址映射。

`load_image()` 逐字节把裸镜像装到基址——这正是 CLI 加载 raw binary 的方式。

## 8. 不变式与安全网

以下性质由测试固定，是修改模型时的回归依据：

- `x0` 恒为 0，写入被丢弃
- 采取的跳转若触发陷阱，**不得**写链接寄存器（`architecture.rs`）
- 陷阱的访存无副作用（`architecture.rs` 检查对齐失败的 store 未改内存）
- 未采取的分支不检查目标对齐
- 保留编码（`slli`/`srli` 的保留 `funct7`、`jalr` 的保留 `funct3`、`fence` 的保留 `fm`、`ecall` 的非零 `rd`）产生非法指令，且 `mtval` 为其自身编码
- `ebreak` 不退休
- debug 与 release 结果一致（模型依赖环绕运算，release 关闭溢出检查）——CI 两个 profile 都跑

## 9. 已知限制

按对验证工作的阻塞程度排列，与 [`TODO.md`](TODO.md) 的优先级一致：

1. **无 `riscv-tests` runner**。这是通往 P0 验收标准的主要缺口：需要 `tohost`/`ecall` pass-fail 约定的 runner 与目标平台定义。CI 目前只跑一个程序。
2. **无中断交付**。`mip` 恒读 0，无 CLINT/PLIC，无内存映射 `mtime`；`wfi` 立即退休。
3. **仅 M 特权级**。`mstatus.MPP` 硬连线为 M，`sret`/`sfence.vma` 为非法编码，`ecall` 固定报告 M 级。
4. **内存无地址映射**。见上一节。
5. **无指令级 trace**。P3 差分测试需要稳定的每指令 trace（PC、指令、寄存器/CSR 变化、内存写入），当前没有。
6. **无外部参考**。Spike 未引入，差分测试缺裁判。

## 10. 扩展模型的方式

按以下顺序，每步都应先有测试：

1. 在 `isa.rs` 加常量（`opcode`/`funct3`/`funct12`）
2. 在 `csr.rs` 若涉及 CSR：加入 `exists()`，按需处理只读与 WARL
3. 在 `cpu.rs` 的 `execute()` 加分派分支
4. 若新扩展的编码当前落在 `Unsupported` 分支，**替换掉**该分支——否则新指令永远到不了你的分派
5. 在 `tests/common/mod.rs` 加编码器，写表驱动测试
6. 更新本文档与 `misa`（若扩展有 `misa` 位）
