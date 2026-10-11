//! All raw pointers and global single-core ownership live at this boundary.
//! C calls exports only on its main loop, never IRQ context; IRQ owns only C UART RX.
#[cfg(not(feature = "uart-smoke"))]
mod production {
use core::{cell::UnsafeCell,slice};
use crate::{keyboard::{self,Descriptor},kvm::{Kvm,State},led,radio_link::Fault};
extern "C" {
    fn bridge_uart_read(out:*mut u8,capacity:usize)->usize;
    fn bridge_uart_write(data:*const u8,len:usize)->usize;
    fn bridge_uart_take_errors()->u32;
    fn bridge_hid_receive(dev:u8,instance:u8)->bool;
    fn bridge_hid_set_protocol(dev:u8,instance:u8,protocol:u8)->bool;
    fn bridge_hid_protocol_state(dev:u8,instance:u8)->u8;
    fn bridge_rgb(mask:u8);
    fn bridge_diagnostic(code:u32,detail:u32);
}
#[derive(Clone,Copy,PartialEq,Eq)] enum Mode { BootWait,ReportWait,Boot,Report,Failed }
struct Binding {dev:u8,instance:u8,descriptor:Descriptor,mode:Mode,protocol_submitted:bool,receiving:bool,protocol_started:u64}
struct App { kvm:Kvm,binding:Option<Binding>,now:u64,last_state:State,last_fault:Option<Fault>,last_rgb:u8 }
impl App {
    const fn new()->Self{Self{kvm:Kvm::new(),binding:None,now:0,last_state:State::Fault,last_fault:None,last_rgb:u8::MAX}}
    fn task(&mut self,now:u64){
        self.now=now;
        let errors=unsafe{bridge_uart_take_errors()};
        if errors!=0{diagnostic(10,errors);self.kvm.link.uart_error();}
        let mut rx=[0;128];
        for _ in 0..8 {
            let n=unsafe{bridge_uart_read(rx.as_mut_ptr(),rx.len())}.min(rx.len());
            if n==0{break;}self.kvm.link.receive(&rx[..n],now);
        }
        if let Some(b)=self.binding.as_mut(){
            if matches!(b.mode,Mode::BootWait|Mode::ReportWait){
                let protocol=if b.mode==Mode::BootWait{0}else{1};
                if !b.protocol_submitted {b.protocol_submitted=unsafe{bridge_hid_set_protocol(b.dev,b.instance,protocol)};}
                if b.protocol_submitted {
                    match unsafe{bridge_hid_protocol_state(b.dev,b.instance)} {
                        2=>{b.mode=if protocol==0{Mode::Boot}else{Mode::Report};diagnostic(3,protocol as u32);}
                        3=>{
                            if b.mode==Mode::BootWait {b.mode=Mode::ReportWait;b.protocol_submitted=false;b.protocol_started=now;}
                            else {b.mode=Mode::Failed;self.kvm.fail(Fault::Keyboard);diagnostic(8,2);}
                        }
                        _=>(),
                    }
                }
                if matches!(b.mode,Mode::BootWait|Mode::ReportWait)&&now.saturating_sub(b.protocol_started)>=2_000_000 {
                    if b.mode==Mode::BootWait {b.mode=Mode::ReportWait;b.protocol_submitted=false;b.protocol_started=now;}
                    else {b.mode=Mode::Failed;self.kvm.fail(Fault::Keyboard);diagnostic(8,2);}
                }
            }
            if !b.receiving && matches!(b.mode,Mode::Boot|Mode::Report) {b.receiving=unsafe{bridge_hid_receive(b.dev,b.instance)};}
        }
        self.kvm.task(now);
        let tx=self.kvm.link.tx();
        if !tx.is_empty(){let n=unsafe{bridge_uart_write(tx.as_ptr(),tx.len().min(64))};self.kvm.link.written(n);}
        if self.kvm.scroll_event {self.kvm.scroll_event=false;diagnostic(4,0);}
        if self.kvm.state!=self.last_state {
            self.last_state=self.kvm.state;diagnostic(5,state_code(self.last_state));
            if self.last_state==State::Ready {diagnostic(13,self.kvm.active as u32);}
        }
        if self.kvm.fault!=self.last_fault {self.last_fault=self.kvm.fault;if let Some(f)=self.last_fault{diagnostic(6,fault_code(f));}}
        let rgb=led::rgb(self.kvm.indicator(),now);
        if rgb!=self.last_rgb{self.last_rgb=rgb;unsafe{bridge_rgb(rgb)};}
    }
}
struct Global(UnsafeCell<App>);
// The SDK loop is the sole owner; native IRQs must never call Rust callbacks.
unsafe impl Sync for Global {}
static APP:Global=Global(UnsafeCell::new(App::new()));
fn app()->&'static mut App {unsafe{&mut *APP.0.get()}}
fn diagnostic(code:u32,detail:u32){unsafe{bridge_diagnostic(code,detail)}}
fn state_code(state:State)->u32{match state{
    State::BootVersion=>0,State::BootSelect=>1,State::BootRelease=>2,State::Ready=>3,
    State::ReleaseOld=>4,State::SelectNew=>5,State::ReleaseNew=>6,
    State::RecoverVersion=>7,State::RecoverStatus=>8,State::RecoverOld=>9,State::RecoverSelect=>10,
    State::RecoverNew=>11,State::WaitRelease=>12,State::UnplugRelease=>13,State::Fault=>14,State::Maintenance=>15,
    State::PairRelease=>16,State::Pairing=>17,State::PairDone=>18,
}}
fn fault_code(f:Fault)->u32{match f{Fault::Timeout=>0,Fault::Uart=>1,Fault::RxOverflow=>2,Fault::QueueOverflow=>3,Fault::Response=>4,Fault::Keyboard=>6}}
#[no_mangle]pub extern "C" fn rp_app_init(){
    let a=app();a.kvm=Kvm::new();a.binding=None;a.now=0;
    a.last_state=State::Fault;a.last_fault=None;a.last_rgb=u8::MAX;
    diagnostic(1,0x00010000);
    diagnostic(11,core::mem::size_of::<App>() as u32);
    diagnostic(12,core::mem::size_of::<Descriptor>() as u32);
}
#[no_mangle]pub extern "C" fn rp_app_task(now_us:u64){app().task(now_us);}
#[no_mangle]pub extern "C" fn rp_hid_mount(dev:u8,instance:u8,_vid:u16,_pid:u16,protocol:u8,descriptor:*const u8,len:u16){
    let a=app();if a.binding.is_some(){return;}
    if descriptor.is_null()||len==0||len>1024{diagnostic(8,0);return;}
    // Callback-owned descriptor is copied into the bounded parsed representation before return.
    let bytes=unsafe{slice::from_raw_parts(descriptor,len as usize)};
    match Descriptor::parse(bytes){
        Ok(descriptor)=>{
            let mode=if protocol==1{Mode::BootWait}else{Mode::Report};
            a.binding=Some(Binding{dev,instance,descriptor,mode,protocol_submitted:false,receiving:false,protocol_started:a.now});
            a.kvm.mount();diagnostic(2,((dev as u32)<<8)|instance as u32);
            if mode==Mode::Report{diagnostic(3,1);}
        }
        Err(reason)=>{if reason!="no keyboard collection"{diagnostic(8,1);}},
    }
}
#[no_mangle]pub extern "C" fn rp_hid_report(dev:u8,instance:u8,data:*const u8,len:u16){
    let a=app();
    let Some(b)=a.binding.as_mut().filter(|b|b.dev==dev&&b.instance==instance) else{return};
    b.receiving=false;
    if data.is_null()||len>256{a.kvm.fail(Fault::Keyboard);}
    else {
        let bytes=unsafe{slice::from_raw_parts(data,len as usize)};
        let decoded=match b.mode{Mode::Boot=>Some(keyboard::boot(bytes)),Mode::Report=>Some(b.descriptor.decode(bytes,a.kvm.physical())),_=>None};
        if let Some(decoded)=decoded{a.kvm.input(decoded,a.now);}
    }
    b.receiving=matches!(b.mode,Mode::Boot|Mode::Report)&&unsafe{bridge_hid_receive(dev,instance)};
}
#[no_mangle]pub extern "C" fn rp_hid_unmount(dev:u8,instance:u8){
    let a=app();if a.binding.as_ref().is_some_and(|b|b.dev==dev&&b.instance==instance){
        a.binding=None;a.kvm.unmount(a.now);diagnostic(9,((dev as u32)<<8)|instance as u32);
    }
}
}

