use crate::{
    file::{
        DeleteFileContentError, File, FileState, ReadFileFromStorageError, WriteFileToStorageError,
    },
    storage::Storage,
};
use std::fmt::Formatter;

/// Internal proxy for a file that tracks some metadata in memory
pub(crate) struct FileInformation<T: Storage + 'static + Send + Sync> {
    /// Starting address of the file (in flash)
    pub address: u32,
    /// Length of the files content in bytes
    pub length: u32,
    /// Name of the file
    pub name: String,
    /// Content of the file
    /// Will be None if the file has been deleted
    content: File<T, { FileState::Weak }>,
}

impl<T: Storage + 'static + Send + Sync> Clone for FileInformation<T> {
    fn clone(&self) -> Self {
        Self {
            address: self.address.clone(),
            length: self.length.clone(),
            name: self.name.clone(),
            content: self.content.clone(),
        }
    }
}

impl<T: Storage + 'static + Send + Sync> std::fmt::Debug for FileInformation<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("File")
            .field("address", &self.address)
            .field("length", &self.length)
            .field("name", &self.name)
            .field("content", &self.content)
            .finish()
    }
}

impl<T: Storage + 'static + Send + Sync> FileInformation<T> {
    /// Read a file from storage.
    ///
    /// address is an address that can be used with storage
    pub fn from_storage(
        storage: &'static T,
        address: u32,
    ) -> Result<FileInformation<T>, ReadFileFromStorageError> {
        let file_content = File::<T, { FileState::Reader }>::from_storage(storage, address)?;

        let information = FileInformation {
            address,
            length: file_content.len() as u32,
            name: file_content.name_str().into(),
            content: file_content.downgrade(),
        };

        return Ok(information);
    }

    /// Create a new file and return a writer
    pub fn to_storage(
        storage: &'static T,
        address: u32,
        length: u32,
        name: &str,
    ) -> Result<(Self, File<T, { FileState::Writer }>), WriteFileToStorageError> {
        let file_content =
            File::<T, { FileState::Writer }>::to_storage(storage, address, length, name)?;

        let information = FileInformation {
            address: address,
            length: length,
            name: name.into(),
            content: file_content.downgrade(),
        };
        return Ok((information, file_content));
    }

    /// Transition to ready by reading content from storage
    pub fn mark_for_deletion(&self) -> Result<(), DeleteFileContentError> {
        self.content.mark_for_deletion()
    }

    /// Check if the file is marked for deletion
    pub fn marked_for_deletion(&self) -> bool {
        self.content.marked_for_deletion()
    }

    /// Check if the file has been deleted
    pub fn deleted(&self) -> bool {
        self.content.deleted()
    }

    /// Check if the file is ready to be read
    pub fn valid(&self) -> bool {
        self.content.ready()
    }

    /// Read the file content
    pub fn read(&self) -> File<T, { FileState::Weak }> {
        return self.content.clone();
    }
}