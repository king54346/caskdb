use std::fmt;
use std::path::{Path, PathBuf};

use log::{error, trace};
use serde::{Deserialize, Serialize};

// Hint File 包含键的元数据信息，如键在 segment file 中的位置（文件名和偏移量）等
// 加载 Hint File 来重建内存中的索引
use crate::error::Result;
use crate::record::reader::Reader;
use crate::record::writer::Writer;
use crate::storage::{File, Storage};


#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct HintEntry {
    pub key: Vec<u8>,
    pub offset: u64,
    pub size: u64,
    pub file_id: u64, // 添加 file_id 字段
}

impl fmt::Display for HintEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "HintEntry(key='{}', offset={}, size={})",
            String::from_utf8_lossy(self.key.as_ref()),
            self.offset,
            self.size,
        )
    }
}

/// 提示文件将键值索引保留在相关数据文件中。如果提示文件存在，可以更快地重建 keydir（内存索引）
pub struct HintFile<S: Storage<F=F> + Clone,F:File> {
    pub path: PathBuf,
    pub id: u64,
    writer: Option<Writer<F>>,
    storage: S,
}

impl <S: Storage<F=F> + Clone,F:File> HintFile<S,F> {
    // 加载hint文件
    pub(crate) fn new(path: &Path,storage: &S) -> Self {
        // File name must starts with valid file id.
        // let file_id = parse_file_id(path).expect("file id not found in file path");、
        // 获取文件id，通过store中的filename
        match storage.open(path) {
            Ok(f) => {
                Self {
                    path: PathBuf::from(path),
                    id: 0,
                    writer: Some(Writer::new(f)),
                    storage: storage.clone(),
                }
            }
            Err(_) => {
                panic!("file not found in file path")
            }
        }
    }
    // 写入hint文件
    pub(crate) fn write(&mut self, key: &[u8],offset:u64,size:u64,file_id:u64) -> Result<()> {
        let entry = HintEntry {key:key.into(), offset,size, file_id };
        trace!("append {} to segement file {}", &entry, self.path.display());
        // avoid immutable borrowing issue.
        let path = self.path.as_path();
        let encoded = bincode::serialize(&entry).unwrap();
        let mut writer = self.writer.as_mut().unwrap();
        let offset = writer.offset().unwrap();
        writer.add_record(encoded.as_slice()).expect("add record error");
        writer.sync().expect("sync error");
        trace!(
            "successfully append {} to data file {}",
            &entry,
            self.path.display()
        );
        Ok(())
    }
    // 读取hint文件
    pub(crate) fn entry_iter(&mut self) -> EntryIter<S, F> {
        EntryIter::new(self)
    }
}
pub(crate) struct EntryIter<'a,S: Storage<F=F> + Clone,F:File> {
    hint_file: &'a mut HintFile<S, F>,
    offset: usize,
}

impl<'a,S: Storage<F=F> + Clone,F:File> EntryIter<'a, S, F> {
    fn new(hint_file: &'a mut HintFile<S, F>) -> Self {
        EntryIter {
            hint_file,
            offset: 0,
        }
    }
}

impl<'a,S: Storage<F=F> + Clone,F:File>Iterator for EntryIter<'a, S, F> {
    type Item = HintEntry;

    fn next(&mut self) -> Option<Self::Item> {
        let f = self.hint_file.storage.open(&self.hint_file.path).unwrap();
        let mut reader = Reader::new(f, None, true, self.offset.try_into().unwrap());
        let mut buf = vec![];
        reader.read_record(&mut buf);
        if buf.is_empty() {
            return None;
        }
        let entry = bincode::deserialize(buf.as_slice()).unwrap();
        self.offset +=buf.len();
        trace!(
            "iter read {} from hint file {}",
            &entry,
            self.hint_file.path.display()
        );
        Some(entry)
    }
}

