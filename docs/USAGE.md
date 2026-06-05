# 使用指南 — unifying-kvm 键盘 KVM 转发器

本文档详细说明如何安装、配对、使用键盘 KVM 转发器,实现一个物理键盘在多台电脑之间切换使用。

---

## 1. 系统概览

```
                    ┌─ 本机(Pi): 键盘正常工作
                    │
物理 USB 键盘 → Pi ─┼─ PC:      键盘通过优联发射到 PC
                    │
                    └─ MacBook:  键盘通过优联发射到 MacBook

切换方式: 右Ctrl + Scroll Lock (循环切换)
```

你的键盘始终物理连接在 Pi 上。通过热键,你可以让它的输入:
- 留在 Pi 本地(正常使用)
- 通过 nRF52840 无线发射到插有优联接收器的 PC
- 通过 nRF52840 无线发射到插有优联接收器的 MacBook

切换是**即时的**(亚秒级),无需拔插任何东西。

---

## 2. 硬件准备

| 设备 | 数量 | 说明 |
| --- | --- | --- |
| 树莓派(任意型号) | 1 | 运行转发器,需要 USB 口 |
| nRF52840 开发板 | 1 | 已刷 nrf52-unifying 固件,USB-CDC 连到 Pi |
| 物理 USB 键盘 | 1 | 你日常使用的键盘,插在 Pi 上 |
| Logitech Unifying 接收器 | 1-3 | 每台远程目标电脑一个(046d:c52b) |

接线示意:
```
[键盘] ──USB──→ [Pi USB口]
                [Pi USB口] ──USB──→ [nRF52840 开发板]
                                        ↓ (2.4GHz 无线)
[PC USB口] ←── [Unifying 接收器 A]
[Mac USB口] ←── [Unifying 接收器 B]
```

---

## 3. 软件安装

### 3.1 Pi 上安装依赖

```bash
# Python 依赖
sudo apt install python3-evdev python3-serial

# 如果 apt 没有 python3-serial:
pip install pyserial --break-system-packages

# 确认
python3 -c "import evdev, serial; print('OK')"
```

### 3.2 确认 nRF52 固件已就绪

```bash
ls /dev/ttyACM0   # nRF52 CDC 串口应存在

# 确认固件版本
python3 /home/lh/nrf52-unifying/tools/unifying.py ver
# 应输出: VER unifying/1 build=...
```

### 3.3 确认键盘已识别

```bash
# 列出所有输入设备,找到你的键盘的 vendor:product
sudo python3 -c "
import evdev
for path in sorted(evdev.list_devices()):
    dev = evdev.InputDevice(path)
    print(f'{path}: {dev.name} [{dev.info.vendor:04x}:{dev.info.product:04x}]')
"
```

输出示例:
```
/dev/input/event2: Logitech K270 [046d:4003]
/dev/input/event5: My USB Keyboard [04d9:0024]    ← 这是你的键盘
```

记下你键盘的 `vendor:product`(如 `04d9:0024`)。

---

## 4. 配对接收器

每个远程目标的接收器需要配对一次(存入 nRF52 的 flash,之后断电不丢)。

### 4.1 配对 slot 0 → PC

**第一步:在 PC 上让接收器进入配对模式**

