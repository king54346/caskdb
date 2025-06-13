use crate::index::btree::BTreeIndexer;
use crate::index::{Indexer, KeyDirEntry};
use crate::options::Options;
use crate::segment::data::{DataEntry, DataFile};
use crate::segment::hint::HintFile;
use crate::storage::Storage;
use crate::utils::collection::HashMap;
use crate::{Error, Result};
use crossbeam_channel::{Receiver, Sender};
use std::collections::vec_deque::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, RwLock};
use std::time;

pub mod filename;
// read 1. keydir 2. keydir-->disk
// write 1.disk 2. keydir
#[derive(Clone)]
pub struct CaskDB<S: Storage + Clone + 'static, I: Indexer> {
    inner: Arc<RwLock<DBImpl<S, I>>>,
    shutdown_batch_processing_thread: (Sender<()>, Receiver<()>),
    shutdown_compaction_thread: (Sender<()>, Receiver<()>),
}
impl<S: Storage + Clone, I: Indexer> CaskDB<S, I> {
    // Create a new WickDB
    // pub fn open_db<P: AsRef<Path>>(
    //     mut options: Options,
    //     db_path: P,
    //     storage: S,
    // ) -> Result<Self> {
    //     let db_path = match db_path.as_ref().to_owned().into_os_string().into_string() {
    //         Ok(s) => s,
    //         Err(_) => {
    //             return Err(Error::Customized(
    //                 "Invalid db path. Expect to use Unicode db path.".to_owned(),
    //             ))
    //         }
    //     };
    //     options.initialize(&db_path, &storage);
    //     debug!("Open db: '{:?}'", &db_path);
    //     let mut db = DBImpl::new(options, db_path, storage);
    //     let (mut edit, should_save_manifest) = db.recover()?;
    //     let mut versions = db.versions.lock().unwrap();
    //     if versions.record_writer.is_none() {
    //         let new_log_number = versions.inc_next_file_number();
    //         let log_file = db.env.create(&generate_filename(
    //             &db.db_path,
    //             FileType::Log,
    //             new_log_number,
    //         ))?;
    //         versions.record_writer = Some(Writer::new(log_file));
    //         edit.set_log_number(new_log_number);
    //         versions.set_log_number(new_log_number);
    //     }
    //     if should_save_manifest {
    //         edit.set_prev_log_number(0);
    //         edit.set_log_number(versions.log_number());
    //         versions.log_and_apply(edit)?;
    //     }
    //
    //     let current = versions.current();
    //     db.delete_obsolete_files(versions)?;
    //     let wick_db = WickDB {
    //         inner: Arc::new(db),
    //         shutdown_batch_processing_thread: crossbeam_channel::bounded(1),
    //         shutdown_compaction_thread: crossbeam_channel::bounded(1),
    //     };
    //     wick_db.process_compaction();
    //     wick_db.process_batch();
    //     // Schedule a compaction to current version for potential unfinished work
    //     debug!("Try to schedule a compaction on opening db");
    //     wick_db.inner.maybe_schedule_compaction(current);
    //     Ok(wick_db)
    // }
}

pub struct DBImpl<S: Storage + Clone, I: Indexer> {
    // 存储环境
    env: S,
    options: Arc<Options>,
    // 物理路径
    db_path: String,

    // 数据库锁 F代表Storage的关联类型 .lock文件
    db_lock: Option<S::F>,
    // holds a bunch of data files.
    //  key 是 file_id value是DataFile
    // 所有只读数据文件的映射，存储所有不再接受写入的历史数据文件，当 current 文件被轮换（rotation）时，旧的 current 文件会被添加到这个映射中
    data_files: RwLock<HashMap<u64, DataFile<S, S::F>>>,
    //  批量写操作的调度队列
    // batch_queue: Mutex<VecDeque<BatchTask>>,
    // 批量写调度相关的条件变量
    // process_batch_sem: Condvar,

