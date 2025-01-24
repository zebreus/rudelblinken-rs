use crate::{
    ble_abstraction::DocumentableCharacteristic,
    storage::{get_filesystem, CreateStorageError, FlashStorage},
};
use esp32_nimble::{
    utilities::{mutex::Mutex, BleUuid},
    BLE2904Format, BLEServer, BLEService, NimbleProperties,
};
use esp_idf_sys::{self as _, ble_svc_gatt_changed, BLE_GATT_CHR_UNIT_UNITLESS};
use itertools::Itertools;
use rudelblinken_filesystem::{
    file::{File as FileContent, FileState, UpgradeFileError},
    Filesystem,
};
use std::{
    io::{Seek, Write},
    sync::Arc,
};
use thiserror::Error;
use upload_request::UploadRequest;
use zerocopy::TryFromBytes;
mod upload_request;

const FILE_UPLOAD_SERVICE: u16 = 0x9160;
// Write data chunks here
const FILE_UPLOAD_SERVICE_DATA: u16 = 0x9161;
// Write metadata here to initiate an upload.
const FILE_UPLOAD_SERVICE_START_UPLOAD: u16 = 0x9162;
// Read this to get the number of uploaded chunks and the IDs of some missing chunks. Returns a list of u16
const FILE_UPLOAD_SERVICE_UPLOAD_PROGRESS: u16 = 0x9163;
// Read here to get the last error as a string
const FILE_UPLOAD_SERVICE_LAST_ERROR: u16 = 0x9164;
// Read to get the hash of the current upload.
const FILE_UPLOAD_SERVICE_CURRENT_HASH: u16 = 0x9166;

const FILE_UPLOAD_SERVICE_UUID: BleUuid = BleUuid::from_uuid16(FILE_UPLOAD_SERVICE);
const FILE_UPLOAD_SERVICE_DATA_UUID: BleUuid = BleUuid::from_uuid16(FILE_UPLOAD_SERVICE_DATA);
const FILE_UPLOAD_SERVICE_START_UPLOAD_UUID: BleUuid =
    BleUuid::from_uuid16(FILE_UPLOAD_SERVICE_START_UPLOAD);
const FILE_UPLOAD_SERVICE_MISSING_CHUNKS_UUID: BleUuid =
    BleUuid::from_uuid16(FILE_UPLOAD_SERVICE_UPLOAD_PROGRESS);
const FILE_UPLOAD_SERVICE_LAST_ERROR_UUID: BleUuid =
    BleUuid::from_uuid16(FILE_UPLOAD_SERVICE_LAST_ERROR);
const FILE_UPLOAD_SERVICE_CURRENT_HASH_UUID: BleUuid =
    BleUuid::from_uuid16(FILE_UPLOAD_SERVICE_CURRENT_HASH);

// #[derive(Clone, Debug)]
// pub struct File {
//     hash: [u8; 32],
//     // TODO: Fix the filename story
//     #[allow(dead_code)]
//     name: String,
//     pub content: FileContent<FlashStorage, { FileState::Weak }>,
// }

#[derive(Debug)]
struct IncompleteFile {
    incomplete_file: FileContent<FlashStorage, { FileState::Writer }>,
    checksums: Vec<u8>,
    received_chunks: Vec<bool>,
    chunk_length: u16,
    length: u32,
    name: String,
    hash: [u8; 32],
}

#[derive(Error, Debug, Clone)]
pub enum ReceiveChunkError {
    #[error("Chunk has an invalid length")]
    InvalidLength,
    #[error("Chunk has the wrong checksum")]
    WrongChecksum,
}

#[derive(Error, Debug, Clone)]
pub enum VerifyFileError {
    #[error("File is not complete")]
    NotComplete,
    #[error("Hashes do not match")]
    HashMismatch,
}

impl IncompleteFile {
    pub fn new(
        hash: [u8; 32],
        checksums: Vec<u8>,
        chunk_length: u16,
        length: u32,
        writer: FileContent<FlashStorage, { FileState::Writer }>,
        name: String,
    ) -> Self {
        Self {
            incomplete_file: writer,
            received_chunks: vec![false; checksums.len()],
            checksums,
            chunk_length,
            length,
            name,
            hash,
        }
    }

