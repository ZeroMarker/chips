# RISC-V 参考

本文档是本仓库的 RISC-V（精简指令集架构，第五代）技术参考。RISC-V 是开放、免授权费的指令集架构（ISA），采用可模块化扩展的设计：一个固定的基础整数 ISA 加上一组可选的扩展。

> 权威来源：
> - The RISC-V Instruction Set Manual, Volume I: Unprivileged ISA（基础+非特权扩展）
> - The RISC-V Instruction Set Manual, Volume II: Privileged Architecture（特权架构）
> - 官方发布：https://riscv.org/technical/specifications/
> - 编码工具：`riscv-opcodes`（https://github.com/riscv/riscv-opcodes）

---

## 1. 设计哲学

- **开放标准**：ISA 规范使用 CC-BY 4.0 许可，可自由实现，无授权费。
- **模块化**：基础整数 ISA（`I`）是唯一的强制部分，其余均为可选扩展（`M/A/F/D/C/V/…`）。
- **可裁剪**：按需组合扩展，形成从嵌入式（`RV32IMC`）到应用处理器（`RV64GC`）的任意配置。
- **精简**：指令编码规整（固定 32 位，压缩扩展 16 位），指令数量少，便于硬件与软件实现。
- **编码空间预留**：RISC-V 刻意保留了大量指令编码空间给未来的扩展（`reserved` 字段）。

---

## 2. 基础整数 ISA（RV32I / RV64I）

`XLEN` 表示寄存器宽度（32 或 64）。`RV32I` 与 `RV64I` 共享同一套 32 个寄存器与指令语义，区别仅在寄存器/寻址宽度与部分指令变体。

### 2.1 寄存器

共 32 个逻辑寄存器 `x0`–`x31`，`PC`（程序计数器）独立。

| 寄存器 | ABI 名称 | 用途 |
|--------|----------|------|
| x0 | zero | 硬连线为 0，不可写 |
| x1 | ra | 返回地址（`jal`/`jalr` 的链接目标） |
| x2 | sp | 栈指针 |
| x3 | gp | 全局指针 |
| x4 | tp | 线程指针 |
| x5–x7 | t0–t2 | 临时寄存器 |
| x8 | s0 / fp | 保存寄存器 / 帧指针（ABI 别名） |
| x9 | s1 | 保存寄存器 |
| x10–x11 | a0–a1 | 函数参数与返回值 |
| x12–x17 | a2–a7 | 函数参数 |
| x18–x27 | s2–s11 | 保存寄存器 |
| x28–x31 | t3–t6 | 临时寄存器 |

### 2.2 指令格式

所有基础指令为固定 32 位（`C` 扩展提供 16 位压缩指令）。编码由 `opcode`（7 位）、`funct3`（3 位）、`funct7`（7 位）分派。

| 格式 | 用途 |
|------|------|
| R | 寄存器-寄存器运算 |
| I | 立即数运算、加载、`jalr` |
| S | 存储 |
| B | 分支（立即数分拆） |
| U | 高位立即数（`lui`/`auipc`） |
| J | 跳转（`jal`） |

立即数在各格式中重排以保持字段对齐，因此需要把 `imm` 按格式重新拼接，而非直接取位。

### 2.3 RV32I 核心指令

- **算术/逻辑**：`add`、`sub`、`sll`、`slt`、`sltu`、`xor`、`srl`、`sra`、`or`、`and`
- **立即数**：`addi`、`slti`、`sltiu`、`xori`、`ori`、`andi`、`slli`、`srli`、`srai`
- **加载**：`lb`、`lh`、`lw`、`lbu`、`lhu`（地址 = `rs1 + imm`，符号扩展/零扩展）
- **存储**：`sb`、`sh`、`sw`
- **分支**：`beq`、`bne`、`blt`、`bge`、`bltu`、`bgeu`（相对 PC 偏移，±4 KiB 范围）
- **跳转**：`jal`（相对 PC ±1 MiB）、`jalr`（`rd = pc+4; pc = rs1+imm`）
- **高位立即数**：`lui`（`rd = imm << 12`）、`auipc`（`rd = pc + (imm << 12)`，用于 `%pcrel_hi` 寻址）

