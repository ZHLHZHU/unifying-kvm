> 历史设计计划：其中 UART、电源和调试方案已有变更。当前实板以 [HARDWARE.md](HARDWARE.md) 为准，勿据此接线。

# unifying-kvm 一体板 PCB 计划与选型

RP2040 + nRF52840 模组单板：USB-C 供电、板载 USB-A Host 口接键盘、UART1 控制 nRF 发射 Unifying。

## Context

Pico 开发板用普通转接线接 ikbc c87 时，键盘 VBUS 无 5V。实测 Pico 侧 USB 总线上没有设备（`SIE_STATUS` 线状态与速度位为 0、SOF 计数为 0、TinyUSB 设备表为空、`hid_dev`/`protocol_state` 全 0），而 `SIE_CTRL=0x20008600` 表明 host 模式与 D+/D- 下拉已正确启用：问题在供电，不在固件。本板解决供电并整合两颗 MCU，固件引脚与协议不变。

```text
USB-C（Sink, 5V）── polyfuse ──┬── USB-A VBUS（键盘，含大容量储能）
                               └── 3.3V LDO ──┬── RP2040（内部 LDO 产生 1.1V）
                                              └── nRF52840 模组
RP2040 USB_DP/DM ── 27Ω ── USB-A D+/D-（Host）
RP2040 GP4/GP5 ── nRF P0.02/P0.06（UART 1 Mbps）
```

## 已确认决策

| 项目 | 决策 |
| --- | --- |
| USB PHY | 独占 USB-A Host，不做 device 模式切换 |
| RP 烧写 | SWD 排针；BOOTSEL 仅作恢复用 |
| nRF 烧写 | 模组 SWD 首刷，之后走其自身 USB 的 CDC OTA |
| 键盘 VBUS | 直通 5V + polyfuse（预留限流开关封装） |
| USB-C | 纯 Sink，CC1/CC2 各 5.1k 下拉，固定 5V |
| 板层 | 4 层、1.6mm；USB 差分需按 4 层叠层算 90Ω |

## Approach

### 步骤 1：电源链

| 位号 | 器件 | 关键参数 | 说明 |
| --- | --- | --- | --- |
| J1 | USB-C 16P 母座（Sink only） | — | 只用 VBUS/GND/CC1/CC2/D± 中的电源与 CC |
| R1, R2 | 5.1kΩ ±5% 0402 | CC1、CC2 各一只到 GND | 向 Source 声明 Sink，取默认 5V |
| F1 | Polyfuse | 保持 1.1A、跳闸 2.2A 级 | 串 USB-C VBUS 入口 |
| D1 | TVS 二极管 | 5V 轨 ESD/浪涌 | 入口对 GND |
| C1 | 10µF 1206 X5R 16V | 5V 入口储能 | — |
| C2 | 220µF 低 ESR | USB-A VBUS 旁 | 抑制键盘浪涌引起的跌落 |
| U1 | LDO 3.3V/1A，如 NCP1117ST33T3G | Vin 5V、Vout 3.3V | 官方最小设计同款 |
| C3, C4 | 10µF 各一只 | U1 输入、输出 | 按 NCP1117 数据手册 |

**预留限流开关：** 在 USB-A VBUS 支路放 SOT-23-5 限流开关封装（TPS2553 兼容）与 ILIM 电阻位，默认不贴、用 0Ω 跨接直通；实测跌落复位时改贴。

> polyfuse 为秒级动作，不防浪涌跌落。c87 带背光，启动电流可能使共用 5V 轨下陷并复位 RP2040。C2 与该预留封装是对此的唯一缓解，必须在验证第 6 项时实测。

### 步骤 2：RP2040 最小系统

按官方《Hardware design with RP2040》取值，不自创参数：

| 位号 | 器件 | 关键参数 | 说明 |
| --- | --- | --- | --- |
| U2 | RP2040 | QFN-56 7×7mm 0.4mm pitch | 必须机器贴装 |
| C5… | 100nF 0402 ×8 | 每个电源脚一只 | 就近放置 |
| C6 | 1µF 0402 X5R 16V | VREG_VIN（脚 45）旁 | 官方明确要求 |
| C7 | 1µF 0402 X5R 16V | VREG_VOUT（脚 44）旁 | VREG_VOUT 直连 DVDD |
| U3 | W25Q128JVSIQ | 16MB QSPI NOR，SOIC-8 | 官方同款；QSPI 短直连 |
| R3 | 1kΩ 0402 | QSPI_SS → BOOTSEL 跳线 | 官方要求，保护过驱 |
| R4 | 10kΩ 0402（DNF） | QSPI_SS 上拉 3.3V | 换其他 flash 时再贴 |
| X1 | Abracon ABM8-272-T3 | 12MHz、CL=10pF、ESR≤50Ω | 官方强烈推荐型号 |
| C8, C9 | 15pF 0402 ±5% | X1 两端各一只 | 与 3pF 寄生合成约 10.5pF |
| R5 | 1kΩ 0402 | XOUT 串联阻尼 | 3.3V IOVDD 下的官方取值 |
| R6, R7 | 27Ω 0402 1% | USB_DP/DM 串联 | 必须靠近 RP2040 |
| SW1 | 轻触开关 | BOOTSEL（短接 R3 到 GND） | 恢复用 |
| SW2 | 轻触开关 | RUN → GND | 复位 |

**flash 容量：** 16MB 远超需要，但与官方参考一致、易采购。如改用 2MB（如 W25Q16JV），须在 board header 覆盖 `PICO_FLASH_SIZE_BYTES`，当前 `pico` 默认即 2MB。

