# 技术文档 — unifying-kvm 键盘 KVM 转发器

本文档详细说明 unifying-kvm 的架构、实现细节和设计决策。

---

## 1. 总体架构

```mermaid
graph TD
    subgraph Pi["树莓派 (Python 3)"]
        Keyboard["物理 USB 键盘<br/>(evdev, 始终 grab)"]
        subgraph KVM["kvm.py"]
            KeyState["KeyState<br/>(状态机)"]
            Forwarder["Forwarder<br/>(主循环)"]
            Targets["Target[]<br/>(目标列表)"]
            UInput["uinput 虚拟键盘<br/>(本机注入)"]
            KeyState -->|UKEYDOWN| Forwarder
            Forwarder -->|本机模式| UInput
        end
        Keyboard -->|"grab (永远)"| KVM
    end

    subgraph NRF["nRF52840 固件"]
        CDC["CDC 命令解析 → UKEYDOWN → send_encrypted_keystroke → ESB 射频发射"]
        SWITCH["USWITCH → 切换 profile slot + connect"]
    end

    subgraph Receiver["Logitech Unifying 接收器"]
        RX["解密 → HID 注入 → 目标电脑"]
    end

    KVM -->|"serial (CDC ACM) / USB<br/>fire-and-forget"| NRF
    NRF -->|"2.4GHz ESB (Unifying)"| Receiver
```

核心数据流:

```mermaid
graph LR
    A["evdev EV_KEY 事件"] --> B["KeyState.process_event()<br/>更新修饰符/按键集合"]
    B --> C["KeyState.to_ukeydown()<br/>格式化为 UKEYDOWN &lt;mod&gt; [keys...]"]
    C --> D["serial.write()<br/>发到 nRF52"]
    D --> E["固件 AES-CTR 加密<br/>+ ESB 射频发送"]
    E --> F["接收器解密<br/>+ HID 注入"]
```

---

## 2. 核心数据结构

### 2.1 KeyState — HID 报文状态机

USB HID 键盘报文是**状态式**的:每帧报告当前按住的所有键(modifier bitmask + 最多 6 个键码),而不是事件式的。因此需要追踪完整的"当前按下键集合"。

```python
class KeyState:
    modifiers: int     # 8-bit bitmask (LCtrl|LShift|LAlt|LGui|RCtrl|RShift|RAlt|RGui)
    keys: set[int]     # 当前按住的 HID 键码(最多 6 个)
```

| 方法 | 作用 |
| --- | --- |
| `process_event(code, value)` | 处理一个 evdev 事件,更新内部状态,返回是否有变化 |
| `to_ukeydown()` | 输出当前状态的 `UKEYDOWN <mod> [keys...]` 命令字符串 |
| `clear()` | 清空(切换目标时防卡键) |

设计要点:
- `value=1` 是按下,`value=0` 是释放,`value=2` 是按键重复(不转发,接收机自己处理重复)
- 修饰键走 bitmask(不占 6 个键码槽位),与 USB HID 规范一致
- 超过 6 键同时按下时丢弃新键(phantom key / rollover 处理)

### 2.2 Target — 输出目标

```python
class Target:
    name: str          # 显示名(如 "pc", "mac", "local")
    slot: int | None   # nRF52 profile slot (0-3), None 表示本机
```

- `slot=None`:本机模式,键盘仍 grab,按键通过 uinput 注入回 Pi
- `slot=0..3`:远程模式,键盘 grab,按键通过 `USWITCH <slot>` + `UKEYDOWN` 转发

### 2.3 Forwarder — 主控制器

```python
class Forwarder:
    keyboard_path: str     # evdev 设备路径
    serial_port: str       # nRF52 CDC 串口
    targets: list[Target]  # 循环切换的目标列表
    target_idx: int        # 当前活跃目标索引
    key_state: KeyState    # HID 状态机
    ser: serial.Serial     # 串口连接
    dev: evdev.InputDevice # 键盘设备
    grabbed: bool          # 当前是否独占抓取
```

