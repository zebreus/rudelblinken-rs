use crate::storage::Storage;
use crate::storage::StorageError;
use enumflags2::make_bitflags;
use enumflags2::BitFlags;
use thiserror::Error;
use zerocopy::IntoBytes;
use zerocopy::TryFromBytes;
use zerocopy::{Immutable, KnownLayout};

#[derive(Error, Debug)]
pub enum ReadMetadataError {
    #[error("Failed to read metadata from flash")]
    ReadStorageError,
}

#[derive(Error, Debug)]
pub enum CreateMetadataError {
    #[error("Metadata seems invalid. This should never happen")]
    CreateMetadataError,
    #[error(transparent)]
    StorageError(#[from] StorageError),
}

#[enumflags2::bitflags]
#[repr(u16)]
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum FileFlags {
    /// All markers need to be set correctly for the memory to be a valid file
    MarkerHighA = 0b0000000000000001,
    MarkerHighB = 0b0000000000100000,
    MarkerHighC = 0b0100000000000000,
    MarkerLowA = 0b0000000000000100,
    MarkerLowB = 0b0000001000000000,
    MarkerLowC = 0b0000000000010000,
    /// If this file has been written completly
    Ready,
    /// This file is marked for deletion
    MarkedForDeletion,
    /// This file has been deleted. It contains invalid content, but its metablock may still be valid
    Deleted,
}

/// Represents a the metadata segment of a file that is memory-mapped into storage.
///
/// Read an existing metadata segment at an address with [from_storage] or place a new one with [new_from_storage]
#[derive(PartialEq, Eq, Clone, KnownLayout, IntoBytes, Immutable, TryFromBytes)]
#[repr(C)]
pub struct FileMetadata {
    /// Type of this block
    /// Access only via the supplied functions
    pub flags: BitFlags<FileFlags>,
    /// Reserved space for alignment reasons
    _reserved: [u8; 2],
    /// Length in bytes
    pub length: u32,
    /// SHA3-256 hash of the file
    pub hash: [u8; 32],
    /// Name of the file, null terminated or 16 chars
    pub name: [u8; 16],
    /// Reserved space to fill the metadata to 64 byte
    _padding: [u8; 8],
}

impl FileMetadata {
    /// Create a new file metadata object in ram
    fn new(name: &str, length: u32) -> Self {
        let mut metadata = FileMetadata {
            flags: make_bitflags!(FileFlags::{MarkerHighA | MarkerHighB | MarkerHighC}),
            _reserved: [0; 2],
            length: length,
            hash: [0; 32],
            name: [0; 16],
            _padding: [0; 8],
        };
        metadata.set_name(name);
        return metadata;
    }
    /// Assert that the marker flags have been set correctly for this file
    pub fn valid_marker(&self) -> bool {
        if !self
            .flags
            .contains(make_bitflags!(FileFlags::{MarkerHighA | MarkerHighB | MarkerHighC}))
        {
            return false;
        }
        if self
            .flags
            .intersects(make_bitflags!(FileFlags::{MarkerLowA | MarkerLowB | MarkerLowC}))
        {
            return false;
        }
        return true;
    }
    /// Convenience function to get the name as a string slice
    pub fn name_str(&self) -> &str {
        let nul_range_end = self.name.iter().position(|&c| c == b'\0').unwrap_or(16);
        return std::str::from_utf8(&self.name[0..nul_range_end]).unwrap_or_default();
    }
    /// Internal function to set the name from a string slice
    fn set_name(&mut self, name: &str) -> () {
        let name_bytes = name.as_bytes();
        let name_length = name.len().clamp(0, 16);
        self.name[0..name_length].copy_from_slice(&name_bytes[0..name_length]);
    }
    /// Set a flag of the metadata in storage
    ///
    /// Assumes that this metadata is located at `address`. Undefined behaviour if it is not or has since been deleted
    pub unsafe fn set_flag_in_storage<T: Storage>(
        &self,
        storage: &mut T,
        address: usize,
        flag: &BitFlags<FileFlags>,
    ) -> Result<(), StorageError> {
        storage.write(address, flag.as_bytes())
    }
    fn to_storage<T: Storage>(&self, storage: &mut T, address: usize) -> Result<(), StorageError> {
        storage.write(address, self.as_bytes())
    }
    /// Store the metadata to the specified storage address
    pub fn new_to_storage<'a, T: Storage>(
        storage: &'a mut T,
        address: usize,
        name: &str,
        length: u32,
    ) -> Result<&'a Self, CreateMetadataError> {
        let new_metadata = Self::new(name, length);
        let memory_mapped_metadata = storage.write_checked(address, new_metadata.as_bytes())?;
        Ok(FileMetadata::try_ref_from_bytes(memory_mapped_metadata)
            .map_err(|_| CreateMetadataError::CreateMetadataError)?)
    }
    /// Read a file metadata object from storage
    ///
    /// Returns a reference to memorymapped flash storage
    pub fn from_storage<T: Storage>(
        storage: &T,
        address: usize,
    ) -> Result<&Self, ReadMetadataError> {
        let data = storage
            .read(address, size_of::<FileMetadata>())
            .map_err(|_| ReadMetadataError::ReadStorageError)?;
        Ok(FileMetadata::try_ref_from_bytes(data)
            .map_err(|_| ReadMetadataError::ReadStorageError)?)
    }
}