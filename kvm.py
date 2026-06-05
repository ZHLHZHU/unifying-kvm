#!/usr/bin/env python3
"""unifying-kvm — keyboard input forwarder via nRF52840 Unifying transmitter.

Grabs a specified USB keyboard (by vendor:product), forwards keystrokes to a
Logitech Unifying receiver via the nRF52 CDC dongle, and supports hotkey-based
switching between multiple targets (local machine, PC, MacBook).

Hotkey: Right Ctrl + Scroll Lock = cycle to next target.

Targets:
  - "local": keyboard released (ungrabbed), keys go to this Pi normally.
  - "pc" / "mac" / etc: keyboard grabbed, keys forwarded via USWITCH + UKEY.

Usage:
  sudo python3 kvm.py --keyboard 04d9:0024 --port /dev/ttyACM0

Requires: python-evdev, pyserial
  pip install evdev pyserial
"""

import argparse
import os
import sys
import time
from pathlib import Path

try:
    import evdev
    from evdev import ecodes
except ImportError:
    print("error: python-evdev not installed (pip install evdev)", file=sys.stderr)
    sys.exit(2)

try:
    import serial
except ImportError:
    print("error: pyserial not installed (pip install pyserial)", file=sys.stderr)
    sys.exit(2)

# ─── Linux evdev keycode → USB HID usage ID mapping ────────────────────────
# evdev codes are NOT the same as USB HID codes. This table covers a full
# keyboard layout. Unmapped keys are silently dropped.

EVDEV_TO_HID = {
    ecodes.KEY_A: 0x04, ecodes.KEY_B: 0x05, ecodes.KEY_C: 0x06, ecodes.KEY_D: 0x07,
    ecodes.KEY_E: 0x08, ecodes.KEY_F: 0x09, ecodes.KEY_G: 0x0A, ecodes.KEY_H: 0x0B,
    ecodes.KEY_I: 0x0C, ecodes.KEY_J: 0x0D, ecodes.KEY_K: 0x0E, ecodes.KEY_L: 0x0F,
    ecodes.KEY_M: 0x10, ecodes.KEY_N: 0x11, ecodes.KEY_O: 0x12, ecodes.KEY_P: 0x13,
    ecodes.KEY_Q: 0x14, ecodes.KEY_R: 0x15, ecodes.KEY_S: 0x16, ecodes.KEY_T: 0x17,
    ecodes.KEY_U: 0x18, ecodes.KEY_V: 0x19, ecodes.KEY_W: 0x1A, ecodes.KEY_X: 0x1B,
    ecodes.KEY_Y: 0x1C, ecodes.KEY_Z: 0x1D,
    ecodes.KEY_1: 0x1E, ecodes.KEY_2: 0x1F, ecodes.KEY_3: 0x20, ecodes.KEY_4: 0x21,
    ecodes.KEY_5: 0x22, ecodes.KEY_6: 0x23, ecodes.KEY_7: 0x24, ecodes.KEY_8: 0x25,
    ecodes.KEY_9: 0x26, ecodes.KEY_0: 0x27,
    ecodes.KEY_ENTER: 0x28, ecodes.KEY_ESC: 0x29, ecodes.KEY_BACKSPACE: 0x2A,
    ecodes.KEY_TAB: 0x2B, ecodes.KEY_SPACE: 0x2C,
    ecodes.KEY_MINUS: 0x2D, ecodes.KEY_EQUAL: 0x2E, ecodes.KEY_LEFTBRACE: 0x2F,
    ecodes.KEY_RIGHTBRACE: 0x30, ecodes.KEY_BACKSLASH: 0x31,
    ecodes.KEY_SEMICOLON: 0x33, ecodes.KEY_APOSTROPHE: 0x34, ecodes.KEY_GRAVE: 0x35,
    ecodes.KEY_COMMA: 0x36, ecodes.KEY_DOT: 0x37, ecodes.KEY_SLASH: 0x38,
    ecodes.KEY_CAPSLOCK: 0x39,
    ecodes.KEY_F1: 0x3A, ecodes.KEY_F2: 0x3B, ecodes.KEY_F3: 0x3C, ecodes.KEY_F4: 0x3D,
    ecodes.KEY_F5: 0x3E, ecodes.KEY_F6: 0x3F, ecodes.KEY_F7: 0x40, ecodes.KEY_F8: 0x41,
    ecodes.KEY_F9: 0x42, ecodes.KEY_F10: 0x43, ecodes.KEY_F11: 0x44, ecodes.KEY_F12: 0x45,
    ecodes.KEY_SYSRQ: 0x46, ecodes.KEY_SCROLLLOCK: 0x47, ecodes.KEY_PAUSE: 0x48,
    ecodes.KEY_INSERT: 0x49, ecodes.KEY_HOME: 0x4A, ecodes.KEY_PAGEUP: 0x4B,
    ecodes.KEY_DELETE: 0x4C, ecodes.KEY_END: 0x4D, ecodes.KEY_PAGEDOWN: 0x4E,
    ecodes.KEY_RIGHT: 0x4F, ecodes.KEY_LEFT: 0x50, ecodes.KEY_DOWN: 0x51,
    ecodes.KEY_UP: 0x52,
    ecodes.KEY_NUMLOCK: 0x53,
    ecodes.KEY_KPSLASH: 0x54, ecodes.KEY_KPASTERISK: 0x55, ecodes.KEY_KPMINUS: 0x56,
    ecodes.KEY_KPPLUS: 0x57, ecodes.KEY_KPENTER: 0x58,
    ecodes.KEY_KP1: 0x59, ecodes.KEY_KP2: 0x5A, ecodes.KEY_KP3: 0x5B,
    ecodes.KEY_KP4: 0x5C, ecodes.KEY_KP5: 0x5D, ecodes.KEY_KP6: 0x5E,
    ecodes.KEY_KP7: 0x5F, ecodes.KEY_KP8: 0x60, ecodes.KEY_KP9: 0x61,
    ecodes.KEY_KP0: 0x62, ecodes.KEY_KPDOT: 0x63,
    ecodes.KEY_102ND: 0x64,  # non-US backslash
    ecodes.KEY_COMPOSE: 0x65,
}

