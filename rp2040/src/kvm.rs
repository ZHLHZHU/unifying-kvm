use crate::keyboard::{Decode,KeyReport,Physical};
use crate::led::Indicator;
use crate::radio_link::{Command,Event,Fault,RadioLink,Reply,SLOTS};
/// Holding Scroll Lock this long pairs the current slot instead of switching.
pub const LONG_PRESS_US:u64=3_000_000;
/// Pause between startup probes; each in-flight command retains the UART reply deadline.
const BOOT_PROBE_US:u64=1_000_000;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum State { BootVersion,BootSelect,BootRelease,Ready,ReleaseOld,SelectNew,ReleaseNew,
    RecoverVersion,RecoverStatus,RecoverOld,RecoverSelect,RecoverNew,WaitRelease,UnplugRelease,Fault,Maintenance,
    PairRelease,Pairing,PairDone }
#[derive(Clone,Copy,Debug,PartialEq,Eq)] enum Action { Switch,Pair }
#[derive(Clone,Copy,Debug,PartialEq,Eq)] enum Entry { Report(KeyReport),Barrier(Action) }
#[derive(Clone,Copy)] struct Queue {items:[Entry;128],head:usize,len:usize}
impl Queue {
    const fn new()->Self {Self{items:[Entry::Barrier(Action::Switch);128],head:0,len:0}}
    fn clear(&mut self){self.head=0;self.len=0;}
    fn front(&self)->Option<Entry>{(self.len!=0).then(||self.items[self.head])}
    fn pop(&mut self){if self.len!=0{self.head=(self.head+1)%128;self.len-=1;}}
    fn push(&mut self,item:Entry)->bool{
        if self.len!=0&&matches!(item,Entry::Report(_))&&self.items[(self.head+self.len-1)%128]==item{return true;}
        if self.len==128{return false;}self.items[(self.head+self.len)%128]=item;self.len+=1;true
    }
}
#[derive(Clone,Copy)] enum Purpose { Control,Input,Idle,Probe }
pub struct Kvm {
    /// `linked`: the active slot is paired and accepts key reports. Empty slots drop input.
    pub link:RadioLink,pub state:State,pub active:u8,pub candidate:u8,pub fault:Option<Fault>,pub linked:bool,
    queue:Queue,physical:Physical,blocked:Physical,selected:KeyReport,scroll:bool,scroll_since:Option<u64>,
    mounted:bool,valid_report:bool,action_pending:bool,rollover:bool,purpose:Purpose,
    next_probe:u64,last_activity:u64,pub scroll_event:bool,
}
impl Default for Kvm {fn default()->Self{Self::new()}}
impl Kvm {
    pub const fn new()->Self{Self{link:RadioLink::new(),state:State::BootVersion,active:0,candidate:0,fault:None,linked:false,
        queue:Queue::new(),physical:Physical{bits:[0;8],modifiers:0},blocked:Physical{bits:[0;8],modifiers:0},
        selected:KeyReport{modifiers:0,keys:[0;6]},scroll:false,scroll_since:None,mounted:false,valid_report:false,
        action_pending:false,rollover:false,purpose:Purpose::Control,next_probe:0,last_activity:0,scroll_event:false}}
    pub fn physical(&self)->Physical{self.physical}
    pub fn indicator(&self)->Indicator{
        // Shown target: the chosen slot as soon as a switch starts; reacquiring its receiver can take seconds.
        let slot=if matches!(self.state,State::SelectNew|State::ReleaseNew){self.candidate}else{self.active};
        match self.state{
            State::Ready|State::ReleaseOld|State::SelectNew|State::ReleaseNew|State::WaitRelease=>
                if self.link.reaching(){Indicator::Connecting(slot)}else{Indicator::Slot(slot)},
            State::PairRelease|State::Pairing|State::PairDone=>Indicator::Pairing(self.active),
            State::Fault=>Indicator::Fault,
            _=>Indicator::Off,
        }
    }
    pub fn mount(&mut self){self.mounted=true;self.valid_report=false;self.physical=Physical::default();self.scroll=false;self.scroll_since=None;}
    pub fn unmount(&mut self,now:u64){
        self.mounted=false;self.valid_report=false;self.physical=Physical::default();self.scroll=false;self.scroll_since=None;
        self.clear_inputs();
        // An in-flight pairing keeps running on the nRF; let its reply land instead of desynchronizing.
        if matches!(self.state,State::Pairing) {return;}
        self.link.cancel();
        if self.state!=State::Maintenance {self.state=State::UnplugRelease;}self.last_activity=now;
    }
    fn clear_inputs(&mut self){self.queue.clear();self.blocked=Physical::default();self.selected=KeyReport::default();self.action_pending=false;self.rollover=false;}
    pub fn fail(&mut self,fault:Fault){self.link.cancel();self.clear_inputs();self.fault=Some(fault);self.state=State::Fault;}
    /// Queue a switch/pair barrier; held keys stay blocked until released on the new target.
    fn barrier(&mut self,action:Action){
        if !self.queue.push(Entry::Barrier(action)){self.fail(Fault::QueueOverflow);return;}
        self.action_pending=true;self.blocked=self.physical;self.selected=KeyReport::default();
    }
    /// Scroll Lock still acts while the current target is unreachable, which may be indefinitely.
    fn can_act(&self)->bool{!self.action_pending||(self.link.reaching()&&matches!(self.state,State::Ready|State::ReleaseNew))}
    /// Give up on input to an unreachable target so a new switch/pair is not queued behind it forever.
    /// Unacknowledged reports never reached that receiver; it drops held keys on its own link timeout.
    fn abandon_unreached(&mut self){
        if !self.link.reaching(){return;}
        match self.state{
            State::Ready=>{self.link.cancel();self.queue.clear();}
            // The nRF is already on the candidate slot; adopt it, then the new barrier moves on from there.
            State::ReleaseNew=>{self.link.cancel();self.finish_switch();self.queue.clear();self.state=State::Ready;}
            _=>(),
        }
    }
    pub fn input(&mut self,decoded:Decode,now:u64){
        let physical=match decoded {
            Decode::State(p)=>{self.rollover=false;self.valid_report=true;p},
            Decode::Rollover=>{
                if self.rollover{return;}self.rollover=true;
                // Never accept rollover as evidence of a physical all-up during recovery.
                self.valid_report=false;
                if self.accepting()&&self.selected!=KeyReport::default(){
                    self.selected=KeyReport::default();if !self.queue.push(Entry::Report(self.selected)){self.fail(Fault::QueueOverflow);}
                }return;
            }
            Decode::Other=>return,Decode::Invalid=>{self.fail(Fault::Keyboard);return;}
        };
        self.physical=physical;self.blocked.retain_held(physical);
        let scroll=physical.contains(0x47);let rising=scroll&&!self.scroll;let falling=!scroll&&self.scroll;self.scroll=scroll;
        if rising {self.scroll_event=true;}
        if self.state==State::Fault {
            if rising {self.clear_inputs();self.state=State::RecoverVersion;self.fault=None;}return;
        }
        if !self.accepting(){self.scroll_since=None;return;}
        if rising {self.scroll_since=self.can_act().then_some(now);}
        // Short press switches on release; a long press already fired from task() and cleared scroll_since.
        if falling {
            if let Some(since)=self.scroll_since.take() {
                if self.can_act()&&now.saturating_sub(since)<LONG_PRESS_US {
                    self.abandon_unreached();self.barrier(Action::Switch);if self.state==State::Fault{return;}
                }
            }
        }
        if !self.linked {return;}
        let next=physical.without(self.blocked).select(self.selected);
        if next!=self.selected {
            self.selected=next;self.last_activity=now;
            if !self.queue.push(Entry::Report(next)){self.fail(Fault::QueueOverflow);}
        }
    }
    fn accepting(&self)->bool {matches!(self.state,State::Ready|State::ReleaseOld|State::SelectNew|State::ReleaseNew)}
    fn command(&mut self,c:Command,p:Purpose,now:u64){if self.link.start(c,now){self.purpose=p;}}
    fn command_once(&mut self,c:Command,now:u64){if self.link.start_once(c,now){self.purpose=Purpose::Control;}}
    pub fn task(&mut self,now:u64){
        // Reports only arrive on change, so a held Scroll Lock is timed here.
        if let Some(since)=self.scroll_since {
            if self.scroll&&self.accepting()&&self.can_act()&&now.saturating_sub(since)>=LONG_PRESS_US {
                self.scroll_since=None;self.abandon_unreached();self.barrier(Action::Pair);
            }
        }
        self.link.poll(now);
        if let Some(event)=self.link.take_event(){self.handle(event,now);}
        // Command-free transitions (barrier start, empty-slot release) issue the next command in the same call.
        for _ in 0..4 {
            if !self.link.idle(){return;}
            let before=self.state;self.dispatch(now);
            if self.state==before{return;}
        }
    }
    fn dispatch(&mut self,now:u64){
        let all_up=Command::Keys(KeyReport::default());
        match self.state {
            State::BootVersion=>{if now>=self.next_probe {self.command(Command::Version,Purpose::Control,now);}},
            State::RecoverVersion=>self.command(Command::Version,Purpose::Control,now),
            State::BootSelect=>self.command(Command::Switch(0),Purpose::Control,now),
            // One attempt, never block: leaving a target, recovering (the slot may be unreachable for a
            // long time and Scroll Lock is inert until recovery ends), or re-pairing a lost one.
            State::ReleaseOld|State::RecoverOld|State::RecoverNew|State::UnplugRelease|State::PairRelease=>{
                if self.linked {self.command_once(all_up,now);} else {self.advance_unlinked(now);}
            }
            // Entering a target: retried until it answers, blinking meanwhile; Scroll Lock still switches away.
            State::BootRelease|State::ReleaseNew|State::PairDone=>{
                // Empty slots reject key reports; there is nothing to release on them.
                if self.linked {self.command(all_up,Purpose::Control,now);} else {self.advance_unlinked(now);}
            }
            State::RecoverStatus=>self.command(Command::Status,Purpose::Control,now),
            State::RecoverSelect=>self.command(Command::Switch(self.active),Purpose::Control,now),
            State::SelectNew=>self.command(Command::Switch(self.candidate),Purpose::Control,now),
            State::Pairing=>self.command(Command::Pair(self.active),Purpose::Control,now),
            State::WaitRelease=>{
                if self.mounted&&self.valid_report&&self.physical.empty(){self.state=State::Ready;self.last_activity=now;}
            }
            State::Ready=>match self.queue.front(){
                Some(Entry::Report(report))=>self.command(Command::Keys(report),Purpose::Input,now),
                Some(Entry::Barrier(Action::Switch))=>{self.candidate=next_slot(self.active);self.state=State::ReleaseOld;}
                Some(Entry::Barrier(Action::Pair))=>{self.queue.pop();self.state=State::PairRelease;}
                None=>{if self.physical.empty()&&now.saturating_sub(self.last_activity)>=1_000_000{
                    self.command(Command::Status,Purpose::Idle,now);
                }}
            },
            State::Maintenance=>{if now>=self.next_probe {self.next_probe=now+1_000_000;self.command(Command::Version,Purpose::Probe,now);}},
            State::Fault=>(),
        }
    }
    /// Same transitions as a successful all-up, for states whose target is an empty slot.
    fn advance_unlinked(&mut self,now:u64){
        self.state=match self.state {
            State::BootRelease|State::RecoverNew|State::PairDone=>State::WaitRelease,
            State::RecoverOld=>State::RecoverSelect,
            State::ReleaseOld=>State::SelectNew,
            State::ReleaseNew=>{self.finish_switch();State::Ready}
            State::UnplugRelease=>State::RecoverVersion,
            State::PairRelease=>State::Pairing,
            other=>other,
        };
        self.last_activity=now;
    }
    fn finish_switch(&mut self){self.active=self.candidate;self.queue.pop();self.action_pending=false;}
    fn handle(&mut self,event:Event,now:u64){
        let reply=match event{Event::Fault(f)=>{
            // nRF may start later than RP. Restart only the boot handshake on silence;
            // runtime failures and malformed replies still require explicit recovery.
            if f==Fault::Timeout&&matches!(self.state,State::BootVersion|State::BootSelect|State::BootRelease) {
                self.clear_inputs();self.state=State::BootVersion;self.next_probe=now+BOOT_PROBE_US;return;
            }
            if self.state==State::Maintenance {self.next_probe=now+1_000_000;self.fault=Some(f);return;}
            self.fail(f);return;
        },Event::Reply(r)=>r};
        if reply==Reply::Busy {
            if matches!(self.state,State::BootVersion|State::BootSelect|State::BootRelease) {
                self.clear_inputs();self.state=State::BootVersion;self.next_probe=now+BOOT_PROBE_US;return;
            }
            self.clear_inputs();self.state=State::Maintenance;self.next_probe=now+1_000_000;return;
        }
        match self.purpose {
            Purpose::Input=>{if reply==Reply::Ok{self.queue.pop();self.last_activity=now;}else{self.fail(Fault::Response);}return;}
            Purpose::Idle=>{
                if matches!(reply,Reply::Status{slot,linked} if slot==self.active&&linked==self.linked){self.last_activity=now;}
                else{self.clear_inputs();self.state=State::RecoverVersion;}return;
            }
            Purpose::Probe=>{
                if reply==Reply::Version{self.state=State::RecoverStatus;self.fault=None;}
                else{self.fail(Fault::Response);}return;
            }
            Purpose::Control=>(),
        }
        self.state=match (self.state,reply){
            (State::BootVersion,Reply::Version)=>State::BootSelect,
            (State::BootSelect,Reply::Switched{slot:0,paired})=>{self.active=0;self.linked=paired;State::BootRelease},
            (State::BootRelease,Reply::Ok)=>State::WaitRelease,
            (State::RecoverVersion,Reply::Version)=>State::RecoverStatus,
            (State::RecoverStatus,Reply::Status{slot,linked})=>{self.active=slot;self.linked=linked;State::RecoverOld},
            (State::RecoverOld,Reply::Ok|Reply::Unreached)=>State::RecoverSelect,
            (State::RecoverSelect,Reply::Switched{slot,paired}) if slot==self.active=>{self.linked=paired;State::RecoverNew},
            (State::RecoverNew,Reply::Ok|Reply::Unreached)=>State::WaitRelease,
            (State::ReleaseOld,Reply::Ok|Reply::Unreached)=>State::SelectNew,
            (State::SelectNew,Reply::Switched{slot,paired}) if slot==self.candidate=>{self.linked=paired;State::ReleaseNew},
            (State::ReleaseNew,Reply::Ok)=>{self.finish_switch();State::Ready}
            (State::UnplugRelease,Reply::Ok|Reply::Unreached)=>State::RecoverVersion,
            (State::PairRelease,Reply::Ok|Reply::Unreached)=>State::Pairing,
            // nRF keeps (or restores) the previous link on failure, and is connected to the new one on success.
            (State::Pairing,Reply::Paired)=>{self.linked=true;self.clear_inputs();State::PairDone},
            (State::Pairing,Reply::PairFailed)=>{self.clear_inputs();State::WaitRelease},
            (State::PairDone,Reply::Ok)=>State::WaitRelease,
            _=>{self.fail(Fault::Response);return;}
        };
        self.last_activity=now;
    }
}
/// Short-press target order.
pub const fn next_slot(slot:u8)->u8{(slot+1)%SLOTS}

