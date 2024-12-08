use std::sync::Mutex;

use rudelblinken_sdk::{
    common::{
        BLEAdvData, BLEAdvNotification, BLEAdvSettings, LEDBrightnessSettings, Log, LogLevel,
    },
    guest::host,
};
use talc::{ClaimOnOom, Span, Talc, Talck};

const NUDGE_STRENGHT: u8 = 20;
const MS_PER_STEP: u32 = 16;
const HEAP_SIZE: usize = 36624;
static mut HEAP: [u8; HEAP_SIZE] = [0u8; HEAP_SIZE];

#[global_allocator]
static ALLOCATOR: Talck<spin::Mutex<()>, ClaimOnOom> =
    Talc::new(unsafe { ClaimOnOom::new(Span::from_array((&raw const HEAP).cast_mut())) }).lock();

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
            prog_time: host::get_time_millis(),
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

            host::host_log(&Log {
                level: LogLevel::Info,
                message: format!("nudging progress ({} advs) = {}", self.off_cnt, nudge).to_owned(),
            });
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

#[no_mangle]
extern "C" fn main() {
    let name = host::get_name();
    host::host_log(&Log {
        level: LogLevel::Info,
        message: format!("name = {}", name).to_owned(),
    });
    let has_host_base = host::has_host_base();
    let has_ble_adv = host::has_ble_adv();
    let has_led_brightness = host::has_led_brightness();

    host::host_log(&Log {
        level: LogLevel::Info,
        message: format!(
            "has_host_base = {}; has_ble_adv = {}; has_led_brightness = {}",
            has_host_base, has_ble_adv, has_led_brightness
        )
        .to_owned(),
    });

    host::set_led_brightness(&LEDBrightnessSettings {
        rgb: [255, 255, 255],
    });

    host::configure_ble_adv(&BLEAdvSettings {
        min_interval: 1024,
        max_interval: 2048,
    });

    host::configure_ble_data(&BLEAdvData {
        data: vec![0x00, 0x00, 0xca, 0x7e, 0xa2, 1],
    });

    let s: &Mutex<_> = Box::leak(Box::new(Mutex::new(CycleState::new())));

    host::set_on_ble_adv_recv_callback(Some(move |info: BLEAdvNotification| {
        let pl = &info.data;
        if pl.len() == 4 && pl[0] == 0x0ca && pl[1] == 0x7e && pl[2] == 0xa2 {
            host::host_log(&Log {
                level: LogLevel::Debug,
                message: format!("received rudelblinken adv: {:?}", info).to_owned(),
            });
            if let Ok(mut s) = s.try_lock() {
                s.off_cnt += 1;
                // double cast for sign extension
                s.off_sum += pl[3].wrapping_sub(s.progress) as i8 as i32;
            } else {
                host::host_log(&Log {
                    level: LogLevel::Info,
                    message: "failed to lock cycle state for callback".to_owned(),
                });
            }
        }
    }));

    let mut led_state = true;

    loop {
        host::rt_yield(500);

        let t = host::get_time_millis();
        let prog = {
            if let Ok(mut s) = s.try_lock() {
                s.update_progress(t);
                s.progress
            } else {
                host::host_log(&Log {
                    level: LogLevel::Info,
                    message: "failed to lock cycle state for update".to_owned(),
                });
                continue;
            }
        };

        host::configure_ble_data(&BLEAdvData {
            data: vec![0x00, 0x00, 0xca, 0x7e, 0xa2, prog],
        });

        if (192 < prog) != led_state {
            led_state = !led_state;
            host::set_led_brightness(&LEDBrightnessSettings {
                rgb: [if led_state { 255 } else { 0 }; 3],
            });
        }
    }
}
