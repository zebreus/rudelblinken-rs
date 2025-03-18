use esp32_nimble::utilities::mutex::Mutex;
use esp_idf_hal::{
    adc::{
        self,
        oneshot::{config::AdcChannelConfig, AdcChannelDriver, AdcDriver},
    },
    gpio::{self},
    ledc::{self, config::TimerConfig, LedcDriver, LedcTimerDriver},
    units::FromValueType,
};
use esp_idf_sys::adc_atten_t_ADC_ATTEN_DB_12;
use rudelblinken_runtime::{
    host::{
        self, AdvertisementSettings, AmbientLightType, BleEvent, Host, LedColor, LedInfo, LogLevel,
        VibrationSensorType, VoltageSensorType,
    },
    linker::linker::WrappedCaller,
};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

use crate::{
    config::{self, get_config, LedStripColor, WasmGuestConfig},
    create_ble_advertisement, BLE_DEVICE,
};

pub static LED_PIN: LazyLock<Mutex<LedcDriver<'static>>> = LazyLock::new(|| {
    Mutex::new(
        LedcDriver::new(
            unsafe { ledc::CHANNEL0::new() },
            LedcTimerDriver::new(
                unsafe { ledc::TIMER0::new() },
                &TimerConfig::new().frequency(25.kHz().into()),
            )
            .expect("timer init failed"),
            unsafe { gpio::Gpio8::new() },
        )
        .expect("ledc driver init failed"),
    )
});

static ADC_DRIVER: LazyLock<Arc<AdcDriver<'static, adc::ADC1>>> =
    LazyLock::new(|| Arc::new(AdcDriver::new(unsafe { adc::ADC1::new() }).unwrap()));

pub static LIGHT_SENSOR_ADC: LazyLock<
    Mutex<AdcChannelDriver<'static, gpio::Gpio3, Arc<AdcDriver<'static, adc::ADC1>>>>,
> = LazyLock::new(|| {
    let pin = AdcChannelDriver::new(
        ADC_DRIVER.clone(),
        unsafe { gpio::Gpio3::new() },
        &AdcChannelConfig {
            attenuation: adc_atten_t_ADC_ATTEN_DB_12,
            resolution: adc::Resolution::Resolution12Bit,
            calibration: true,
        },
    )
    .unwrap();
    Mutex::new(pin)
});

// pub static VIBRATION_SENSOR_ADC: LazyLock<
//     Mutex<AdcChannelDriver<'static, gpio::Gpio4, Arc<AdcDriver<'static, adc::ADC1>>>>,
// > = LazyLock::new(|| {
//     let pin = AdcChannelDriver::new(
//         ADC_DRIVER.clone(),
//         unsafe { gpio::Gpio4::new() },
//         &AdcChannelConfig {
//             attenuation: adc_atten_t_ADC_ATTEN_DB_12,
//             resolution: adc::Resolution::Resolution12Bit,
//             calibration: true,
//         },
//     )
//     .unwrap();
//     Mutex::new(pin)
// });

pub static VOLTAGE_SENSOR_ADC: LazyLock<
    Mutex<AdcChannelDriver<'static, gpio::Gpio2, Arc<AdcDriver<'static, adc::ADC1>>>>,
> = LazyLock::new(|| {
    let pin = AdcChannelDriver::new(
        ADC_DRIVER.clone(),
        unsafe { gpio::Gpio2::new() },
        &AdcChannelConfig {
            attenuation: adc_atten_t_ADC_ATTEN_DB_12,
            resolution: adc::Resolution::Resolution12Bit,
            calibration: true,
        },
    )
    .unwrap();
    Mutex::new(pin)
});

#[derive(Clone)]
pub struct WasmHostConfiguration {
    reset_fuel: u32,
}

impl Default for WasmHostConfiguration {
    fn default() -> Self {
        Self {
            reset_fuel: 999_999,
        }
    }
}

/// Events sent from the wasm host to the main system
pub enum WasmEvent {}

/// Events sent from the host to the wasm guest
pub enum HostEvent {
    /// Send whenever an advertisment was received
    BleEvent(BleEvent),
    /// The host requests the guest to shut down because the program changed
    ProgramChanged(),
}

#[derive(Clone)]
pub struct WasmHost {
    pub host_events: Arc<Mutex<Receiver<HostEvent>>>,
    // TODO: Actually use this. We build this to allow bidirectional communication between the host and the wasm guest in the emulator, but dont need that currently
    #[allow(dead_code)]
    pub wasm_events: Sender<WasmEvent>,
    config: WasmHostConfiguration,
}