#[cfg(feature = "uart-smoke")]
mod smoke {
    use core::{cell::UnsafeCell, ptr};
    use crate::uart_smoke::{Smoke, Stats};

    extern "C" {
        fn bridge_uart_read(out: *mut u8, capacity: usize) -> usize;
        fn bridge_uart_write(data: *const u8, len: usize) -> usize;
        fn bridge_uart_take_errors() -> u32;
        fn bridge_hid_receive(dev: u8, instance: u8) -> bool;
    }

    struct Global(UnsafeCell<Smoke>);
    // Only the SDK main loop calls the Rust exports. UART IRQ owns only C RX.
    unsafe impl Sync for Global {}
    static APP: Global = Global(UnsafeCell::new(Smoke::new()));

    // Exported RAM snapshot, 28 bytes: tx_frames, rx_bytes, peer_frames,
    // bad_frames, uart_errors, last_peer_seq, task_count. Each field is u32.
    // Main-loop publication is volatile so SWD observes actual changing counters.
    #[used]
    #[no_mangle]
    pub static mut rp_uart_smoke_stats: Stats = Stats::ZERO;

    fn app() -> &'static mut Smoke { unsafe { &mut *APP.0.get() } }
    fn publish(stats: Stats) {
        unsafe { ptr::write_volatile(ptr::addr_of_mut!(rp_uart_smoke_stats), stats); }
    }

