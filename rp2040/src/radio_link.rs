use crate::keyboard::KeyReport;
const TIMEOUT: u64 = 2_000_000;
/// nRF UPAIR runs up to 8 full channel scans before answering.
const PAIR_TIMEOUT: u64 = 30_000_000;
/// Each ERR SEND already spans ~1.4s of nRF channel hopping; resend promptly so it keeps scanning.
/// There is no give-up bound: an unreachable target blinks until it answers or the user switches away.
const SEND_RETRY_US: u64 = 20_000;
/// Slots the KVM cycles through; nRF stores up to four.
pub const SLOTS: u8 = 3;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command { Version, Status, Switch(u8), Keys(KeyReport), Pair(u8) }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// `Unreached`: a best-effort command got ERR SEND; the receiver did not see it and it is not retried.
pub enum Reply { Version, Status { slot: u8, linked: bool }, Switched { slot: u8, paired: bool }, Ok, Busy, Paired, PairFailed, Unreached }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault { Timeout, Uart, RxOverflow, QueueOverflow, Response, Keyboard }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event { Reply(Reply), Fault(Fault) }
#[derive(Clone, Copy)]
/// `retried`: the nRF answered ERR SEND at least once, so the receiver is currently unreachable.
/// `once`: report ERR SEND as `Unreached` instead of resending.
struct Pending { command: Command, id: u32, started: u64, retried: bool, once: bool, retry_at: Option<u64> }
pub struct RadioLink {
    next_id: u32, pending: Option<Pending>, tx: [u8;256], tx_len: usize, tx_offset: usize,
    rx: [u8;256], rx_len: usize, discard: bool, event: Option<Event>, resync_tx: bool,
}
impl Default for RadioLink { fn default() -> Self { Self::new() } }
impl RadioLink {
    pub const fn new() -> Self { Self { next_id: 1, pending: None, tx:[0;256], tx_len:0,tx_offset:0,rx:[0;256],rx_len:0,discard:false,event:None,resync_tx:false } }
    pub fn idle(&self) -> bool { self.pending.is_none() }
    /// The receiver has not acknowledged the in-flight command yet (at least one ERR SEND).
    pub fn reaching(&self) -> bool { self.pending.is_some_and(|p| p.retried || p.retry_at.is_some()) }
    pub fn cancel(&mut self) {
        // A canceled partial command must not concatenate with the next numbered line.
        self.resync_tx |= self.tx_offset != 0 && self.tx_offset < self.tx_len;
        self.pending=None; self.tx_len=0;self.tx_offset=0;self.event=None;
    }
    pub fn start(&mut self, command: Command, now: u64) -> bool { self.begin(command,now,false) }
    /// Single attempt: releases are courtesy, a receiver that cannot hear us drops held keys itself.
    pub fn start_once(&mut self, command: Command, now: u64) -> bool { self.begin(command,now,true) }
    fn begin(&mut self, command: Command, now: u64, once: bool) -> bool {
        if !self.idle() || self.event.is_some() { return false; }
        self.issue(command,now,false,once); true
    }
    fn issue(&mut self, command: Command, now: u64, retried: bool, once: bool) {
        let id=self.next_id; self.next_id=self.next_id.wrapping_add(1);
        self.pending=Some(Pending {command,id,started:now,retried,once,retry_at:None});
        self.tx_offset=0; self.tx_len=0;
        if self.resync_tx { self.byte(b'\n'); self.resync_tx=false; }
        self.put(b"@");
        for shift in (0..8).rev() { self.byte(hex(((id>>(shift*4))&15) as u8)); }
        self.byte(b' ');
        match command {
            Command::Version=>self.put(b"VER"), Command::Status=>self.put(b"USTATUS"),
            Command::Switch(slot)=>{self.put(b"USWITCH ");self.hex_byte(slot);},
            Command::Pair(slot)=>{self.put(b"UPAIR ");self.hex_byte(slot);},
            Command::Keys(report)=>{self.put(b"UKEYDOWN ");self.hex_byte(report.modifiers);for key in report.keys {if key!=0 {self.byte(b' ');self.hex_byte(key);}}},
        }
        self.byte(b'\n');
    }
    fn put(&mut self, bytes:&[u8]) {for &b in bytes {self.byte(b);}}
    fn byte(&mut self,b:u8) {self.tx[self.tx_len]=b;self.tx_len+=1;}
    fn hex_byte(&mut self,b:u8) {self.byte(hex(b>>4));self.byte(hex(b&15));}
    pub fn tx(&self)->&[u8] {&self.tx[self.tx_offset..self.tx_len]}
    pub fn written(&mut self, count:usize) {self.tx_offset=(self.tx_offset+count).min(self.tx_len);}
    pub fn poll(&mut self,now:u64) {
        if let Some(p)=self.pending {
            if let Some(deadline)=p.retry_at { if now>=deadline {self.issue(p.command,now,true,p.once);} }
            else if now.saturating_sub(p.started)>=(if matches!(p.command,Command::Pair(_)){PAIR_TIMEOUT}else{TIMEOUT}) {self.fail(Fault::Timeout);}
        }
    }
    pub fn uart_error(&mut self) {self.rx_len=0;self.discard=true;self.fail(Fault::Uart);}
    fn fail(&mut self,fault:Fault) {self.cancel();self.event=Some(Event::Fault(fault));}
    pub fn take_event(&mut self)->Option<Event> {self.event.take()}
    pub fn receive(&mut self,bytes:&[u8],now:u64) {
        for &b in bytes {
            if self.discard {if b==b'\n' {self.discard=false;}continue;}
            if b==b'\n' {
                let len=if self.rx_len>0&&self.rx[self.rx_len-1]==b'\r' {self.rx_len-1}else{self.rx_len};
                // Parse without retaining a borrow of the receive buffer across state changes.
                let result=parse_line(&self.rx[..len]);self.rx_len=0;
                if let Some((id,reply))=result {
                    let Some(p)=self.pending else {continue};
                    if id!=p.id || p.retry_at.is_some() || self.tx_offset!=self.tx_len {continue;}
                    match reply {
                        Parsed::Send if p.once=>{self.cancel();self.event=Some(Event::Reply(Reply::Unreached));}
                        Parsed::Send=>{self.pending=Some(Pending{retry_at:Some(now+SEND_RETRY_US),..p});}
                        Parsed::Reply(Reply::Busy)=>{self.cancel();self.event=Some(Event::Reply(Reply::Busy));}
                        Parsed::Reply(reply)=>{
                            let valid=matches!((p.command,reply),(Command::Version,Reply::Version)|(Command::Status,Reply::Status{..})|(Command::Keys(_),Reply::Ok)|(Command::Pair(_),Reply::Paired|Reply::PairFailed))
                                || matches!((p.command,reply),(Command::Switch(a),Reply::Switched{slot,..}) if a==slot);
                            if valid {self.cancel();self.event=Some(Event::Reply(reply));} else {self.fail(Fault::Response);}
                        }
                        Parsed::Error=>self.fail(Fault::Response),
                    }
                }
            } else if self.rx_len==self.rx.len() {
                self.rx_len=0;self.discard=true;self.fail(Fault::RxOverflow);
            } else {self.rx[self.rx_len]=b;self.rx_len+=1;}
        }
    }
}
fn hex(v:u8)->u8 {b"0123456789ABCDEF"[v as usize]}
fn unhex(b:u8)->Option<u32> {match b {b'0'..=b'9'=>Some((b-b'0') as u32),b'A'..=b'F'=>Some((b-b'A'+10) as u32),_=>None}}
#[derive(Clone,Copy)] enum Parsed { Reply(Reply),Send,Error }
fn parse_line(line:&[u8])->Option<(u32,Parsed)> {
    if line.len()<11||line[0]!=b'@'||line[9]!=b' ' {return None;}
    let mut id=0;for &b in &line[1..9] {id=(id<<4)|unhex(b)?;}
    let body=&line[10..];
    let reply=if body==b"OK" {Parsed::Reply(Reply::Ok)}
        else if body==b"ERR BUSY" {Parsed::Reply(Reply::Busy)}
        else if body==b"ERR SEND" {Parsed::Send}
        else if body.starts_with(b"VER unifying/1 build=") && body.len()>21 {Parsed::Reply(Reply::Version)}
        else if let Some(rest)=body.strip_prefix(b"STATUS SLOT=") {
            let mut tokens=rest.split(|&b| b==b' ');
            let slot=decimal(tokens.next()?)?;
            let paired=tokens.next()?;let conn=tokens.next()?;let ch=tokens.next()?;let cnt=tokens.next()?;
            if slot>=SLOTS as u32||!matches!(paired,b"PAIRED=0"|b"PAIRED=1")||!matches!(conn,b"CONN=0"|b"CONN=1")
                || decimal(ch.strip_prefix(b"CH=")?).is_none()||decimal(cnt.strip_prefix(b"CNT=")?).is_none()||tokens.next().is_some() {Parsed::Error}
            else {Parsed::Reply(Reply::Status{slot:slot as u8,linked:paired==b"PAIRED=1"})}
        }
        else if let Some(rest)=body.strip_prefix(b"SWITCHED ") {
            let mut tokens=rest.split(|&b| b==b' ');let slot=decimal(tokens.next()?)?;
            let paired=match tokens.next()? {b"CONNECTED"|b"NOCONN"=>Some(true),b"EMPTY"=>Some(false),_=>None};
            match paired {Some(paired) if slot<SLOTS as u32&&tokens.next().is_none()=>Parsed::Reply(Reply::Switched{slot:slot as u8,paired}),_=>Parsed::Error}
        }
        else if body==b"ERR PAIR" {Parsed::Reply(Reply::PairFailed)}
        else if let Some(rest)=body.strip_prefix(b"PAIRED ") {
            // PAIRED <10 hex address> CH=<n>
            let mut tokens=rest.split(|&b| b==b' ');
            let address=tokens.next()?;let ch=tokens.next()?;
            if address.len()==10&&address.iter().all(|&b| unhex(b).is_some())&&decimal(ch.strip_prefix(b"CH=")?).is_some()&&tokens.next().is_none() {Parsed::Reply(Reply::Paired)}else{Parsed::Error}
        }else{Parsed::Error};
    Some((id,reply))
}
fn decimal(bytes:&[u8])->Option<u32> {if bytes.is_empty(){return None;}let mut n=0u32;for &b in bytes{if !b.is_ascii_digit(){return None;}n=n.checked_mul(10)?.checked_add((b-b'0')as u32)?;}Some(n)}

