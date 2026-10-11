# 当前一体板硬件与调试记录

更新：2026-10-11。本文以已经焊接、上电和联调的实板为准。

## 仓库与连接

| 项目 | 位置 / 链接 |
| --- | --- |
| Linux 虚拟机 | `ssh lh@lh-pc-vm`（用户名 lh；从开发 Mac 使用现有 SSH 配置） |
| RP / KVM 仓库 | `/home/lh/unifying-kvm`；https://github.com/ZHLHZHU/unifying-kvm |
| nRF 仓库 | `/home/lh/nrf52-unifying`；https://github.com/ZHLHZHU/nrf52-unifying |
| Pico SDK | `/home/lh/pico-sdk`，2.2.0，`a1438dff1d38bd9c65dbd693f0e5db4b9ae91779` |
| TinyUSB | SDK 子模块 `86ad6e56c1700e85f1c5678607a762cfe3aa2f47` |
| EDA 项目（Mac） | `/Users/lh/Documents/LCEDA-Pro/projects/unifying-kvm` |
| 模组资料 | https://bluetoothdx.com/nicenano.html |
| 模组说明书 | https://daxiongble.oss-cn-shenzhen.aliyuncs.com/zmk/nice%EF%BC%81nano%E4%BA%A7%E5%93%81%E8%AF%B4%E6%98%8E%E4%B9%A6.pdf |

## 实板配置

- 四层板：L2 为 GND，L3 用于电源；nRF 天线区各层遵守禁铜区。
- USB-C 输入 5V，经 F1 到 `5V_SYS`；RT9013 产生 RP 所需 3.3V。
- RP2040 内置线性稳压器产生 1.1V 内核供电，非 DC-DC，不需要电感。
- U3 是 W25Q128JVSIQ，SOIC-8、128Mbit（16MiB）；构建设置 `PICO_FLASH_SIZE_BYTES=16777216`。
- X1 为 5032 两脚无源 12MHz 晶振；普通电阻、电容使用 0603，较大容量 MLCC 按实际封装。
- USB-A 是 RP2040 原生 USB Host。TPS2553 控制 `HOST_5V`；GP14 高电平使能，GP15 为低有效开漏故障输入。
- 设计限流范围约 0.8–1.5A；曾讨论 24k / 27k ILIM，最终实装阻值尚未核对。
- nice!nano 兼容 nRF52840 模组用邮票孔直接焊接。板内 `NRF_PWR` 经 R12 接模组 VBUS（5V 输入）；不能把模组 VBUS 当成 3.3V IO 电源。
- nRF USB 接口被电容挡住；目前通过 RP SWD 转发 UART 更新 nRF。
- 独立 UART 调试排针已删除；保留 RP 与 nRF 的 SWD。

## 实际信号连接

| 功能 | RP 引脚 / 网络 | nRF 模组引脚 |
| --- | --- | --- |
| RP 发送，nRF 接收 | GP4 / UART1 TX → `NRF_RXD` | `006` / P0.06 RX，模组焊盘 2 |
| nRF 发送，RP 接收 | GP5 / UART1 RX ← `NRF_TXD` | `008` / P0.08 TX，模组焊盘 3 |
| 公共地 | GND | GND |
| UART 参数 | 1,000,000 baud，8N1，无流控，RX 内部上拉 | UARTE0 |
| USB Host 使能 | GP14 / `USB_EN` | 不连接 nRF |
| USB Host 故障 | GP15 / `USB_FAULT_N`，内部上拉 | 不连接 nRF |
| RGB | GP16 / GP17 / GP18，共阳、低有效 | — |

**002 / P0.02 在当前 PCB 未连接。旧原型的 P0.02 RX、P0.06 TX 配置不适用于本板。**

## RGB LED1 接线与驱动

LED1 型号 SZYY0603RGB-B，为普通四脚共阳 RGB，不是 WS2812，不使用串行数据协议。

| 颜色 | RP2040 GPIO（芯片脚） | 串联电阻（原理图值） | LED1 阴极脚 |
| --- | --- | --- | --- |
| 红 | GP16（27）/ LED_R | R9，1kΩ | 1 / LED_R_K |
| 绿 | GP17（28）/ LED_G | R10，1kΩ | 2 / LED_G_K |
| 蓝 | GP18（29）/ LED_B | R11，1kΩ | 3 / LED_B_K |

