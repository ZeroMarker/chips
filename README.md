# chips

一颗 RISC-V 处理器的双轨研发仓库：**Rust 功能模型**（黄金参考）与**硬件 RTL**（尚未开始）并行推进，以交叉验证为耦合主线。

当前轨道 A 已实现一个可执行的 **RV32IMZicsr** 核心：完整 RV32I 基础整数集、RV32M 乘除、Zicsr CSR 访问，**M/S/U 三级特权**与陷阱进入、CLINT 中断交付与内存映射 `mtime`。它是后续 RTL 的**黄金参考模型**——RTL 必须与它跑同一套程序、得到一致结果。

模型已接入官方 **`riscv-tests`** 一致性套件：`rv32ui-p-*` 与 `rv32mi-p-*` 共 58 个测试中 **56 个通过**，另 2 个因需要尚未实现的硬件设施而排除（详见下文）。

模型同时接入 **Spike 差分测试**：`scripts/difftest.sh` 在同一程序上逐步比较 PC、指令字、目的寄存器与其值、内存写入的地址与值。当前自带的 225 步探针程序两侧一致，尚未覆盖随机指令流、CSR 写入与完整最终架构状态。

- 目标配置：`RV32IMC` 起步（当前 `RV32IMZicsr`，`C` 扩展待做）
- 特权级：M / S / U；支持 `medeleg`/`mideleg` 陷阱委派与 S 级中断，地址翻译仅支持 Bare
- 硬件轨道：RTL 与 Verilator 尚未纳入本仓库；Spike 作为外部参考已接入差分测试

## 当前完成程度

截至 2026-10-16 本地复测：**软件功能模型与验证基础已建立，硬件实现尚未开始**。按路线图，P0 基线已达成；P1 模型大部分实现，但官方套件仍有排除项，随机差分验收尚未达成；P2 RTL 与 P3 模型/RTL 交叉验证未开始。

| 本地检查 | 结果 |
|----------|------|
| Rust 测试（debug / release） | 每种 profile 均为 150 通过、0 失败 |
| 官方选定套件（`rv32ui-p-*` / `rv32mi-p-*`） | 56 通过、0 失败、2 排除、0 跳过 |
| Spike 自带探针 | 225 步程序一致；脚本共比较 1999 条退休记录，包含末尾自旋 |
| 交叉工具链冒烟测试 | 通过 |
| 格式、Clippy、rustdoc | 通过 |

这些结果描述当前选定测试的覆盖范围，不代表完整 ISA/特权规范认证或硬件验收。下一步优先补齐特权与陷阱语义、扩大差分覆盖并固定工具版本，再实现 C 扩展和最小单周期 RTL。完整范围见工作清单与路线图。

## 文档

| 文档 | 内容 |
|------|------|
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | 模型架构设计：模块划分、执行循环、陷阱与 CSR 语义、内存模型、已知限制 |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | P0–P8 长期里程碑与验收标准 |
| [`docs/TODO.md`](docs/TODO.md) | 可执行工作清单（按优先级排列，含完成状态） |
| [`docs/RISC-V.md`](docs/RISC-V.md) | ISA 背景参考（**非**本项目状态，权威来源为官方手册） |

想了解现状先看 `docs/TODO.md`；想改代码先看 `docs/ARCHITECTURE.md`。

## 环境要求

- Rust 工具链（stable）。若 `cargo` 不在 `PATH` 中，添加 `~/.cargo/bin`：
  ```sh
  export PATH="$HOME/.cargo/bin:$PATH"
  ```
- 可选，仅端到端冒烟测试需要：RISC-V 交叉工具链
  ```sh
  apt-get install gcc-riscv64-unknown-elf   # 提供 riscv64-unknown-elf-gcc / -objcopy
  ```
  工具链虽名为 `riscv64-*`，冒烟程序的目标是 `rv32im`。

## 构建与测试

