pub mod btree;

use std::error::Error;
use std::ops::RangeInclusive;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyDirEntry {
    /// data file id that stores key value pair.
    pub(crate) segment_id: u64,
    /// data entry offset in data file.
    pub(crate) offset: u64,
    /// data entry size.
    pub(crate) size: u64,
}

impl KeyDirEntry {
    pub fn new(segment_id: u64, offset: u64, size: u64) -> Self {
        KeyDirEntry {
            segment_id,
            offset,
            size,
        }
    }
}
pub trait Indexer:Sync + Send
{   
    //  put 方法将key值和对应的 KeyDirEntry 存储到索引中。
    fn put(&self, key: &[u8], position: KeyDirEntry) -> Option<KeyDirEntry>;
    fn get(&self, key: &[u8]) -> Option<KeyDirEntry>;
    fn delete(&self, key: &[u8]) -> Option<KeyDirEntry>;
    fn size(&self) -> usize;
    fn ascend<F>(&self, handle_fn: F) -> Result<(), Box<dyn Error>> where F: FnMut(&[u8], &KeyDirEntry) -> Result<bool, Box<dyn Error>>;
    fn ascend_range<F>(&self, range: RangeInclusive<&[u8]>, handle_fn: F) -> Result<(), Box<dyn Error>> where F: FnMut(&[u8], &KeyDirEntry) -> Result<bool, Box<dyn Error>>;
    fn ascend_greater_or_equal<F>(&self, key: &[u8], handle_fn: F) -> Result<(), Box<dyn Error>> where F: FnMut(&[u8], &KeyDirEntry) -> Result<bool, Box<dyn Error>>;
    fn descend<F>(&self, handle_fn: F) -> Result<(), Box<dyn Error>> where F: FnMut(&[u8], &KeyDirEntry) -> Result<bool, Box<dyn Error>>;
    fn descend_range<F>(&self, range: RangeInclusive<&[u8]>, handle_fn: F) -> Result<(), Box<dyn Error>> where F: FnMut(&[u8], &KeyDirEntry) -> Result<bool, Box<dyn Error>>;
    fn descend_less_or_equal<F>(&self, key: &[u8], handle_fn: F) -> Result<(), Box<dyn Error>>where F: FnMut(&[u8], &KeyDirEntry) -> Result<bool, Box<dyn Error>>;
}