    pub fn receive_chunk(&mut self, data: &[u8], index: u16) -> Result<(), ReceiveChunkError> {
        // Verify length for all but the last chunk
        if (index as usize != self.checksums.len() - 1)
            && (data.len() != self.chunk_length as usize)
        {
            return Err(ReceiveChunkError::InvalidLength);
        }
        // Verify length for the last chunk
        if (index as usize == self.checksums.len() - 1)
            && (data.len() != (self.length as usize % self.chunk_length as usize))
        {
            return Err(ReceiveChunkError::InvalidLength);
        }

        // TODO: Find out if generating a new crc8 generator costs anything
        let crc8_generator = crc::Crc::<u8>::new(&crc::CRC_8_LTE);
        let checksum = crc8_generator.checksum(data);

        if self.checksums[index as usize] != checksum {
            ::tracing::error!(target: "file-upload", "Received chunk with invalid checksum");
            return Err(ReceiveChunkError::WrongChecksum);
        }

        let offset = self.chunk_length as usize * index as usize;
        self.incomplete_file
            .seek(std::io::SeekFrom::Start(offset as u64))
            .unwrap();
        self.incomplete_file.write(data).unwrap();
        // self.incomplete_file.content[offset..(data.len() + offset)].copy_from_slice(data);
        self.received_chunks[index as usize] = true;

        Ok(())
    }
    /// Get all chunks that have not yet been received
    pub fn get_missing_chunks(&self) -> Vec<u16> {
        self.received_chunks
            .iter()
            .enumerate()
            .filter(|(_, received)| received == &&false)
            .map(|(index, _)| index as u16)
            .collect_vec()
    }
    pub fn chunk_count(&self) -> u16 {
        self.received_chunks.len() as u16
    }
    /// Check if the file is complete
    pub fn is_complete(&self) -> bool {
        self.received_chunks.iter().all(|received| *received)
    }
    /// Verify that the received file is complete and has the correct hash
    pub fn verify_hash(
        self,
        filesystem: &Filesystem<FlashStorage>,
    ) -> Result<FileContent<FlashStorage, { FileState::Weak }>, VerifyFileError> {
        if !self.is_complete() {
            return Err(VerifyFileError::NotComplete);
        }
        self.incomplete_file.commit().unwrap();
        let file = filesystem.read_file(&self.name).unwrap();
        let mut hasher = blake3::Hasher::new();
        hasher.update(file.upgrade().unwrap().as_ref());

        // TODO: I am sure there is a better way to convert this into an array but I didnt find it after 10 minutes.
        let mut hash: [u8; 32] = [0; 32];
        hash.copy_from_slice(hasher.finalize().as_bytes());

        if hash != self.hash {
            ::tracing::warn!(target: "file-upload", "Hashes dont match.\nExpected: {:?}\nGot     : {:?}", self.hash, hash);
            return Err(VerifyFileError::HashMismatch);
        }
        ::tracing::info!(target: "file-upload", "Hashes match");

        Ok(file)
    }
    /// Get the uploaded file, if the upload is finished, otherwise this return None and you just destroyed your incomplete file for no reason
    pub fn into_file(
        self,
        filesystem: &Filesystem<FlashStorage>,
    ) -> Result<FileContent<FlashStorage, { FileState::Weak }>, VerifyFileError> {
        let file = self.verify_hash(filesystem)?;
        Ok(file)
    }

    pub fn get_hash(&self) -> &[u8; 32] {
        &self.hash
    }
}

#[derive(Debug)]
pub struct FileUploadService {
    currently_receiving: Option<IncompleteFile>,
    last_error: Option<FileUploadError>,
}

