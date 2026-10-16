# chips

一颗 RISC-V 处理器的双轨研发仓库：**Rust 功能模型**（黄金参考）与**硬件 RTL**（尚未开始）并行推进，以交叉验证为耦合主线。

当前轨道 A 已实现一个可执行的 **RV32IMZicsr** 核心：完整 RV32I 基础整数集、RV32M 乘除、Zicsr CSR 访问，**M/S/U 三级特权**与陷阱进入、CLINT 中断交付与内存映射 `mtime`，以及 16 项 PMP 物理内存保护与两个原生地址触发器。它是后续 RTL 的**黄金参考模型**——RTL 必须与它跑同一套程序、得到一致结果。

模型已接入官方 **`riscv-tests`** 一致性套件：`rv32ui-p-*` 与 `rv32mi-p-*` 共 58 个测试中 **58 个通过**，**0 失败、0 排除、0 跳过**（详见下文）。

模型同时接入 **Spike 差分测试**：四组探针逐步比较退休指令与内存写入，并独立读取 PC、特权级、全部 32 个整数寄存器及 40 个 CSR 的共有字段，覆盖陷阱入口及最终状态。尚未覆盖随机指令流和全部 CSR 能力。

- 目标配置：`RV32IMC` 起步（当前 `RV32IMZicsr`，`C` 扩展待做）
- 特权级：M / S / U；支持 `medeleg`/`mideleg` 陷阱委派与 S 级中断，地址翻译仅支持 Bare
- 硬件轨道：RTL 与 Verilator 尚未纳入本仓库；Spike 作为外部参考已接入差分测试

## 当前完成程度

截至 2026-10-16 本地复测：**软件功能模型与验证基础已建立，硬件实现尚未开始**。按路线图，P0 基线已达成；P1 的选定官方定向套件已全部通过，随机差分验收尚未达成；P2 RTL 与 P3 模型/RTL 交叉验证未开始。

| 本地检查 | 结果 |
|----------|------|
| Rust 测试（debug / release） | 每种 profile 均为 182 通过、0 失败 |
| 官方选定套件（`rv32ui-p-*` / `rv32mi-p-*`） | 58 通过、0 失败、0 排除、0 跳过 |
| Spike 探针 | 通用 / PMP / 触发器 / 委派：225 / 103 / 105 / 53 步，全部一致；不计末尾自旋 |
| 交叉工具链冒烟测试 | 通过 |
| 格式、Clippy、rustdoc | 通过 |

这些结果描述当前选定测试的覆盖范围，不代表完整 ISA/特权规范认证或硬件验收。下一步扩展受限随机差分覆盖，再实现 C 扩展和最小单周期 RTL。完整范围见工作清单与路线图。

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

182 个测试，分布在：

| 测试文件 | 数量 | 覆盖内容 |
|----------|------|----------|
| `tests/rv32im.rs` | 7 | 手工编码指令字的 RV32I/M/Zicsr 执行语义 |
| `tests/isa_coverage.rs` | 9 | 表驱动的指令组覆盖与架构边界情况 |
| `tests/traps.rs` | 16 | 陷阱进入/`mret`、CSR 字段语义、扩展缺失诊断 |
| `tests/triggers.rs` | 12 | 双触发器、特权门控、断点委派、精确异常与设备副作用 |
| `tests/pmp.rs` | 12 | PMP 权限、匹配边界、锁定、MPRV、访问故障、设备副作用 |
| `tests/memory.rs` | 15 | 地址映射、区域权限、访问故障、后备实现等价性 |
| `tests/htif.rs` | 13 | `tohost` 协议编解码、`riscv-tests` 平台内存布局 |
| `tests/interrupts.rs` | 19 | CLINT 寄存器、`mip`/`mie`/`MIE` 门控、优先级、向量入口、WFI |
| `tests/delegation.rs` | 11 | 委派、中断门控矩阵、CSR 别名、嵌套陷阱、TSR、Bare 能力与 trace |
| `tests/privilege.rs` | 22 | M/S/U 转换、`ecall` 原因码、`mret`/`sret`、CSR 特权判定、别名 |
| `tests/trace.rs` | 23 | trace 格式的逐字文本，以及「挂 trace 不改变执行」 |
| `tests/architecture.rs` | 7 | 取指对齐、非对齐数据访问、计数器、保留编码 |
| `tests/cli.rs` | 16 | 驱动层：参数、退出码、检查点、独立状态快照与 trace 输出 |