#[cfg(test)] mod tests {
    use super::*;
    fn reply(k:&mut Kvm,body:&str,now:u64){
        let frame=k.link.tx();let mut bytes=[0;256];bytes[..9].copy_from_slice(&frame[..9]);bytes[9]=b' ';
        bytes[10..10+body.len()].copy_from_slice(body.as_bytes());bytes[10+body.len()]=b'\n';
        let n=frame.len();k.link.written(n);k.link.receive(&bytes[..11+body.len()],now);k.task(now);
    }
    fn ready()->Kvm{
        let mut k=Kvm::new();k.mount();k.input(Decode::State(Physical::default()),0);k.task(0);
        reply(&mut k,"VER unifying/1 build=test",1);reply(&mut k,"SWITCHED 0 CONNECTED",2);reply(&mut k,"OK",3);k.task(4);
        assert_eq!(k.state,State::Ready);k
    }
    fn physical(keys:&[u8],modifiers:u8)->Physical{let mut p=Physical{modifiers,..Physical::default()};for &key in keys{p.insert(key);}p}
    fn tap_scroll(k:&mut Kvm,held:&[u8],now:u64){
        let mut with=held.to_vec();with.push(0x47);
        k.input(Decode::State(physical(&with,0)),now);k.input(Decode::State(physical(held,0)),now+1);
    }
    #[test] fn delayed_nrf_startup_waits_then_initializes_without_replaying_keys(){
        let mut k=Kvm::new();k.mount();k.task(0);
        let first=k.link.tx()[..9].to_vec();k.link.written(k.link.tx().len());
        k.input(Decode::State(physical(&[4],0)),1);
        k.task(2_000_000);
        assert_eq!((k.state,k.fault),(State::BootVersion,None));
        assert!(k.link.tx().is_empty());assert_eq!(k.queue.len,0);
        k.task(2_999_999);assert!(k.link.tx().is_empty());
        k.task(3_000_000);assert_ne!(&k.link.tx()[..9],first.as_slice());
        reply(&mut k,"ERR BUSY",3_000_001);
        assert_eq!((k.state,k.fault),(State::BootVersion,None));
        k.task(4_000_000);assert!(k.link.tx().is_empty());
        k.task(4_000_001);reply(&mut k,"VER unifying/1 build=test",4_000_002);
        reply(&mut k,"SWITCHED 0 CONNECTED",4_000_003);reply(&mut k,"OK",4_000_004);
        assert_eq!(k.state,State::WaitRelease);assert_eq!(k.queue.len,0);
        k.input(Decode::State(Physical::default()),4_000_005);k.task(4_000_005);
        assert_eq!(k.state,State::Ready);
        k.input(Decode::State(physical(&[5],0)),4_000_006);k.task(4_000_006);
        assert!(k.link.tx().ends_with(b"UKEYDOWN 00 05\n"));
    }
    #[test] fn boot_selection_timeout_restarts_probe_but_runtime_timeout_faults(){
        let mut k=Kvm::new();k.task(0);reply(&mut k,"VER unifying/1 build=test",1);
        k.link.written(k.link.tx().len());k.task(2_000_001);
        assert_eq!((k.state,k.fault),(State::BootVersion,None));
        k.task(3_000_001);reply(&mut k,"VER unifying/1 build=test",3_000_002);
        reply(&mut k,"SWITCHED 0 CONNECTED",3_000_003);
        k.link.written(k.link.tx().len());k.task(5_000_003);
        assert_eq!((k.state,k.fault),(State::BootVersion,None));
        let mut running=ready();running.task(1_000_004);
        running.link.written(running.link.tx().len());running.task(3_000_004);
        assert_eq!((running.state,running.fault),(State::Fault,Some(Fault::Timeout)));
    }
    #[test] fn short_press_release_preserves_fifo(){
        let mut k=ready();k.input(Decode::State(physical(&[4],0)),5);k.input(Decode::State(Physical::default()),6);k.task(7);
        assert!(k.link.tx().ends_with(b"UKEYDOWN 00 04\n"));reply(&mut k,"OK",8);
        assert!(k.link.tx().ends_with(b"UKEYDOWN 00\n"));reply(&mut k,"OK",9);assert_eq!(k.queue.len,0);
    }
    #[test] fn switch_on_release_with_barrier_and_held_keys(){
        let mut k=ready();k.input(Decode::State(physical(&[4],1)),5);k.task(5);
        k.input(Decode::State(physical(&[4,0x47],1)),6);
        assert_eq!(k.queue.len,1); // Pressing Scroll Lock alone does not switch yet.
        k.input(Decode::State(physical(&[4],1)),7);k.input(Decode::State(physical(&[4,5],1)),8);
        reply(&mut k,"OK",9);assert_eq!(k.state,State::ReleaseOld);
        reply(&mut k,"OK",10);assert_eq!(k.state,State::SelectNew);assert_eq!(k.indicator(),Indicator::Slot(1));
        reply(&mut k,"SWITCHED 1 CONNECTED",11);assert_eq!(k.active,0);assert_eq!(k.indicator(),Indicator::Slot(1));
        // Receiver not reached yet: the new color blinks until the release is acknowledged, across resends.
        reply(&mut k,"ERR SEND",11);assert_eq!(k.indicator(),Indicator::Connecting(1));
        k.task(11+30_000);assert_eq!(k.indicator(),Indicator::Connecting(1));
        reply(&mut k,"OK",12);assert_eq!(k.active,1);assert_eq!(k.indicator(),Indicator::Slot(1));
        // Modifier held across the switch stays blocked too, so only the newly pressed key goes out.
        assert!(k.link.tx().ends_with(b"UKEYDOWN 00 05\n"));reply(&mut k,"OK",13);
        k.input(Decode::State(Physical::default()),15);k.input(Decode::State(physical(&[4],1)),16);k.task(17);
        assert!(k.link.tx().ends_with(b"UKEYDOWN 00\n"));reply(&mut k,"OK",18);
        assert!(k.link.tx().ends_with(b"UKEYDOWN 01 04\n"));
    }
    #[test] fn slots_cycle_through_three_and_empty_slot_drops_input(){
        let mut k=ready();
        tap_scroll(&mut k,&[],5);k.task(7);reply(&mut k,"OK",8);reply(&mut k,"SWITCHED 1 CONNECTED",9);reply(&mut k,"OK",10);
        assert_eq!((k.active,k.state),(1,State::Ready));
        tap_scroll(&mut k,&[],11);k.task(13);reply(&mut k,"OK",14);
        assert!(k.link.tx().ends_with(b"USWITCH 02\n"));reply(&mut k,"SWITCHED 2 EMPTY",15);
        // No all-up on an empty slot; the switch completes without another command.
        assert_eq!((k.active,k.state,k.linked),(2,State::Ready,false));assert!(k.link.tx().is_empty());
        assert_eq!(k.indicator(),Indicator::Slot(2));
        k.input(Decode::State(physical(&[4],0)),16);k.task(17);assert!(k.link.tx().is_empty());
        k.input(Decode::State(Physical::default()),18);
        // Leaving an empty slot sends no release to it.
        tap_scroll(&mut k,&[],19);k.task(21);assert!(k.link.tx().ends_with(b"USWITCH 00\n"));
        reply(&mut k,"SWITCHED 0 CONNECTED",22);reply(&mut k,"OK",23);assert_eq!((k.active,k.linked),(0,true));
    }
    #[test] fn long_press_pairs_current_slot_without_switching(){
        let mut k=ready();k.input(Decode::State(physical(&[0x47],0)),100);
        k.task(100+LONG_PRESS_US-1);assert_eq!(k.state,State::Ready);
        k.task(100+LONG_PRESS_US);assert_eq!(k.state,State::PairRelease);assert_eq!(k.indicator(),Indicator::Pairing(0));
        assert!(k.link.tx().ends_with(b"UKEYDOWN 00\n"));reply(&mut k,"OK",LONG_PRESS_US+200);
        assert!(k.link.tx().ends_with(b"UPAIR 00\n"));
        k.input(Decode::State(Physical::default()),LONG_PRESS_US+300); // Releasing after the long press must not switch.
        reply(&mut k,"PAIRED A54A930029 CH=44",LONG_PRESS_US+5_000_000);
        // Keys were already released, so PairDone's all-up chains straight back to Ready.
        assert_eq!(k.state,State::PairDone);reply(&mut k,"OK",LONG_PRESS_US+5_000_001);
        assert_eq!((k.state,k.active,k.linked,k.queue.len),(State::Ready,0,true,0));
    }
    #[test] fn failed_pairing_on_empty_slot_returns_without_fault(){
        let mut k=ready();tap_scroll(&mut k,&[],5);k.task(7);reply(&mut k,"OK",8);reply(&mut k,"SWITCHED 1 EMPTY",9);
        assert_eq!((k.active,k.linked),(1,false));
        k.input(Decode::State(physical(&[0x47],0)),10);k.task(10+LONG_PRESS_US);
        assert!(k.link.tx().ends_with(b"UPAIR 01\n")); // No release is sent to the empty slot first.
        k.input(Decode::State(Physical::default()),11+LONG_PRESS_US);
        reply(&mut k,"ERR PAIR",12+LONG_PRESS_US);
        assert_eq!((k.state,k.fault,k.linked),(State::Ready,None,false));
    }
    #[test] fn queue_overflow_faults_without_replaying(){
        let mut k=ready();for n in 0..129{k.input(Decode::State(if n%2==0{physical(&[4],0)}else{Physical::default()}),10+n);}
        assert_eq!(k.state,State::Fault);assert_eq!(k.fault,Some(Fault::QueueOverflow));assert_eq!(k.queue.len,0);
        assert_eq!(k.indicator(),Indicator::Fault);
    }
    #[test] fn rollover_releases_once_and_recovers(){
        let mut k=ready();k.input(Decode::State(physical(&[4],0)),5);k.input(Decode::Rollover,6);k.input(Decode::Rollover,7);
        assert_eq!(k.queue.len,2);k.task(8);reply(&mut k,"OK",9);reply(&mut k,"OK",10);
        k.input(Decode::State(physical(&[5],0)),11);k.task(12);assert!(k.link.tx().ends_with(b"UKEYDOWN 00 05\n"));
    }
    #[test] fn maintenance_recovers_actual_slot_and_requires_physical_release(){
        let mut k=ready();k.input(Decode::State(physical(&[4],0)),5);k.task(6);reply(&mut k,"ERR BUSY",7);
        assert_eq!(k.state,State::Maintenance);k.input(Decode::State(physical(&[5],0)),8);
        k.task(1_000_007);reply(&mut k,"VER unifying/1 build=test",1_000_008);
        reply(&mut k,"STATUS SLOT=2 PAIRED=1 CONN=1 CH=5 CNT=7",1_000_009);
        reply(&mut k,"OK",1_000_010);reply(&mut k,"SWITCHED 2 CONNECTED",1_000_011);reply(&mut k,"OK",1_000_012);
        assert_eq!(k.state,State::WaitRelease);assert_eq!(k.active,2);assert_eq!(k.queue.len,0);
        k.input(Decode::State(physical(&[5],0)),1_000_013);k.task(1_000_014);assert_eq!(k.state,State::WaitRelease);
        k.input(Decode::State(Physical::default()),1_000_015);k.task(1_000_016);assert_eq!(k.state,State::Ready);
    }
    // Field 23:22:05: fault recovery landed on a slot whose receiver was out of reach. RecoverNew
    // retried its all-up forever, the LED was off and Scroll Lock was ignored, until it reconnected.
    #[test] fn recovery_on_unreachable_slot_reaches_ready_and_scroll_lock_works(){
        let mut k=ready();k.fail(Fault::Timeout);
        k.input(Decode::State(physical(&[0x47],0)),10);k.input(Decode::State(Physical::default()),11);k.task(12);
        reply(&mut k,"VER unifying/1 build=test",13);reply(&mut k,"STATUS SLOT=1 PAIRED=1 CONN=1 CH=5 CNT=7",14);
        reply(&mut k,"ERR SEND",15);assert_eq!(k.state,State::RecoverSelect);
        reply(&mut k,"SWITCHED 1 CONNECTED",16);assert_eq!(k.state,State::RecoverNew);
        reply(&mut k,"ERR SEND",17);
        assert_eq!((k.state,k.active),(State::Ready,1));
        tap_scroll(&mut k,&[],18);k.task(20);assert!(k.link.tx().ends_with(b"UKEYDOWN 00\n"));
        reply(&mut k,"ERR SEND",21);assert!(k.link.tx().ends_with(b"USWITCH 02\n"));
    }
    #[test] fn noconn_switch_without_all_up_ack_faults_without_routing(){
        let mut k=ready();tap_scroll(&mut k,&[],5);k.task(6);
        k.input(Decode::State(physical(&[4],0)),7);
        reply(&mut k,"OK",8);reply(&mut k,"SWITCHED 1 NOCONN",9);
        assert_eq!(k.state,State::ReleaseNew);assert_eq!(k.active,0);
        let n=k.link.tx().len();k.link.written(n);k.task(2_000_009);
        assert_eq!(k.state,State::Fault);assert_eq!(k.fault,Some(Fault::Timeout));
        assert_eq!(k.active,0);assert_eq!(k.queue.len,0);
        assert!(k.link.tx().is_empty());
    }
    #[test] fn unreachable_target_blinks_forever_and_switching_away_still_works(){
        let mut k=ready();k.input(Decode::State(physical(&[4],0)),5);k.input(Decode::State(Physical::default()),6);k.task(7);
        // Receiver gone for a minute: same report resent, no fault, slot color blinks.
        let mut now=8;
        while now<60_000_000 {reply(&mut k,"ERR SEND",now);assert_eq!(k.state,State::Ready);now+=30_000;k.task(now);}
        assert_eq!((k.fault,k.indicator()),(None,Indicator::Connecting(0)));
        assert!(k.link.tx().ends_with(b"UKEYDOWN 00 04\n"));
        // Short press switches anyway; the old target gets one release attempt that may fail.
        tap_scroll(&mut k,&[],now);k.task(now+2);
        assert_eq!(k.state,State::ReleaseOld);assert!(k.link.tx().ends_with(b"UKEYDOWN 00\n"));
        reply(&mut k,"ERR SEND",now+3);assert_eq!(k.state,State::SelectNew);
        assert!(k.link.tx().ends_with(b"USWITCH 01\n"));reply(&mut k,"SWITCHED 1 CONNECTED",now+4);reply(&mut k,"OK",now+5);
        assert_eq!((k.state,k.active,k.fault,k.queue.len),(State::Ready,1,None,0));
        assert_eq!(k.indicator(),Indicator::Slot(1));
    }
    #[test] fn switch_while_new_target_unreached_moves_on_from_it(){
        let mut k=ready();tap_scroll(&mut k,&[],5);k.task(7);reply(&mut k,"OK",8);reply(&mut k,"SWITCHED 1 CONNECTED",9);
        reply(&mut k,"ERR SEND",10);assert_eq!((k.state,k.indicator()),(State::ReleaseNew,Indicator::Connecting(1)));
        // Slot 1 never answers; the next press continues the cycle to slot 2, not back to 1.
        k.task(10+30_000);tap_scroll(&mut k,&[],10+30_001);k.task(10+30_003);
        assert_eq!(k.active,1);assert!(k.link.tx().ends_with(b"UKEYDOWN 00\n"));
        reply(&mut k,"ERR SEND",10+30_004);assert!(k.link.tx().ends_with(b"USWITCH 02\n"));
        reply(&mut k,"SWITCHED 2 EMPTY",10+30_005);assert_eq!((k.state,k.active,k.fault),(State::Ready,2,None));
    }
    #[test] fn long_press_repairs_an_unreachable_slot(){
        let mut k=ready();k.input(Decode::State(physical(&[4],0)),5);k.input(Decode::State(Physical::default()),6);k.task(7);
        reply(&mut k,"ERR SEND",8);k.input(Decode::State(physical(&[0x47],0)),9);k.task(9+LONG_PRESS_US);
        assert_eq!(k.state,State::PairRelease);assert!(k.link.tx().ends_with(b"UKEYDOWN 00\n"));
        reply(&mut k,"ERR SEND",10+LONG_PRESS_US);assert!(k.link.tx().ends_with(b"UPAIR 00\n"));
    }
    #[test] fn fault_scroll_edge_recovers_instead_of_switching(){
        let mut k=ready();k.fail(Fault::Response);
        k.input(Decode::State(physical(&[0x47],0)),11);k.task(12);
        assert_eq!(k.state,State::RecoverVersion);
        assert!(k.link.tx().ends_with(b"VER\n"));
        k.input(Decode::State(Physical::default()),13);assert_eq!(k.queue.len,0);
    }
}
