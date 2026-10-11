//! Diagnostic-only UART traffic. No KVM commands, keyboard contents, or radio ACKs.
const TX_PREFIX: &[u8] = b"#RP2040 PING ";
const PEER_PREFIX: &[u8] = b"#NRF52840 PING ";
const TX_LEN: usize = TX_PREFIX.len() + 8 + 1;
const RX_CAPACITY: usize = PEER_PREFIX.len() + 8 + 1; // optional CR, LF is not stored
const PERIOD_US: u64 = 1_000_000;

/// SWD ABI: seven consecutive u32 words, offsets 0, 4, 8, 12, 16, 20, 24.
/// Counters wrap. tx_frames counts whole frames accepted by the UART FIFO,
/// not an electrical capture. uart_errors counts nonzero bridge error-mask observations.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub tx_frames: u32,
    pub rx_bytes: u32,
    pub peer_frames: u32,
    pub bad_frames: u32,
    pub uart_errors: u32,
    pub last_peer_seq: u32,
    pub task_count: u32,
}
impl Stats {
    pub const ZERO: Self = Self {
        tx_frames: 0, rx_bytes: 0, peer_frames: 0, bad_frames: 0,
        uart_errors: 0, last_peer_seq: 0, task_count: 0,
    };
}
const _: [(); 28] = [(); core::mem::size_of::<Stats>()];

pub(crate) struct Smoke {
    pub stats: Stats,
    rx: [u8; RX_CAPACITY],
    rx_len: usize,
    discard: bool,
    tx: [u8; TX_LEN],
    tx_offset: usize,
    tx_active: bool,
    next_seq: u32,
    next_due: u64,
}
impl Smoke {
    pub const fn new() -> Self {
        Self {
            stats: Stats::ZERO, rx: [0; RX_CAPACITY], rx_len: 0, discard: false,
            tx: [0; TX_LEN], tx_offset: 0, tx_active: false, next_seq: 1, next_due: 0,
        }
    }

    pub fn task(&mut self, now_us: u64) {
        self.stats.task_count = self.stats.task_count.wrapping_add(1);
        if self.tx_active || now_us < self.next_due { return; }
        self.tx[..TX_PREFIX.len()].copy_from_slice(TX_PREFIX);
        for index in 0..8 {
            let nibble = ((self.next_seq >> (28 - index * 4)) & 15) as usize;
            self.tx[TX_PREFIX.len() + index] = b"0123456789ABCDEF"[nibble];
        }
        self.tx[TX_LEN - 1] = b'\n';
        self.tx_offset = 0;
        self.tx_active = true;
        self.next_seq = self.next_seq.wrapping_add(1);
        // Anchor to now: a stalled loop never generates missed-period bursts.
        self.next_due = now_us.saturating_add(PERIOD_US);
    }

    pub fn tx(&self) -> &[u8] {
        if self.tx_active { &self.tx[self.tx_offset..] } else { &[] }
    }
    pub fn written(&mut self, count: usize) {
        if !self.tx_active { return; }
        self.tx_offset += count.min(TX_LEN - self.tx_offset);
        if self.tx_offset == TX_LEN {
            self.stats.tx_frames = self.stats.tx_frames.wrapping_add(1);
            self.tx_active = false;
        }
    }

    pub fn uart_error(&mut self) {
        self.stats.uart_errors = self.stats.uart_errors.wrapping_add(1);
        self.damage_line();
    }
    fn damage_line(&mut self) {
        if !self.discard { self.stats.bad_frames = self.stats.bad_frames.wrapping_add(1); }
        self.rx_len = 0;
        self.discard = true;
    }
    pub fn receive(&mut self, bytes: &[u8]) {
        self.stats.rx_bytes = self.stats.rx_bytes.wrapping_add(bytes.len() as u32);
        for &byte in bytes {
            if byte == b'\n' {
                if !self.discard {
                    let mut line = &self.rx[..self.rx_len];
                    if line.last() == Some(&b'\r') { line = &line[..line.len() - 1]; }
                    if let Some(seq) = peer_sequence(line) {
                        self.stats.peer_frames = self.stats.peer_frames.wrapping_add(1);
                        self.stats.last_peer_seq = seq;
                    } else {
                        self.stats.bad_frames = self.stats.bad_frames.wrapping_add(1);
                    }
                }
                self.rx_len = 0;
                self.discard = false;
            } else if !self.discard {
                if self.rx_len == self.rx.len() { self.damage_line(); }
                else { self.rx[self.rx_len] = byte; self.rx_len += 1; }
            }
        }
    }
}