# Modifier keys: evdev keycode → HID modifier bitmask bit
MODIFIER_KEYS = {
    ecodes.KEY_LEFTCTRL: 0x01,
    ecodes.KEY_LEFTSHIFT: 0x02,
    ecodes.KEY_LEFTALT: 0x04,
    ecodes.KEY_LEFTMETA: 0x08,
    ecodes.KEY_RIGHTCTRL: 0x10,
    ecodes.KEY_RIGHTSHIFT: 0x20,
    ecodes.KEY_RIGHTALT: 0x40,
    ecodes.KEY_RIGHTMETA: 0x80,
}

# Maximum simultaneous non-modifier keys in a single HID report.
MAX_KEYS = 6


class Target:
    """Represents one output target (local or a remote receiver slot)."""

    def __init__(self, name, slot=None):
        self.name = name
        self.slot = slot  # None = local (no forwarding)

    @property
    def is_local(self):
        return self.slot is None

    def __repr__(self):
        return f"Target({self.name}, slot={self.slot})"


class KeyState:
    """Tracks the current modifier bitmask and pressed non-modifier keys."""

    def __init__(self):
        self.modifiers = 0
        self.keys = set()  # set of HID keycodes currently held

    def process_event(self, code, value):
        """Process an evdev key event. Returns True if state changed."""
        if code in MODIFIER_KEYS:
            bit = MODIFIER_KEYS[code]
            if value >= 1:  # press or repeat
                if self.modifiers & bit:
                    return False
                self.modifiers |= bit
            else:  # release
                if not (self.modifiers & bit):
                    return False
                self.modifiers &= ~bit
            return True

        hid = EVDEV_TO_HID.get(code)
        if hid is None:
            return False

        if value == 1:  # press
            if hid in self.keys:
                return False
            if len(self.keys) >= MAX_KEYS:
                return False  # rollover
            self.keys.add(hid)
            return True
        elif value == 0:  # release
            if hid not in self.keys:
                return False
            self.keys.discard(hid)
            return True
        # value == 2 is repeat — we don't re-send (receiver handles repeat)
        return False

    def to_ukey(self):
        """Format current state as a UKEY command line."""
        keys_sorted = sorted(self.keys)[:MAX_KEYS]
        tokens = " ".join(f"{k:02X}" for k in keys_sorted)
        line = f"UKEY {self.modifiers:02X}"
        if tokens:
            line += f" {tokens}"
        return line

    def clear(self):
        """Reset all keys (used when switching targets to avoid stuck keys)."""
        self.modifiers = 0
        self.keys.clear()


