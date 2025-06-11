//! Maintain data files.
use crate::error::{Result, Error};
use serde::{Deserialize, Serialize};

use log::{error, trace};
use std::{fmt, io};
use std::io::{copy, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use crate::Error::IO;
use crate::options::Options;
use crate::record::reader::Reader;
use crate::record::writer;
use crate::record::writer::Writer;
use crate::storage::mem::MemStorage;
use crate::storage::{File, Storage};
use crate::store::filename::parse_filename;
use crate::utils::crc32::hash;

/// 内部数据条目的定义
#[derive(Serialize, Deserialize, Debug)]
struct InnerEntry {
    key: Vec<u8>,
    value: Vec<u8>,
    // crc32 checksum
    checksum: u32,
}

impl InnerEntry {
    /// 接收键和值的引用 (&[u8])，并计算并设置校验和。
    fn new(key: &[u8], value: &[u8]) -> Self {
        let mut ent = InnerEntry {
            key: key.into(),
            value: value.into(),
            checksum: 0,
        };
        ent.checksum = ent.fresh_checksum();
        ent
    }
    // 计算当前值的校验和
    fn fresh_checksum(&self) -> u32 {
        hash(&self.value)
    }

    /// 检查当前条目的校验和是否有效
    fn is_valid(&self) -> bool {
        self.checksum == self.fresh_checksum()
    }
}

impl fmt::Display for InnerEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "DataInnerEntry(key='{}', checksum={})",
            String::from_utf8_lossy(self.key.as_ref()),
            self.checksum,
        )
    }
}

#[derive(Debug)]
pub(crate) struct DataEntry {
    inner: InnerEntry,
    // size of inner entry in data file.
    pub size: u64,
    // position of inner entry in data file.
    pub offset: u64,
    // related data file id.(SegmentId)
    pub file_id: u64,
    // BatchId uint64
    // Expire  int64
}

impl DataEntry {
    /// Create a new entry instance with size and offset.
    fn new(file_id: u64, inner: InnerEntry, size: u64, offset: u64) -> Self {
        Self {
            inner,
            size,
            offset,
            file_id,
        }
    }

    /// Check the inner data entry is corrupted or not.
    pub(crate) fn is_valid(&self) -> bool {
        self.inner.is_valid()
    }

    /// Return key of the inner entry.
    pub(crate) fn key(&self) -> &[u8] {
        &self.inner.key
    }

    /// Return value of the inner entry.
    pub(crate) fn value(&self) -> &[u8] {
        &self.inner.value
    }
}

impl fmt::Display for DataEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "DataEntry(file_id={}, key='{}', offset={}, size={})",
            self.file_id,
            String::from_utf8_lossy(self.key().as_ref()),
            self.offset,
            self.size,
        )
    }
}

/// DataFile represents a data file.
pub(crate) struct DataFile<S: Storage<F=F> + Clone,F:File> {
    options: Arc<Options>,

    pub path: PathBuf,
    /// Data file id (12 digital characters).
    pub id: u64,
    /// File handle of data file for writting.
    pub writer: Option<Writer<F>>,
    storage: S,
    /// 数据文件的大小
    pub size: u64,
}
// 读写datafile
impl<S: Storage<F=F> + Clone,F:File> DataFile<S,F> {
    /// 创建一个新的数据文件实例。
    /// 它从文件路径中解析数据id，其中包含一个可选的
    /// writer（仅适用于可写段文件）和reader。
    pub(crate) fn new(path: &Path,storage: &S, writeable: bool) -> Self {
        // let file_id = parse_file_id(path).expect("file id not found in file path");
        // todo 获取segment的文件id
        match storage.open(path) {
            Ok(f) => {
                let w =if writeable{Some(Writer::new(f))}else { None };
                Self {
                    path: PathBuf::from(path),
                    id: 0,
                    writer: w,
                    options: Arc::new(Options::default()),
                    size: 0,
                    storage: storage.clone(),
                }
            }
            Err(_) => {
                panic!("file not found in file path")
            }
        }
    }

    /// Save key-value pair to segement file.
    pub(crate) fn write(&mut self, key: &[u8], value: &[u8]) -> Result<DataEntry> {
        let inner = InnerEntry::new(key, value);
        trace!("append {} to segement file {}", &inner, self.path.display());
        // avoid immutable borrowing issue.
        let path = self.path.as_path();
        let encoded = bincode::serialize(&inner).unwrap();
        let mut writer= match self.writer.as_mut() {
            None => {
                return Err(Error::Customized("data file is not writeable".to_owned()))
            }
            Some(w) => {
                w
            }
        };
        let offset = writer.offset().unwrap();
        writer.add_record(encoded.as_slice()).expect("add record error");
        writer.sync().expect("sync error");
        self.size = offset + encoded.len() as u64;
        let entry = DataEntry::new(self.id, inner, encoded.len() as u64, offset);
        trace!(
            "successfully append {} to data file {}",
            &entry,
            self.path.display()
        );

        Ok(entry)
    }

