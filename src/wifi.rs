//! Bring-up of the Pico 2 W's CYW43439 Wi-Fi radio.
//!
//! [`init`] powers the radio up, loads its firmware over the gSPI bus in
//! [`crate::cyw43_bus`], starts the `cyw43` driver as a background task, and
//! returns the [`Control`] handle used to scan, join networks and drive the
//! radio's GPIOs (including the board's LED on GPIO 0).
//!
//! The steps follow embassy's `blinky_wifi.rs` example for the Arm cores.

use cyw43::{Control, PowerManagementMode, Runner, SpiBus, State};
use embassy_executor::Spawner;
use rp235x_hal::gpio::bank0::{Gpio23, Gpio24, Gpio25, Gpio29};
use rp235x_hal::gpio::{DefaultTypeState, FunctionSioOutput, Pin, PinState, PullDown};
use rp235x_hal::pac::{PIO0, RESETS};
use rp235x_hal::pio::PIOExt;
use static_cell::StaticCell;

use crate::cyw43_bus::PioSpi;

/// A pin in the state [`rp235x_hal::gpio::Pins::new`] hands it out in.
type Unconfigured<I> = Pin<I, <I as DefaultTypeState>::Function, <I as DefaultTypeState>::PullType>;

/// The radio's power enable, `WL_ON`.
type PowerPin = Pin<Gpio23, FunctionSioOutput, PullDown>;

/// Start the radio and return its control handle.
///
/// The pins are the Pico 2 W's fixed wiring to the radio: GPIO23 powers it,
/// and GPIO25, GPIO24 and GPIO29 are the gSPI chip select, data and clock.
///
/// # Panics
/// If called more than once, or if the driver task cannot be spawned.
pub async fn init(
    spawner: Spawner,
    pio0: PIO0,
    pwr: Unconfigured<Gpio23>,
    cs: Unconfigured<Gpio25>,
    dio: Unconfigured<Gpio24>,
    clk: Unconfigured<Gpio29>,
    resets: &mut RESETS,
) -> Control<'static> {
    static STATE: StaticCell<State> = StaticCell::new();

    let fw = cyw43::aligned_bytes!("../cyw43-firmware/43439A0.bin");
    let clm = cyw43::aligned_bytes!("../cyw43-firmware/43439A0_clm.bin");
    let nvram = cyw43::aligned_bytes!("../cyw43-firmware/nvram_rp2040.bin");

    let (pio, sm0, _, _, _) = pio0.split(resets);
    let spi = PioSpi::new(
        pio,
        sm0,
        cs.into_push_pull_output_in_state(PinState::High),
        dio.reconfigure(),
        clk.reconfigure(),
    );

    let state = STATE.init(State::new());
    let (_net_device, mut control, runner) =
        cyw43::new(state, pwr.into_push_pull_output(), spi, fw, nvram).await;
    spawner.spawn(run(runner).expect("radio driver task spawned once"));

    control.init(clm).await;
    control
        .set_power_management(PowerManagementMode::PowerSave)
        .await;
    control
}

/// Run the `cyw43` driver, which services every request to the radio.
#[embassy_executor::task]
async fn run(runner: Runner<'static, SpiBus<PowerPin, PioSpi>>) -> ! {
    runner.run().await
}