---

## 3. evdev 输入处理

### 3.1 设备发现

通过 `vendor:product` 匹配 (`find_keyboard()`):

```python
for path in evdev.list_devices():
    dev = evdev.InputDevice(path)
    if dev.info.vendor == vid and dev.info.product == pid:
        if EV_KEY in dev.capabilities():
            return path
```

有些键盘(尤其复合设备)会注册多个 event 节点(键盘+消费者控制+系统控制)。我们选有 `EV_KEY` capability 的那个。

### 3.2 独占抓取 (EVIOCGRAB)

键盘**始终处于 grab 状态**,不论当前目标是本机还是远程。这一设计确保进程对物理键盘 LED 的可靠控制(LED 写入需要设备被 grab)。

- grab 后内核不再将该设备的事件传递给其他 handler(包括 getty/X11/Wayland)
- 本机模式下,通过 uinput 虚拟键盘将按键事件注入回 OS,等效于键盘正常工作
- 远程模式下,按键转发为 UKEYDOWN 命令发到 nRF52

### 3.3 事件循环

```python
for event in dev.read_loop():
    if event.type != EV_KEY:
        continue
    # 热键检测
    # 状态更新
    # 转发(如果远程模式)
```

`read_loop()` 是阻塞式的,一个事件触发一次 `UKEYDOWN` 发送。这保证了**每个按键状态变化都精确对应一帧 HID 报文**,不会丢键或乱序。

---

## 4. evdev 键码 → USB HID 键码映射

Linux evdev 键码和 USB HID usage ID 是**不同的编号体系**,需要转换表。

| evdev 常量 | evdev 值 | HID 值 | 说明 |
| --- | --- | --- | --- |
| KEY_A | 30 | 0x04 | |
| KEY_1 | 2 | 0x1E | |
| KEY_ENTER | 28 | 0x28 | |
| KEY_F1 | 59 | 0x3A | |
| KEY_LEFTCTRL | 29 | (modifier) | 不映射为键码,而是 bitmask |
| KEY_UP | 103 | 0x52 | |

映射表 `EVDEV_TO_HID` 覆盖:
- 26 个字母(KEY_A..KEY_Z → 0x04..0x1D)
- 10 个数字(KEY_1..KEY_0 → 0x1E..0x27)
- 标点/符号(约 15 个)
- 功能键 F1-F12
- 编辑键(Enter, Esc, Backspace, Tab, Space, Delete, Insert)
- 导航键(方向键, Home, End, PageUp/Down)
- 小键盘(KP0-9, KP 运算符, KPEnter)
- 杂项(CapsLock, PrintScreen, ScrollLock, Pause, NumLock)

修饰键单独处理(`MODIFIER_KEYS`):

| evdev | HID bitmask |
| --- | --- |
| KEY_LEFTCTRL | 0x01 |
| KEY_LEFTSHIFT | 0x02 |
| KEY_LEFTALT | 0x04 |
| KEY_LEFTMETA | 0x08 |
| KEY_RIGHTCTRL | 0x10 |
| KEY_RIGHTSHIFT | 0x20 |
| KEY_RIGHTALT | 0x40 |
| KEY_RIGHTMETA | 0x80 |

未在映射表中的键(如多媒体键)会被静默丢弃。

---

## 5. 串口通信协议

### 5.1 非阻塞 Fire-and-Forget 设计

热路径(UKEYDOWN)的串口通信采用 **fire-and-forget** 模式:写入后不等待固件响应。这是为了防止事件循环被固件操作(如 USWITCH 信道扫描,可能耗时数百毫秒)阻塞。

设计要点:
- 写超时设为 0.5s(`serial.Serial(write_timeout=0.5)`)
- 写入异常(超时、串口错误)被静默捕获,不中断事件循环
- 丢帧不会导致状态错误:下一帧发送完整状态,自动纠正
- 只有 USWITCH 等控制命令在切换时同步等待响应