```sh
cargo build
cargo test --all-targets
cargo test --release --all-targets    # release 关闭溢出检查，结果必须一致
```

150 个测试，分布在：

| 测试文件 | 数量 | 覆盖内容 |
|----------|------|----------|
| `tests/rv32im.rs` | 7 | 手工编码指令字的 RV32I/M/Zicsr 执行语义 |
| `tests/isa_coverage.rs` | 9 | 表驱动的指令组覆盖与架构边界情况 |
| `tests/traps.rs` | 16 | 陷阱进入/`mret`、CSR 字段语义、扩展缺失诊断 |
| `tests/memory.rs` | 15 | 地址映射、区域权限、访问故障、后备实现等价性 |
| `tests/htif.rs` | 13 | `tohost` 协议编解码、`riscv-tests` 平台内存布局 |
| `tests/interrupts.rs` | 19 | CLINT 寄存器、`mip`/`mie`/`MIE` 门控、优先级、向量入口、WFI |
| `tests/delegation.rs` | 11 | 委派、中断门控矩阵、CSR 别名、嵌套陷阱、TSR、Bare 能力与 trace |
| `tests/privilege.rs` | 22 | M/S/U 转换、`ecall` 原因码、`mret`/`sret`、CSR 特权判定、别名 |
| `tests/trace.rs` | 23 | trace 格式的逐字文本，以及「挂 trace 不改变执行」 |
| `tests/architecture.rs` | 7 | 取指对齐、非对齐数据访问、计数器、保留编码 |
| `tests/cli.rs` | 8 | 驱动层：参数、退出码、状态输出 |

测试用手工编码的指令字（`tests/common/mod.rs` 提供编码器，避免手写位域出错）。`cargo test` 只跑 Rust 测试，**不**验证编译器产生的真实编码——那需要交叉工具链，由下面的冒烟测试与一致性套件负责。

## 生成并运行裸机程序

模型执行**裸二进制镜像**（raw binary），不是 ELF。用交叉工具链从汇编生成：

```sh
# 汇编并抽取 .text 为裸二进制
riscv64-unknown-elf-gcc -march=rv32im -mabi=ilp32 -nostdlib -nostartfiles \
    -Wl,-Ttext=0x80000000 -o smoke.elf scripts/smoke.S
riscv64-unknown-elf-objcopy -O binary --only-section=.text smoke.elf smoke.bin

# 在模型上运行
cargo run --bin chips -- smoke.bin
```

从 ELF 工程抽取镜像的通用写法：`objcopy -O binary --only-section=.text <in.elf> <out.bin>`。

## CLI 用法

```
chips <image.bin> [start_addr] [max_instructions] [--htif[=tohost_addr]]
```

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `image.bin` | 裸指令字节（由裸机程序链接后抽取 `.text` 得到） | 必填 |
| `start_addr` | 复位向量，十进制或 `0x` 十六进制 | `0x80000000` |
| `max_instructions` | 指令预算，用尽后报告为 `Limit` | `10000000` |
| `--htif` | 按 `riscv-tests` 平台运行，并从 `tohost` 判定结果（见下） | 关 |
| `--htif=ADDR` | 同上，但指定 `tohost` 寄存器地址 | `0x80001000` |

输出为最终机器状态（真实运行 `scripts/smoke.S` 的结果，省略中间的零值寄存器）：

```console
$ chips smoke.bin
stopped: Ebreak
pc = 0x80000020
x00 zero 00000000
x01 ra   00000000
...
x10 a0   00000006
x11 a1   00000007
x12 a2   0000002a
x13 a3   ffffffec
x14 a4   00000003
x15 a5   fffffffa
x16 a6   fffffffe
x17 a7   ffffffff
...
cycle = 9  instret = 8  time = 9
misa = 0x40001100
```

寄存器按 ABI 名称打印（`x10` 显示为 `a0`）。`pc` 停在 `ebreak` 自身——`ebreak` 不推进 PC 也不退休，故 `instret` 比 `cycle` 少 1。安装了陷阱处理程序（`mtvec != 0`）时额外打印 `mtvec`/`mepc`/`mcause`/`mtval`/`mstatus`。

