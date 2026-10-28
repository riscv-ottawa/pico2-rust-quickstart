//! Logging over the RP2350's own USB port, for boards without a debug probe.
//!
//! [`init`] brings the USB controller up as a serial port (CDC ACM) and
//! installs a [`log`] logger that writes each record to that port and to RTT,
//! so the same output reaches a terminal on the USB cable and `probe-rs` when
//! a probe is attached. Panics are logged the same way.
//!
//! The device uses Raspberry Pi's vendor and product ID for Pico SDK USB
//! serial (`2e8a:0009`) and carries the SDK's reset interface alongside the
//! serial port. That is what `picotool` looks for on a running board, so
//! `picotool load -f` can reboot it into BOOTSEL without the button.
//!
//! USB is serviced from `USBCTRL_IRQ`. The logger only appends each line to a
//! queue, and the interrupt for each completed packet sends the next one, so
//! logging never waits on the host. The queue holds output until a terminal
//! opens the port, including anything logged before the board enumerates.
//! When it is full, new lines are dropped whole.

use core::cell::RefCell;
use core::fmt::{self, Write};
use core::panic::PanicInfo;

use embassy_executor::Spawner;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};
use heapless::Deque;
use log::{LevelFilter, Log, Metadata, Record};
use rp235x_hal::clocks::UsbClock;
use rp235x_hal::pac::{self, Interrupt};
use rp235x_hal::reboot::{RebootArch, RebootKind, reboot};
use rp235x_hal::{arch, usb::UsbBus};
use static_cell::StaticCell;
use usb_device::class_prelude::{
    ControlOut, DescriptorWriter, InterfaceNumber, UsbBusAllocator, UsbClass,
};
use usb_device::control::{Recipient, RequestType};
use usb_device::prelude::{StringDescriptors, UsbDevice, UsbDeviceBuilder, UsbVidPid};
use usbd_serial::SerialPort;

/// Bytes of log output held until the host reads them.
const TX_QUEUE: usize = 1024;

/// Longest log line, including the line ending. Longer records are cut short.
const LINE: usize = 256;

/// How long output is held back after a terminal opens the port.
const SETTLE: Duration = Duration::from_millis(50);

/// Raised when a terminal opens the port, to release output after [`SETTLE`].
static OPENED: Signal<CriticalSectionRawMutex, ()> = Signal::new();

struct Usb {
    device: UsbDevice<'static, UsbBus>,
    serial: SerialPort<'static, UsbBus>,
    reset: ResetInterface,
    /// Output waiting for the serial port. It outlives the port's own buffer,
    /// which is cleared whenever the host resets the bus.
    tx: Deque<u8, TX_QUEUE>,
    /// When a terminal opened the port, or `None` while none has it open.
    opened_at: Option<Instant>,
}

impl Usb {
    fn poll(&mut self) {
        self.device.poll(&mut [&mut self.serial, &mut self.reset]);
        // Input is unused, but a received packet holds `USBCTRL_IRQ` asserted
        // until it is read, so discard it.
        while self.serial.read(&mut [0; 64]).is_ok() {}

        match (self.serial.dtr(), self.opened_at) {
            (true, None) => {
                self.opened_at = Some(Instant::now());
                OPENED.signal(());
            }
            (false, Some(_)) => self.opened_at = None,
            _ => {}
        }
        self.send();
    }

    /// Queue a whole line, or drop it if it does not fit.
    fn queue(&mut self, line: &[u8]) {
        if self.tx.capacity() - self.tx.len() >= line.len() {
            self.tx.extend(line);
            self.send();
        }
    }

    /// Move queued output into the serial port, once a terminal has had the
    /// port open for [`SETTLE`].
    fn send(&mut self) {
        if !self.opened_at.is_some_and(|at| at.elapsed() >= SETTLE) {
            return;
        }
        while let Ok(n @ 1..) = self.serial.write(self.tx.as_slices().0) {
            for _ in 0..n {
                self.tx.pop_front();
            }
        }
    }
}

static USB: Mutex<CriticalSectionRawMutex, RefCell<Option<Usb>>> = Mutex::new(RefCell::new(None));

/// Run `f` on the USB device, unless it is not set up yet or already in use
/// further up the stack (only possible if that use panicked).
fn with_usb(f: impl FnOnce(&mut Usb)) {
    USB.lock(|usb| {
        if let Ok(mut usb) = usb.try_borrow_mut()
            && let Some(usb) = usb.as_mut()
        {
            f(usb);
        }
    });
}

