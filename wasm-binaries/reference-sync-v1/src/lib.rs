use rudelblinken_sdk::{
    export, exports, log, set_advertisement_data, set_leds, time, yield_now, BleEvent, BleGuest,
    Guest, LogLevel,
};
use std::sync::{LazyLock, Mutex};
use talc::{ClaimOnOom, Span, Talc, Talck};

const HEAP_SIZE: usize = 36624;
static mut HEAP: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

#[global_allocator]
static ALLOCATOR: Talck<spin::Mutex<()>, ClaimOnOom> =
    Talc::new(unsafe { ClaimOnOom::new(Span::from_array((&raw const HEAP).cast_mut())) }).lock();

const NUDGE_STRENGHT: u8 = 20;
const MS_PER_STEP: u32 = 8;

const SINE_TABLE: [u8; 256] = [
    0x80, 0x83, 0x86, 0x89, 0x8C, 0x90, 0x93, 0x96, 0x99, 0x9C, 0x9F, 0xA2, 0xA5, 0xA8, 0xAB, 0xAE,
    0xB1, 0xB3, 0xB6, 0xB9, 0xBC, 0xBF, 0xC1, 0xC4, 0xC7, 0xC9, 0xCC, 0xCE, 0xD1, 0xD3, 0xD5, 0xD8,
    0xDA, 0xDC, 0xDE, 0xE0, 0xE2, 0xE4, 0xE6, 0xE8, 0xEA, 0xEB, 0xED, 0xEF, 0xF0, 0xF1, 0xF3, 0xF4,
    0xF5, 0xF6, 0xF8, 0xF9, 0xFA, 0xFA, 0xFB, 0xFC, 0xFD, 0xFD, 0xFE, 0xFE, 0xFE, 0xFF, 0xFF, 0xFF,
    0xFF, 0xFF, 0xFF, 0xFF, 0xFE, 0xFE, 0xFE, 0xFD, 0xFD, 0xFC, 0xFB, 0xFA, 0xFA, 0xF9, 0xF8, 0xF6,
    0xF5, 0xF4, 0xF3, 0xF1, 0xF0, 0xEF, 0xED, 0xEB, 0xEA, 0xE8, 0xE6, 0xE4, 0xE2, 0xE0, 0xDE, 0xDC,
    0xDA, 0xD8, 0xD5, 0xD3, 0xD1, 0xCE, 0xCC, 0xC9, 0xC7, 0xC4, 0xC1, 0xBF, 0xBC, 0xB9, 0xB6, 0xB3,
    0xB1, 0xAE, 0xAB, 0xA8, 0xA5, 0xA2, 0x9F, 0x9C, 0x99, 0x96, 0x93, 0x90, 0x8C, 0x89, 0x86, 0x83,
    0x80, 0x7D, 0x7A, 0x77, 0x74, 0x70, 0x6D, 0x6A, 0x67, 0x64, 0x61, 0x5E, 0x5B, 0x58, 0x55, 0x52,
    0x4F, 0x4D, 0x4A, 0x47, 0x44, 0x41, 0x3F, 0x3C, 0x39, 0x37, 0x34, 0x32, 0x2F, 0x2D, 0x2B, 0x28,
    0x26, 0x24, 0x22, 0x20, 0x1E, 0x1C, 0x1A, 0x18, 0x16, 0x15, 0x13, 0x11, 0x10, 0x0F, 0x0D, 0x0C,
    0x0B, 0x0A, 0x08, 0x07, 0x06, 0x06, 0x05, 0x04, 0x03, 0x03, 0x02, 0x02, 0x02, 0x01, 0x01, 0x01,
    0x01, 0x01, 0x01, 0x01, 0x02, 0x02, 0x02, 0x03, 0x03, 0x04, 0x05, 0x06, 0x06, 0x07, 0x08, 0x0A,
    0x0B, 0x0C, 0x0D, 0x0F, 0x10, 0x11, 0x13, 0x15, 0x16, 0x18, 0x1A, 0x1C, 0x1E, 0x20, 0x22, 0x24,
    0x26, 0x28, 0x2B, 0x2D, 0x2F, 0x32, 0x34, 0x37, 0x39, 0x3C, 0x3F, 0x41, 0x44, 0x47, 0x4A, 0x4D,
    0x4F, 0x52, 0x55, 0x58, 0x5B, 0x5E, 0x61, 0x64, 0x67, 0x6A, 0x6D, 0x70, 0x74, 0x77, 0x7A, 0x7D,
];

