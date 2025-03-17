//! Test wasm files on an emulated rudelblinken device.
mod emulated_host;
use clap::Args;
use emulated_host::{EmulatedHost, HostEvent};
use rudelblinken_runtime::host::{Advertisement, BleEvent, ManufacturerData};
use std::{
    ffi::OsStr,
    path::PathBuf,
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::{
    fs::{create_dir_all, read, read_dir, remove_file},
    net::UnixDatagram,
    time::interval,
};
use zerocopy::{Immutable, IntoBytes, KnownLayout, TryFromBytes};

#[derive(Error, Debug)]
pub enum EmulatorError {
    #[error("Failed to read the WASM source file")]
    FailedToReadWasmFile(#[from] std::io::Error),
    #[error("The name needs to be at least 3 characters long")]
    NameTooShort(),
    #[error("The name can be at most 16 bytes long")]
    NameTooLong(),
    #[error("The name can only contain [-_a-zA-Z0-9]")]
    InvalidCharacters(),
    #[error(transparent)]
    RuntimeError(#[from] rudelblinken_runtime::Error),
}

#[derive(Args, Debug)]
pub struct EmulateCommand {
    /// WASM file to run
    file: PathBuf,

    /// Name of the instance
    #[arg(short, long)]
    name: Option<String>,
}

pub struct Emulator {
    wasm: Vec<u8>,
    name: String,
    address: [u8; 6],
    socket: UnixDatagram,
    socket_dir: PathBuf,
}

/// Generate a random 6 byte mac address
fn random_mac() -> [u8; 6] {
    use rand::distributions::Standard;
    use rand::Rng;
    let mut rng = rand::thread_rng();
    rng.sample(Standard)
}

/// Generate a name from a mac address
fn mac_to_name(mac: &[u8; 6]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

#[repr(C)]
#[derive(IntoBytes, TryFromBytes, Clone, Copy, KnownLayout, Immutable)]
pub enum DataType {
    Advertisement,
}

impl Into<DataType> for u8 {
    fn into(self) -> DataType {
        match self {
            0 => DataType::Advertisement,
            _ => unreachable!(),
        }
    }
}

impl Emulator {
    pub async fn new(command: EmulateCommand) -> Result<Self, EmulatorError> {
        log::debug!("Emulating WASM file: {:?}", command.file);
        let wasm = read(&command.file).await?;

        let mac: [u8; 6] = random_mac();

        let name = match command.name {
            Some(name) => name,
            None => mac_to_name(&mac),
        };
        if name.as_bytes().len() < 3 {
            return Err(EmulatorError::NameTooShort());
        }
        if name.as_bytes().len() > 16 {
            return Err(EmulatorError::NameTooLong());
        }
        log::debug!("Using name: {}", name);
        if !name
            .chars()
            .all(|c| char::is_ascii_alphanumeric(&c) || c == '-' || c == '_')
        {
            return Err(EmulatorError::InvalidCharacters());
        }

        let tempdir = std::env::temp_dir().join("rudelblinken/emulator");
        create_dir_all(&tempdir).await?;
        log::debug!(
            "Using socket: {}",
            tempdir.join(format!("{}.socket", name)).display()
        );
        let my_socket = UnixDatagram::bind(tempdir.join(format!("{}.socket", name)))?;

        Ok(Self {
            wasm,
            name,
            address: mac,
            socket: my_socket,
            socket_dir: tempdir,
        })
    }

    pub async fn broadcast(&self, data: &[u8]) -> Result<(), EmulatorError> {
        let mut sockets = read_dir(&self.socket_dir).await?;
        let mut other_sockets: Vec<PathBuf> = Vec::new();
        while let Some(socket) = sockets.next_entry().await? {
            if socket.path().extension() != Some("socket".as_ref()) {
                continue;
            }
            if socket.path().file_stem() == Some(&OsStr::new(self.name.as_str())) {
                continue;
            }
            other_sockets.push(socket.path());
        }
        // println!("Found {} sockets", other_sockets.len());
        let futures = other_sockets
            .into_iter()
            .map(|socket_name| async {
                let other_socket = UnixDatagram::unbound()?;
                match other_socket.send_to(data, socket_name.clone()).await {
                    Ok(_) => {
                        // println!("Sent data to {}", socket.display());
                    }
                    Err(err) => {
                        log::info!("Failed to send data to {}: {}", socket_name.display(), err);
                        remove_file(socket_name).await?;
                    }
                }
                Ok(()) as Result<(), EmulatorError>
            })
            .collect::<Vec<_>>();

        futures::future::try_join_all(futures).await?;
        Ok(())
    }

    pub async fn emulate(&self) -> Result<(), EmulatorError> {
        let (sender, mut receiver, host) = EmulatedHost::new(self.address, self.name.clone());
        let mut instance = rudelblinken_runtime::linker::setup(&self.wasm, host)?;
        let start_time = Instant::now();
        let mut advertisment_data: Vec<u8> = Vec::new();

        std::thread::spawn(move || {
            instance.run().unwrap();
        });

        let mut advertisement_interval = interval(Duration::from_millis(150));

        loop {
            let mut buffer: Vec<u8> = Vec::new();
            let ble_event = self.socket.recv_buf(&mut buffer);
            let wasm_event = receiver.recv();
            let timer_event = advertisement_interval.tick();

            tokio::select! {
                _ = ble_event => {
                    let (data_type, content) = buffer.split_at(1);
                    let data_type: DataType = data_type[0].into();

                    match data_type {
                        DataType::Advertisement => {
                            let Ok(ble_event) = serde_json::from_slice::<BleEvent>(content) else {
                                // Failed to parse the advertisement
                                  break;
                            };
                            let ble_event = match ble_event {
                                BleEvent::Advertisement(mut adv) => {
                                    adv.received_at = start_time.elapsed().as_micros() as u64;
                                    BleEvent::Advertisement(adv)}
                            };

                            sender
                                .send(HostEvent::BleEvent(ble_event))
                                .await
                                .unwrap();
                        }
                    }
                }
                val = wasm_event => {
                    let val = val.unwrap();
                    match val {
                        emulated_host::WasmEvent::SetAdvertismentSettings( settings) => {
                            advertisement_interval = interval(Duration::from_millis(settings.max_interval as u64));
                        },
                        emulated_host::WasmEvent::SetAdvertismentData(data) => {
                            advertisment_data = data;
                        },
                    }
                }
                _val = timer_event => {
                    let mut data_packet = Vec::new();
                    data_packet.extend_from_slice(&DataType::Advertisement.as_bytes()[..1]);


                    // let mut advertisment_data_array = [0u8; 32];
                    // let advertisment_data_length = std::cmp::min(32, advertisment_data.len());
                    // advertisment_data_array[0..advertisment_data_length]
                    //     .copy_from_slice(&advertisment_data[0..advertisment_data_length]);
                    let ble_event = BleEvent::Advertisement(Advertisement {
                        address:u64::from_le_bytes([
                            self.address[0],
                            self.address[1],
                            self.address[2],
                            self.address[3],
                            self.address[4],
                            self.address[5],
                            0,
                            0
                        ]),
                        received_at: 0,
                        manufacturer_data: Some(ManufacturerData{
                            manufacturer_id: 0,
                            data: advertisment_data.clone()
                        }),
                        service_data: Vec::new()
                    });
                    let event_slice = serde_json::to_vec(&ble_event).unwrap();
                    data_packet.extend_from_slice(&event_slice);

                    self.broadcast(&data_packet).await.unwrap();
                }
            }
        }

        Ok(())
    }
}