LED1 的 4 脚为公共阳极，接 3V3。电流方向：3V3 → LED → 电阻 → RP GPIO。
GPIO 输出低电平点亮对应通道，高电平熄灭；PWM 控制时占空比也要按低有效处理。
初始化先 gpio_put(pin, 1)，再设置输出，避免初始化闪灯。三个 GPIO 均为高时全灭。
例：红色为 GP16=0、GP17=1、GP18=1；绿色为 1/0/1；蓝色为 1/1/0。
原理图阻值为 1kΩ，实际焊装阻值未单独测量。

KVM 固件用这颗 RGB 显示目标状态（规则见 README）：slot 0 绿、1 蓝、2 红；连不上快闪；配对慢闪；故障白色。键盘 Scroll Lock 灯不再使用。nRF 模组自己的 LED 是独立的。

## SWD 接线与供电

当前只有一个 Air32 CMSIS-DAP，最后连接在 RP2040。

| 调试器信号 | RP 调试时 | nRF 调试时 |
| --- | --- | --- |
| SWDIO | RP SWDIO | 模组 DIO / SWD |
| SWCLK | RP SWCLK | 模组 CLK / SWC |
| GND | 板 GND | 板 GND |
| 电源输出 | 断开，由板 USB-C 供电 | 断开，由板 USB-C 供电 |

SWD IO 是 3.3V 电平。有单独 VTref 输入的调试器应接目标 3.3V；不能把调试器电源输出误当 VTref。
可以同时将 Type-C 和调试器 USB 插入电脑，但不并联调试器供电输出。

- 探针 VID:PID：`0d28:0204`，Air32 CMSIS-DAPv2。
- 序列号：`000100011a5a5bb60000000000074c2ea5a5a5a597969908`。
- 已用 OpenOCD 0.12.0、SWD 1000kHz 成功烧录和回读验证两颗芯片。
- 多探针时用 `-c 'cmsis_dap_serial <序列号>'` 选择。

RP 构建与烧录（在虚拟机执行）：

```sh
cd /home/lh/unifying-kvm
cmake -S rp2040 -B rp2040/build -DPICO_BOARD=pico -DPICO_SDK_PATH=/home/lh/pico-sdk -DPICO_FLASH_SIZE_BYTES=16777216 -DRP_UART_SMOKE=OFF
cmake --build rp2040/build --target rp2040_kvm
openocd -f interface/cmsis-dap.cfg -f target/rp2040.cfg -c 'adapter speed 1000' -c 'program /home/lh/unifying-kvm/rp2040/build/rp2040_kvm.elf verify reset exit'
```

nRF 应用位于 `target/thumbv7em-none-eabihf/release/nrf-demo-app`，这是无扩展名 ELF。
需要 SWD 恢复应用时，先提取真实应用段，避免 ELF 的地址 0 文件头覆盖 bootloader：

```sh
cd /home/lh/nrf52-unifying
cargo build --locked -p nrf-demo-app --release
arm-none-eabi-objcopy -O ihex --only-section=.vector_table --only-section=.text --only-section=.rodata --only-section=.data target/thumbv7em-none-eabihf/release/nrf-demo-app /tmp/nrf-app.hex
openocd -f interface/cmsis-dap.cfg -f target/nrf52.cfg -c 'adapter speed 1000' -c 'program /tmp/nrf-app.hex verify reset exit'
```

不要直接用 OpenOCD `program` 烧上述 nRF ELF，也不要整片擦除已有配对数据。

nRF 分区：bootloader `0x00000–0x05FFF`，boot state `0x06000–0x06FFF`，app `0x07000–0x7EFFF`，DFU `0x7F000–0xF7FFF`，配对存储 `0xF8000–0xFFFFF`。
后续应用升级优先使用 [UART OTA 指南](UART-OTA.md)，DAPLink 留在 RP 即可。

## 已验证与下一步

2026-10-11 实测：

- 空载电流约 11mA；3.3V、1.1V 电源正常（用户万用表测量）。
- RP SWD 识别及 16MiB Flash 烧录/回读通过；RGB 闪灯测试通过。
- nRF SWD、修正后的 UART 双向握手通过。
- 经 RP 转发 UART 上传 46,760 字节，nRF 回读 CRC、重启、新版本及 BOOT 状态全部确认；版本 `1791650792` → `1791651163`。
- nRF slot 0：`PAIRED=1 CONN=1`；RP 开机释放全部按键命令获得成功回复。
- slot 1：`PAIRED=0`，选择它返回 `SWITCHED 1 EMPTY`。
- RP 最新状态 `WAIT_RELEASE`（状态码 12）、fault=0；键盘已枚举，接口 0 为 Boot 键盘，正在等待有效的全释放输入报告。

2026-10-11 本机 SWD 联调补充（探针接 RP2040，整板由 USB-C 供电）：