    //  表缓存
    // table_cache: TableCache<S>,
    // key 索引 索引器接口，负责索引的持久化和恢复 btree索引
    keydir: I,
    // 加载的hint文件或者写入 hint文件, 方式通过读取hintfile中的entry 重新插入到map中实现
    //  只有1个hintfile，loadIndexes方法,用于将读取索引和持久化索引
    hint_file: Option<HintFile<S, S::F>>,
    // 统计信息
    stats: Stats,
    // 当前活跃的数据文件，用于写入新数据
    active_data_file: Option<DataFile<S, S::F>>,
    // 后台任务完成的信号，如压缩操作 Condvar条件变量用与线程间通讯
    background_work_finished_signal: Condvar,
    // 标记是否已经安排了后台压缩任务。
    background_compaction_scheduled: AtomicBool,
    // 用于触发压缩操作的通信信道。
    do_compaction: (Sender<()>, Receiver<()>),

    // 记录后台操作（如压缩）中遇到的错误
    bg_error: RwLock<Option<Error>>,
    // 标记数据库是否正在关闭过程中。
    is_shutting_down: AtomicBool,

    // isMerging atomic.Bool
    // current 当前元数据文件的路径
    current: Option<PathBuf>, // 当前元数据文件的路径
                              // metadata *metadata.MetaData - 元数据信息，如统计信息、配置等
}

#[derive(Debug, Copy, Clone, Default)]
pub struct Stats {
    /// 过时条目的总大小（字节），包括标记删除的。
    pub size_of_stale_entries: u64,
    /// 数据文件中的过时条目总数。
    pub total_stale_entries: u64,
    // 数据库中活动键值对的总数。
    pub total_active_entries: u64,
    // 数据文件总数
    pub total_data_files: u64,
    // 所有数据文件的总大小（字节）。
    pub size_of_all_data_files: u64,
}