| 操作系统 | 方法 |
| --- | --- |
| Linux | `sudo ltunify pair 60`(安装:`sudo apt install ltunify`) |
| Windows | 安装 [Logitech Unifying Software](https://support.logi.com/hc/en-us/articles/360025297913) → Advanced → Pair |
| macOS | 安装 Logitech Unifying Software(`brew install --cask logitech-unifying`) → Pair |

你应看到提示:"Please turn your wireless device off and on to start pairing" 或类似。

**第二步:在 Pi 上 60 秒内执行配对**

```bash
python3 /home/lh/nrf52-unifying/tools/unifying.py raw "UPAIR 00"
```

成功输出:
```
PAIRED 3D41950E0C CH=32
```

失败输出 `ERR PAIR` 表示未收到接收器回应——确认接收器确实在配对模式中,多试几次。

### 4.2 配对 slot 1 → MacBook

同样步骤,在 MacBook 上打开配对,然后:

```bash
python3 /home/lh/nrf52-unifying/tools/unifying.py raw "UPAIR 01"
```

### 4.3 验证配对

```bash
# 看 slot 0
python3 /home/lh/nrf52-unifying/tools/unifying.py raw "USWITCH 00"
python3 /home/lh/nrf52-unifying/tools/unifying.py status
# STATUS SLOT=0 PAIRED=1 CONN=1 ...

# 看 slot 1
python3 /home/lh/nrf52-unifying/tools/unifying.py raw "USWITCH 01"
python3 /home/lh/nrf52-unifying/tools/unifying.py status
# STATUS SLOT=1 PAIRED=1 CONN=1 ...
```

### 4.4 快速测试

```bash
# 切到 slot 0, 发几个字符到 PC
python3 /home/lh/nrf52-unifying/tools/unifying.py raw "USWITCH 00"
python3 /home/lh/nrf52-unifying/tools/unifying.py type "hello from pi"
# PC 上应出现 "hello from pi"
```

---

## 5. 启动转发器

```bash
sudo python3 /home/lh/unifying-kvm/kvm.py \
  --keyboard 04d9:0024 \
  --port /dev/ttyACM0 \
  --targets "local:_,pc:0,mac:1"
```

参数说明:

| 参数 | 值 | 含义 |
| --- | --- | --- |
| `--keyboard` | `04d9:0024` | 你键盘的 vendor:product(第3步查到的) |
| `--port` | `/dev/ttyACM0` | nRF52 CDC 串口 |
| `--targets` | `local:_,pc:0,mac:1` | 目标列表(见下文) |
| `--debug` | (可选) | 打印底层协议收发,排错用 |

### 目标列表格式

`名称:slot,名称:slot,...`

- slot 为数字 `0`-`3`:对应 nRF52 的配对 slot,键击转发到该接收器
- slot 为 `_`:本机模式,键盘正常工作不转发

示例:
- `"local:_,pc:0"` — 两路:本机 ↔ PC
- `"local:_,pc:0,mac:1"` — 三路:本机 ↔ PC ↔ MacBook
- `"pc:0,mac:1"` — 只在两个远程之间切(不回本机)

### 启动后输出

```
Keyboard: My USB Keyboard (/dev/input/event5)
[KVM] Active target: local
[KVM] Running. Right Ctrl + Scroll Lock to switch targets.
[KVM] Targets: local, pc, mac
```

---

## 6. 日常使用

### 6.1 切换目标

按住 **右 Ctrl**,再按 **Scroll Lock**,循环切换到下一个目标:

```
local → pc → mac → local → pc → ...
```

切换时终端会打印:
```
[KVM] Switched to: pc
[KVM] Switched to: mac
[KVM] Switched to: local
```

### 6.2 各模式下的行为

| 模式 | 键盘状态 | 按键去向 |
| --- | --- | --- |
| 本机(local) | 未抓取(ungrab) | Pi 本地正常接收 |
| 远程(pc/mac) | 独占抓取(grab) | 实时转发到对应接收器 |

远程模式下:
- 你打的每个键(包括功能键、Ctrl/Alt/Shift 组合)都会实时发到远程电脑
- 键盘完全被转发器独占,Pi 本地不会收到任何按键
- 唯一例外是切换热键(右Ctrl+ScrollLock)始终被转发器截获

### 6.3 退出

按 `Ctrl+C`(在运行转发器的终端里)。退出时会自动释放所有按住的键并 ungrab 键盘。

---

## 7. 开机自启(可选)

如果想让 Pi 开机就自动运行转发器:

```bash
sudo tee /etc/systemd/system/unifying-kvm.service << 'EOF'
[Unit]
Description=Unifying KVM Keyboard Forwarder
After=dev-ttyACM0.device

[Service]
ExecStart=/usr/bin/python3 /home/lh/unifying-kvm/kvm.py \
  --keyboard 04d9:0024 --port /dev/ttyACM0 \
  --targets "local:_,pc:0,mac:1"
Restart=on-failure
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now unifying-kvm
sudo systemctl status unifying-kvm
```

查看日志:
```bash
sudo journalctl -u unifying-kvm -f
```

---

## 8. 故障排查

### 键盘找不到

```
error: keyboard 04d9:0024 not found
```

- 确认键盘已插入 Pi
- 确认 vendor:product 正确(重新执行第3步列表命令)
- 有些键盘有多个 event 节点,用 `--keyboard /dev/input/eventX` 直接指定

### 权限不够

```
PermissionError: [Errno 13] Permission denied: '/dev/input/event5'
```

需要 root:
```bash
sudo python3 kvm.py ...
```
或把用户加入 input 组:`sudo usermod -aG input $USER`(重新登录生效)。

### 切到远程后按键没反应

1. 确认对应 slot 已配对:
   ```bash
   python3 /home/lh/nrf52-unifying/tools/unifying.py raw "USWITCH 00"
   python3 /home/lh/nrf52-unifying/tools/unifying.py status
   ```
   应显示 `PAIRED=1`。

2. 确认接收器插在目标电脑上且电脑已开机。

3. 加 `--debug` 看 UKEY 命令是否返回 OK:
   ```bash
   sudo python3 kvm.py --keyboard 04d9:0024 --debug
   ```
   应看到每次按键都有 `> UKEY xx xx` / `< OK`。

4. 如果看到 `< ERR SEND`,说明射频链路断了。检查:
   - 接收器是否在范围内
   - 是否需要重新 `UCONNECT`(正常情况下 USWITCH 自动重连)

### 切换热键不生效

- 确认是**右 Ctrl**(不是左 Ctrl)
- 先按住右 Ctrl 不放,再按 Scroll Lock
- 如果键盘没有 Scroll Lock 键,需要改代码里的热键定义

### 卡键(某个键一直按住)

切换目标时转发器会自动发送 all-keys-up 报文。如果仍卡键:
```bash
# 手动释放
python3 /home/lh/nrf52-unifying/tools/unifying.py raw "UKEY 00"
```

---

## 9. 命令速查

| 操作 | 命令 |
| --- | --- |
| 列出键盘 | `sudo python3 kvm.py --keyboard list` |
| 启动(双目标) | `sudo python3 kvm.py --keyboard VID:PID --targets "local:_,pc:0"` |
| 启动(三目标) | `sudo python3 kvm.py --keyboard VID:PID --targets "local:_,pc:0,mac:1"` |
| 切换目标 | 右Ctrl + Scroll Lock |
| 退出 | Ctrl+C |
| 配对 slot 0 | `python3 unifying.py raw "UPAIR 00"` |
| 配对 slot 1 | `python3 unifying.py raw "UPAIR 01"` |
| 查看状态 | `python3 unifying.py status` |
| 切换 slot | `python3 unifying.py raw "USWITCH 01"` |
| 测试输入 | `python3 unifying.py type "test"` |