fn peer_sequence(line: &[u8]) -> Option<u32> {
    if line.len() != PEER_PREFIX.len() + 8 || !line.starts_with(PEER_PREFIX) { return None; }
    let mut seq = 0u32;
    for &byte in &line[PEER_PREFIX.len()..] {
        let nibble = match byte {
            b'0'..=b'9' => byte - b'0',
            b'A'..=b'F' => byte - b'A' + 10,
            _ => return None,
        };
        seq = (seq << 4) | nibble as u32;
    }
    Some(seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_coalesced_lf_and_crlf_frames() {
        let mut smoke = Smoke::new();
        smoke.receive(b"#NRF52840 PING 0000");
        assert_eq!(smoke.stats.peer_frames, 0);
        smoke.receive(b"0001\r");
        assert_eq!(smoke.stats.peer_frames, 0);
        smoke.receive(b"\n#NRF52840 PING ABCDEF02\n");
        assert_eq!(smoke.stats.peer_frames, 2);
        assert_eq!(smoke.stats.last_peer_seq, 0xABCDEF02);
        assert_eq!(smoke.stats.bad_frames, 0);
        assert_eq!(smoke.stats.rx_bytes, 49);
    }

    #[test]
    fn requires_exact_peer_prefix_and_eight_uppercase_hex_digits() {
        let mut smoke = Smoke::new();
        smoke.receive(b"#RP2040 PING 00000001\n#NRF52840 PING abcdef01\n#NRF52840 PING 0000000\n#NRF52840 PING 000000001\n#NRF52840 PING 0000000G\n#NRF52840 PING 00000001\r\r\n");
        assert_eq!(smoke.stats.peer_frames, 0);
        assert_eq!(smoke.stats.bad_frames, 6);
        smoke.receive(b"#NRF52840 PING FFFFFFFF\n");
        assert_eq!(smoke.stats.peer_frames, 1);
        assert_eq!(smoke.stats.last_peer_seq, u32::MAX);
    }

    #[test]
    fn oversized_line_discards_valid_looking_suffix_until_lf() {
        let mut smoke = Smoke::new();
        smoke.receive(&[b'X'; RX_CAPACITY + 1]);
        smoke.receive(b"#NRF52840 PING 00000001\n#NRF52840 PING 00000002\n");
        assert_eq!(smoke.stats.bad_frames, 1);
        assert_eq!(smoke.stats.peer_frames, 1);
        assert_eq!(smoke.stats.last_peer_seq, 2);
    }

    #[test]
    fn hardware_error_discards_partial_line_and_recovers_at_lf() {
        let mut smoke = Smoke::new();
        smoke.receive(b"#NRF52840 PING ");
        smoke.uart_error();
        smoke.uart_error();
        smoke.receive(b"00000001#NRF52840 PING 00000002\n#NRF52840 PING 00000003\n");
        assert_eq!(smoke.stats.uart_errors, 2);
        assert_eq!(smoke.stats.bad_frames, 1);
        assert_eq!(smoke.stats.peer_frames, 1);
        assert_eq!(smoke.stats.last_peer_seq, 3);
    }

    #[test]
    fn partial_tx_counts_only_complete_fifo_acceptance_and_repeats_without_peer() {
        let mut smoke = Smoke::new();
        smoke.task(50);
        assert_eq!(smoke.tx(), b"#RP2040 PING 00000001\n");
        smoke.written(5);
        assert_eq!(smoke.tx(), b"40 PING 00000001\n");
        assert_eq!(smoke.stats.tx_frames, 0);
        smoke.written(TX_LEN);
        assert_eq!(smoke.stats.tx_frames, 1);
        smoke.task(1_000_049);
        assert_eq!(smoke.tx(), b"");
        smoke.task(1_000_050);
        assert_eq!(smoke.tx(), b"#RP2040 PING 00000002\n");
    }

    #[test]
    fn delayed_loop_does_not_catch_up_and_sequence_wraps() {
        let mut smoke = Smoke::new();
        smoke.next_seq = u32::MAX;
        smoke.task(8_000_000);
        assert_eq!(smoke.tx(), b"#RP2040 PING FFFFFFFF\n");
        smoke.written(TX_LEN);
        smoke.task(8_000_001);
        assert_eq!(smoke.tx(), b"");
        smoke.task(20_000_000);
        assert_eq!(smoke.tx(), b"#RP2040 PING 00000000\n");
        smoke.written(TX_LEN);
        smoke.task(20_000_001);
        assert_eq!(smoke.tx(), b"");
    }
}
