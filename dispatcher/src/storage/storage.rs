use std::ops::Bound::{Excluded, Included, Unbounded};
use std::ops::{Bound, RangeBounds};

use async_trait::async_trait;
use bytes::Bytes;

#[async_trait]
pub trait Storage: StorageRead {
    // apply needs to write the given batch atomically
    async fn apply(&self, batch: Vec<Record>) -> StorageResult<WriteResult>;
}

#[async_trait]
pub trait StorageRead: Send + Sync {
    /// Retrieves a single record by key
    ///
    /// Returns `Ok(None)` if the key does not exist
    async fn get(&self, key: Bytes) -> StorageResult<Option<Record>>;

    /// Retrieves an iterator for a range of keys
    ///
    /// Ok(Iterator) means that nothing went wrong trying to collect the records from storage
    async fn scan_iter(&self, range: BytesRange) -> StorageResult<Box<dyn StorageIterator + Send>>;
}

/// A range over byte sequences, used for key range queries.
#[derive(Clone, Debug)]
pub struct BytesRange {
    pub start: Bound<Bytes>,
    pub end: Bound<Bytes>,
}

impl BytesRange {
    pub fn new(start: Bound<Bytes>, end: Bound<Bytes>) -> Self {
        Self { start, end }
    }

    pub fn contains(&self, k: &[u8]) -> bool {
        (match &self.start {
            Included(s) => k >= s,
            Excluded(s) => k > s,
            Unbounded => true,
        }) && (match &self.end {
            Included(e) => k <= e,
            Excluded(e) => k < e,
            Unbounded => true,
        })
    }

    /// Creates a range that scans everything.
    pub fn unbounded() -> Self {
        Self {
            start: Unbounded,
            end: Unbounded,
        }
    }
}

impl RangeBounds<Bytes> for BytesRange {
    fn start_bound(&self) -> Bound<&Bytes> {
        self.start.as_ref()
    }
    fn end_bound(&self) -> Bound<&Bytes> {
        self.end.as_ref()
    }
}

/// Iterator over storage records.
#[async_trait]
pub trait StorageIterator {
    /// Returns the next record from this iterator.
    ///
    /// Returns `Ok(None)` when the iterator is exhausted.
    async fn next(&mut self) -> StorageResult<Option<Record>>;
}

/// Result type alias for storage operations
pub type StorageResult<T> = std::result::Result<T, StorageError>;

/// Result of a write operation, containing the sequence number assigned by the storage engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteResult {
    /// The sequence number assigned to this write by the underlying storage engine.
    pub seqnum: u64,
}

#[derive(Clone, Debug)]
pub struct Record {
    pub key: Bytes,
    pub value: Bytes,
}

impl Record {
    pub fn new(key: Bytes, value: Bytes) -> Self {
        Self { key, value }
    }

    pub fn empty(key: Bytes) -> Self {
        Self::new(key, Bytes::new())
    }
}

/// Error type for storage operations
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// Storage-related errors
    Storage(String),
    /// Internal errors
    Internal(String),
}

impl std::error::Error for StorageError {}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            StorageError::Storage(msg) => write!(f, "Storage error: {}", msg),
            StorageError::Internal(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl StorageError {
    /// Converts a storage error to StorageError::Storage.
    pub fn from_storage(e: impl std::fmt::Display) -> Self {
        StorageError::Storage(e.to_string())
    }
}