- 经板内 UART 中继读到 nRF `build=1791651163`、`STATUS SLOT=0 PAIRED=1 CONN=1`，计数随命令递增。
- UART 注入的 `UKEYDOWN 00 04` / `UKEYDOWN 00` 与 `UKEYDOWN 00 05` / `UKEYDOWN 00` 均返回 `OK`，`CNT` 每次按下+释放增加 2（5640→5642、5643→5645）。尚无目的端输入记录；用户报告接收器未插电脑，与 ACK 现象存在待解释的矛盾，不能据此确认目的端身份或完整键盘路径。
- 切槽往返：`USWITCH 01` → `SWITCHED 1 EMPTY`（`SLOT=1 PAIRED=0`），`USWITCH 00` → `SWITCHED 0 CONNECTED` 且恢复 `PAIRED=1 CONN=1`，计数未倒退。
- RP `reset run` 后仍 `PAIRED=1 CONN=1`，KVM 自然回到 `state=12`、`fault=0`，无 UART 超时。

USB-A 侧实测与结论：

| 读数 | 值 | 含义 |
| --- | --- | --- |
| `GPIO_OUT`/`OE`/`IN` 的 GP14 | 均为高 | TPS2553 使能信号正确 |
| GP15（`USB_FAULT_N`） | 高 | 无过流，保护未跳闸 |
| `USB_MAIN_CTRL` | `0x3` | 控制器使能 + host 模式 |
| `USB_SIE_CTRL` | `0x20008600` | `SOF_EN`/`KEEP_ALIVE_EN`/`PULLDOWN_EN` 已置位 |
| `SIE_STATUS` | `0x50000205` / `0x50000209` | 位 9:8 为 2，设备为全速；线状态变化不能直接解释为掉线 |
| `SOF_RD`（`USB_BASE+0x48`） | 实读 `0x616`、`0x623` | 主机帧计数在变化 |
| HID 接口 0 | 地址 1、IN 端点 `0x81`、8 字节、Boot 模式 | 已枚举，SET_PROTOCOL 成功 |

此前把速度位当作位 17:16，并把 `USB_BASE+0x28` 当作 SOF 读寄存器，导致“无设备、无帧、硬件故障”的判断错误，已撤回。

### 持续按住右箭头的排查

用户确认键盘为普通有线 iKBC，并持续按住右箭头。设备标识实读 `04b4:0042`。

- 两个接口均已枚举：接口 0 为 8 字节 Boot 键盘；接口 1 的描述符还包含报告 ID 5 的 NKRO 键盘位图。应用只绑定第一个键盘接口是已确认的限制，但不能据此认定当前故障原因。
- 对两个接口提交接收、分别设置 Boot/Report 协议后，均未收到中断报告；第二接口切换到 Report 协议也未解决问题。
- 使用现有 UART-smoke 固件作对照：不运行 KVM 解码和 LED 输出，两个接口均成功枚举为 Report 模式且接收已提交，输入回调仍为 0。因此当前故障不只发生在 Rust KVM 解码层。
- 控制端点实测：`GET_CONFIGURATION` 成功、实际长度 1、配置值 1；接口 0 的 `GET_REPORT` 成功、实际长度 8、数据全 0；接口 1 的报告 ID 5 查询返回 STALL、实际长度 0。控制查询结果不是右箭头输入证据。
- `SET_IDLE` 设置为 16ms 获得成功回复，但没有产生输入回调。主机 IRQ 采样中 RX timeout、DATA_SEQ_ERROR、STALL 均为 0；枚举后 BUFF_STATUS/TRANS_COMPLETE 计数保持不变。
- 键盘断电重枚举、启动时显式 USB 总线复位、缓冲控制写入间隔延长至 1µs，均未恢复输入。这一轮尚未取得 USB 报文，不能判定根因；后续 HCD 采样见下节。
- 全部临时采样与 SDK 修改已撤除，正式版和 UART-smoke 均重新构建。板上已刷回正式版并校验通过：两个 HID 接口挂载，Boot 协议成功，`WAIT_RELEASE`、fault=0。

### HCD 定向采样与线上 PID 证据

采样使用现有 UART-smoke 固件，临时记录 HCD 提交和完成状态，不改变 HID 解析或 KVM 上层。

| IN 端点 | `bInterval` | 提交时 `EP_CTRL` | 提交时 buffer control | 该包 PID / 下次软件 PID |
| --- | --- | --- | --- | --- |
| `0x81` | 10ms | `0xac090200` | `0x5408` | DATA0 / DATA1 |
| `0x82` | 1ms | `0xac000240` | `0x5440` | DATA0 / DATA1 |