impl WasmHost {
    pub fn new() -> (Sender<HostEvent>, Receiver<WasmEvent>, Self) {
        LazyLock::force(&LED_PIN);
        let (host_sender, host_receiver) = channel::<HostEvent>();
        let (wasm_sender, wasm_receiver) = channel::<WasmEvent>();
        return (
            host_sender,
            wasm_receiver,
            WasmHost {
                host_events: Arc::new(Mutex::new(host_receiver)),
                wasm_events: wasm_sender,
                config: WasmHostConfiguration::default(),
            },
        );
    }
}

impl Host for WasmHost {
    fn yield_now(
        caller: &mut WrappedCaller<'_, Self>,
        micros: u64,
    ) -> Result<u32, rudelblinken_runtime::Error> {
        let yield_until = unsafe { esp_idf_sys::esp_timer_get_time() } as u64 + micros;

        loop {
            // Sleep for 1 freeRTOS tick to force yielding
            std::thread::sleep(Duration::from_millis(1));

            loop {
                let receiver = caller.data().host_events.lock();
                let Ok(event) = receiver.try_recv() else {
                    break;
                };
                drop(receiver);
                match event {
                    HostEvent::BleEvent(advertisement) => {
                        // Make sure we have enough fuel to process the event
                        let fuel_before = caller.inner().get_fuel()?;
                        let reset_fuel = caller.data().config.reset_fuel;
                        caller.inner().set_fuel(reset_fuel as u64)?;

                        caller.on_event(advertisement)?;

                        // Restore the fuel
                        caller.inner().set_fuel(fuel_before)?;
                    }
                    HostEvent::ProgramChanged() => {
                        // TODO: Improve termination behaviour
                        return Err(rudelblinken_runtime::Error::new("Terminated as requested"));
                    }
                }
            }
            if yield_until < unsafe { esp_idf_sys::esp_timer_get_time() } as u64 {
                break;
            }
        }

        let reset_fuel = caller.data().config.reset_fuel;
        caller.inner().set_fuel(reset_fuel as u64)?;
        Ok(reset_fuel)
    }

    fn sleep(
        _caller: &mut WrappedCaller<'_, Self>,
        micros: u64,
    ) -> Result<(), rudelblinken_runtime::Error> {
        std::thread::sleep(Duration::from_micros(micros));
        Ok(())
    }

    fn time(_caller: &mut WrappedCaller<'_, Self>) -> Result<u64, rudelblinken_runtime::Error> {
        let time = unsafe { esp_idf_sys::esp_timer_get_time() };
        Ok(time as u64)
    }

    fn log(
        _caller: &mut WrappedCaller<'_, Self>,
        level: LogLevel,
        message: &str,
    ) -> Result<(), rudelblinken_runtime::Error> {
        match level {
            LogLevel::Error => ::tracing::error!(target: "wasm-guest", msg = &message),
            LogLevel::Warn => ::tracing::warn!(target: "wasm-guest",msg = &message),
            LogLevel::Info => ::tracing::info!(target: "wasm-guest",msg = &message),
            LogLevel::Debug => ::tracing::debug!(target: "wasm-guest",msg = &message),
            LogLevel::Trace => ::tracing::trace!(target: "wasm-guest",msg = &message),
        }
        Ok(())
    }

    fn get_name(
        _caller: &mut WrappedCaller<'_, Self>,
    ) -> Result<String, rudelblinken_runtime::Error> {
        let mut name = config::device_name::get().unwrap_or_default();
        let closest = name.floor_char_boundary(16);
        let name = name.split_off(closest);
        Ok(name)
    }

    fn get_config(
        _caller: &mut WrappedCaller<'_, Self>,
    ) -> Result<Vec<u8>, rudelblinken_runtime::Error> {
        Ok(get_config::<WasmGuestConfig>())
    }

    fn set_leds(
        _caller: &mut WrappedCaller<'_, Self>,
        first_id: u16,
        lux: &[u16],
    ) -> Result<u32, rudelblinken_runtime::Error> {
        if first_id == 0 && 0 < lux.len() {
            host::to_error_code(LED_PIN.lock().set_duty(lux[0] as u32), 1)
        } else {
            Ok(0)
        }
    }