class Forwarder:
    """Main KVM forwarder logic."""

    def __init__(self, keyboard_path, serial_port, targets, debug=False):
        self.keyboard_path = keyboard_path
        self.serial_port = serial_port
        self.targets = targets
        self.target_idx = 0
        self.debug = debug
        self.key_state = KeyState()
        self.ser = None
        self.dev = None
        self.grabbed = False

    @property
    def current_target(self):
        return self.targets[self.target_idx]

    def open_serial(self):
        if self.ser and self.ser.is_open:
            return
        self.ser = serial.Serial(self.serial_port, 115200, timeout=1, write_timeout=2)
        self.ser.dtr = True
        self.ser.rts = True
        time.sleep(0.3)
        self.ser.reset_input_buffer()
        # Absorb banner
        self.ser.write(b"\n")
        time.sleep(0.1)
        self.ser.reset_input_buffer()

    def close_serial(self):
        if self.ser and self.ser.is_open:
            self.ser.close()
        self.ser = None

    def send_cmd(self, cmd):
        """Send a command and read the response line."""
        if not self.ser or not self.ser.is_open:
            self.open_serial()
        if self.debug:
            print(f"  > {cmd}", file=sys.stderr)
        self.ser.reset_input_buffer()
        self.ser.write(cmd.encode() + b"\n")
        self.ser.timeout = 2
        resp = self.ser.readline().decode("ascii", errors="replace").strip()
        if self.debug:
            print(f"  < {resp}", file=sys.stderr)
        return resp

    def open_keyboard(self):
        self.dev = evdev.InputDevice(self.keyboard_path)
        print(f"Keyboard: {self.dev.name} ({self.keyboard_path})")

    def grab(self):
        if not self.grabbed:
            self.dev.grab()
            self.grabbed = True

    def ungrab(self):
        if self.grabbed:
            self.dev.ungrab()
            self.grabbed = False

    def switch_target(self):
        """Cycle to next target."""
        # Release all keys on current target before switching.
        if not self.current_target.is_local and self.key_state.keys or self.key_state.modifiers:
            self.key_state.clear()
            self.send_cmd("UKEY 00")  # all-up report

        self.target_idx = (self.target_idx + 1) % len(self.targets)
        target = self.current_target
        print(f"[KVM] Switched to: {target.name}")

        if target.is_local:
            self.ungrab()
        else:
            self.grab()
            # Switch the nRF52 to the correct profile slot and connect.
            resp = self.send_cmd(f"USWITCH {target.slot:02X}")
            if self.debug:
                print(f"  USWITCH -> {resp}", file=sys.stderr)

        self.key_state.clear()

    def is_hotkey(self, code, value):
        """Check if this event is the switch hotkey (RCtrl held + ScrollLock press)."""
        return (
            code == ecodes.KEY_SCROLLLOCK
            and value == 1
            and (self.key_state.modifiers & MODIFIER_KEYS[ecodes.KEY_RIGHTCTRL])
        )

    def forward_key_state(self):
        """Send the current key state to the nRF52 as a UKEY command."""
        cmd = self.key_state.to_ukey()
        self.send_cmd(cmd)

    def run(self):
        self.open_keyboard()
        self.open_serial()

        # Start on the first target.
        target = self.current_target
        print(f"[KVM] Active target: {target.name}")
        if target.is_local:
            self.ungrab()
        else:
            self.grab()
            self.send_cmd(f"USWITCH {target.slot:02X}")

        print("[KVM] Running. Right Ctrl + Scroll Lock to switch targets.")
        print(f"[KVM] Targets: {', '.join(t.name for t in self.targets)}")

        try:
            for event in self.dev.read_loop():
                if event.type != ecodes.EV_KEY:
                    continue

                # Check hotkey before processing
                if self.is_hotkey(event.code, event.value):
                    self.switch_target()
                    continue

                # Process the key event into our state tracker.
                changed = self.key_state.process_event(event.code, event.value)
                if not changed:
                    continue

                # If local mode, we don't forward (keyboard is ungrabbed, OS sees it).
                if self.current_target.is_local:
                    continue

                # Forward the full current state as one UKEY report.
                self.forward_key_state()

        except KeyboardInterrupt:
            print("\n[KVM] Shutting down.")
        finally:
            # Release all keys on remote target.
            if not self.current_target.is_local:
                self.key_state.clear()
                try:
                    self.send_cmd("UKEY 00")
                except Exception:
                    pass
            self.ungrab()
            self.close_serial()