// todo 构建hint文件 1. 合并的时候会对应生成hintfile 2. 开启的时候会从activedatafile转为old，创建一个新的active，然后读取hint
impl<S: Storage + Clone + 'static, I: Indexer> DBImpl<S, I> {
    fn new(options: Options, db_path: &str, storage: S, indexer: I) -> Self {
        let o = Arc::new(options);
        Self {
            env: storage.clone(),
            options: o.clone(),
            db_path: db_path.to_string(),
            db_lock: None,
            data_files: RwLock::new(HashMap::default()),
            keydir: indexer,
            hint_file: None,
            stats: Stats::default(),
            active_data_file: None,
            background_work_finished_signal: Condvar::new(),
            background_compaction_scheduled: AtomicBool::new(false),
            do_compaction: crossbeam_channel::unbounded(),
            bg_error: RwLock::new(None),
            is_shutting_down: AtomicBool::new(false),
            current: None,
        }
    }
    // 从hint_file构建内存索引
    fn build_keydir_from_hint_file(&mut self, path: &Path) -> Result<()> {
        trace!("build keydir from hint file {}", path.display());
        let mut hint_file = HintFile::new(path, &self.env);
        // 迭代器
        for entry in hint_file.entry_iter() {
            let keydir_ent = KeyDirEntry::new(entry.file_id, entry.offset, entry.size);
            let old = self.keydir.put(entry.key.as_slice(), keydir_ent);
            // 插入时有旧的条目
            if let Some(old_ent) = old {
                self.stats.size_of_stale_entries += old_ent.size;
                self.stats.total_stale_entries += 1;
            }
        }
        Ok(())
    }

    // fn save_indexes_to_hint_file(&mut self, path: &Path) -> Result<()> {
    //     trace!("save keydir to hint file {}", path.display());
    //     let mut hint_file = HintFile::new(path, &self.env);
    //     // 遍历keydir中的所有键值对
    //     self.keydir.ascend(|key, entry| {
    //         hint_file.write(key, entry.offset, entry.size, entry.segment_id).map_err( )
    //     }).expect("TODO: panic message");
    //     Ok(())
    // }

    fn save_metadata_to_current(&self) -> Result<()> {
        // todo 保存元数据到 current 文件
        // 1. 获取当前的元数据
        // 2. 写入到 current 文件
        // 3. 确保写入成功
        Ok(())
    }

    fn load_data_files(&mut self) -> Result<()> {
        // todo 从磁盘加载数据文件
        // 1. 遍历数据目录，找到所有的数据文件
        // 2. 将每个数据文件加载到 data_files 中
        // 3. 更新 stats
        Ok(())
    }

    fn load_indexes(&mut self) -> Result<()> {
        // todo 从磁盘加载索引
        // 1. 如果 hint_file 存在，则从 hint_file 中加载索引
        // 2. 如果 hint_file 不存在 （文件不存在或损坏），则从数据文件重新构建索引
        //          即使索引成功加载，还会检查元数据中的 IndexUpToDate 标志 (表示索引不是最新的，可能是因为上次操作中数据库未正常关闭)则也会从数据文件重新构建索引
        // 如果索引成功加载且是最新的，则直接返回加载的索引
        // 3. 更新 stats
        if let Some(hint_file_path) = self.hint_file.as_ref().map(|hf| hf.path.clone()) {
            self.load_indexes_from_hint_file(hint_file_path.as_path())
        } else {
            self.load_indexes_from_data_files()
        }
    }

    fn load_indexes_from_hint_file(&mut self, path: &Path) -> Result<()> {
        trace!("build keydir from hint file {}", path.display());
        let mut hint_file = HintFile::new(path, &self.env);
        // 迭代器
        for entry in hint_file.entry_iter() {
            let keydir_ent = KeyDirEntry::new(entry.file_id, entry.offset, entry.size);
            let old = self.keydir.put(entry.key.as_slice(), keydir_ent);
            // 插入时有旧的条目
            if let Some(old_ent) = old {
                self.stats.size_of_stale_entries += old_ent.size;
                self.stats.total_stale_entries += 1;
            }
        }
        Ok(())
    }

    fn load_indexes_from_data_files(&mut self) -> Result<()> {
        // todo 从数据文件加载索引
        // 1. 遍历 data_files 中的每个数据文件
        // 2. 读取每个数据文件中的条目，并更新 keydir
        // 3. 更新 stats
        Ok(())
    }

    fn load_indexes_from_data_file(&mut self) -> Result<()> {
        // 1. 遍历 data_files 中的每个数据文件
        // 2. 读取每个数据文件中的条目，并更新 keydir
        // 3. 更新 stats
        Ok(())
    }

    fn load_metadata(&mut self) -> Result<()> {
        // todo 从元数据文件加载元数据
        // 1. 读取元数据文件
        // 2. 更新 stats
        // 3. 更新 options
        Ok(())
    }

    // Ascend 按升序为数据库中的每个键/值对调用handleFn。
    // fn ascend<F>(&self, handle_fn: F) -> Result<()> where F: Fn(&[u8], &[u8]) {
    //     self.keydir.ascend(|key, pos| {
    //
    //     })
    // }

    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        if let Some(keydir_entry) = self.keydir.get(key) {
            trace!(
                "found key '{}' in keydir, got value {:?}",
                String::from_utf8_lossy(key),
                &keydir_entry
            );
            let df;
            let data_files = self.data_files.read().unwrap();
            //根据找到的条目信息中的 FileID 确定需要从哪个数据文件读取
            //如果 FileID 与当前活动数据文件相同，使用当前数据文件 active_data_file
            //否则，从只读数据文件映射 datafiles 中获取对应的数据文件
            if keydir_entry.segment_id == self.active_data_file.as_ref().map_or(0,|df| df.id) {
                // 使用当前活跃的数据文件
                df = self.active_data_file.as_ref().expect("active data file not found");
            } else {
                df = data_files
                    .get(&keydir_entry.segment_id)
                    .unwrap_or_else(|| panic!("data file {} not found", &keydir_entry.segment_id));
            }
           
            let entry = df.read(keydir_entry.offset)?;
            if !entry.is_valid() {
                // entry无效
                Err(Error::Corruption("DataEntryCorrupted".to_owned()))
            } else {
                Ok(Some(entry.value().into()))
            }
        } else {
            // 没查到
            Ok(None)
        }
    }

    // 写入数据到活跃的数据文件中
    fn write(&mut self, key: &[u8], value: &[u8]) -> Result<DataEntry> {
        let mut df = self
            .active_data_file
            .as_mut()
            .expect("active data file not found");

        let entry = df.write(key, value)?;
        Ok(entry)
    }

    pub fn set(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        // 追加到当前活跃的数据文件中。
        let ent = self.write(key, value)?;

        // update keydir, the in-memory index.
        // 更新 keydir，将数据文件的偏移量、大小等信息存储到 keydir 中。
        let old = self
            .keydir
            .put(key, KeyDirEntry::new(ent.file_id, ent.offset, ent.size));
        match old {
            None => {
                // 新的条目
                self.stats.total_active_entries += 1;
            }
            Some(entry) => {
                // 更新的条目
                self.stats.size_of_stale_entries += entry.size;
                self.stats.total_stale_entries += 1;
            }
        }
        self.stats.size_of_all_data_files += ent.size;

        Ok(())
    }

    pub fn delete(&mut self, key: &[u8]) -> Result<()> {
        match self.keydir.get(key) {
            None => {
                trace!(
                    "remove key '{}' failed, not found in datastore",
                    String::from_utf8_lossy(key)
                );
                Err(Error::NotFound(None))
            }
            Some(_) => {
                trace!(
                    "remove key '{}' from datastore",
                    String::from_utf8_lossy(key)
                );
                // write TOMBSTONE, will be removed on compaction.
                let entry = self.write(key, b"%TINKV_REMOVE_TOMESTOME%")?;
                // remove key from in-memory index.
                let old = self.keydir.delete(key).expect("key not found");

                self.stats.total_active_entries -= 1;
                self.stats.total_stale_entries += 1;
                self.stats.size_of_all_data_files += entry.size;
                self.stats.size_of_stale_entries += old.size + entry.size;

                Ok(())
            }
        }
    }

    // 生成一个新的data_file
    fn new_active_data_file(&mut self, file_id: Option<u64>) -> Result<()> {
        // default next file id should be `max_file_id` + 1
        let next_file_id: u64 = file_id
            .unwrap_or_else(|| self.data_files.read().unwrap().keys().max().unwrap_or(&0) + 1);

        // build data file path.
        let p = "/000001.data";
        self.env.create(p).expect("TODO: panic message");
        self.active_data_file = Some(DataFile::new(Path::new(p), &self.env, true));

        // 准备一个只读数据文件，路径相同。
        let df = DataFile::new(Path::new(p), &self.env, false);
        self.data_files.write().unwrap().insert(df.id, df);

        self.stats.total_data_files += 1;

        Ok(())
    }

    // // 生成一个老的data_file
    // fn new_old_data_file(&mut self, file_id: Option<u64>) -> Result<u64> {
    //     // default next file id should be `max_file_id` + 1
    //     let next_file_id: u64 =
    //         file_id.unwrap_or_else(|| self.data_files.keys().max().unwrap_or(&0) + 1);
    //
    //     // build data file path.
    //     // let p = segment_data_file_path(&self.path, next_file_id);
    //     let p = "/test.data";
    //     debug!("new data file at: {}", &p.display());
    //     // preapre a read-only data file with the same path.
    //     let df = DataFile::new(Path::new(p),&self.env,false);
    //     let id =df.id;
    //     self.data_files.insert(id, df);
    //     self.stats.total_data_files += 1;
    //
    //     Ok(id)
    // }
    // /// todo 清除数据文件中的陈旧条目并回收磁盘空间。
    // /// 遍历所有的旧数据文件（注意是不可变的旧数据文件，活跃文件不会遍历）
    // /// 获取记录的索引位置indexpos 并检查索引位置是否和当前记录位置匹配
    // /// 然后将所有有效（没有被删除）的键的最新版本写入到新的文件中，最后再将旧数据文件删除，同时生成 hint 文件
    // pub fn merge(&mut self) -> Result<()> {
    //     let begin_at = time::Instant::now();
    //
    //     info!(
    //         "there are {} data files need to be compacted",
    //         self.data_files.len()
    //     );
    //
    //     let next_file_id = self.next_file_id();
    //     // 创建一个old_data_file
    //     let p = "/test.data";
    //     debug!("new data file at: {}", &p.display());
    //     // preapre a read-only data file with the same path.
    //     let mut df = DataFile::new(Path::new(p), &self.env, false);
    //     let mut hf = HintFile::new(Path::new(p), &self.env);
    //
    //     let active_file_id = self.active_data_file.unwrap().id;
    //     // 遍历之前所有的old_data_file,合并，并生成hint
    //     for (key, value) in &self.data_files {
    //         if key != active_file_id{
    //             match value.read(0) {
    //                 Ok(v) => {
    //                     match self.keydir.get(v.key()) {
    //                         None => {}
    //                         Some(index) => {
    //                             if index.offset!=v.offset && index.segment_id!=v.file_id && index.size!=v.size{
    //                                 df.write(v.key(), v.value()).expect("TODO: panic message");
    //                                 hf.write(v.key(),v.offset,v.size,active_file_id)
    //                             }
    //                         }
    //                     };
    //                 }
    //                 Err(_) => {}
    //             };
    //         }
    //     }
    //     info!(
    //         "compaction progress done in {:?}",
    //         time::Instant::now().duration_since(begin_at)
    //     );
    //     // 写入mergeFinFile，当数据库重启时，可以通过检查该文件是否存在来决定是否需要重新进行合并操作。
    //
    //     // 更新状态
    //     self.stats.total_data_files = self.data_files.len() as u64;
    //     self.stats.total_active_entries = self.keydir.len() as u64;
    //     self.stats.total_stale_entries = 0;
    //     self.stats.size_of_stale_entries = 0;
    //     // self.stats.size_of_all_data_files = total_size_of_compaction_files;
    //
    //     Ok(())
    // }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn len(&self) -> u64 {
        self.keydir.size() as u64
    }
    pub fn contains_key(&self, key: &[u8]) -> bool {
        match self.keydir.get(key) {
            None => false,
            Some(_) => true,
        }
    }
}

