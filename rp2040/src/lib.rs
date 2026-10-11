#![cfg_attr(not(test), no_std)]
#![deny(unsafe_code)]

pub mod keyboard;
pub mod radio_link;
pub mod kvm;
pub mod led;
#[cfg(feature = "uart-smoke")]
pub mod uart_smoke;
#[cfg(target_os = "none")]
#[allow(unsafe_code)]
mod ffi;

#[cfg(all(target_os = "none", not(test)))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! { ffi::reboot() }