def find_keyboard(vid_pid):
    """Find the evdev device path matching a given vendor:product hex string.

    A composite USB device (like a keyboard with media keys) registers multiple
    event nodes. We identify the real keyboard node by checking it has the full
    set of letter keys (KEY_A..KEY_Z) — only the actual keyboard interface has
    these; System Control, Consumer Control, and Mouse nodes do not.
    """
    vid, pid = (int(x, 16) for x in vid_pid.split(":"))
    alphabet_keys = set(range(ecodes.KEY_A, ecodes.KEY_Z + 1))  # 30..51

    for path in sorted(evdev.list_devices()):
        dev = evdev.InputDevice(path)
        if dev.info.vendor == vid and dev.info.product == pid:
            caps = dev.capabilities(verbose=False)
            if ecodes.EV_KEY in caps:
                key_caps = set(caps[ecodes.EV_KEY])
                if alphabet_keys.issubset(key_caps):
                    return path
    return None


def build_parser():
    p = argparse.ArgumentParser(
        description="Keyboard KVM forwarder via nRF52840 Unifying transmitter"
    )
    p.add_argument(
        "--keyboard", required=True,
        help="Keyboard vendor:product ID (hex, e.g. 04d9:0024) or evdev path (/dev/input/eventX)"
    )
    p.add_argument("--port", default="/dev/ttyACM0", help="nRF52 CDC serial port")
    p.add_argument(
        "--targets", default="local:_,pc:0,mac:1",
        help="Comma-separated target list as name:slot (slot=_ for local). "
             "Default: local:_,pc:0,mac:1"
    )
    p.add_argument("--debug", action="store_true", help="Print protocol I/O")
    return p


def parse_targets(spec):
    """Parse 'local:_,pc:0,mac:1' into a list of Target objects."""
    targets = []
    for part in spec.split(","):
        name, slot_str = part.strip().split(":")
        if slot_str.strip() == "_":
            targets.append(Target(name.strip(), slot=None))
        else:
            targets.append(Target(name.strip(), slot=int(slot_str.strip())))
    return targets


def main():
    args = build_parser().parse_args()

    # Resolve keyboard device path.
    if args.keyboard.startswith("/dev/"):
        kb_path = args.keyboard
    else:
        kb_path = find_keyboard(args.keyboard)
        if not kb_path:
            print(f"error: keyboard {args.keyboard} not found", file=sys.stderr)
            print("Available devices:", file=sys.stderr)
            for path in sorted(evdev.list_devices()):
                dev = evdev.InputDevice(path)
                print(f"  {path}: {dev.name} [{dev.info.vendor:04x}:{dev.info.product:04x}]",
                      file=sys.stderr)
            return 1

    targets = parse_targets(args.targets)
    if not targets:
        print("error: no targets specified", file=sys.stderr)
        return 1

    fwd = Forwarder(kb_path, args.port, targets, debug=args.debug)
    try:
        fwd.run()
    except Exception as e:
        print(f"error: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
