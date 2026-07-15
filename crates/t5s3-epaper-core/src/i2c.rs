//! The shared I2C0 bus, owned entirely by a worker that runs on the second
//! CPU core. This module only handles the bus itself — request/response
//! plumbing, poll scheduling, deep-sleep handshake, timing instrumentation —
//! the wire protocol for each device lives in that device's own module
//! (`crate::gt911`, `crate::pca9555`, `crate::bq27220`, `crate::bq25896`,
//! `crate::pcf8563`).
//!
//! Every on-board I2C peripheral (PCA9555 IO expander, TPS65185 panel PMIC,
//! GT911 touch controller, BQ27220 fuel gauge, BQ25896 charger, PCF8563
//! external RTC) lives on this one bus. A previous design shared the bus
//! across cores behind a spinlock-guarded `RefCell`, but both cores issuing
//! transactions against the same peripheral (serialized only by a coarse
//! atomic, not real ordering) corrupted the GT911 protocol under load
//! (duplicated keystrokes). Giving the bus a single owner removes that
//! failure mode entirely: core 0 never touches the peripheral, it only sends
//! [`Request`]s over a queue and reads results back, so there is exactly one
//! thread of execution ever driving the wire.
//!
//! Every device registers its own bus address by implementing [`Registered`]
//! for its [`Addr`] (see e.g. `gt911.rs`'s `impl Registered for
//! Addr<ADDR_LOW>`); the manifest at the bottom of this file asserts one of
//! those impls exists for every address the board is documented to carry a
//! device at. Two devices claiming the same address is `E0119` (conflicting
//! impls); a documented address with no device wired up, or a device file
//! that forgot to register, is `E0277` (trait bound not satisfied) naming
//! the address — both compile errors, not runtime surprises.
//!
//! Devices that need autonomous sampling implement [`PolledDevice`], which
//! carries its own poll interval; [`Worker::run`] drives each one through a
//! generic [`PollTimer`], so its scheduling logic never hardcodes how or
//! when to poll a device — it just calls `T::POLL_INTERVAL_US` and
//! `device.poll(..)` for whatever `T` it's holding. Taps/home presses go on
//! a bounded [`Event`] queue, the aux button state into a single cached
//! slot. Everything else (panel power sequencing, battery, charger, RTC) is
//! rare/occasional and goes through the blocking [`Request`]/[`Response`]
//! channel instead.

use alloc::collections::VecDeque;
use core::{
    cell::RefCell,
    sync::atomic::{AtomicBool, Ordering},
};

use critical_section::Mutex;
use esp_hal::{
    delay::Delay,
    i2c::master::{Config as I2cConfig, ConfigError, I2c},
    peripherals,
    time::Rate,
    Blocking,
};

use crate::gt911::Gt911;
pub use crate::gt911::TouchPinConfig;

const FREQUENCY_KHZ: u32 = 100;

// core1's base loop tick: kept short so a queued request (display power
// sequencing, mainly) never waits long behind a device poll.
const LOOP_POLL_US: u32 = 2_000;

// touch/home events are best-effort notifications: a full queue means core 0
// is falling behind, so the oldest-pending policy is to drop the newest
// rather than block core 1's polling loop.
const EVENT_QUEUE_CAP: usize = 10;
// core 0 only ever has one request in flight at a time (every public
// accessor below blocks for its response before returning), so this is
// generous headroom, not a real backlog.
const REQUEST_QUEUE_CAP: usize = 4;
// requests other than the known-slow whole-procedure ones are expected to be
// a handful of I2C bytes; anything slower than this points at real bus
// contention worth investigating, not just a chip's own protocol delay.
const SLOW_THRESHOLD_US: u64 = 5_000;

/// A bus address as a type, so that claiming one is a trait impl rather
/// than an entry in a list. Two `impl Registered for Addr<N>` for the same
/// `N` anywhere in the crate is an ordinary conflicting-impl error — the
/// compiler enforces uniqueness for us, nothing here has to check for it.
pub(crate) struct Addr<const A: u8>;

/// Implemented once per device, on that device's own [`Addr`], in that
/// device's own module — see the bottom of `gt911.rs`, `pca9555.rs`,
/// `bq27220.rs`, `bq25896.rs`, `pcf8563.rs`, and `ed047tc1.rs`.
pub(crate) trait Registered {}

const fn assert_registered<T: Registered>() {}