### 2.4 RV64I 额外指令

RV64I 在 RV32I 基础上增加：`ld`、`lwu`、`sd`，以及 32 位字长的词操作（结果为零扩展/符号扩展至 XLEN 的 32 位值）：

- 32 位算术（`OP-32` 编码）：`addw`、`subw`、`sllw`、`srlw`、`sraw`
- 32 位立即数（`OP-IMM-32`）：`addiw`、`slliw`、`srliw`、`sraiw`

---

## 3. 扩展

| 扩展 | 内容 |
|------|------|
| M | 乘除：`mul`、`mulh`、`mulhsu`、`mulhu`、`div`、`divu`、`rem`、`remu`（RV64 有 `*w` 版本） |
| A | 原子：`lr`/`sc`、`amoswap`、`amoadd`、`amoxor`、`amoand`、`amoor`、`amomin`、`amomax`、`amominu`、`amomaxu`（`.w`/`.d` 宽度），以及 `aq`/`rl` 语义位 |
| F | 单精度浮点（FLEN=32）：`flw`、`fsw`、`fadd.s`、`fsub.s`、`fmul.s`、`fdiv.s`、`fsqrt.s`、`fmadd.s`、`fmsub.s`、`fnmadd.s`、`fnmsub.s`、比较/分类/类型转换/寄存器搬移 |
| D | 双精度浮点（FLEN=64，隐含依赖 F）：`fld`、`fsd`，运算后缀 `.d` |
| C | 压缩指令（16 位）：`c.*`，用于减小代码体积；与 32 位指令混合 |
| Zicsr | CSR 访问：`csrrw`、`csrrs`、`csrrc` 及立即数版本 `csrrwi/csrrsi/csrrci` |
| Zifencei | 指令流同步：`fence.i`（I-cache 冲刷） |
| V | 向量扩展（VLEN≥128，向量寄存器 `v0`–`v31`，向量长度 `vl`，依赖 F/D） |
| Zba/Zbb/Zbc/Zbs | 位操作扩展 |
| H | 虚拟化/虚拟机监视器扩展，提供 VS/VU 虚拟特权级 |

- **G**（通用）是一个便捷组合：`G = IMAFD_Zicsr_Zifencei`，即 RV32G/RV64G 桌面/通用配置。
- 浮点扩展需要独立的 32 个浮点寄存器 `f0`–`f31` 与状态寄存器 `fcsr`（含 `frm` 舍入模式与 `fflags` 标志）。
- **NaN-boxing**：当 FLEN=64（含 D）时，把单精度值放至高位置于双精度寄存器；读取单精度时忽略高半部分。

---

## 4. 内存模型与寻址

- **字节寻址**，默认 **小端（little-endian）**（规范允许双端，实现可配置）。
- **load-store 架构**：只有 load/store 类指令访问内存，运算均在寄存器上进行。
- 基础指令要求对齐访问（`lw` 需 4 字节对齐）；未对齐为保留/按实现行为。`.w`/`.d` 原子与压缩指令同样有对齐要求。
- **地址空间**：32 位/64 位扁平寻址；`RV64` 支持 `satp` 分页（Sv39/Sv48/Sv57）用于虚拟内存。
- **A 扩展**提供内存顺序保证：`aq`（acquire）与 `rl`（release）位控制原子操作的内存序。
- **主内存一致性**：需要 `fence`（`FENCE`/`FENCE.I`）提供显式排序，多核架构依赖内存模型（RVWMO）。

---

## 5. 特权架构

### 5.1 特权级

| 模式 | 名称 | 说明 |
|------|------|------|
| M | Machine | 最高特权，上电入口，唯一必须实现 |
| S | Supervisor | 操作系统内核 |
| U | User | 应用程序 |
| VS / VU | (H 扩展) | 虚拟化下的 guest supervisor/user |

`misa` 寄存器标识实现的 ISA 与扩展（每位一个扩展）。上电从 M 模式开始，通过 `mret` 逐级降权。

### 5.2 陷阱（Trap）处理

