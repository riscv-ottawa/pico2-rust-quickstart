//! Quickstart firmware for the Raspberry Pi Pico 2, running on the RP2350's
//! Hazard3 RISC-V cores.
//!
//! It blinks the on-board LED (GPIO25) from one task while the main task logs
//! a message once a second. Logs appear on the board's USB serial port; see
//! the README for how to open it.
#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::Timer;
use embedded_hal::digital::StatefulOutputPin;
use log::info;
use pico2_rust_quickstart::{time_driver, usb_log};
use rp235x_hal::gpio::bank0::Gpio25;
use rp235x_hal::gpio::{FunctionSioOutput, Pin, PullDown};
use rp235x_hal::{self as hal, pac};

/// The Pico 2 clocks the RP2350 from a 12 MHz crystal.
const XTAL_FREQ_HZ: u32 = 12_000_000;

/// Tells the boot ROM how to start this image. Built for a `riscv32` target,
/// it selects the RISC-V cores.
#[unsafe(link_section = ".start_block")]
#[used]
static IMAGE_DEF: hal::block::ImageDef = hal::block::ImageDef::secure_exe();

/// Program details shown by `picotool info`.
#[unsafe(link_section = ".bi_entries")]
#[used]
static PICOTOOL_ENTRIES: [hal::binary_info::EntryAddr; 4] = [
    hal::binary_info::rp_cargo_bin_name!(),
    hal::binary_info::rp_cargo_version!(),
    hal::binary_info::rp_program_description!(c"Pico 2 RISC-V quickstart"),
    hal::binary_info::rp_program_build_attribute!(),
];

#[embassy_executor::main(entry = "rp235x_hal::entry")]
async fn main(spawner: Spawner) -> ! {
    let mut pac = pac::Peripherals::take().expect("peripherals taken once");
    let mut watchdog = hal::Watchdog::new(pac.WATCHDOG);
    let clocks = hal::clocks::init_clocks_and_plls(
        XTAL_FREQ_HZ,
        pac.XOSC,
        pac.CLOCKS,
        pac.PLL_SYS,
        pac.PLL_USB,
        &mut pac.RESETS,
        &mut watchdog,
    )
    .expect("clocks initialise");

    let timer = hal::Timer::new_timer0(pac.TIMER0, &mut pac.RESETS, &clocks);
    usb_log::init(
        spawner,
        pac.USB,
        pac.USB_DPRAM,
        clocks.usb_clock,
        &mut pac.RESETS,
        log::LevelFilter::Info,
    );
    time_driver::init(timer);
    info!("Embassy initialized!");

    let sio = hal::Sio::new(pac.SIO);
    let pins = hal::gpio::Pins::new(
        pac.IO_BANK0,
        pac.PADS_BANK0,
        sio.gpio_bank0,
        &mut pac.RESETS,
    );
    spawner.spawn(blink(pins.gpio25.into_push_pull_output()).expect("blink task spawned once"));

    loop {
        info!("Hello, RISC-V Ottawa!");
        Timer::after_secs(1).await;
    }
}

/// Toggle the LED twice a second.
#[embassy_executor::task]
async fn blink(mut led: Pin<Gpio25, FunctionSioOutput, PullDown>) -> ! {
    loop {
        let Ok(()) = led.toggle();
        Timer::after_millis(500).await;
    }
}