```python
# 热路径: fire-and-forget
try:
    self.ser.write(f"{cmd}\n".encode())
except (serial.SerialTimeoutException, serial.SerialException):
    pass  # 丢帧由下一帧纠正
```

这意味着:
- 正常情况下延迟极低(串口写入 ~1ms)
- 固件繁忙时(扫频、重连)丢几帧按键,但不会卡住整个程序
- 进程永远不会因串口问题崩溃,systemd 在其他异常情况下自动重启

### 5.2 UKEYDOWN 命令格式

KVM 使用 `UKEYDOWN`(而非 `UKEY`)发送键击。区别在于 `UKEYDOWN` 只发按下报文,不自动释放。这对状态式转发至关重要:KVM 每次状态变化发送完整的当前按键集合,由状态变化本身驱动释放。如果使用 `UKEY`(自带 press+release),当多个键重叠按下时,中间状态会各自触发一个完整的 press+release 周期,导致快速打字时出现重复按键。

```
UKEYDOWN <modifier-hex> [keycode1-hex keycode2-hex ... up to 6]
```

示例:
- `UKEYDOWN 00` — 所有键释放(空报文)
- `UKEYDOWN 00 04` — 按下 'a'(无修饰符)
- `UKEYDOWN 02 04` — Shift + 'a'(= 'A')
- `UKEYDOWN 05 04 05` — Ctrl+Alt + 'a' + 'b'

每次 KeyState 变化都发送**完整当前状态**(不是增量),这样:
- 丢一帧不会导致卡键(下一帧会纠正)
- nRF52 固件端不需要维护状态,只需逐帧加密发射

### 5.2 AES Counter 持久化

nRF52 固件自动处理 AES counter 的持久化,KVM 端无需关心:
- **周期存储**:`UKEYDOWN` 每 128 帧自动写一次 flash
- **掉电保护**:nRF52840 POFCON 检测电压下降时紧急写入
- **启动安全余量**:每次上电从 flash 加载 counter 后自动 +512,覆盖未持久化的增量

因此即使 nRF52 意外断电,重启后 counter 也不会回退,键击不会被接收器拒绝。

### 5.2 USWITCH 命令

切换目标时发送:
```
USWITCH <slot-hex>
```

固件会:
1. 从 flash 加载该 slot 的配对信息(地址、AES 密钥、counter)
2. 配置射频(地址、信道)
3. 自动 `connect()`(wake-up 扫频重连)
4. 返回 `SWITCHED <slot> CONNECTED` 或 `SWITCHED <slot> NOCONN`

### 5.4 通信时序

```mermaid
sequenceDiagram
    participant App as kvm.py
    participant FW as nRF52 固件

    Note over App: 按键按下
    App->>App: process_event → to_ukeydown
    App->>FW: serial.write("UKEYDOWN 00 04\n")
    Note over App: 不等待响应,继续处理下一事件

    Note over App: 按键释放
    App->>App: process_event → to_ukeydown
    App->>FW: serial.write("UKEYDOWN 00\n")
    Note over App: 不等待响应
```

热路径是**异步非阻塞**的:写入后立即返回,不等待固件回复。这保证了即使固件正在执行耗时操作(如 USWITCH 扫频),事件循环也不会被阻塞。丢帧由 UKEYDOWN 的状态式设计自动纠正(下一帧包含完整状态)。

串口波特率 115200,一条 `UKEYDOWN 00 04\n` 约 15 字节 ≈ 1.3ms 传输时间。加上固件处理+射频发送+ACK,端到端延迟约 **3-8ms**——人感知不到。

---

## 6. 热键检测

### 6.1 热键定义

**Scroll Lock**(单键)