// The board's documented i2c addresses, cross-checked against whichever
// `impl Registered` happen to exist anywhere in the crate. This list is the
// only thing that can drift from the hardware — a device module that exists
// but never registers its `Addr`, or an address below with no device at
// all, fails to compile (`E0277`, naming the address) rather than surfacing
// as a bus timeout in the field.
const _: () = {
    assert_registered::<Addr<{ crate::pca9555::ADDR }>>(); // PCA9555 IO expander
    assert_registered::<Addr<{ crate::ed047tc1::TPS65185_ADDR }>>(); // TPS65185 panel PMIC
    assert_registered::<Addr<{ crate::gt911::ADDR_LOW }>>(); // GT911 touch (strap low)
    assert_registered::<Addr<{ crate::gt911::ADDR_HIGH }>>(); // GT911 touch (strap high)
    assert_registered::<Addr<{ crate::bq27220::ADDR }>>(); // BQ27220 fuel gauge
    assert_registered::<Addr<{ crate::bq25896::ADDR }>>(); // BQ25896 charger
    assert_registered::<Addr<{ crate::pcf8563::ADDR }>>(); // PCF8563 RTC
};

/// Touch tap (already mapped to screen coordinates by the caller) or a home
/// button press, as detected by core 1's autonomous touch poll.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    Tap { x: u16, y: u16 },
    Home,
}

/// A request for the worker's owned i2c bus. The generic byte-level ops are
/// handled here; everything device-specific is a request type owned by that
/// device's own module, so this file never needs to know their wire details.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Request {
    WriteByte { addr: u8, reg: u8, value: u8 },
    ReadByte { addr: u8, reg: u8 },
    Rtc(crate::pcf8563::Request),
    Battery(crate::bq27220::Request),
    Charger(crate::bq25896::Request),
}

impl Request {
    // a handful of whole-procedure requests are known to take longer than
    // `SLOW_THRESHOLD_US` by design (multi-step register sequences with their
    // own settle delays), so they're excluded from the slow-bus-op warning —
    // that warning is for catching unexpected contention, not re-reporting a
    // chip's own documented timing.
    fn is_known_slow(&self) -> bool {
        matches!(
            self,
            Request::Battery(crate::bq27220::Request::ProgramCapacity(_))
                | Request::Battery(crate::bq27220::Request::Diagnostics)
                | Request::Charger(crate::bq25896::Request::Status)
        )
    }
}

pub(crate) enum Response {
    Unit(crate::Result<()>),
    U8(crate::Result<u8>),
    Rtc(crate::pcf8563::Response),
    Battery(crate::bq27220::Response),
    Charger(crate::bq25896::Response),
}

/// A device [`Worker::run`] samples autonomously rather than on request.
/// Implemented once per polled device, in that device's own module (see
/// `gt911.rs`'s `impl PolledDevice for Gt911` and `pca9555.rs`'s `impl
/// PolledDevice for AuxButton`) — this file only ever sees `T: PolledDevice`
/// through [`PollTimer`], so adding a new polled device never touches this
/// file at all.
pub(crate) trait PolledDevice {
    /// How often this device should be sampled, matched to how fast its
    /// hardware can actually produce a fresh reading — not simply as fast
    /// as the bus loop spins.
    const POLL_INTERVAL_US: u64;

    fn poll(&mut self, i2c: &mut I2c<'_, Blocking>) -> crate::Result<()>;
}

/// Schedules one [`PolledDevice`] on its own interval. A plain generic
/// wrapper rather than dynamic dispatch: the interval and the poll call are
/// both resolved at compile time per concrete `T`, so scheduling costs
/// nothing beyond a subtraction and a comparison.
struct PollTimer<T> {
    device: T,
    last_poll_us: u64,
}

impl<T: PolledDevice> PollTimer<T> {
    fn new(device: T) -> Self {
        PollTimer {
            device,
            last_poll_us: 0,
        }
    }

    fn maybe_poll(&mut self, i2c: &mut I2c<'_, Blocking>, now: u64) {
        if now.wrapping_sub(self.last_poll_us) < T::POLL_INTERVAL_US {
            return;
        }
        self.last_poll_us = now;
        self.device.poll(i2c).ok();
    }
}

impl<T> core::ops::Deref for PollTimer<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.device
    }
}

impl<T> core::ops::DerefMut for PollTimer<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.device
    }
}