统一机制处理中断与异常：

- Control/status 寄存器（CSR）：`mstatus`、`mtvec`、`mepc`、`mcause`、`mtval`、`mip`/`mie`。
- 陷阱发生时：保存当前 `pc` 到 `mepc`，原因写入 `mcause`，隔离/额外信息写入 `mtval`，然后跳转到 `mtvec` 指定的处理入口（`direct` 或 `vectored`）。
- `mstatus.MPIE`/`MIE` 保存/恢复中断使能与全局中断位；`MPP` 记录陷阱发生前的特权级。
- `ecall`（环境调用）、`ebreak`（调试断点）、`wfi`（等待中断）、`mret`/`sret` 为系统指令。

### 5.3 常用 CSR

CSR 以 12 位地址编码，通过 `Zicsr` 访问。权限随特权级：读某 CSR 时，U 级访问 `m*` 会触发非法指令异常。

| CSR | 地址 | 描述 |
|-----|------|------|
| `mstatus` | 0x300 | 机器状态（中断使能/优先级、MPRV/MPP、FS/XS 等） |
| `misa` | 0x301 | ISA/扩展标识位 |
| `medeleg` / `mideleg` | 0x302 / 0x303 | 异常/中断委派给 S 模式 |
| `mie` / `mip` | 0x304 / 0x344 | 中断使能 / 中断挂起 |
| `mtvec` | 0x305 | 机器陷阱向量基址 |
| `mscratch` | 0x340 | 机器陷阱暂存 |
| `mepc` | 0x341 | 机器异常程序计数器 |
| `mcause` / `mtval` | 0x342 / 0x343 | 机器陷阱原因 / 值 |
| `mhartid` | 0xF14 | 硬件线程 ID |
| `satp` | 0x180 | (S) 地址翻译：页表根与 Svxx 模式 |
| `sstatus`/`sepc`/`scause`/`stval`/`stvec` | 0x100/0x141/0x142/0x143/0x105 | 主管级对应的 CSR |
| `cycle` / `time` / `instret` | 0xC00/0xC01/0xC02 | 周期 / 时间 / 指令计数（U 只读） |

---

## 6. 生态与工具链

- **GCC**：`riscv64-unknown-elf-gcc`（裸机）、`riscv64-unknown-linux-gnu-gcc`（裸机 Linux）。
- **LLVM/Clang**：`--target=riscv64-unknown-elf`、`clang` 原生支持。
- **Rust**：内置 target `riscv64gc-unknown-linux-gnu`、`riscv64gc-unknown-none-elf`、`riscv64imac-unknown-none-elf`；`cargo build --target ...` 交叉编译后由 `rust-objcopy` 生成 `.bin`/`.hex`。
- **模拟器**：QEMU（`qemu-system-riscv64` / `qemu-riscv64`）、Spike（官方 RISC-V 模拟器）。
- **二进制工具**：`riscv64-unknown-elf-objdump`、`readelf`、`objcopy`。
- **链接脚本**：`layout.ld` / `memory.x` 定义内存段（`.text`/`.data`/`.bss`/`.stack`），复杂系统需要 `linker script`（`PROVIDE`、`ENTRY` 等）。

---

## 7. 示例：最小启动（RV64GC 裸机）

```asm
.section .text
.globl _start
_start:
    la sp, _stack_top      # 初始化栈指针
    la gp, __global_pointer$ # 全局指针
    call main
loop:
    wfi                    # 主循环：等待中断
    j loop
```

对应 Rust 交叉编译后链接脚本需放置 `.text` 到约定地址（通常从 0x80000000 DRAM 起始），并用 `riscv64gc-unknown-none-elf` target 编译。

---

## 8. 参考实现与规范

- 官方规范（PDF/HTML）：https://riscv.org/technical/specifications/
- RISC-V 指令手册仓库：https://github.com/riscv/riscv-isa-manual
- 特权架构手册仓库：https://github.com/riscv/riscv-isa-manual
- 指令编码数据库：https://github.com/riscv/riscv-opcodes
- 官方模拟器 Spike：https://github.com/riscv/riscv-isa-sim
