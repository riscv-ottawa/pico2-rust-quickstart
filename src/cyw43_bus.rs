//! The bus the [`cyw43`] driver uses to talk to the Pico 2 W's radio.
//!
//! The radio is connected by a SPI-like bus that a PIO state machine drives.
//! This module is a port of [`cyw43-pio`] to `rp235x-hal`, since the original
//! is built on `embassy-rp`, which does not yet build for the RISC-V cores.
//! It is fixed to the Pico 2 W's wiring and leaves out DMA.
//!
//! This module was generated with the help of AI, working from the upstream
//! source. Review it with that in mind.
//!
//! [`cyw43-pio`]: https://github.com/embassy-rs/embassy/tree/main/cyw43-pio

use core::future::poll_fn;
use core::task::Poll;

use cyw43::SpiBusCyw43;
use embassy_sync::waitqueue::AtomicWaker;
use embedded_hal::digital::OutputPin;
use pio::{Instruction, InstructionOperands, JmpCondition, OutDestination, SetDestination};
use rp235x_hal::arch;
use rp235x_hal::gpio::bank0::{Gpio24, Gpio25, Gpio29};
use rp235x_hal::gpio::{
    FunctionPio0, FunctionSioOutput, OutputDriveStrength, OutputSlewRate, Pin, PullDown, PullNone,
};
use rp235x_hal::pac::{self, Interrupt, PIO0};
use rp235x_hal::pio::{
    PIO, PIOBuilder, PinDir, PinState, Running, Rx, SM0, ShiftDirection, StateMachine, Tx,
    UninitStateMachine,
};

/// PIO clock divider. 150 MHz / 3 gives a 50 MHz PIO clock and a 25 MHz bus
/// clock. Faster settings are unreliable on the Pico 2 W; see
/// <https://github.com/embassy-rs/embassy/issues/3960>.
const CLOCK_DIVIDER: u16 = 3;

const DIO_PIN: u8 = 24;
const CLK_PIN: u8 = 29;

type Sm = (PIO0, SM0);

/// Wakes [`SpiBusCyw43::wait_for_event`] when the radio raises an event.
static EVENT_WAKER: AtomicWaker = AtomicWaker::new();

/// Chip select, low for the duration of a transfer.
pub type CsPin = Pin<Gpio25, FunctionSioOutput, PullDown>;
/// Shared data line, driven by the PIO state machine.
pub type DioPin = Pin<Gpio24, FunctionPio0, PullNone>;
/// Bus clock, driven by the PIO state machine through side-set.
pub type ClkPin = Pin<Gpio29, FunctionPio0, PullNone>;

/// gSPI bus to the CYW43439, for use with [`cyw43::new`].
pub struct PioSpi {
    cs: CsPin,
    pio: PIO<PIO0>,
    // Only ever `None` for the duration of `start`.
    sm: Option<StateMachine<Sm, Running>>,
    rx: Rx<Sm>,
    tx: Tx<Sm>,
    wrap_target: u8,
    _dio: DioPin,
    _clk: ClkPin,
}

impl PioSpi {
    /// Load the bus program into PIO0 and start state machine 0 on it.
    ///
    /// # Panics
    ///
    /// If PIO0 has no room left for the bus program.
    pub fn new(
        mut pio: PIO<PIO0>,
        sm0: UninitStateMachine<Sm>,
        cs: CsPin,
        dio: DioPin,
        clk: ClkPin,
    ) -> Self {
        let program = pio::pio_asm!(
            ".side_set 1"

            ".wrap_target"
            // write out x-1 bits
            "lp:"
            "out pins, 1    side 0"
            "jmp x-- lp     side 1"
            // switch directions
            "set pindirs, 0 side 0"
            "nop            side 0"
            // read in y-1 bits
            "lp2:"
            "in pins, 1     side 1"
            "jmp y-- lp2    side 0"

            // wait for event and irq host
            "wait 1 pin 0   side 0"
            "irq 0          side 0"

            ".wrap"
        );
        let installed = pio
            .install(&program.program)
            .expect("PIO0 has room for the bus program");
        let wrap_target = installed.wrap_target();

        let mut dio = dio;
        dio.set_schmitt_enabled(true);
        dio.set_drive_strength(OutputDriveStrength::TwelveMilliAmps);
        dio.set_slew_rate(OutputSlewRate::Fast);
        let mut clk = clk;
        clk.set_drive_strength(OutputDriveStrength::TwelveMilliAmps);
        clk.set_slew_rate(OutputSlewRate::Fast);

        // Skip the two-cycle input synchroniser on DIO, as `cyw43-pio` does;
        // the program's sampling point assumes it.
        // SAFETY: only DIO's bit changes, and no other state machine uses it.
        unsafe {
            (*PIO0::ptr())
                .input_sync_bypass()
                .modify(|r, w| w.bits(r.bits() | (1 << DIO_PIN)));
        }

        let (mut sm, rx, tx) = PIOBuilder::from_installed_program(installed)
            .out_pins(DIO_PIN, 1)
            .in_pin_base(DIO_PIN)
            .set_pins(DIO_PIN, 1)
            .side_set_pin_base(CLK_PIN)
            .out_shift_direction(ShiftDirection::Left)
            .in_shift_direction(ShiftDirection::Left)
            .autopull(true)
            .autopush(true)
            .clock_divisor_fixed_point(CLOCK_DIVIDER, 0)
            .build(sm0);
        sm.set_pindirs([(CLK_PIN, PinDir::Output), (DIO_PIN, PinDir::Output)]);
        sm.set_pins([(CLK_PIN, PinState::Low), (DIO_PIN, PinState::Low)]);

        // Route IRQ flag 0, raised by the program's `irq 0`, to PIO0_IRQ_0.
        // The line itself stays masked until an event is awaited.
        pio.irq0().enable_sm_interrupt(0);

        Self {
            cs,
            pio,
            sm: Some(sm.start()),
            rx,
            tx,
            wrap_target,
            _dio: dio,
            _clk: clk,
        }
    }