**退出码**：

| 码 | 含义 |
|----|------|
| `0` | 正常停止：`Ecall`、`Ebreak`（程序自行停机）或 `Limit`（指令预算用尽） |
| `1` | 发生陷阱且未安装处理程序，状态已转储到 stderr |
| `2` | 用法错误，或镜像文件无法读取 |

未安装处理程序时 `ecall`/`ebreak` 结束运行循环——这正是裸机程序自行停机的方式。

### `--htif` 模式

`riscv-tests` 的测试**不会停机**：成功或失败时它把一个结果码写入 `tohost` 寄存器然后自旋。因此 `--htif` 会加载 `riscv-tests` 平台（含 RAM、设备页与栈 RAM），逐条执行并在每条之后检查 `tohost`，一旦结果落定立即停止：

```console
$ chips rv32ui-p-add.bin 0x80000000 --htif=0x80001000
PASS (506 instructions, 501 retired)
```

结果码约定（来自 `env/p/riscv_test.h`）：`1` 为通过；任意大于 1 的奇数 `(check << 1) | 1` 表示第 `check` 项检查失败。

`tohost` **不是固定地址**：链接脚本把它放在 `.text.init` 之后的第一个 4 KiB 边界上，所以 `.text.init` 超过一页的测试会得到更高的地址。必须从符号表读取（`scripts/riscv-tests.sh` 就是这么做的），不能依赖写死的数值。

### `--trace` 模式

`--trace` 把逐指令 trace 写到 stdout 且**只**写 trace（状态转储被抑制，以免与 trace 交织），格式见 [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) §10：

```console
$ chips program.bin 0x100 --trace
v1
i 1 pc=00000100 inst=00100513 x10=00000001
t 2 pc=00000104 inst=00000073 cause=11 tval=00000000 c341=00000104 c342=0000000b c343=00000000
x 3 pc=00000300 inst=00000073
```

该格式是**为差分测试冻结的线路格式**，不是调试辅助：ROADMAP P3 要逐步比较模型与 RTL，两者必须能对每一步说出同一件事，而格式必须在 RTL 存在之前定下来——否则硬件会被迫适配一个仍在变动的格式，每一次分歧都会变成关于格式的争论而不是关于内核的争论。`tests/trace.rs` 逐字锁定每一条 trace 行的文本。

## 一致性套件

```sh
scripts/riscv-tests.sh                # 克隆 riscv-tests、构建并运行
scripts/riscv-tests.sh --require      # 缺少工具链则失败——CI 用此模式
scripts/riscv-tests.sh rv32ui         # 只跑一个套件
RISCV_TESTS_DIR=/path/to/tree scripts/riscv-tests.sh   # 复用已有克隆（不会被删除）
```

脚本会克隆 `riscv-tests`（含 `env` 子模块）、以 `XLEN=32` 配置、构建 `rv32ui-p-*` 与 `rv32mi-p-*`，然后逐个在模型上运行。

当前结果：**58 个测试，56 通过，0 失败，2 排除**。被排除的两个需要模型尚未实现的设施，因此单独报告而非计入通过：

| 测试 | 需要 |
|------|------|
| `rv32mi-p-breakpoint` | 调试触发模块（`tcontrol`/`tselect`/`tdata1-3`） |
| `rv32mi-p-pmpaddr` | 物理内存保护（`pmpcfg0`/`pmpaddr0-15`） |

`rv32mi-p-illegal` 已解除排除并通过：支持软件置起 SSIP、正确的中断原因码与向量入口、`mideleg` 及 TSR 门控。模型选择 Bare-only 能力：`satp`、TVM、SUM、MXR 读零，`SFENCE.VMA` 产生非法指令异常，测试走无分页分支。剩余排除项在脚本中显式列出，任何其他测试失败都会令脚本以非零码退出。