选择理由:
- Scroll Lock 在现代键盘上完全没有功能
- 单键切换比组合键更快捷可靠
- 与 IP-KVM 的传统(Scroll Lock 切换)一致
- 物理键盘上普遍存在

### 6.2 检测逻辑

```python
def is_hotkey(self, code, value):
    return (
        code == KEY_SCROLLLOCK
        and value == 1  # 按下(不是释放或重复)
    )
```

关键:热键检测在事件转发**之前**,所以 Scroll Lock 的按下事件如果触发了热键,不会被转发或注入(被截获)。

### 6.3 切换流程

```python
def switch_target(self):
    # 1. 释放当前目标上的所有键(防卡键)
    if not current_target.is_local:
        send("UKEYDOWN 00")  # all-keys-up

    # 2. 切到下一个目标
    target_idx = (target_idx + 1) % len(targets)

    # 3. 根据新目标类型:
    if new_target.is_local:
        # 切换到 uinput 注入模式
        pass
    else:
        send(f"USWITCH {slot:02X}")  # 切 nRF52 profile

    # 4. 清空状态(新目标从零开始)
    key_state.clear()

    # 5. LED 反馈(闪烁 Scroll Lock LED)
    blink_led(target_blink_count)
```

---

## 7. 键盘始终 Grab + uinput 架构

### 7.1 设计原理

键盘**始终被 grab**(独占抓取),不论当前目标是本机还是远程。本机模式下通过 uinput 虚拟键盘将按键事件注入回操作系统。

为什么不在本机模式下 ungrab?
- **LED 可靠性**:只有 grab 状态下才能可靠控制物理键盘的 LED(Scroll Lock LED 用于指示目标)
- **状态一致性**:避免 grab/ungrab 切换时的竞争条件和事件丢失
- **简化逻辑**:事件循环始终以相同方式运行,只是输出目标不同

### 7.2 状态转换图

```mermaid
stateDiagram-v2
    [*] --> LOCAL_Grabbed_UInput
    LOCAL_Grabbed_UInput --> REMOTE_Grabbed_Serial : 热键
    REMOTE_Grabbed_Serial --> REMOTE2_Grabbed_Serial : 热键 (循环)
    REMOTE2_Grabbed_Serial --> LOCAL_Grabbed_UInput : 热键
    REMOTE_Grabbed_Serial --> LOCAL_Grabbed_UInput : 热键
```

### 7.3 关键保证

- **切换时不漏键**:切之前发 `UKEYDOWN 00`(all-up)确保远程目标不会有卡住的键
- **切换后状态干净**:清空 `key_state`,新目标从全部释放状态开始
- **异常退出安全**:`finally` 块保证退出时 ungrab + 释放远程键 + 销毁 uinput 设备
- **热键始终有效**:grab 状态下事件循环始终在读事件,热键检测永远可靠

---

## 8. LED 指示

### 8.1 Scroll Lock LED 闪烁协议

切换目标后,物理键盘的 Scroll Lock LED 会闪烁指示当前目标:

| 目标 | 闪烁次数 |
| --- | --- |
| PC (slot 0) | 1 次 |
| Local | 2 次 |
| Mac (slot 1) | 3 次 |

闪烁结束后 LED **始终熄灭**(OFF 状态)。

### 8.2 实现方式

通过 evdev 的 `device.set_led()` 控制物理键盘的 LED:

```python
def blink_led(self, count):
    for _ in range(count):
        self.dev.set_led(LED_SCROLLL, 1)  # ON
        time.sleep(0.15)
        self.dev.set_led(LED_SCROLLL, 0)  # OFF
        time.sleep(0.15)
```

由于键盘始终处于 grab 状态,LED 控制是可靠的——不会被内核的 LED 管理干扰。

---

## 9. 延迟分析

端到端延迟(按键到字符出现):