// fn segment_data_file_path(dir: String, segment_id: u64) -> PathBuf {
//     segment_file_path(dir, segment_id, ".data")
// }
// fn segment_file_path(dir: String, segment_id: u64, suffix: &str) -> PathBuf {
//     let mut p = dir.to_path_buf();
//     p.push(format!("{:012}{}", segment_id, suffix));
//     p
// }
#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::mem::MemStorage;
    use crate::storage::SegmentedMemStorage::SegmentedMemStorage;
    #[test]
    fn test_db() {
        let store = SegmentedMemStorage::default();
        let name = "/000001.hint";
        store.create(name).expect("TODO: panic message");
        let mut file = HintFile::new(Path::new(name), &store);
        let result = file.write("hello".as_bytes(), 2, 4, 1).unwrap();
        let result = file.write("hello1".as_bytes(), 1, 1, 1).unwrap();
        let result = file.write("hello2".as_bytes(), 1, 1, 1).unwrap();
        let result = file.write("hello3".as_bytes(), 1, 1, 1).unwrap();
        let result = file.write("hello4".as_bytes(), 1, 1, 1).unwrap();

        let mut db = DBImpl::new(Options::default(), name, store.clone(), BTreeIndexer::new());
        db.hint_file = Some(HintFile::new(Path::new(name), &store));
        db.load_indexes().expect("TODO: panic message");
        let option = db.keydir.get("hello".as_bytes());
        println!("{:?}", option);
        let option2 = db.keydir.get("hello4".as_bytes());
        println!("{:?}", option2);
        let option3 = db.keydir.get("hello5".as_bytes());
        println!("{:?}", option3);
    }
    #[test]
    fn test_get() {
        let store = MemStorage::default();
        let name = "/test.Hint";
        let mut db = DBImpl::new(Options::default(), name, store.clone(), BTreeIndexer::new());
        db.new_active_data_file(None);
        db.set("hello".as_bytes(), "world".as_bytes());
        let result = db.get("hello".as_bytes()).unwrap().unwrap();
        println!("{:?}", String::from_utf8(result));
        let result1 = db.delete("hello".as_bytes());
        println!("{:?}", result1);
        let result2 = db.get("hello".as_bytes()).unwrap();
        assert!(result2.is_none(), "Expected None after deletion");
    }
}
