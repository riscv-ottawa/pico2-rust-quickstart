//! Support code for the Pico 2 quickstart binaries in `src/bin/`: the pieces
//! the RISC-V cores need but no published crate provides yet, logging over
//! USB, and the Pico 2 W's radio bring-up.
#![no_std]

pub mod cyw43_bus;
pub mod time_driver;
pub mod usb_log;
pub mod wifi;