    /// Read key value in data file.
    pub(crate) fn read(&self, offset: u64) -> Result<DataEntry> {
        trace!(
            "read key value with offset {} in data file {}",
            offset,
            self.path.display()
        );
        let f = self.storage.open(&self.path).unwrap();
        // Note: we have to get a mutable reader here.
        let mut reader = Reader::new(f, None, true, offset);
        let mut buf = vec![];
        reader.read_record(&mut buf);
        let inner: InnerEntry = bincode::deserialize(buf.as_slice()).unwrap();
        let entry = DataEntry::new(self.id, inner, buf.len() as u64, offset);
        trace!(
            "successfully read {} from data log file {}",
            &entry,
            self.path.display()
        );
        Ok(entry)
    }

    // Copy `size` bytes from `src` data file.
    // Return offset of the newly written entry
    // 返回新写入的entry的offset
    pub(crate) fn copy_bytes_from(
        &mut self,
        src: &mut DataFile<S, F>,
        offset: u64,
        size: u64,
    ) -> Result<u64> {
        // 从 src 的 datafile 中复制到此 datafile 中
        // 获取 src 的 reader
        let mut src_file = self.storage.open(&src.path).unwrap();
        let mut dest_file = self.storage.open(&self.path).unwrap();
        let mut total_bytes_copied = 0;
        let mut buffer = [0u8; 1024 * 8]; // 8 KB 缓冲区
        src_file.seek(SeekFrom::Start(offset))?;
        let w = self.writer.as_mut().expect("data file is not writeable");
        let offset =w.offset().unwrap();
        while total_bytes_copied < size {
            let bytes_to_read = ((size - total_bytes_copied) as usize).min(buffer.len());
            let bytes_read = src_file.read(&mut buffer[..bytes_to_read])?;
            if bytes_read == 0 {
                break;
            }
            let bytes_written = dest_file.write(&buffer[..bytes_read])?;
            if bytes_written != bytes_read {
                return Err(Error::IO(io::Error::new(io::ErrorKind::WriteZero, "failed to write all bytes")));
            }
            total_bytes_copied += bytes_written as u64;
        }
        dest_file.flush()?;
        if total_bytes_copied != size {
            return Err(Error::IO(io::Error::new(io::ErrorKind::UnexpectedEof, "did not copy expected number of bytes")));
        }
        self.size += total_bytes_copied;
        Ok(offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_entry() {
        let ent = InnerEntry::new(&b"key".to_vec(), &b"value".to_vec());
        assert_eq!(ent.checksum, 494360628);
    }

    #[test]
    fn test_checksum_valid() {
        let ent = InnerEntry::new(&b"key".to_vec(), &b"value".to_vec());
        assert_eq!(ent.is_valid(), true);
    }

    #[test]
    fn test_checksum_invalid() {
        let mut ent = InnerEntry::new(&b"key".to_vec(), &b"value".to_vec());
        ent.value = b"value_changed".to_vec();
        assert_eq!(ent.is_valid(), false);
    }
    #[test]
    fn test_data_file(){
        let mut s = MemStorage::default();
        s.create("/test").expect("TODO: panic message");
        let mut file = DataFile::new(Path::new("/test"), &s,true);
        let result = file.write("hello".as_bytes(), "world".as_bytes()).unwrap();
        let result = file.write("hello1".as_bytes(), "world".as_bytes()).unwrap();
        let result = file.write("hello2".as_bytes(), "world".as_bytes()).unwrap();
        let result = file.write("hello3".as_bytes(), "world".as_bytes()).unwrap();
        let result = file.write("hello4".as_bytes(), "world".as_bytes()).unwrap();
        println!("{}", result);
        let result1 = file.read(result.offset).unwrap();
        println!("{}", result1);
    }
    #[test]
    fn test_data_file_copy(){
        let mut s = MemStorage::default();
        s.create("/test").expect("TODO: panic message");
        s.create("/test2").expect("TODO: panic message");
        let mut file = DataFile::new(Path::new("/test"),  &s,true);
        let result = file.write("hello".as_bytes(), "world".as_bytes()).unwrap();
        let result2 = file.write("hello3".as_bytes(), "world".as_bytes()).unwrap();
        let result3 = file.write("hello4".as_bytes(), "world".as_bytes()).unwrap();
        let result4 = file.write("hello1".as_bytes(), "world".as_bytes()).unwrap();
        let result1 = file.read(result.offset).unwrap();
        let mut file2 = DataFile::new(Path::new("/test2"),  &s,true);
        let result = file2.write("hello".as_bytes(), "world".as_bytes()).unwrap();
        let result = file2.write("hello1".as_bytes(), "world".as_bytes()).unwrap();
        let result3 = file2.copy_bytes_from(&mut file, result2.offset,result4.offset-result2.offset);
        // file2.read()
        println!("{:?}", result3);
        let result5 = file2.read(result3.unwrap());
        let entry = result5.unwrap();
        println!("{}",entry);
        let result6 = file2.read(entry.offset+entry.size);
        println!("{}", result6.unwrap());
    }
}