/// Start USB serial logging and route [`log`] records up to `level` to it and
/// to RTT.
///
/// Call this straight after the clocks are up, so that later start-up messages
/// are captured.
///
/// # Panics
///
/// If called more than once.
pub fn init(
    spawner: Spawner,
    usb: pac::USB,
    dpram: pac::USB_DPRAM,
    clock: UsbClock,
    resets: &mut pac::RESETS,
    level: LevelFilter,
) {
    static BUS: StaticCell<UsbBusAllocator<UsbBus>> = StaticCell::new();

    rtt_target::rtt_init_print!();
    let bus = BUS.init(UsbBusAllocator::new(UsbBus::new(
        usb, dpram, clock, true, resets,
    )));
    let serial = SerialPort::new(bus);
    let reset = ResetInterface(bus.interface());
    let device = UsbDeviceBuilder::new(bus, UsbVidPid(0x2e8a, 0x0009))
        .strings(&[StringDescriptors::default()
            .manufacturer("RISC-V Ottawa")
            .product("Pico 2 RISC-V")])
        .expect("one language")
        // A 64-byte control endpoint avoids an enumeration race with Windows
        // hosts; see the `rp235x_hal::usb` documentation.
        .max_packet_size_0(64)
        .expect("valid packet size")
        .composite_with_iads()
        .build();

    USB.lock(|cell| {
        cell.replace(Some(Usb {
            device,
            serial,
            reset,
            tx: Deque::new(),
            opened_at: None,
        }))
    });
    spawner.spawn(release_on_open().expect("release task spawned once"));
    log::set_logger(&Logger).expect("logger set once");
    log::set_max_level(level);
    // SAFETY: the handler only touches `USB`, which is behind a critical
    // section.
    unsafe { arch::interrupt_unmask(Interrupt::USBCTRL_IRQ) };
}

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
extern "C" fn USBCTRL_IRQ() {
    with_usb(Usb::poll);
}

/// Send the queued output once a newly opened port has settled. Without this,
/// it would wait for the next log line or USB event.
#[embassy_executor::task]
async fn release_on_open() -> ! {
    loop {
        OPENED.wait().await;
        Timer::after(SETTLE).await;
        with_usb(Usb::send);
    }
}

struct Logger;

impl Log for Logger {
    fn enabled(&self, _: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        let mut line = Line::default();
        let _ = write!(line, "[{:<5}] {}", record.level(), record.args());
        let line = line.finish();
        rtt_target::rprint!(line);
        with_usb(|usb| usb.queue(line.as_bytes()));
    }

    fn flush(&self) {}
}

/// One log record, formatted on the stack so that it can be queued whole.
struct Line {
    buf: [u8; LINE],
    len: usize,
}

impl Default for Line {
    fn default() -> Self {
        Self {
            buf: [0; LINE],
            len: 0,
        }
    }
}

impl Line {
    fn push(&mut self, s: &str) {
        // Keep two bytes for the line ending, and cut on a character boundary.
        let mut n = s.len().min(LINE - 2 - self.len);
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        self.buf[self.len..][..n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
    }

    /// End the line, which always has room for its line ending.
    fn finish(&mut self) -> &str {
        self.buf[self.len..][..2].copy_from_slice(b"\r\n");
        self.len += 2;
        // SAFETY: `push` only ever copies whole UTF-8 characters.
        unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
    }
}

impl Write for Line {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // Terminals in raw mode need a carriage return before each line feed,
        // and multi-line messages such as panics carry bare ones.
        for (i, part) in s.split('\n').enumerate() {
            if i > 0 {
                self.push("\r\n");
            }
            self.push(part);
        }
        Ok(())
    }
}

/// Log the panic, then keep servicing USB so the host can read the message
/// and `picotool` can still reboot the board.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    arch::interrupt_disable();
    log::error!("{info}");
    loop {
        with_usb(Usb::poll);
    }
}

/// The Pico SDK's reset interface, which `picotool` uses to reboot a running
/// board. It has no endpoints, only two class requests addressed to it.
struct ResetInterface(InterfaceNumber);

impl ResetInterface {
    const REQUEST_BOOTSEL: u8 = 0x01;
    const REQUEST_FLASH: u8 = 0x02;
}

impl UsbClass<UsbBus> for ResetInterface {
    fn get_configuration_descriptors(&self, w: &mut DescriptorWriter) -> usb_device::Result<()> {
        // Vendor-specific class, subclass 0, protocol 1.
        w.interface(self.0, 0xff, 0x00, 0x01)
    }

    fn control_out(&mut self, xfer: ControlOut<UsbBus>) {
        let req = xfer.request();
        if req.request_type != RequestType::Class
            || req.recipient != Recipient::Interface
            || req.index != u16::from(u8::from(self.0))
        {
            return;
        }
        // Neither request is acknowledged; `picotool` expects the board to
        // drop off the bus instead.
        match req.request {
            Self::REQUEST_BOOTSEL => reboot(
                RebootKind::BootSel {
                    // The low two bits of `wValue` select boot ROM interfaces
                    // to disable, in the order the ROM takes them.
                    msd_disabled: req.value & 0b01 != 0,
                    picoboot_disabled: req.value & 0b10 != 0,
                },
                RebootArch::Normal,
            ),
            Self::REQUEST_FLASH => reboot(RebootKind::Normal, RebootArch::Normal),
            _ => {}
        }
    }
}