#[derive(Debug, Clone)]
struct CycleState {
    progress: u8,
    prog_time: u32,
    off_sum: i32,
    off_cnt: u16,
    nudge_rem: i8,
}

impl CycleState {
    fn new() -> Self {
        Self {
            progress: 0,
            prog_time: (time() / 1000) as u32,
            off_sum: 0,
            off_cnt: 0,
            nudge_rem: 0,
        }
    }

    fn update_progress(&mut self, timestamp: u32) {
        if self.off_cnt != 0 {
            let div = self.off_cnt as i32 * NUDGE_STRENGHT as i32;
            let nudge_base = self.off_sum + self.nudge_rem as i32;
            let nudge = nudge_base / div;
            self.nudge_rem = (nudge_base % div) as i8;

            self.progress = self.progress.wrapping_add(nudge as u8);
            self.off_sum = 0;
            self.off_cnt = 0;
        }

        let dt = self.prog_time - timestamp;
        let t_off = dt % MS_PER_STEP;
        self.prog_time = timestamp - t_off;

        let steps = dt / MS_PER_STEP;
        self.progress = self.progress.wrapping_add(steps as u8);
    }
}

static CYCLE_STATE: LazyLock<Mutex<CycleState>> = LazyLock::new(|| Mutex::new(CycleState::new()));

fn calc_bright(fraction: u8) -> u32 {
    // relative brightness to use in bright ambient conditions (>= MAX_AMBIENT); 0-255
    const MAX_BRIGHTNESS: u8 = (0.9 * 255.0) as u8;
    // relative brightness to use in dark ambient conditions (<= MIN_AMBIENT); 0-255
    // const MIN_BRIGHTNESS: u8 = (0.3 * 255.0) as u8;

    // TODO: Include ambient here
    let brightness_factor = MAX_BRIGHTNESS;

    let brightness = (SINE_TABLE[fraction as usize] as u32 * brightness_factor as u32) / 255;

    // brightness ^ 3
    let adjusted_brightness = ((brightness * brightness * brightness) / 255) / 255;

    adjusted_brightness
}

struct Test;
impl Guest for Test {
    fn run() {
        loop {
            yield_now(0);
            let progress = {
                let Ok(mut state) = CYCLE_STATE.try_lock() else {
                    continue;
                };
                state.update_progress((time() / 1000) as u32);
                state.progress
            };
            set_advertisement_data(&vec![0x00, 0x00, 0xca, 0x7e, 0xa2, progress]);
            // TODO: Add high-level API for setting led
            set_leds(0, &[calc_bright(progress) as u16]);
        }
    }
}

impl BleGuest for Test {
    fn on_event(event: BleEvent) {
        let BleEvent::Advertisement(advertisement) = event;
        let Some(data) = advertisement.manufacturer_data else {
            return;
        };

        if data.manufacturer_id != 0 {
            return;
        }

        log(LogLevel::Info, "Received advertisement");

        let [0xca, 0x7e, 0xa2, other_progress] = data.data.as_slice() else {
            return;
        };

        let Ok(mut state) = CYCLE_STATE.try_lock() else {
            return;
        };

        state.off_cnt += 1;
        state.off_sum += other_progress.wrapping_sub(state.progress) as i8 as i32;
        state.update_progress((advertisement.received_at / 1000) as u32)
    }
}

/// Main is required for `cargo run`
#[allow(dead_code)]
fn main() {}

export! {Test}
