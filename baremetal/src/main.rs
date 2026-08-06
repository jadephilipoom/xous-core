#![cfg_attr(not(test), no_main)]
#![cfg_attr(not(test), no_std)]

extern crate alloc;
// contains runtime setup
mod asm;

mod platform;
mod repl;

mod erase;

use alloc::collections::VecDeque;
use core::cell::RefCell;
#[cfg(feature = "bao1x-usb")]
use core::sync::atomic::Ordering;

#[cfg(feature = "bao1x-usb")]
use bao1x_hal::{iox::Iox, usb::driver::UsbDeviceState};
use critical_section::Mutex;
use platform::*;
#[allow(unused_imports)]
use utralib::*;

#[allow(unused_imports)]
use crate::delay;
#[cfg(feature = "bao1x-usb")]
use crate::usb::glue;

static UART_RX: Mutex<RefCell<VecDeque<u8>>> = Mutex::new(RefCell::new(VecDeque::new()));
#[allow(dead_code)]
static USB_RX: Mutex<RefCell<VecDeque<u8>>> = Mutex::new(RefCell::new(VecDeque::new()));
static USB_TX: Mutex<RefCell<VecDeque<u8>>> = Mutex::new(RefCell::new(VecDeque::new()));
static USB_CONNECTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn uart_irq_handler() {
    use crate::debug::SerialRead;
    let mut uart = crate::debug::Uart {};

    loop {
        match uart.getc() {
            Some(c) => {
                critical_section::with(|cs| {
                    UART_RX.borrow(cs).borrow_mut().push_back(c);
                });
            }
            _ => break,
        }
    }
}

/// Entrypoint
///
/// # Safety
///
/// This function is safe to call exactly once.
#[export_name = "rust_entry"]
pub unsafe extern "C" fn rust_entry() -> ! {
    crate::platform::early_init();
    crate::println!("\n~~Baremetal up!~~\n");

    // provide some feedback on the run state of the BIO by peeking at the program counter
    // value, and provide feedback on the CPU operation by flashing the RGB LEDs.
    let mut repl = crate::repl::Repl::new();

    #[cfg(feature = "bao1x-usb")]
    let iox = Iox::new(utra::iox::HW_IOX_BASE as *mut u32);
    #[cfg(feature = "bao1x-usb")]
    let (mut last_usb_state, mut portsc) = crate::platform::usb::glue::hotplug_usb(&iox);
    #[cfg(feature = "bao1x-usb")]
    crate::println!(
        "  [usb connected {:?}] [tx idle {:?}]",
        USB_CONNECTED.load(Ordering::SeqCst),
        crate::platform::usb::TX_IDLE.load(Ordering::SeqCst)
    );

    #[cfg(feature = "bao1x-usb")]
    // do the main loop through either USB interface or serial port
    loop {
        let (new_usb_state, new_portsc) = glue::usb_status();

        // provide feedback when connection is established
        if new_usb_state != last_usb_state {
            crate::println_d!("new state {:?}", new_usb_state);
            if new_usb_state == UsbDeviceState::Configured {
                crate::println!("USB is connected!");
                last_usb_state = new_usb_state;
                USB_CONNECTED.store(true, core::sync::atomic::Ordering::SeqCst);
            }
        }

        // repl handling; USB is entirely interrupt driven, so there is no loop to handle it
        if USB_CONNECTED.load(Ordering::SeqCst) {
            // fetch characters from the Rx buffer
            critical_section::with(|cs| {
                let mut queue = USB_RX.borrow(cs).borrow_mut();
                while let Some(byte) = queue.pop_front() {
                    repl.rx_char(byte);
                }
            });

            // Process any command line requests
            match repl.process() {
                Err(e) => {
                    if let Some(m) = e.message {
                        crate::println!("{}", m);
                        repl.abort_cmd();
                    }
                }
                _ => (),
            };
            glue::flush_tx();
        } else {
            // Handle keyboard events.
            critical_section::with(|cs| {
                let mut queue = UART_RX.borrow(cs).borrow_mut();
                while let Some(byte) = queue.pop_front() {
                    repl.rx_char(byte);
                }
            });

            // Process any command line requests
            match repl.process() {
                Err(e) => {
                    if let Some(m) = e.message {
                        crate::println!("{}", m);
                        repl.abort_cmd();
                    }
                }
                _ => (),
            };
        }

        // return control to hard-wired serial port when USB is disconnected
        if new_portsc != portsc {
            portsc = new_portsc;
            if glue::is_disconnected(portsc) && new_usb_state == UsbDeviceState::Configured {
                crate::println_d!("USB disconnected!");
                USB_CONNECTED.store(false, core::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    #[cfg(not(feature = "bao1x-usb"))]
    // do the main loop through only the serial port
    loop {
        // Handle keyboard events.
        critical_section::with(|cs| {
            let mut queue = UART_RX.borrow(cs).borrow_mut();
            while let Some(byte) = queue.pop_front() {
                repl.rx_char(byte);
            }
        });

        // Process any command line requests
        match repl.process() {
            Err(e) => {
                if let Some(m) = e.message {
                    crate::println!("{}", m);
                    repl.abort_cmd();
                }
            }
            _ => (),
        };

        // Animate the LED flashing to indicate repl loop is running
        delay(1);
        count += 1;
    }
}
