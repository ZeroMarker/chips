# chips

一颗 RISC-V 处理器的双轨研发仓库：**Rust 功能模型**（黄金参考）与**硬件 RTL**（尚未开始）并行推进，以交叉验证为耦合主线。

当前轨道 A 已实现一个可执行的 **RV32IMZicsr** 核心：完整 RV32I 基础整数集、RV32M 乘除、Zicsr CSR 访问，M 特权级陷阱进入与 `mret` 返回。它是后续 RTL 的**黄金参考模型**——RTL 必须与它跑同一套程序、得到一致结果。

- 目标配置：`RV32IMC` 起步（当前 `RV32IMZicsr`，`C` 扩展待做）
- 特权级：当前仅 M 级
- 硬件轨道：RTL、Spike/QEMU、Verilator 尚未纳入本仓库

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

45 个测试，分布在：

| 测试文件 | 数量 | 覆盖内容 |
|----------|------|----------|
| `tests/rv32im.rs` | 7 | 手工编码指令字的 RV32I/M/Zicsr 执行语义 |
| `tests/isa_coverage.rs` | 9 | 表驱动的指令组覆盖与架构边界情况 |
| `tests/traps.rs` | 16 | 陷阱进入/`mret`、CSR 字段语义、扩展缺失诊断 |
| `tests/architecture.rs` | 6 | 对齐检查、计数器、保留编码 |
| `tests/cli.rs` | 7 | 驱动层：参数、退出码、状态输出 |

测试用手工编码的指令字（`tests/common/mod.rs` 提供编码器，避免手写位域出错）。`cargo test` 只跑 Rust 测试，**不**验证编译器产生的真实编码——那需要交叉工具链。

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
chips <image.bin> [start_addr] [max_instructions]
```

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `image.bin` | 裸指令字节（由裸机程序链接后抽取 `.text` 得到） | 必填 |
| `start_addr` | 复位向量，十进制或 `0x` 十六进制 | `0x80000000` |
| `max_instructions` | 指令预算，用尽后报告为 `Limit` | `10000000` |

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

## 端到端冒烟测试

`scripts/riscv-smoke.sh` 汇编 `scripts/smoke.S`，抽取镜像，在模型上运行并校验最终寄存器状态。这是 CI 中唯一验证**真实编译器编码**的环节（其余测试用手写指令字）。

```sh
scripts/riscv-smoke.sh              # 缺少工具链则跳过（exit 0）
scripts/riscv-smoke.sh --require    # 缺少工具链则失败——CI 用此模式
```

工具名可用 `RISCV_GCC` / `RISCV_OBJCOPY` 覆盖。

## CI 覆盖范围

`.github/workflows/ci.yml` 共三个 job：

| Job | 内容 |
|-----|------|
| `lint` | `cargo fmt --check`、Clippy（`-D warnings`）、`cargo doc`（`-D warnings`） |
| `test` | Linux / macOS / Windows，debug 与 release 两种 profile |
| `riscv-smoke` | 安装交叉工具链，以 `--require` 运行冒烟测试 |

**CI 尚不覆盖**：

- `riscv-tests` 官方测试套件（`rv32ui-*` / `rv32mi-*`）——缺少 runner 与目标平台定义
- 与 Spike / QEMU 的差分比对——外部参考未引入
- RTL 硬件轨道——本仓库尚无 RTL

另有一条硬性约束：`Cargo.toml` 中声明 `unsafe_code = "forbid"`，模型全部为安全 Rust。CI 将警告视为失败，因此这条约束是构建期检查而非约定。

## 已知限制

尚未实现，且会在遇到相应编码时明确报错而非静默出错：

- `C`（压缩）、`A`（原子）、`F`/`D`（浮点）、`V`（向量）扩展
- S / U 特权级（`mstatus.MPP` 硬连线为 M）
- 中断交付（`mip` 恒读 0，无 CLINT/PLIC，`wfi` 立即退休）
- 虚拟内存与地址翻译
- 内存无地址映射：任意地址都可访问，未映射区域读 0，因此无法建模访问故障

完整清单与优先级见 [`docs/TODO.md`](docs/TODO.md)。

## 许可

MIT，见 [`LICENSE`](LICENSE)。