- 禁用全部轮询、单独启用 `0x81`、单独启用 `0x82`、同时启用两者：每组清除 NAK 锁存后观察 10 次，均无重新置位。EPX 软件状态为空闲，SOF 计数推进，HCD 完成/错误计数不变。这是锁存采样结果，不是精确 NAK 包计数，也不能证明未发 IN。
- 通过空闲 DMA 通道只读采样 `USBPHY_DIRECT` 的 RX 状态位，再通过 SWD 导出 SRAM；未设置 PHY override、未超频。131,072 字节采样以 31.25MHz 为目标，覆盖约 4ms。
- NRZI 解码并验证 SYNC、PID 补码、位填充、EOP 和 CRC：解出 344 个地址 1、端点 1 的 IN token（字节 `69 81 58`，CRC5 正确），随后为 344 个 DATA1 包（8 字节全 0，CRC16 为 `0xf4bf`）。未解出 ACK、NAK、STALL 或端点 2 的 IN token。原始值说明 **IN 已发出、设备已返回有效 DATA1，而软件提交缓冲期待 DATA0**；不能再把当前问题归为“没有启动接收”。
- 延迟轮询启用至缓冲准备好、完成时关闭轮询的实验未单独恢复输入，键盘断电重枚举后也未恢复。因此“端点启用过早”尚未证实。
- 将首包与后续软件 PID 一致地设置为 DATA1 的临时判别实验，获得 3 次成功 HCD 完成，真实缓冲为 `00 00 4f 4f 4f 4f 4f 4f`（右箭头 `0x4f`）；随后再次卡住。固定首包 DATA1 不是正式修复，未保留任何端点特判。
- 两份原始采样和解码证据保存在 `rp2040/build/tmp/rp-hcd-usbphy-001.bin`、`rp-hcd-usbphy-002.bin`、`rp-hcd-evidence.json`。全部 SDK 实验与采样代码已撤除，固件恢复原版；PID/ACK 不同步的完整成因仍未确认。
- 普通鼠标/简单 Boot 键盘的实板对照未执行：本机 USB 列表只有 SWD 探针及 VMware 虚拟设备，没有可由软件接入板载 USB-A 的对照外设。

### 根因与修复：EPX-only HCD

根因是 RP2040 芯片缺陷（[pico-feedback #394](https://github.com/raspberrypi/pico-feedback/issues/394)、[TinyUSB #2776](https://github.com/hathach/tinyusb/issues/2776)）：硬件自动轮询的 EP1–15 收到 PID 不符的 DATA 包时，既不 ACK 也不报 DATA_SEQ_ERROR，导致设备无限重发。上面线上抓到的 344 组 IN/DATA1 无 ACK、IRQ 错误计数为 0、强制 DATA1 后只收 3 包又卡死，都与此一致。截至 2026-10-11，上游 TinyUSB master 仍使用自动轮询端点。

- `rp2040/native/hcd_epx.c` 替换 SDK 的 `hcd_rp2040.c`（`CMakeLists.txt` 已从 `tinyusb_host_base` 移除原文件）。所有端点都走 EPX；每次只做一次握手，NAK 时按 `bInterval` 重新排队。遇到 DATA_SEQ_ERROR 时，EPX 按 USB 8.6.4 回 ACK 并丢弃该包，PID 不翻转。
- 回归：早期版本在 `hcd_port_reset` 中置位 `SIE_CTRL.RESET_BUS`。每次复位都会重新触发 CONN_DIS 插入事件，实测每约 500ms 断开、重连一次，`tuh_task` 一直卡在枚举延时里。现改为空实现，与上游一致：插入后的总线复位由控制器自动完成。
- 实板（正式 KVM 固件，iKBC 插板载 USB-A）：地址 1 挂载，Boot 协议成功，进入 READY（state=3）、fault=0。60 秒内接口 0 收到 29 份报告，解出 `0x1d`、`0x2a`、`0x0f` 的按下与释放，期间无故障。

- 端到端：用户确认在键盘上输入后，目的电脑同步收到（键盘 → RP → UART → nRF → Unifying 接收器 → 电脑）。

**当前可日常使用：** 三目标短按切换、长按配对、RGB 状态、目标连不上时无限重试都已在实板上使用。仍未验证 USB 电气波形、阻抗及带载浪涌；完整历程与待办见 [JOURNEY.md](JOURNEY.md)。

SWD 诊断变量地址随构建变化，应从当前 ELF 用 `arm-none-eabi-nm` 查询 `rp_debug_*`、`hid_dev`、`rp_ota_mailbox`，不要照抄旧地址。