#[derive(Error, Debug, Clone)]
#[repr(u8)]
pub enum FileUploadError {
    #[error(transparent)]
    ReceiveChunkError(#[from] ReceiveChunkError),
    #[error(transparent)]
    VerifyFileError(#[from] VerifyFileError),
    #[error("Cannot receive chunk when no upload is active")]
    NoUploadActive,
    #[error("Received chunk is way too short")]
    ReceivedChunkWayTooShort,
    #[error("There is no checksum file with the supplied hash")]
    ChecksumFileDoesNotExist,
    #[error("Failed to decode upload request {0}")]
    MalformedUploadRequest(String),
    #[error("There was an error reading the checksums file {0}")]
    FailedToReadChecksums(UpgradeFileError),
    #[error("The checksums file does not have the expected size (Expected {expected}; Got {got}")]
    WrongNumberOfChecksums { expected: u32, got: u32 },
    #[error(transparent)]
    SetupFilesystemError(#[from] CreateStorageError),
    #[error("Failed to lock filesystem")]
    LockFilesystemError,
    #[error("Failed to create file: FilesystemWriteError: {0}")]
    FailedToCreateFile(String),
}

impl FileUploadService {
    /// Start an upload with the last received settings. Cancels a currently ongoing upload
    fn start_upload(
        &self,
        upload_request: &UploadRequest,
    ) -> Result<IncompleteFile, FileUploadError> {
        let checksums =
            self.load_checksums(&upload_request.checksums, &upload_request.chunk_count())?;

        let mut bytes = [0u8; 4];
        unsafe { esp_idf_sys::esp_fill_random(bytes.as_mut_ptr() as *mut core::ffi::c_void, 4) };
        let random_name = format!("fw-{}", u32::from_le_bytes(bytes));
        let writer = {
            let mut filesystem_writer = get_filesystem()?
                .write()
                .map_err(|_| FileUploadError::LockFilesystemError)?;
            filesystem_writer
                .get_file_writer(&random_name, upload_request.file_size, &upload_request.hash)
                .map_err(|error| FileUploadError::FailedToCreateFile(format!("{}", error)))?
        };

        Ok(IncompleteFile::new(
            upload_request.hash,
            checksums.clone(),
            upload_request.chunk_size,
            upload_request.file_size,
            writer,
            random_name,
        ))
    }

    fn log_error(&mut self, error: FileUploadError) {
        ::tracing::error!(target: "file-upload", "{}", error);
        self.last_error = Some(error);
    }

    /// Get the UUID of the file upload service
    pub const fn uuid() -> BleUuid {
        FILE_UPLOAD_SERVICE_UUID
    }

    /// This will be called on writes to the data characteristic
    ///
    /// We use this wrapper to make error handling easier
    fn data_write(
        &mut self,
        args: &mut esp32_nimble::OnWriteArgs<'_>,
    ) -> Result<(), FileUploadError> {
        let maybe_current_upload = &mut self.currently_receiving;
        let received_data = args.recv_data();

        if received_data.len() < 3 {
            ::tracing::warn!(target: "file-upload", "data length is too short {}", received_data.len());

            return Err(FileUploadError::ReceivedChunkWayTooShort);
        }

        let index = u16::from_le_bytes([received_data[0], received_data[1]]);
        let data = &received_data[2..];

        ::tracing::info!(target: "file-upload", "Received chunk #{}", index);

        let Some(current_upload) = maybe_current_upload.as_mut() else {
            // Should never happen, because we called ensure_upload above
            return Err(FileUploadError::NoUploadActive);
        };
        current_upload.receive_chunk(data, index)?;
        if current_upload.is_complete() {
            let incomplete_file = maybe_current_upload
                .take()
                .ok_or(FileUploadError::NoUploadActive)?;
            // let hash = incomplete_file.hash.clone();
            // let name = incomplete_file.name.clone();
            let _file = incomplete_file.into_file(&get_filesystem().unwrap().read().unwrap())?;
            // self.files.push(File {
            //     hash,
            //     name: name,
            //     content: file,
            // });
        }
        Ok(())
    }

    /// This will be called on writes to the hash characteristic
    ///
    /// We use this wrapper to make error handling easier
    fn request_upload(
        &mut self,
        args: &mut esp32_nimble::OnWriteArgs<'_>,
    ) -> Result<(), FileUploadError> {
        let received_data = args.recv_data();
        let upload_request = UploadRequest::try_ref_from_bytes(received_data)
            .map_err(|error| FileUploadError::MalformedUploadRequest(error.to_string()))?;

        ::tracing::info!(target: "file-upload", "Received request {:?}", upload_request);

        ::tracing::info!(target: "file-upload", "Received hash {:?}", upload_request.hash);

        let incomplete_file = self.start_upload(upload_request)?;
        self.currently_receiving = Some(incomplete_file);

        Ok(())
    }

    // /// This will be called on writes to the hash characteristic
    // ///
    // /// We use this wrapper to make error handling easier
    // fn hash_write(
    //     &mut self,
    //     args: &mut esp32_nimble::OnWriteArgs<'_>,
    // ) -> Result<(), FileUploadError> {
    //     let received_data = args.recv_data();
    //     if received_data.len() != 32 {
    //         ::tracing::info!(target: "file-upload", "hash length is too short {}", received_data.len());

    //         return Err(FileUploadError::ReceivedChunkWayTooShort);
    //     }

    //     let new_hash: [u8; 32] = received_data.try_into().unwrap();
    //     ::tracing::info!(target: "file-upload", "Received hash {:?}", new_hash);
    //     if self.latest_hash.as_ref() == Some(&new_hash) {
    //         return Ok(());
    //     }
    //     self.latest_hash = Some(new_hash);
    //     self.currently_receiving = None;
    //     Ok(())
    // }

    pub fn get_file(
        &self,
        hash: &[u8; 32],
    ) -> Option<rudelblinken_filesystem::file::File<FlashStorage, { FileState::Weak }>> {
        let filesystem = get_filesystem().unwrap();
        let filesystem_reader = filesystem.read().unwrap();
        filesystem_reader.read_file_by_hash(hash)
    }

    /// This will be called on writes to the checksum characteristic
    ///
    /// We use this wrapper to make error handling easier
    fn load_checksums(
        &self,
        checksums: &[u8; 32],
        chunk_count: &u32,
    ) -> Result<Vec<u8>, FileUploadError> {
        if chunk_count <= &32 {
            ::tracing::info!(target: "file-upload", "Successfully loaded {} checksums from request", chunk_count);

            return Ok(checksums[0..(*chunk_count as usize)].to_vec());
        }

        let hash: &[u8; 32] = checksums.into();
        let Some(file) = self.get_file(hash) else {
            return Err(FileUploadError::ChecksumFileDoesNotExist);
        };
        let new_checksums: Vec<u8> = file
            .upgrade()
            .map_err(|error| FileUploadError::FailedToReadChecksums(error))?
            .to_vec();
        if (new_checksums.len() as u32) != *chunk_count {
            return Err(FileUploadError::WrongNumberOfChecksums {
                expected: *chunk_count,
                got: new_checksums.len() as u32,
            });
        }

        ::tracing::info!(target: "file-upload", "Successfully loaded {} checksums from file", new_checksums.len());

        return Ok(new_checksums);
    }

    pub fn new(server: &mut BLEServer) -> Arc<Mutex<FileUploadService>> {
        let file_upload_service = Arc::new(Mutex::new(FileUploadService {
            currently_receiving: None,
            last_error: None,
        }));

        let service = server.create_service(FILE_UPLOAD_SERVICE_UUID);

        // Write a upload request to start a new upload.
        // Read to get the hash of the current upload.
        let upload_request_characteristic = service.lock().create_characteristic(
            FILE_UPLOAD_SERVICE_START_UPLOAD_UUID,
            NimbleProperties::READ | NimbleProperties::WRITE,
        );
        upload_request_characteristic.document(
            "File Upload Request",
            BLE2904Format::OPAQUE,
            0,
            BLE_GATT_CHR_UNIT_UNITLESS,
        );

        let current_hash_characteristic = service.lock().create_characteristic(
            FILE_UPLOAD_SERVICE_CURRENT_HASH_UUID,
            NimbleProperties::READ,
        );
        current_hash_characteristic.document(
            "Hash of the current upload",
            BLE2904Format::OPAQUE,
            0,
            BLE_GATT_CHR_UNIT_UNITLESS,
        );

        let upload_status_characteristic = service.lock().create_characteristic(
            FILE_UPLOAD_SERVICE_MISSING_CHUNKS_UUID,
            NimbleProperties::READ,
        );
        upload_status_characteristic.document(
            "Number of received chunks + Missing Chunks",
            BLE2904Format::OPAQUE,
            0,
            BLE_GATT_CHR_UNIT_UNITLESS,
        );

        let last_error_characteristic = service
            .lock()
            .create_characteristic(FILE_UPLOAD_SERVICE_LAST_ERROR_UUID, NimbleProperties::READ);
        last_error_characteristic.document(
            "Last error code",
            BLE2904Format::UINT16,
            0,
            BLE_GATT_CHR_UNIT_UNITLESS,
        );

        let file_upload_service_clone = file_upload_service.clone();
        upload_request_characteristic.lock().on_write(move |args| {
            println!("Writing upload request");
            let mut service = file_upload_service_clone.lock();
            if let Err(e) = service.request_upload(args) {
                service.log_error(e);
            }
            unsafe {
                ble_svc_gatt_changed(FILE_UPLOAD_SERVICE_DATA, FILE_UPLOAD_SERVICE_DATA);
            };
        });
        let file_upload_service_clone = file_upload_service.clone();
        current_hash_characteristic.lock().on_read(move |value, _| {
            println!("Read current hash");
            let service = file_upload_service_clone.lock();
            let current_hash = match &service.currently_receiving {
                Some(currently_receiving) => currently_receiving.get_hash(),
                None => &[0u8; 32],
            };
            value.set_value(current_hash);
        });

        let file_upload_service_clone = file_upload_service.clone();
        upload_status_characteristic
            .lock()
            .on_read(move |value, _| {
                println!("Reading missing chunks");
                let service = file_upload_service_clone.lock();
                let maybe_currently_receiving = service.currently_receiving.as_ref();
                let missing_chunks = maybe_currently_receiving
                    .map(|incomplete_file| incomplete_file.get_missing_chunks())
                    .unwrap_or(Default::default());

                let chunk_count = maybe_currently_receiving
                    .map(|incomplete_file| incomplete_file.chunk_count())
                    .unwrap_or(0);
                let progress = chunk_count - missing_chunks.len() as u16;

                let mut upload_status: Vec<u8> = Vec::new();
                upload_status.extend_from_slice(&progress.to_le_bytes());
                upload_status.extend(
                    missing_chunks
                        .into_iter()
                        .take(100)
                        .flat_map(u16::to_le_bytes),
                );

                value.set_value(&upload_status);
            });

        let file_upload_service_clone = file_upload_service.clone();
        last_error_characteristic.lock().on_read(move |value, _| {
            let service = file_upload_service_clone.lock();
            let Some(last_error) = &service.last_error else {
                value.set_value(&[]);
                return;
            };

            value.set_value(&(unsafe { *<*const _>::from(last_error).cast::<u8>() }).to_le_bytes());
        });

        file_upload_service
    }
}

fn setup_data_characteristic(
    upload_service: Arc<Mutex<FileUploadService>>,
    ble_service: &mut BLEService,
) {
    let data_characteristic = ble_service.create_characteristic(
        FILE_UPLOAD_SERVICE_DATA_UUID,
        NimbleProperties::WRITE_NO_RSP | NimbleProperties::WRITE,
    );
    data_characteristic.document(
        "Chunk Upload",
        BLE2904Format::OPAQUE,
        0,
        BLE_GATT_CHR_UNIT_UNITLESS,
    );

    data_characteristic.lock().on_write(move |args| {
        let mut service = upload_service.lock();
        if let Err(e) = service.data_write(args) {
            service.log_error(e);
        }
    });
}