另有 20 个 Python 回归（13 个差分 harness、7 个构建缓存）：`python3 -m unittest discover -s scripts/tests -v`，已接入 CI，无需 Spike 或交叉工具链。

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
| `0` | 正常停止：`Ecall`、`Ebreak`、`Limit` 或到达指定检查点 |
| `1` | 未处理陷阱、HTIF 失败或未到达指定检查点；诊断在 stderr |
| `2` | 用法错误，或镜像 / 状态日志 I/O 失败 |

未安装处理程序时 `ecall`/`ebreak` 结束运行循环——这正是裸机程序自行停机的方式。

`--stop-pc=ADDR` 在执行指定地址之前停止；预算耗尽或提前停机却未到达该地址时返回 1，不能与 `--htif` 同用。`--state-log=FILE` 将初始状态及每次尝试执行后的实际状态写入独立文件，含陷阱进入；`--trace` 的 stdout 始终只含 trace。快照格式见架构文档 §11。

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

当前结果：**58 个测试，58 通过，0 失败，0 排除，0 跳过**。脚本不再维护排除清单，选定的 `rv32ui-p-*` 与 `rv32mi-p-*` 程序全部执行。

`rv32mi-p-breakpoint` 已纳入门禁并通过。模型实现两个独立的 type-2 `mcontrol` 地址触发器，支持执行/读/写、M/S/U 模式选择与执行前断点异常。使用 MIE/SIE 门控防止处理程序重复触发，保留 `medeleg[3]` 断点委派；可选的 `tcontrol` 未实现。支持范围与扩展限制见架构文档。

`rv32mi-p-illegal` 已解除排除并通过：支持软件置起 SSIP、正确的中断原因码与向量入口、`mideleg` 及 TSR 门控。模型选择 Bare-only 能力：`satp`、TVM、SUM、MXR 读零，`SFENCE.VMA` 产生非法指令异常，测试走无分页分支。任何测试失败都会令脚本以非零码退出。

`rv32mi-p-pmpaddr` 也已纳入门禁并通过。本模型 PMP 为 16 项、4 字节粒度（G=0）、32 位物理地址，支持 OFF/TOR/NA4/NAPOT、R/W/X 与 L 锁定。该官方测试在 G=0 时仅检查寄存器探测路径；实际隔离行为另由 12 个 Rust 回归和 PMP 汇编探针验证。复位后 S/U 默认没有访存权限，固件必须先配置 PMP 放行规则；MPRV 可让 M 级数据访问使用 MPP 权限。

## 与 Spike 的差分测试

```sh
bash scripts/difftest.sh             # 缺 Spike 则 SKIP（exit 0）
bash scripts/difftest.sh --require   # 缺 Spike 则失败
bash scripts/difftest.sh -v          # 比较起点、检查点与超时配置
SPIKE=/path/to/spike bash scripts/difftest.sh
```

仓库将 Spike 源码固定在 [`spike.lock.json`](scripts/spike.lock.json) 中的提交 `0bff12123b1fd510e19e19634dd997dbade70e54`。可直接构建并运行：

```sh
# Ubuntu 构建依赖；另外需安装 gcc-riscv64-unknown-elf、Rust 和 Python 3.10+
sudo apt-get install build-essential git device-tree-compiler libgmp-dev libmpc-dev
SPIKE=$(bash scripts/build-spike.sh --jobs 4)
export SPIKE
bash scripts/difftest.sh --require
```

构建器从上游按完整 SHA 获取干净源码，固定配置为 `--with-boost=no`、GCC/G++ 与 `-O2`，只构建差分所需的 `spike` 可执行文件，不安装 SDK 或 `pk`。stdout 只返回二进制绝对路径；进度写入 stderr，完整构建日志保存在 `target/tools/spike/<key>.log`。首次构建可能耗时数分钟，默认最多 4 个并行任务。

缓存位于 `target/tools/spike/<key>/`，`build.json` 记录源码、构建脚本摘要、主机 ABI、编译器/链接器身份和二进制 SHA-256。命中缓存时重新校验这些内容；中断或失败的构建不会发布缓存条目。`--cache-dir DIR` 可指定缓存根目录，`--cache-key` 仅输出缓存标识，不下载或构建。若条目损坏，删除报错中的生成目录或选用新的缓存目录后重试。临时源码与对象文件自动清理，构建日志保留。

