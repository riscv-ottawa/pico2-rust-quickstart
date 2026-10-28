//! Quickstart firmware for the Raspberry Pi Pico 2 W, running on the RP2350's
//! Hazard3 RISC-V cores.
//!
//! It starts the CYW43439 Wi-Fi radio and scans for nearby networks every ten
//! seconds, lighting the on-board LED (wired to the radio, not the RP2350)
//! while each scan runs. The main task logs a message once a second. Logs
//! appear on the board's USB serial port; see the README for how to open it.
#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_time::Timer;
use heapless::{String, Vec};
use log::info;
use pico2_rust_quickstart::{time_driver, usb_log, wifi};
use rp235x_hal::{self as hal, pac};

/// The Pico 2 W clocks the RP2350 from a 12 MHz crystal.
const XTAL_FREQ_HZ: u32 = 12_000_000;

/// Networks remembered per scan, so that each one is logged only once.
const MAX_NETWORKS: usize = 32;

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
    hal::binary_info::rp_program_description!(c"Pico 2 W RISC-V quickstart"),
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
    let control = wifi::init(
        spawner,
        pac.PIO0,
        pins.gpio23,
        pins.gpio25,
        pins.gpio24,
        pins.gpio29,
        &mut pac.RESETS,
    )
    .await;
    spawner.spawn(scan(control).expect("scan task spawned once"));

    loop {
        info!("Hello, RISC-V Ottawa!");
        Timer::after_secs(1).await;
    }
}

/// Scan for Wi-Fi networks every ten seconds and log each one found, with the
/// LED (the radio's GPIO 0) lit while the scan runs.
#[embassy_executor::task]
async fn scan(mut control: cyw43::Control<'static>) -> ! {
    loop {
        control.gpio_set(0, true).await;
        info!("Scanning for Wi-Fi networks...");

        // A network can be reported more than once per scan, for example
        // once per access point, so remember the names already logged.
        let mut seen: Vec<String<32>, MAX_NETWORKS> = Vec::new();
        let mut scanner = control.scan(cyw43::ScanOptions::default()).await;
        while let Some(bss) = scanner.next().await {
            // Copy the fields out, as `BssInfo` is a packed struct.
            let (ssid, len, rssi, channel) = (bss.ssid, bss.ssid_len, bss.rssi, bss.ctl_ch);
            let name = match core::str::from_utf8(&ssid[..usize::from(len).min(ssid.len())]) {
                Ok("") => "<hidden>",
                Ok(name) => name,
                Err(_) => "<non-UTF-8 name>",
            };
            if seen.iter().any(|known| known == name) {
                continue;
            }
            // A full list only means later repeats are logged again.
            let _ = seen.push(String::try_from(name).expect("SSIDs are at most 32 bytes"));
            info!("  {name}: {rssi} dBm, channel {channel}");
        }
        drop(scanner);

        control.gpio_set(0, false).await;
        Timer::after_secs(10).await;
    }
}