#[cfg(test)] mod tests {
    use super::*;
    fn transmit(link:&mut RadioLink){let n=link.tx().len();link.written(n);}
    #[test] fn delayed_ack_never_confirms_next_request() {
        let mut l=RadioLink::new();l.start(Command::Keys(KeyReport::default()),0);transmit(&mut l);
        l.receive(b"@00000001 OK\r\n",1);assert_eq!(l.take_event(),Some(Event::Reply(Reply::Ok)));
        l.start(Command::Keys(KeyReport{modifiers:1,keys:[0;6]}),2);transmit(&mut l);
        l.receive(b"@00000001 OK\r\n",3);assert_eq!(l.take_event(),None);assert!(!l.idle());
        l.receive(b"@00000002 O",4);l.receive(b"K\r\n",5);assert_eq!(l.take_event(),Some(Event::Reply(Reply::Ok)));
    }
    #[test] fn send_retry_uses_new_number_and_deadline() {
        let mut l=RadioLink::new();l.start(Command::Keys(KeyReport::default()),0);transmit(&mut l);
        l.receive(b"@00000001 ERR SEND\r\n",100);l.poll(100+SEND_RETRY_US-1);assert!(l.tx().is_empty());
        l.poll(100+SEND_RETRY_US);assert!(l.tx().starts_with(b"@00000002 "));transmit(&mut l);
        l.receive(b"@00000001 OK\n",30_000);assert!(l.take_event().is_none());
        l.receive(b"@00000002 OK\n",30_100);assert_eq!(l.take_event(),Some(Event::Reply(Reply::Ok)));
    }
    #[test] fn overflow_and_uart_error_discard_to_newline() {
        let mut l=RadioLink::new();l.start(Command::Version,0);transmit(&mut l);
        l.receive(&[b'x';257],1);assert_eq!(l.take_event(),Some(Event::Fault(Fault::RxOverflow)));
        l.receive(b"@00000002 OK\n",2);l.start(Command::Version,3);transmit(&mut l);
        l.uart_error();assert_eq!(l.take_event(),Some(Event::Fault(Fault::Uart)));
        l.receive(b"junk\n",4);assert!(l.idle());
    }
    #[test] fn malformed_and_wrong_semantics_do_not_ack() {
        let mut l=RadioLink::new();l.start(Command::Version,0);transmit(&mut l);
        l.receive(b"@0000000a OK\n",1);assert_eq!(l.take_event(),None);
        l.receive(b"@00000001 OK\n",2);assert_eq!(l.take_event(),Some(Event::Fault(Fault::Response)));
    }
    #[test] fn cancellation_terminates_partial_frame_before_recovery() {
        let mut l=RadioLink::new();l.start(Command::Version,0);l.written(5);l.cancel();
        l.start(Command::Version,1);
        assert_eq!(l.tx(), b"\n@00000002 VER\n");
    }
    #[test] fn timeout_is_not_a_success_and_send_retries_never_give_up() {
        let mut l=RadioLink::new();l.start(Command::Status,0);transmit(&mut l);l.poll(2_000_000);
        assert_eq!(l.take_event(),Some(Event::Fault(Fault::Timeout)));
        let start=3_000_000;l.start(Command::Keys(KeyReport::default()),start);transmit(&mut l);assert!(!l.reaching());
        // A receiver unreachable for a minute keeps the same report in flight, then completes on OK.
        let mut now=start;let mut id=2u32;
        while now<start+60_000_000 {
            let mut line=*b"@00000000 ERR SEND\n";line[1..9].copy_from_slice(format!("{id:08X}").as_bytes());
            l.receive(&line,now);assert_eq!(l.take_event(),None);assert!(l.reaching());
            now+=SEND_RETRY_US;l.poll(now);assert!(l.tx().ends_with(b"UKEYDOWN 00\n"));transmit(&mut l);id+=1;
        }
        let mut line=*b"@00000000 OK\n";line[1..9].copy_from_slice(format!("{id:08X}").as_bytes());
        l.receive(&line,now);assert_eq!(l.take_event(),Some(Event::Reply(Reply::Ok)));assert!(!l.reaching());
    }
    #[test] fn best_effort_send_reports_unreached_without_resending() {
        let mut l=RadioLink::new();l.start_once(Command::Keys(KeyReport::default()),0);transmit(&mut l);
        l.receive(b"@00000001 ERR SEND\r\n",1);assert_eq!(l.take_event(),Some(Event::Reply(Reply::Unreached)));
        assert!(l.idle());l.poll(1+SEND_RETRY_US);assert!(l.tx().is_empty());
    }
    #[test] fn pair_waits_beyond_normal_timeout_and_parses_both_outcomes() {
        let mut l=RadioLink::new();l.start(Command::Pair(2),0);assert_eq!(l.tx(),b"@00000001 UPAIR 02\n");transmit(&mut l);
        l.poll(TIMEOUT);assert_eq!(l.take_event(),None);
        l.receive(b"@00000001 PAIRED A54A930029 CH=44\r\n",TIMEOUT+1);assert_eq!(l.take_event(),Some(Event::Reply(Reply::Paired)));
        l.start(Command::Pair(0),1);transmit(&mut l);l.receive(b"@00000002 ERR PAIR\r\n",2);
        assert_eq!(l.take_event(),Some(Event::Reply(Reply::PairFailed)));
        l.start(Command::Pair(0),3);transmit(&mut l);l.poll(3+PAIR_TIMEOUT);assert_eq!(l.take_event(),Some(Event::Fault(Fault::Timeout)));
    }
    #[test] fn switched_reports_empty_and_rejects_out_of_range_slot() {
        let mut l=RadioLink::new();l.start(Command::Switch(2),0);transmit(&mut l);
        l.receive(b"@00000001 SWITCHED 2 EMPTY\r\n",1);assert_eq!(l.take_event(),Some(Event::Reply(Reply::Switched{slot:2,paired:false})));
        l.start(Command::Switch(3),2);transmit(&mut l);l.receive(b"@00000002 SWITCHED 3 CONNECTED\r\n",3);
        assert_eq!(l.take_event(),Some(Event::Fault(Fault::Response)));
    }
}