    /// Restart the program for a transfer of `write_bits` out then
    /// `read_bits` in. Both counts are one less than the number of bits, as
    /// the program's `jmp x--` loops expect.
    fn start(&mut self, write_bits: u32, read_bits: u32) {
        let mut sm = self.sm.take().expect("state machine present").stop();
        // Load X and Y through the TX FIFO, since `set` only takes 5 bits.
        self.tx.write(write_bits);
        sm.exec_instruction(out(OutDestination::X));
        self.tx.write(read_bits);
        sm.exec_instruction(out(OutDestination::Y));
        sm.exec_instruction(side0(InstructionOperands::SET {
            destination: SetDestination::PINDIRS,
            data: 1,
        }));
        sm.exec_instruction(side0(InstructionOperands::JMP {
            condition: JmpCondition::Always,
            address: self.wrap_target,
        }));
        self.sm = Some(sm.start());
    }

    fn push(&mut self, word: u32) {
        while !self.tx.write(word) {}
    }

    fn pull(&mut self) -> u32 {
        loop {
            if let Some(word) = self.rx.read() {
                return word;
            }
        }
    }

    /// Write `words` and return the status word that follows.
    fn write(&mut self, words: &[u32]) -> u32 {
        self.start(words.len() as u32 * 32 - 1, 31);
        for &word in words {
            self.push(word);
        }
        self.pull()
    }

    /// Write `cmd`, fill `read` with the response, and return the status word.
    fn read(&mut self, cmd: u32, read: &mut [u32]) -> u32 {
        self.start(31, read.len() as u32 * 32 + 32 - 1);
        self.push(cmd);
        for slot in read.iter_mut() {
            *slot = self.pull();
        }
        self.pull()
    }
}

fn side0(operands: InstructionOperands) -> Instruction {
    Instruction {
        operands,
        delay: 0,
        side_set: Some(0),
    }
}

fn out(destination: OutDestination) -> Instruction {
    side0(InstructionOperands::OUT {
        destination,
        bit_count: 32,
    })
}

impl SpiBusCyw43 for PioSpi {
    async fn cmd_write(&mut self, write: &[u32]) -> u32 {
        self.cs.set_low().unwrap();
        let status = self.write(write);
        self.cs.set_high().unwrap();
        status
    }

    async fn cmd_read(&mut self, write: u32, read: &mut [u32]) -> u32 {
        self.cs.set_low().unwrap();
        let status = self.read(write, read);
        self.cs.set_high().unwrap();
        status
    }

    // While idle, the program waits for the radio to raise DIO and then sets
    // IRQ flag 0, which drives PIO0_IRQ_0, so this sleeps instead of polling.
    async fn wait_for_event(&mut self) {
        poll_fn(|cx| {
            EVENT_WAKER.register(cx.waker());
            if self.pio.get_irq_raw() & 1 != 0 {
                self.pio.clear_irq(1);
                Poll::Ready(())
            } else {
                // SAFETY: the handler only masks the line and wakes the task.
                unsafe { arch::interrupt_unmask(Interrupt::PIO0_IRQ_0) };
                Poll::Pending
            }
        })
        .await
    }
}

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
extern "C" fn PIO0_IRQ_0() {
    // The flag stays raised until `wait_for_event` clears it, so mask the
    // level-triggered line here to stop it firing again in the meantime.
    arch::interrupt_mask(pac::Interrupt::PIO0_IRQ_0);
    EVENT_WAKER.wake();
}