static REQUEST_QUEUE: Mutex<RefCell<VecDeque<Request>>> = Mutex::new(RefCell::new(VecDeque::new()));
static RESPONSE_SLOT: Mutex<RefCell<Option<Response>>> = Mutex::new(RefCell::new(None));
static TOUCH_EVENTS: Mutex<RefCell<VecDeque<Event>>> = Mutex::new(RefCell::new(VecDeque::new()));
static AUX_BUTTON: Mutex<RefCell<bool>> = Mutex::new(RefCell::new(false));

// deep-sleep handshake: core 0 sets SLEEP_REQUESTED and waits for
// SLEEP_DONE, since the GT911 sleep sequence (and the pad hold that keeps it
// latched through deep sleep) must complete on core 1, which owns the pins.
static SLEEP_REQUESTED: AtomicBool = AtomicBool::new(false);
static SLEEP_DONE: AtomicBool = AtomicBool::new(false);

fn now_us() -> u64 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_micros()
}

/// Queue a request against the bus and block for its response. Called by
/// device modules' own channel-based public accessors (e.g.
/// [`crate::pcf8563::read_unix`]) — this file owns the queue, not the wire
/// protocol behind any given request.
pub(crate) fn submit(request: Request) -> Response {
    loop {
        let pushed = critical_section::with(|cs| {
            let mut queue = REQUEST_QUEUE.borrow_ref_mut(cs);
            if queue.len() < REQUEST_QUEUE_CAP {
                queue.push_back(request);
                true
            } else {
                false
            }
        });
        if pushed {
            break;
        }
        core::hint::spin_loop();
    }
    loop {
        if let Some(response) = critical_section::with(|cs| RESPONSE_SLOT.borrow_ref_mut(cs).take())
        {
            return response;
        }
        core::hint::spin_loop();
    }
}

fn pop_request() -> Option<Request> {
    critical_section::with(|cs| REQUEST_QUEUE.borrow_ref_mut(cs).pop_front())
}

fn set_response(response: Response) {
    critical_section::with(|cs| *RESPONSE_SLOT.borrow_ref_mut(cs) = Some(response));
}

/// Push a touch/home event onto the shared output queue. Called by polled
/// device modules (currently just [`crate::gt911`]) as they detect edges.
pub(crate) fn push_event(event: Event) {
    critical_section::with(|cs| {
        let mut queue = TOUCH_EVENTS.borrow_ref_mut(cs);
        if queue.len() < EVENT_QUEUE_CAP {
            queue.push_back(event);
        } else {
            log::warn!("i2c: touch event queue full, dropping event");
        }
    });
}

/// Drain the next pending touch/home event, if any.
pub fn poll_event() -> Option<Event> {
    critical_section::with(|cs| TOUCH_EVENTS.borrow_ref_mut(cs).pop_front())
}

/// The aux button's last-polled state; cheap enough to read every main-loop
/// pass.
pub fn aux_button_pressed() -> bool {
    critical_section::with(|cs| *AUX_BUTTON.borrow_ref_mut(cs))
}

/// Cache the aux button's polled state. Called by [`crate::pca9555`].
pub(crate) fn set_aux_button(pressed: bool) {
    critical_section::with(|cs| *AUX_BUTTON.borrow_ref_mut(cs) = pressed);
}

/// Ask core 1 to sleep the touch controller and park, then block until it
/// confirms (up to 200ms; times out rather than hanging forever if core 1
/// is wedged).
///
/// Call this last, after every other i2c operation needed on the way into
/// deep sleep. Once core 1 sees the request it sleeps the GT911 and parks
/// for good in [`Worker::run`], never servicing the request queue again, so
/// any i2c call after this returns blocks core 0 forever.
pub fn sleep_and_park(delay: &Delay) {
    SLEEP_REQUESTED.store(true, Ordering::Release);
    for _ in 0..200 {
        if SLEEP_DONE.load(Ordering::Acquire) {
            return;
        }
        delay.delay_millis(1);
    }
}

pub(crate) fn write_byte(addr: u8, reg: u8, value: u8) -> crate::Result<()> {
    match submit(Request::WriteByte { addr, reg, value }) {
        Response::Unit(r) => r,
        _ => unreachable!("i2c: WriteByte always answers Response::Unit"),
    }
}