    #[no_mangle]
    pub extern "C" fn rp_app_init() {
        *app() = Smoke::new();
        publish(Stats::ZERO);
    }

    #[no_mangle]
    pub extern "C" fn rp_app_task(now_us: u64) {
        let app = app();
        if unsafe { bridge_uart_take_errors() } != 0 { app.uart_error(); }
        let mut rx = [0; 128];
        for _ in 0..8 {
            let count = unsafe { bridge_uart_read(rx.as_mut_ptr(), rx.len()) }.min(rx.len());
            if count == 0 { break; }
            app.receive(&rx[..count]);
        }
        app.task(now_us);
        let tx = app.tx();
        if !tx.is_empty() {
            let count = unsafe { bridge_uart_write(tx.as_ptr(), tx.len()) };
            app.written(count);
        }
        publish(app.stats);
    }

    // Retain the normal USB host loop, but discard HID data without KVM routing.
    #[no_mangle]
    pub extern "C" fn rp_hid_mount(dev: u8, instance: u8, _vid: u16, _pid: u16,
        _protocol: u8, _descriptor: *const u8, _len: u16) {
        unsafe { bridge_hid_receive(dev, instance); }
    }
    #[no_mangle]
    pub extern "C" fn rp_hid_report(dev: u8, instance: u8, _data: *const u8, _len: u16) {
        unsafe { bridge_hid_receive(dev, instance); }
    }
    // Diagnostics retain no keyboard binding or output-report state to clean up.
    #[no_mangle]
    pub extern "C" fn rp_hid_unmount(_dev: u8, _instance: u8) {}
}
pub fn reboot()->!{
    extern "C" { fn bridge_reboot(); }
    unsafe{bridge_reboot()};
    loop{core::hint::spin_loop();}
}