    fn set_rgb(
        _caller: &mut WrappedCaller<'_, Self>,
        _color: &LedColor,
        lux: u32,
    ) -> Result<u32, rudelblinken_runtime::Error> {
        host::to_error_code(LED_PIN.lock().set_duty(lux), 1)
    }

    fn led_count(
        _caller: &mut WrappedCaller<'_, Self>,
    ) -> Result<u16, rudelblinken_runtime::Error> {
        Ok(1)
    }

    fn get_led_info(
        _caller: &mut WrappedCaller<'_, Self>,
        id: u16,
    ) -> Result<LedInfo, rudelblinken_runtime::Error> {
        if id == 0 {
            Ok(LedInfo {
                color: get_config::<LedStripColor>(),
                max_lux: LED_PIN.lock().get_max_duty() as u16,
            })
        } else {
            Ok(LedInfo {
                color: LedColor::new(0, 0, 0),
                max_lux: 0 as u16,
            })
        }
    }

    fn get_ambient_light_type(
        _caller: &mut WrappedCaller<'_, Self>,
    ) -> Result<AmbientLightType, rudelblinken_runtime::Error> {
        Ok(AmbientLightType::Basic)
    }

    fn get_ambient_light(
        _caller: &mut WrappedCaller<'_, Self>,
    ) -> Result<u32, rudelblinken_runtime::Error> {
        match LIGHT_SENSOR_ADC.lock().read() {
            Ok(v) => Ok(v as u32),
            Err(err) => {
                tracing::warn!(?err, "reading ambient light failed");
                Ok(u32::MAX)
            }
        }
    }

    fn get_vibration_sensor_type(
        _caller: &mut WrappedCaller<'_, Self>,
    ) -> Result<VibrationSensorType, rudelblinken_runtime::Error> {
        Ok(VibrationSensorType::None)
    }

    fn get_vibration(
        _caller: &mut WrappedCaller<'_, Self>,
    ) -> Result<u32, rudelblinken_runtime::Error> {
        Ok(0)
    }

    fn get_voltage_sensor_type(
        _caller: &mut WrappedCaller<'_, Self>,
    ) -> Result<VoltageSensorType, rudelblinken_runtime::Error> {
        Ok(VoltageSensorType::Basic)
    }

    fn get_voltage(
        _caller: &mut WrappedCaller<'_, Self>,
    ) -> Result<u32, rudelblinken_runtime::Error> {
        const SAMPLES: u32 = 20;
        let mut sum_of_measurements = 0u32;
        let mut number_of_measurements = 0u32;
        for _ in 0..SAMPLES {
            match VOLTAGE_SENSOR_ADC.lock().read() {
                Ok(v) => {
                    number_of_measurements += 1;
                    sum_of_measurements += v as u32;
                }
                Err(err) => {
                    tracing::warn!(?err, "reading voltage failed");
                }
            };
        }
        let average_measurement = sum_of_measurements / number_of_measurements;
        let calibrated_voltage = average_measurement * 2;

        return Ok(calibrated_voltage);
    }

    fn configure_advertisement(
        _caller: &mut WrappedCaller<'_, Self>,
        settings: AdvertisementSettings,
    ) -> Result<u32, rudelblinken_runtime::Error> {
        let min_interval = settings.min_interval.clamp(400, 1000);
        let max_interval = settings.max_interval.clamp(min_interval, 1500);

        let mut ble_advertising = BLE_DEVICE.get_advertising().lock();
        ble_advertising
            .stop()
            .map_err(|err| rudelblinken_runtime::Error::new(format!("{:?}", err)))?;
        ble_advertising
            .min_interval(min_interval)
            .max_interval(max_interval);
        ble_advertising
            .start()
            .map_err(|err| rudelblinken_runtime::Error::new(format!("{:?}", err)))?;
        Ok(0)
    }

    fn set_advertisement_data(
        _caller: &mut WrappedCaller<'_, Self>,
        data: &[u8],
    ) -> Result<u32, rudelblinken_runtime::Error> {
        let mut ble_advertising = BLE_DEVICE.get_advertising().lock();
        ble_advertising
            .stop()
            .map_err(|err| rudelblinken_runtime::Error::new(format!("{:?}", err)))?;

        let mut advertisement = create_ble_advertisement(Some(&data));
        if let Err(_) = ble_advertising.set_data(&mut advertisement) {
            return Ok(1);
        }
        ble_advertising
            .start()
            .map_err(|err| rudelblinken_runtime::Error::new(format!("{:?}", err)))?;

        Ok(0)
    }
}