### 步骤 3：USB-A Host 口

| 位号 | 器件 | 说明 |
| --- | --- | --- |
| J2 | USB-A 母座，直立或卧式 | VBUS/D+/D-/GND |
| D2 | 低电容 TVS 阵列（D+/D-） | ESD，Cj 尽量小 |

- D+/D- 走 90Ω 差分、等长，整条路径下方必须是连续地。
- RP2040 host 模式自带 D+/D- 下拉，**不加外部上拉/下拉**（实测 `SIE_CTRL` 的 `PULLDOWN_EN` 已置位）。
- 差分线远离 UART、LDO 与大电流 VBUS 路径。

### 步骤 4：nRF52840 模组

沿用 nice!nano 兼容模组及现有 pin map，固件不改：

| 模组引脚 | 本板连接 | 说明 |
| --- | --- | --- |
| P0.02（`002`/D19） | RP2040 GP4（TX→RX） | UART 1 Mbps、8N1 |
| P0.06（`006`/D1） | RP2040 GP5（RX←TX） | — |
| GND | 共地 | — |
| `SWD`/`SWC` | 2.54 排针 | 首刷 bootloader/app |
| 模组 USB-C | 保持可插拔 | CDC/OTA，独立于 RP 的 USB |

**供电必须核对模组丝印语义。** nice!nano 的 `3.3V` 引脚是其板载稳压器的**输出**，不可反灌；`RAW`/`B+` 是电池/输入侧。本板给模组供电只能走其输入引脚或模组自身 USB，不能把本板 3.3V 接到模组的 3.3V 输出脚。实际使用的大熊兼容模组须按其原理图确认后再定；未确认前按“模组自带 USB 供电、本板只接 UART 与 GND”布线并保留供电跳线位。

模组天线区下方不铺铜，按模组手册留 keep-out。

### 步骤 5：调试与诊断接口

| 接口 | 引脚 | 说明 |
| --- | --- | --- |
| RP SWD | SWCLK、SWDIO、GND、3.3V | 2.54 排针，日常烧写与读 `rp_debug_*` |
| RP UART0 | GP0（TX）、GP1（RX）、GND | 115200 诊断，不接无线 UART |
| nRF SWD | `SWC`、`SWD`、GND | 首刷用 |

### 不改动的部分

- 固件引脚与 UART 参数（GP4/GP5、1 Mbps、8N1）— 板子适配固件
- nRF UARTE0 的 P0.02/P0.06 映射
- Unifying/ESB 协议、配对存储格式、CDC/OTA 线协议

## Critical files

| 文件 | 锚点 | 用途 |
| --- | --- | --- |
| `rp2040/native/bridge.c` | `bridge_init` | UART1 引脚/波特率；新板沿用 GP4/GP5 |
| `rp2040/CMakeLists.txt` | `PICO_BOARD` | 若 flash 容量或引脚偏离 pico 默认需加 board header |
| `nrf52-unifying/app/src/main.rs` | `uarte::Uarte::new` | nRF 侧 P0.02/P0.06 与 1 Mbps |
| `docs/HARDWARE.md` | 全文 | 本计划的电源与接口边界说明 |

## Verification

1. 空板上电、不插键盘 → 5V 与 3.3V 轨在容差内，静态电流无异常，LDO 不过热。
2. SWD 识别 RP2040 → 刷现有 `rp2040_kvm.elf` 并回读校验通过。
3. nRF 模组 SWD 识别 `nRF52840_xxAA` → 刷 bootloader/app，CDC 返回项目 `VER`。
4. 不插键盘复位 RP → 读 `rp_debug_*`，自然收到 `VER` 与 `SWITCHED 0 ...`，无 UART timeout。
5. 插入 c87 → `SIE_STATUS` 出现线状态与速度位、SOF 递增、`hid_dev`/`protocol_state` 非 0。
6. 插入 c87 瞬间示波器看 5V 轨 → 跌落不触发复位（`rp_debug_task_count` 不归零）；若复位则贴限流开关并增大 C2。
7. 实际按键 → 目的电脑事件记录器看到正确 down/up；Scroll Lock 切换与闪灯符合固件行为。
8. USB 差分阻抗按所选叠层核算或实测 → 接近 90Ω；FS 速率有容差但须记录实测值。

## Assumptions

| 若实际情况不同 | 处理 |
| --- | --- |
| c87 浪涌导致 5V 跌落复位 | 贴预留限流开关、增大 USB-A 侧容量；不改固件 |
| 所用 nRF 模组供电脚语义与 nice!nano 不同 | 按该模组原理图定供电方式，保留跳线位，不反灌其 3.3V 输出 |
| 模组 UART 引脚与现有 pin map 不同 | 改原理图并同步 nRF 固件 UARTE 引脚 |
| flash 不用 16MB | 加 board header 覆盖 `PICO_FLASH_SIZE_BYTES` |
| 需要 BOOTSEL 拖 UF2 做日常烧写 | 改版加 USB 模拟开关；当前设计只支持 SWD |
| 无法采购 ABM8-272-T3 | 需按官方要求对新晶振做全温区起振测试，不直接替换 |

## 参考

- Raspberry Pi, *Hardware design with RP2040*（最小设计取值：1µF VREG 去耦、27Ω USB 串联、1kΩ QSPI_SS、15pF 晶振负载、1kΩ XOUT 阻尼、ABM8-272-T3）：https://pip.raspberrypi.com/documents/RP-008279-DS-hardware-design-with-rp2040.pdf
- nice!nano 引脚与原理图（模组 pin map 与供电脚语义）：https://nicekeyboards.com/docs/nice-nano/pinout-schematic