## 与 Spike 的差分测试

```sh
bash scripts/difftest.sh             # 缺 Spike 则 SKIP（exit 0）
bash scripts/difftest.sh --require   # 缺 Spike 则失败
bash scripts/difftest.sh -v          # 首次分歧的完整上下文
SPIKE=/path/to/spike bash scripts/difftest.sh
```

在同一程序上分别跑模型与 Spike，逐步比较**两者都报告的交集**：PC、指令字、目的寄存器与其值、内存写入的地址与值。只比较已退休的指令——Spike 的提交日志仅记录这些。

自带的探针程序是 `scripts/difftest.S`（**不是**任何套件成员）：官方测试的序言会探测可选 CSR，两侧实现的子集不同，可能在真实运算之前就走出不同路径。当前结果：**225 步程序，两侧完全一致**。

两处必须留在比较范围之外，都是「架构未规定」而非缺陷：**非对齐数据访问**（Spike 陷阱、模型完成，两者都合规）与**取值由实现决定的寄存器**（`misa` 报告扩展位，计数器只在各自机器上有意义）。详见 [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) §11。

## 端到端冒烟测试

`scripts/riscv-smoke.sh` 汇编 `scripts/smoke.S`，抽取镜像，在模型上运行并校验最终寄存器状态。这是不依赖 riscv-tests 的最小端到端检查。

```sh
scripts/riscv-smoke.sh              # 缺少工具链则跳过（exit 0）
scripts/riscv-smoke.sh --require    # 缺少工具链则失败——CI 用此模式
```

工具名可用 `RISCV_GCC` / `RISCV_OBJCOPY` 覆盖。

## CI 覆盖范围

`.github/workflows/ci.yml` 共四个 job：

| Job | 内容 |
|-----|------|
| `lint` | `cargo fmt --check`、Clippy（`-D warnings`）、`cargo doc`（`-D warnings`） |
| `test` | Linux / macOS / Windows，debug 与 release 两种 profile |
| `riscv-smoke` | 安装交叉工具链，以 `--require` 运行冒烟测试 |
| `riscv-tests` | 安装交叉工具链与 autoconf 等，以 `--require` 运行官方一致性套件 |

**CI 尚不覆盖**：

- 与 Spike 的差分比对——Spike **不在 apt 源中**，需从源码构建（`riscv-isa-sim`）。当前 CI 未配置 Spike 的固定版本构建与缓存，因此尚未将它纳入门禁：`bash scripts/difftest.sh` 在缺 Spike 时打印 `SKIP` 并以 0 退出，加 `--require` 可让缺失变成失败
- RTL 硬件轨道——本仓库尚无 RTL
- `rv32mi-p-breakpoint` 与 `rv32mi-p-pmpaddr`（见上文排除说明）

另有一条硬性约束：`Cargo.toml` 中声明 `unsafe_code = "forbid"`，模型全部为安全 Rust。CI 将警告视为失败，因此这条约束是构建期检查而非约定。

## 已知限制

当前限制如下。未实现的指令扩展与 CSR 访问会报错；部分已存在的 CSR 只实现字段子集或固定读值（如 `satp` 固定为 Bare），不能据此视为完整支持：

- `C`（压缩）、`A`（原子）、`F`/`D`（浮点）、`V`（向量）扩展
- 虚拟内存与地址翻译：`satp` 固定读零（Bare），TVM/SUM/MXR 读零，`SFENCE.VMA` 非法；TSR 可写且对 S 级 SRET 生效
- PMP 物理内存保护、调试触发模块
- PLIC（故 `MEIP` 永不被置起）、外设总线与 boot ROM
- 差分比较不含 CSR 写入——Spike 提交日志不记录它们，不在交集内

完整清单与优先级见 [`docs/TODO.md`](docs/TODO.md)。

## 许可

MIT，见 [`LICENSE`](LICENSE)。