| 阶段 | 延迟 |
| --- | --- |
| evdev 事件传递 | < 1ms |
| Python 处理 + 格式化 | < 0.5ms |
| 串口传输(~12 字节 @ 115200) | ~1ms |
| nRF52 固件解析 + AES 加密 | < 0.5ms |
| ESB 射频发送 + ACK | 0.5-3ms(含重传) |
| 接收器解密 + USB HID 注入 | < 1ms |
| **总计** | **3-8ms** |

人类感知延迟阈值约 50-100ms,所以 3-8ms 的延迟**完全不可感知**。

---

## 10. 可靠性机制

### 9.1 固件侧保活

nRF52 固件在连接态下每 8ms 执行一次 `tick()`(通过 `select(read_packet, Timer)`)。`tick()` 在保活窗口到期时发送 keep-alive 包,维持接收机的跳频同步。这保证了:
- 即使长时间没有按键,链路也不会断
- 下一次按键永远能第一时间送达

### 9.2 射频重传

`EsbRadio.transmit_payload()` 内置 15 次硬件级重传(250μs 间隔),单帧丢失概率极低。

### 9.3 UKEYDOWN 状态式设计

每帧发**完整状态**(不是增量),所以:
- 丢一帧 `UKEYDOWN`(如 `ERR SEND`)只影响当帧,下一帧自动纠正
- 不存在"按下了但释放丢了导致卡键"的问题

### 10.4 切换时 all-up

切换目标前强制发 `UKEYDOWN 00`,即使某帧被吞,接收机也不会在旧目标上卡键。

### 10.5 串口容错

热路径写入超时(0.5s)和串口异常被静默捕获:
- 进程不会因串口错误崩溃
- systemd 配置 `Restart=on-failure` + `RestartSec=3` 在其他异常时自动重启
- `PYTHONUNBUFFERED=1` 环境变量确保日志实时输出,方便排查

---

## 11. 安全考虑

- **本机 root 权限**:evdev grab 需要 root(或 input 组),转发器以 root 运行
- **AES 加密**:所有转发的键击都经过 AES-128-CTR 加密(在 nRF52 固件里),空中无法窃听
- **物理安全**:键盘物理连接在 Pi 上,只有物理接触才能改变转发目标
- **无网络暴露**:整条链路(evdev → serial → RF)不经过任何 IP 网络

---

## 12. 局限与已知问题

| 局限 | 说明 |
| --- | --- |
| 不支持鼠标 | 当前只转发键盘(firmware 已移除鼠标支持) |
| 不支持多媒体键 | 音量/播放等消费者控制键未映射 |
| 单键盘 | 只抓取一个 evdev 设备 |
| 最多 4 个远程目标 | nRF52 flash 布局限制(4 个 profile slot) |
| 热键固定 | Scroll Lock 硬编码,修改需改代码 |
| 接收器需物理在范围内 | 2.4GHz 射频,典型 5-10m 有效距离 |

---

## 13. 文件清单

```
unifying-kvm/
├── kvm.py              # 主程序:事件循环、状态管理、串口通信
├── README.md           # 项目说明
├── requirements.txt    # Python 依赖
└── docs/
    ├── USAGE.md        # 使用指南(本文档)
    └── TECHNICAL.md    # 技术文档(你正在看的)
```

---

## 14. 依赖关系

```mermaid
graph TD
    KVM["unifying-kvm (本项目)"]
    EVDEV["python3-evdev<br/>(键盘事件读取 + grab)"]
    SERIAL["pyserial<br/>(CDC 串口通信)"]
    FW["nrf52-unifying 固件<br/>(刷在 nRF52840 上)"]
    LIB["rust-unifying 库<br/>(Unifying 协议实现)"]

    KVM --> EVDEV
    KVM --> SERIAL
    KVM --> FW
    FW --> LIB
```

三个项目各自独立,通过 USB-CDC 串口文本协议松耦合。替换任一层(如用另一种射频方案)只需实现同样的串口命令接口。