pub(crate) fn read_byte(addr: u8, reg: u8) -> crate::Result<u8> {
    match submit(Request::ReadByte { addr, reg }) {
        Response::U8(r) => r,
        _ => unreachable!("i2c: ReadByte always answers Response::U8"),
    }
}

/// Owns the physical I2C0 peripheral and every polled device's state. Built
/// on core 0 during single-threaded boot, then moved wholesale into
/// [`Worker::run`] on the second core via `CpuControl::start_app_core` —
/// ownership transfer via `Send`, not a shared borrow, so there is no
/// cross-core aliasing to guard against.
pub struct Worker<'d> {
    i2c: I2c<'d, Blocking>,
    touch: PollTimer<Gt911<'d>>,
    aux_button: PollTimer<crate::pca9555::AuxButton>,
}

impl<'d> Worker<'d> {
    /// Build the worker on I2C0. Polled devices are initialized lazily on
    /// their first poll inside [`Worker::run`].
    pub fn new(
        i2c: peripherals::I2C0<'d>,
        sda: peripherals::GPIO39<'d>,
        scl: peripherals::GPIO40<'d>,
        touch: TouchPinConfig<'d>,
    ) -> core::result::Result<Self, ConfigError> {
        let i2c = I2c::new(
            i2c,
            I2cConfig::default().with_frequency(Rate::from_khz(FREQUENCY_KHZ)),
        )?
        .with_sda(sda)
        .with_scl(scl);

        Ok(Worker {
            i2c,
            touch: PollTimer::new(Gt911::new(touch)),
            aux_button: PollTimer::new(crate::pca9555::AuxButton),
        })
    }

    /// Run the worker loop forever: services queued requests, polls every
    /// registered `PolledDevice` on its own schedule, and handles the
    /// deep-sleep handshake. Call this from the closure passed to
    /// `CpuControl::start_app_core`.
    ///
    /// The `SLEEP_REQUESTED` branch is a one-way trip: it sleeps the GT911,
    /// signals `SLEEP_DONE`, then parks forever without returning to the
    /// request queue. See [`sleep_and_park`] for what that means for
    /// callers.
    pub fn run(mut self) -> ! {
        let delay = Delay::new();

        loop {
            if SLEEP_REQUESTED.load(Ordering::Acquire) {
                self.touch.sleep(&mut self.i2c).ok();
                SLEEP_DONE.store(true, Ordering::Release);
                loop {
                    core::hint::spin_loop();
                }
            }

            if let Some(request) = pop_request() {
                let start = now_us();
                let response = self.dispatch(request);
                let elapsed_us = now_us().wrapping_sub(start);
                log::debug!("i2c: {:?} took {}us", request, elapsed_us);
                if elapsed_us > SLOW_THRESHOLD_US && !request.is_known_slow() {
                    log::warn!("i2c: slow bus op {:?} took {}us", request, elapsed_us);
                }
                set_response(response);
            }

            // each polled device gets its own schedule, matched to how fast
            // its hardware can actually produce a fresh sample, rather than
            // being sampled on every ~2ms loop tick.
            let now = now_us();
            self.touch.maybe_poll(&mut self.i2c, now);
            self.aux_button.maybe_poll(&mut self.i2c, now);

            delay.delay_micros(LOOP_POLL_US);
        }
    }

    fn dispatch(&mut self, request: Request) -> Response {
        match request {
            Request::WriteByte { addr, reg, value } => {
                Response::Unit(self.write_byte_raw(addr, reg, value))
            }
            Request::ReadByte { addr, reg } => Response::U8(self.read_byte_raw(addr, reg)),
            Request::Rtc(req) => Response::Rtc(crate::pcf8563::dispatch(&mut self.i2c, req)),
            Request::Battery(req) => {
                Response::Battery(crate::bq27220::dispatch(&mut self.i2c, req))
            }
            Request::Charger(req) => {
                Response::Charger(crate::bq25896::dispatch(&mut self.i2c, req))
            }
        }
    }

    fn write_byte_raw(&mut self, addr: u8, reg: u8, value: u8) -> crate::Result<()> {
        self.i2c
            .write(addr, &[reg, value])
            .map_err(crate::Error::I2c)
    }

    fn read_byte_raw(&mut self, addr: u8, reg: u8) -> crate::Result<u8> {
        let mut value = [0u8; 1];
        self.i2c
            .write_read(addr, &[reg], &mut value)
            .map_err(crate::Error::I2c)?;
        Ok(value[0])
    }
}