这固定了源码和构建配置，不保证跨系统产物逐字节相同；系统依赖仍由宿主机提供。更新参考版本时修改 lock，并用新生成的二进制重新验证全部探针。CI 使用同一构建器和严格匹配的缓存，Ubuntu 24.04 上以 `--require` 执行四组差分；本地直接调用 `difftest.sh` 的缺工具跳过行为保留。

默认构建并运行四组探针：通用指令 225 步、PMP 103 步、触发器 105 步、M/S/U 委派与嵌套陷阱 53 步；对应退休记录为 224 / 100 / 101 / 51 条。程序到达 `pass_spin` 即停止，不比较末尾自旋。四组逐步状态均一致。

harness 比较退休记录中的 PC、指令字、目的寄存器写入及内存写入，还通过模型 `--state-log` 和 Spike 调试器分别读取每步后的 PC、特权级、x0–x31 与 **40 个 CSR 的共有字段**。陷阱入口即使没有退休记录，其 `mepc`/`mcause`/`mtval` 和 S 级对应状态也被直接核对。CSR 写入同值或别名的文本记录不要求一一相同；比较的是实际逐步效果。掩码及边界见 [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) §11。

需要 Python 3.10+ 标准库、Rust、交叉工具链和支持 `--debug-cmd`/`--log-commits` 的 Spike。运行超时、异常退出、日志缺失或任何分歧均失败，不再按可选 CSR 降级为 gap。`--budget` 限制模型步数，`--timeout` 限制每个外部命令的秒数（默认 30）。`-v` 显示比较起点与检查点。

也可指定 ELF，默认检查 `pass_spin`；用 `--checkpoint=符号或地址` 指定其他成功检查点：

```sh
riscv64-unknown-elf-gcc -march=rv32im_zicsr -mabi=ilp32 -nostdlib -nostartfiles \
    -T scripts/difftest.ld -o target/pmp.elf scripts/pmp.S
SPIKE=/path/to/spike bash scripts/difftest.sh --require target/pmp.elf
```

四个探针先显式清零 Spike 默认开放的 PMP 项，再从 `difftest_start` 开始比较状态；初始化指令仍参与退休记录比较。自定义 ELF 没有此符号时从初始状态开始核对，因此也会报告启动配置差异。非对齐数据访问、计数器与能力寄存器等实现差异仍须在程序设计时处理，不能要求两侧任意程序都一致。

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
| `lint` | 格式、Clippy、rustdoc 与 20 个 Python 差分 / 缓存回归 |
| `test` | Linux / macOS / Windows，debug 与 release 两种 profile |
| `riscv-smoke` | 安装交叉工具链，以 `--require` 运行冒烟测试 |
| `riscv-tests` | 安装交叉工具链与 autoconf 等，以 `--require` 运行官方一致性套件 |
| `spike-difftest` | Ubuntu 24.04，固定 Spike 源码构建 / 校验缓存，以 `--require` 运行四组探针 |

**CI 尚不覆盖**：

- RTL 硬件轨道——本仓库尚无 RTL

另有一条硬性约束：`Cargo.toml` 中声明 `unsafe_code = "forbid"`，模型全部为安全 Rust。CI 将警告视为失败，因此这条约束是构建期检查而非约定。

## 已知限制

当前限制如下。未实现的指令扩展与 CSR 访问会报错；部分已存在的 CSR 只实现字段子集或固定读值（如 `satp` 固定为 Bare），不能据此视为完整支持：

- `C`（压缩）、`A`（原子）、`F`/`D`（浮点）、`V`（向量）扩展
- 虚拟内存与地址翻译：`satp` 固定读零（Bare），TVM/SUM/MXR 读零，`SFENCE.VMA` 非法；TSR 可写且对 S 级 SRET 生效
- 完整外部调试：JTAG/DMI、Debug Mode、单步尚未实现；原生触发器仅支持 type-2 地址等值匹配与执行前异常，无 `mcontrol6`、链式/数据值匹配、hit 位或上下文筛选
- 可选 `tcontrol` 不存在；通过 MIE/SIE 门控支持原生断点。PMP 未实现 Smepmp 增强
- PLIC（故 `MEIP` 永不被置起）、外设总线与 boot ROM
- 差分仅覆盖四组定向探针、40 个 CSR 的共有字段和当前选中触发器；随机流、计数器、设备 pending 与全部能力字段尚未纳入

完整清单与优先级见 [`docs/TODO.md`](docs/TODO.md)。

## 许可

MIT，见 [`LICENSE`](LICENSE)。
