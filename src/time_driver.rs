//! `embassy-time` driver for the RP2350 RISC-V cores.
//!
//! `embassy-rp` ships a time driver, but that crate only targets the Arm cores,
//! so this module provides the equivalent on top of `rp235x-hal`. It is a port
//! of `embassy-rp`'s driver: `TIMER0` counts microseconds from the 1 MHz tick
//! that [`rp235x_hal::clocks::init_clocks_and_plls`] starts, and alarm 0 wakes
//! the executor when the earliest pending timer expires.
//!
//! The alarm interrupt reaches us through `rp235x-hal`'s `MachineExternal`
//! handler, which reads the next pending IRQ from the Hazard3 interrupt
//! controller and calls the function with the matching name, here
//! [`TIMER0_IRQ_0`].

use core::cell::{Cell, RefCell};
use core::task::Waker;

use critical_section::CriticalSection;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_time_driver::Driver;
use embassy_time_queue_utils::Queue;
use rp235x_hal::pac::{self, Interrupt};
use rp235x_hal::timer::CopyableTimer0;
use rp235x_hal::{Timer, arch};

struct TimerDriver {
    /// Expiry of the armed alarm in ticks, or `u64::MAX` when none is armed.
    alarm_at: Mutex<CriticalSectionRawMutex, Cell<u64>>,
    queue: Mutex<CriticalSectionRawMutex, RefCell<Queue>>,
}

embassy_time_driver::time_driver_impl!(static DRIVER: TimerDriver = TimerDriver {
    alarm_at: Mutex::const_new(CriticalSectionRawMutex::new(), Cell::new(u64::MAX)),
    queue: Mutex::new(RefCell::new(Queue::new())),
});

fn timer0() -> &'static pac::timer0::RegisterBlock {
    // SAFETY: once `init` has run, this module is the only user of TIMER0's
    // alarm 0 registers, and reads of the raw counter have no side effects.
    unsafe { &*pac::TIMER0::ptr() }
}

impl Driver for TimerDriver {
    fn now(&self) -> u64 {
        let timer = timer0();
        // The 64-bit counter is read as two halves, so retry if the high word
        // rolled over between the reads.
        loop {
            let hi = timer.timerawh().read().bits();
            let lo = timer.timerawl().read().bits();
            if hi == timer.timerawh().read().bits() {
                return (u64::from(hi) << 32) | u64::from(lo);
            }
        }
    }

    fn schedule_wake(&self, at: u64, waker: &Waker) {
        critical_section::with(|cs| {
            if self.queue.borrow(cs).borrow_mut().schedule_wake(at, waker) {
                self.rearm(cs);
            }
        })
    }
}

impl TimerDriver {
    /// Arm alarm 0 for `at`. Returns `false` if `at` has already passed, in
    /// which case the alarm will not fire and the caller must handle it now.
    fn set_alarm(&self, cs: CriticalSection, at: u64) -> bool {
        let timer = timer0();
        self.alarm_at.borrow(cs).set(at);

        // The alarm compares only the low 32 bits of the counter, so it may
        // fire early for a deadline more than 2^32 us (about 72 minutes) out.
        // `on_interrupt` checks the full deadline and re-arms in that case.
        // SAFETY: any 32-bit value is a valid alarm target.
        timer.alarm0().write(|w| unsafe { w.bits(at as u32) });

        if at <= self.now() {
            // Writing a 1 to ARMED disarms the alarm.
            // SAFETY: bit 0 selects alarm 0; other alarms are untouched.
            timer.armed().write(|w| unsafe { w.bits(1) });
            self.alarm_at.borrow(cs).set(u64::MAX);
            false
        } else {
            true
        }
    }

    /// Arm the alarm for the next queued expiry, waking any timers that are
    /// already due along the way.
    fn rearm(&self, cs: CriticalSection) {
        let mut queue = self.queue.borrow(cs).borrow_mut();
        while !self.set_alarm(cs, queue.next_expiration(self.now())) {}
    }

    fn on_interrupt(&self) {
        critical_section::with(|cs| {
            timer0().intr().write(|w| w.alarm_0().clear_bit_by_one());

            let at = self.alarm_at.borrow(cs).get();
            if at <= self.now() {
                self.rearm(cs);
            } else {
                // Fired early because the deadline is more than 2^32 us out.
                // SAFETY: any 32-bit value is a valid alarm target.
                timer0().alarm0().write(|w| unsafe { w.bits(at as u32) });
            }
        })
    }
}

/// Start the time driver and enable machine external interrupts.
///
/// Taking the HAL [`Timer`] proves that `TIMER0` has been brought out of reset
/// with the microsecond tick running. Call this once, before any
/// `embassy-time` timer is awaited.
pub fn init(_timer: Timer<CopyableTimer0>) {
    timer0().inte().modify(|_, w| w.alarm_0().set_bit());
    // SAFETY: the handler only touches state guarded by critical sections.
    unsafe {
        arch::interrupt_unmask(Interrupt::TIMER0_IRQ_0);
        arch::interrupt_enable();
    }
}

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
extern "C" fn TIMER0_IRQ_0() {
    DRIVER.on_interrupt();
}
