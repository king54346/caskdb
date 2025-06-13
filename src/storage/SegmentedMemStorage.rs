use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::path::{Path, PathBuf, Component, MAIN_SEPARATOR};
use std::io::{Cursor, Error as IOError, ErrorKind, Read, Seek, SeekFrom, Write};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use std::thread;
use std::time::Duration;
use crate::storage::mem::{FileNode, Node};
use crate::{Error, map_io_res, Result};
use crate::storage::{File, Storage};

#[derive(Clone)]
pub struct SegmentedMemStorage {
    //  例如 / 为一个分段，/dir2 为另一个分段，每个分段下是子路径
    segments: Vec<Arc<RwLock<HashMap<String, Node>>>>,
    segment_count: usize,

    // ---- Parameters for fault injection
    /// sstable/log `flush()` calls are blocked.
    pub delay_data_sync: Arc<AtomicBool>,

    /// sstable/log `flush()` calls return an error
    pub data_sync_error: Arc<AtomicBool>,

    /// Simulate no-space errors
    pub no_space: Arc<AtomicBool>,

    /// Simulate non-writable file system
    pub non_writable: Arc<AtomicBool>,

    /// Force sync of manifest files to fail
    pub manifest_sync_error: Arc<AtomicBool>,

    /// Force write to manifest files to fail
    pub manifest_write_error: Arc<AtomicBool>,

    /// Whether enable to record the count of random reads to files
    pub count_random_reads: bool,

    pub random_read_counter: Arc<AtomicUsize>,
}

impl Default for SegmentedMemStorage {
    fn default() -> Self {
        Self::new(16) // 默认16个分段
    }
}

impl SegmentedMemStorage {
    pub fn new(segment_count: usize) -> Self {
        let segment_count = segment_count.max(1); // 至少要有1个分段
        let mut segments = Vec::with_capacity(segment_count);

        // 初始化所有分段
        for _ in 0..segment_count {
            segments.push(Arc::new(RwLock::new(HashMap::new())));
        }

        let storage = Self {
            segments,
            segment_count,
            delay_data_sync: Arc::new(AtomicBool::new(false)),
            data_sync_error: Arc::new(AtomicBool::new(false)),
            no_space: Arc::new(AtomicBool::new(false)),
            non_writable: Arc::new(AtomicBool::new(false)),
            manifest_sync_error: Arc::new(AtomicBool::new(false)),
            manifest_write_error: Arc::new(AtomicBool::new(false)),
            count_random_reads: false,
            random_read_counter: Arc::new(AtomicUsize::new(0)),
        };

        // 在正确的分段中创建根目录
        let root_path = MAIN_SEPARATOR.to_string();
        let root_index = storage.get_segment_index(&root_path);
        if let Ok(mut guard) = storage.segments[root_index].write() {
            guard.insert(root_path, Node::Dir);
        }

        storage
    }

    /// 根据路径计算对应的分段索引
    fn get_segment_index(&self, path: &str) -> usize {
        let mut hasher = DefaultHasher::new();
        path.hash(&mut hasher);
        (hasher.finish() as usize) % self.segment_count
    }

    /// 获取指定路径的读锁
    fn get_read_lock(&self, path: &str) -> Result<RwLockReadGuard<HashMap<String, Node>>> {
        let index = self.get_segment_index(path);
        match self.segments[index].read() {
            Ok(guard) => Ok(guard),
            Err(poison_err) => {
                // 在锁污染的情况下，我们可以选择恢复或返回错误
                Err(Error::IO(IOError::new(
                    ErrorKind::Other,
                    format!("Lock poisoned for segment {}: {}", index, poison_err),
                )))
            }
        }
    }

    /// 获取指定路径的写锁
    fn get_write_lock(&self, path: &str) -> Result<RwLockWriteGuard<HashMap<String, Node>>> {
        let index = self.get_segment_index(path);
        match self.segments[index].write() {
            Ok(guard) => Ok(guard),
            Err(poison_err) => {
                // 在锁污染的情况下，我们可以选择恢复或返回错误
                Err(Error::IO(IOError::new(
                    ErrorKind::Other,
                    format!("Lock poisoned for segment {}: {}", index, poison_err),
                )))
            }
        }
    }

    /// 获取多个路径的写锁（按索引排序避免死锁）
    fn get_multiple_write_locks(&self, paths: &[&str]) -> Vec<(usize, RwLockWriteGuard<HashMap<String, Node>>)> {
        let mut indices: Vec<_> = paths.iter()
            .map(|path| self.get_segment_index(path))
            .collect();
        indices.sort_unstable();
        indices.dedup();

        let mut locks = Vec::new();
        for &index in &indices {
            match self.segments[index].write() {
                Ok(guard) => locks.push((index, guard)),
                Err(poison_err) => {
                    // 在锁污染的情况下，我们可以选择恢复或返回错误
                    locks.push((index, poison_err.into_inner()));
                }
            }
        }
        locks
    }

    /// 在所有分段中查找匹配的键
    fn find_in_all_segments<F, R>(&self, predicate: F) -> Vec<R>
    where
        F: Fn(&String, &Node) -> Option<R> + Send + Sync,
        R: Send,
    {
        let mut results = Vec::new();
        for segment in &self.segments {
            if let Ok(guard) = segment.read() {
                for (key, node) in guard.iter() {
                    if let Some(result) = predicate(key, node) {
                        results.push(result);
                    }
                }
            }
        }
        results
    }

    // Checking whether the path is good for create a file.
    // Return `Err` when the parent dir is not exist
    // 检查是否可以创建文件
    // 如果父目录不存在，则返回 `Err`
    fn is_ok_to_create<P: AsRef<Path>>(&self, name: P) -> Result<()> {
        if let Some(p) = name.as_ref().parent() {
            if !self.is_exist_dir(p) {
                return Err(Error::IO(IOError::new(
                    ErrorKind::NotFound,
                    format!("{:?}: No directory or file exist", p),
                )));
            }

            let path_str = name.as_ref().to_str().unwrap();
            if let Ok(guard) = self.get_read_lock(path_str) {
                if let Some(n) = guard.get(path_str) {
                    if n.is_file() {
                        // File can be truncated
                        return Ok(());
                    } else {
                        // Exist dir
                        return Err(Error::IO(IOError::new(
                            ErrorKind::AlreadyExists,
                            format!("{:?}: File exists", p),
                        )));
                    }
                }
            }
            Ok(())
        } else {
            // root file
            Err(Error::IO(IOError::new(
                ErrorKind::AlreadyExists,
                "Unable to create root",
            )))
        }
    }

    // Whether the given `path` is a existed directory
    // 用于检查给定路径是否是一个已存在的目录
    fn is_exist_dir<P: AsRef<Path>>(&self, path: P) -> bool {
        let path_str = path.as_ref().to_str().unwrap();
        if let Ok(guard) = self.get_read_lock(path_str) {
            guard.get(path_str).map_or(false, |n| n.is_dir())
        } else {
            false
        }
    }
}

// Remove all the relative part (also the root prefix) and rebuild a new `PathBuf`
// by concatenating all normal components.
// Remove all the relative part (also the root prefix) and rebuild a new `PathBuf`
// by concatenating all normal components.
fn clean<P: AsRef<Path>>(path: P) -> PathBuf {
    let components: Vec<_> = path.as_ref()
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s),
            _ => None,
        })
        .collect();

    if components.is_empty() {
        // 如果没有正常组件，返回根目录
        PathBuf::from(MAIN_SEPARATOR.to_string())
    } else {
        // 从根目录开始构建路径
        let mut pb = PathBuf::from(MAIN_SEPARATOR.to_string());
        for component in components {
            pb.push(component);
        }
        pb
    }
}


impl Storage for SegmentedMemStorage {
    type F = FileNode;

    fn create<P: AsRef<Path>>(&self, name: P) -> Result<Self::F> {
        if self.non_writable.load(Ordering::Acquire) {
            return Err(Error::IO(IOError::new(
                ErrorKind::Other,
                "simulate non writable error",
            )));
        }

        let path = clean(name);
        self.is_ok_to_create(path.as_path())?;
        let name = path.to_str().unwrap().to_owned();

        let mut file_node = FileNode::new(&name);
        file_node.delay_data_sync = self.delay_data_sync.clone();
        file_node.data_sync_error = self.data_sync_error.clone();
        file_node.no_space = self.no_space.clone();
        file_node.manifest_sync_error = self.manifest_sync_error.clone();
        file_node.manifest_write_error = self.manifest_write_error.clone();
        file_node.count_random_reads = Arc::new(AtomicBool::new(self.count_random_reads));
        file_node.random_read_counter = self.random_read_counter.clone();

        if let Ok(mut guard) = self.get_write_lock(&name) {
            match guard.entry(name) {
                Entry::Occupied(n) => match n.get() {
                    Node::File(f) => return Ok(f.clone()),
                    Node::Dir => {
                        return Err(Error::IO(IOError::new(
                            ErrorKind::Other,
                            format!("{} is a directory", n.key()),
                        )))
                    }
                },
                Entry::Vacant(v) => {
                    v.insert(Node::File(file_node.clone()));
                }
            };
            Ok(file_node)
        } else {
            Err(Error::IO(IOError::new(
                ErrorKind::Other,
                "Failed to acquire write lock",
            )))
        }
    }

    fn open<P: AsRef<Path>>(&self, name: P) -> Result<Self::F> {
        let path = clean(name).to_str().unwrap().to_owned();
        if let Ok(guard) = self.get_read_lock(&path) {
            match guard.get(&path) {
                Some(n) => match n {
                    Node::Dir => Err(Error::IO(IOError::new(
                        ErrorKind::NotFound,
                        format!("{}: Try to open a directory", &path),
                    ))),
                    Node::File(f) => Ok(f.clone()),
                },
                None => Err(Error::IO(IOError::new(
                    ErrorKind::NotFound,
                    format!("{}: No such file", &path),
                ))),
            }
        } else {
            Err(Error::IO(IOError::new(
                ErrorKind::Other,
                "Failed to acquire read lock",
            )))
        }
    }

    fn remove<P: AsRef<Path>>(&self, name: P) -> Result<()> {
        let key = clean(name).to_str().unwrap().to_owned();
        if let Ok(mut guard) = self.get_write_lock(&key) {
            if let Some(n) = guard.get(&key) {
                match n {
                    Node::Dir => Err(Error::IO(IOError::new(
                        ErrorKind::NotFound,
                        format!("{}: No such file", &key),
                    ))),
                    Node::File(_) => {
                        guard.remove(&key);
                        Ok(())
                    }
                }
            } else {
                Err(Error::IO(IOError::new(
                    ErrorKind::NotFound,
                    format!("{}: No such file", &key),
                )))
            }
        } else {
            Err(Error::IO(IOError::new(
                ErrorKind::Other,
                "Failed to acquire write lock",
            )))
        }
    }

    fn remove_dir<P: AsRef<Path>>(&self, dir: P, recursively: bool) -> Result<()> {
        let key = clean(dir).to_str().unwrap().to_owned();

        if recursively {
            // 递归删除需要在所有分段中查找
            // 首先收集所有需要删除的路径，但不持有锁
            let mut to_delete = Vec::new();
            let mut found_target = false;

            // 分别在每个分段中查找，避免同时持有多个锁
            for segment in &self.segments {
                if let Ok(guard) = segment.read() {
                    for (k, n) in guard.iter() {
                        if *k == key && n.is_dir() {
                            found_target = true;
                            if key != MAIN_SEPARATOR.to_string() {
                                to_delete.push(k.clone());
                            }
                        } else if *k != key && k.starts_with(&key) {
                            to_delete.push(k.clone());
                        }
                    }
                }
            }

            if !found_target {
                return Err(Error::IO(IOError::new(
                    ErrorKind::NotFound,
                    format!("{}: No such directory", &key),
                )));
            }

            // 按分段分组删除
            let mut segments_to_delete: HashMap<usize, Vec<String>> = HashMap::new();
            for path in to_delete {
                let index = self.get_segment_index(&path);
                segments_to_delete.entry(index).or_default().push(path);
            }

            // 按索引顺序依次锁定并删除每个分段中的项，避免死锁
            let mut indices: Vec<_> = segments_to_delete.keys().cloned().collect();
            indices.sort_unstable();

            for index in indices {
                if let Some(paths) = segments_to_delete.get(&index) {
                    if let Ok(mut guard) = self.segments[index].write() {
                        for path in paths {
                            guard.remove(path);
                        }
                    }
                }
            }
            Ok(())
        } else {
            // 非递归删除只需要检查当前目录是否为空
            if let Ok(mut guard) = self.get_write_lock(&key) {
                if let Some(n) = guard.get(&key) {
                    match n {
                        Node::Dir => {
                            // 检查是否为空目录（需要在所有分段中检查）
                            // 先释放当前锁，然后检查所有分段
                            drop(guard);

                            let mut has_children = false;
                            for segment in &self.segments {
                                if let Ok(read_guard) = segment.read() {
                                    for k in read_guard.keys() {
                                        if *k != key && k.starts_with(&key) {
                                            has_children = true;
                                            break;
                                        }
                                    }
                                }
                                if has_children {
                                    break;
                                }
                            }

                            if has_children {
                                return Err(Error::IO(IOError::new(
                                    ErrorKind::NotFound,
                                    format!("{}: is not an empty dir", &key),
                                )));
                            }

                            // 重新获取写锁并删除
                            if let Ok(mut guard) = self.get_write_lock(&key) {
                                guard.remove(&key);
                            }
                            Ok(())
                        }
                        Node::File(_) => Err(Error::IO(IOError::new(
                            ErrorKind::NotFound,
                            format!("{}: is a file not dir", &key),
                        ))),
                    }
                } else {
                    Err(Error::IO(IOError::new(
                        ErrorKind::NotFound,
                        format!("{}: No such directory", &key),
                    )))
                }
            } else {
                Err(Error::IO(IOError::new(
                    ErrorKind::Other,
                    "Failed to acquire write lock",
                )))
            }
        }
    }

    fn exists<P: AsRef<Path>>(&self, name: P) -> bool {
        let path = clean(name).to_str().unwrap().to_owned();
        if let Ok(guard) = self.get_read_lock(&path) {
            guard.contains_key(&path)
        } else {
            false
        }
    }

    fn rename<P: AsRef<Path>>(&self, old: P, new: P) -> Result<()> {
        let old = clean(old).to_str().unwrap().to_owned();
        if old == MAIN_SEPARATOR.to_string() {
            return Err(Error::IO(IOError::new(
                ErrorKind::InvalidInput,
                "Unable to rename the root",
            )));
        }
        let new = clean(new).to_str().unwrap().to_owned();

        let old_index = self.get_segment_index(&old);
        let new_index = self.get_segment_index(&new);

        if old_index == new_index {
            // 同一分段内的重命名
            if let Ok(mut guard) = self.segments[old_index].write() {
                match guard.remove(&old) {
                    Some(f) => {
                        guard.insert(new, f);
                        Ok(())
                    }
                    None => Err(Error::IO(IOError::new(
                        ErrorKind::NotFound,
                        format!("{}: No such file or directory", old),
                    ))),
                }
            } else {
                Err(Error::IO(IOError::new(
                    ErrorKind::Other,
                    "Failed to acquire write lock",
                )))
            }
        } else {
            // 跨分段的重命名，需要按顺序锁定两个分段
            let paths = vec![old.as_str(), new.as_str()];
            let mut locks = self.get_multiple_write_locks(&paths);

            // 分别查找对应的锁索引
            let old_lock_pos = locks.iter().position(|(i, _)| *i == old_index);
            let new_lock_pos = locks.iter().position(|(i, _)| *i == new_index);

            match (old_lock_pos, new_lock_pos) {
                (Some(old_pos), Some(new_pos)) => {
                    if old_pos == new_pos {
                        // 实际上是同一个分段（不应该发生，但为了安全）
                        let (_, guard) = &mut locks[old_pos];
                        match guard.remove(&old) {
                            Some(f) => {
                                guard.insert(new, f);
                                Ok(())
                            }
                            None => Err(Error::IO(IOError::new(
                                ErrorKind::NotFound,
                                format!("{}: No such file or directory", old),
                            ))),
                        }
                    } else {
                        // 安全地拆分可变引用
                        let (left, right) = if old_pos < new_pos {
                            let (left, right) = locks.split_at_mut(new_pos);
                            (&mut left[old_pos].1, &mut right[0].1)
                        } else {
                            let (left, right) = locks.split_at_mut(old_pos);
                            (&mut right[0].1, &mut left[new_pos].1)
                        };

                        let (old_guard, new_guard) = if old_pos < new_pos {
                            (left, right)
                        } else {
                            (right, left)
                        };

                        match old_guard.remove(&old) {
                            Some(f) => {
                                new_guard.insert(new, f);
                                Ok(())
                            }
                            None => Err(Error::IO(IOError::new(
                                ErrorKind::NotFound,
                                format!("{}: No such file or directory", old),
                            ))),
                        }
                    }
                }
                _ => Err(Error::IO(IOError::new(
                    ErrorKind::Other,
                    "Failed to acquire necessary write locks",
                )))
            }
        }
    }

    fn mkdir_all<P: AsRef<Path>>(&self, dir: P) -> Result<()> {
        let path = clean(dir);
        let components: Vec<String> = path
            .ancestors()
            .map(|p| p.to_str().unwrap().to_owned())
            .collect();

        // 按分段分组需要创建的目录
        let mut segments_to_create: HashMap<usize, Vec<String>> = HashMap::new();
        for component in &components {
            let index = self.get_segment_index(component);
            segments_to_create.entry(index).or_default().push(component.clone());
        }

        // 首先检查是否有文件冲突
        for (index, paths) in &segments_to_create {
            if let Ok(guard) = self.segments[*index].read() {
                for path in paths {
                    if let Some(n) = guard.get(path) {
                        match n {
                            Node::Dir => { /* creating same dir is idempotent */ }
                            Node::File(_) => {
                                return Err(Error::IO(IOError::new(
                                    ErrorKind::AlreadyExists,
                                    format!("{}: File exists", path),
                                )))
                            }
                        }
                    }
                }
            }
        }

        // 创建目录
        for (index, paths) in segments_to_create {
            if let Ok(mut guard) = self.segments[index].write() {
                for path in paths {
                    guard.insert(path, Node::Dir);
                }
            }
        }
        Ok(())
    }

    fn list<P: AsRef<Path>>(&self, dir: P) -> Result<Vec<PathBuf>> {
        let path = clean(dir).to_str().unwrap().to_owned();

        // 首先检查目录是否存在
        if !self.exists(&path) {
            return Err(Error::IO(IOError::new(
                ErrorKind::NotFound,
                format!("{}: No such directory", &path),
            )));
        }

        // 在所有分段中查找子项
        let results = self.find_in_all_segments(|k, _n| {
            if *k != path && k.starts_with(&path) {
                Some(PathBuf::from(k))
            } else {
                None
            }
        });

        Ok(results)
    }
}

/// `File` implementation based on memory
/// This is handy for our tests.
struct InmemFile {
    lock: AtomicBool,
    contents: Cursor<Vec<u8>>,
}

impl Default for InmemFile {
    fn default() -> Self {
        Self {
            lock: AtomicBool::new(false),
            contents: Cursor::new(vec![]),
        }
    }
}

impl Drop for InmemFile {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

impl File for InmemFile {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let pos = self.contents.position();
        // Set position to last to prevent overwritting
        self.contents
            .set_position(self.contents.get_ref().len() as u64);
        let r = self.contents.write(buf);
        // Prevent position from being modified
        self.contents.set_position(pos);
        map_io_res!(r)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        // Reset read cursor to 0
        self.contents.set_position(0);
        Ok(())
    }

    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        let r = self.contents.seek(pos);
        map_io_res!(r)
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let r = self.contents.read(buf);
        map_io_res!(r)
    }

    fn read_all(&mut self, buf: &mut Vec<u8>) -> Result<usize> {
        self.contents.set_position(0);
        let r = self.contents.read_to_end(buf);
        map_io_res!(r)
    }

    fn len(&self) -> Result<u64> {
        Ok(self.contents.get_ref().len() as u64)
    }

    fn lock(&self) -> Result<()> {
        // Unlike described in comments, returns Err instead of blocking if locked
        if self.lock.load(Ordering::Acquire) {
            Err(Error::IO(IOError::new(ErrorKind::Other, "Already locked")))
        } else {
            self.lock.store(true, Ordering::Release);
            Ok(())
        }
    }

    fn unlock(&self) -> Result<()> {
        self.lock.store(false, Ordering::Release);
        Ok(())
    }

    fn read_at(&self, buf: &mut [u8], offset: u64) -> Result<usize> {
        if buf.is_empty() {
            Ok(0)
        } else {
            let inner = self.contents.get_ref();
            let length = inner.len() as u64;
            if offset > length - 1 {
                return Ok(0);
            }
            let exact = if buf.len() as u64 + offset > length {
                return Err(Error::IO(IOError::new(ErrorKind::UnexpectedEof, "EOF")));
            } else {
                buf.len()
            };
            buf.copy_from_slice(&inner.as_slice()[offset as usize..offset as usize + exact]);
            Ok(exact as usize)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{File, Storage};
    use crate::utils::coding::put_fixed_32;
    // use crate::util::coding::put_fixed_32;

    impl SegmentedMemStorage {
        fn assert_node_exists<P: AsRef<Path>>(&self, target: P) -> Node {
            let path = clean(target);
            let path_str = path.to_str().unwrap();

            // 获取对应分段的读锁
            let index = self.get_segment_index(path_str);
            let guard = self.segments[index].read().expect("Failed to acquire read lock");

            // 检查路径是否存在
            let v = guard.get(path_str);
            assert!(v.is_some(), "Node at '{}' does not exist", path_str);
            v.unwrap().clone()
        }

        fn assert_file_exists<P: AsRef<Path>>(&self, target: P) {
            let node = self.assert_node_exists(target);
            assert!(node.is_file(), "Node exists but is not a file");
        }

        fn assert_dir_exists<P: AsRef<Path>>(&self, target: P) {
            let node = self.assert_node_exists(target);
            assert!(node.is_dir(), "Node exists but is not a directory");
        }
    }

    impl InmemFile {
        fn pos_and_data(&self) -> (u64, &[u8]) {
            (self.contents.position(), self.contents.get_ref().as_slice())
        }
    }

    #[test]
    fn test_mem_file_read_write() {
        let mut f = InmemFile::default();
        let written1 = f.write(b"hello world").unwrap();
        assert_eq!(written1, 11);
        let written2 = f.write(b"|hello world").unwrap();
        assert_eq!(written2, 12);
        let (pos, data) = f.pos_and_data();
        assert_eq!(pos, 0);
        assert_eq!(
            String::from_utf8(Vec::from(data)).unwrap(),
            "hello world|hello world"
        );
        let mut read_buf = vec![0u8; 5];
        let read = f.read(read_buf.as_mut_slice()).unwrap();
        assert_eq!(read, 5);
        let (pos, _) = f.pos_and_data();
        assert_eq!(pos, 5);
        read_buf.clear();
        let all = f.read_all(&mut read_buf).unwrap();
        assert_eq!(all, written1 + written2);
        assert_eq!(
            String::from_utf8(read_buf.clone()).unwrap(),
            "hello world|hello world"
        );
    }

    #[test]
    fn test_mem_file_lock_unlock() {
        let f = InmemFile::default();
        f.lock().unwrap();
        f.unlock().unwrap();
        f.lock().unwrap();
        assert_eq!(
            f.lock().unwrap_err().to_string(),
            "I/O operation error: Already locked"
        );
    }

    #[test]
    fn test_mem_file_read_at() {
        let mut f = InmemFile::default();
        let mut buf = vec![];
        for i in 0..100 {
            put_fixed_32(&mut buf, i);
        }
        f.write(&buf).expect("");

        for (offset, buf_len, is_ok) in vec![
            (0, 0, true),
            (0, 400, true),
            (0, 100, true),
            (300, 100, true),
            (340, 100, false),
        ]
        .drain(..)
        {
            let mut read_buf = vec![0u8; buf_len];
            let res = f.read_at(read_buf.as_mut_slice(), offset);
            assert_eq!(
                res.is_ok(),
                is_ok,
                "offset: {}, buf_len: {}",
                offset,
                buf_len
            );
            match res {
                Ok(size) => {
                    assert_eq!(buf_len, size);
                    assert_eq!(
                        read_buf.as_slice(),
                        &buf.as_slice()[offset as usize..offset as usize + buf_len]
                    )
                }
                Err(e) => assert_eq!(e.to_string(), "I/O operation error: EOF"),
            }
        }
    }

    #[test]
    fn test_storage_basic() {
        let store = SegmentedMemStorage::default();
        // Test `create`
        let mut f = store.create("test1").unwrap();
        assert!(store.exists("test1"));
        f.write(b"hello world").unwrap();

        // Test `open` a non-exist file
        let expected_not_found = store.open("not exist");
        assert!(expected_not_found.is_err());

        f = store.open("test1").unwrap();
        let mut read_buf = vec![];
        f.read_all(&mut read_buf).unwrap();
        assert_eq!(String::from_utf8(read_buf).unwrap(), "hello world");

        let expected_not_found = store.rename("not exist", "test3");
        assert!(expected_not_found.is_err());

        // Test `rename`
        store.rename("test1", "test2").unwrap();
        assert!(!store.exists("test1"));
        assert!(store.exists("test2"));

        f = store.open("test2").unwrap();
        let mut read_buf = vec![];
        f.read_all(&mut read_buf).unwrap();
        assert_eq!(String::from_utf8(read_buf).unwrap(), "hello world");

        // Test `remove`
        store.remove("test2").unwrap();
        assert!(!store.exists("test2"));
    }

    #[test]
    fn test_storage_create() {
        let store = SegmentedMemStorage::default();
        store.mkdir_all("/a/b/c").unwrap();
        let f = Node::File(FileNode::new("/a/b/c/d"));
        store.segments[store.get_segment_index("/a/b/c/d")]
            .write()
            .unwrap()
            .insert("/a/b/c/d".to_string(), f);
        let tests = vec![
            ("/", false),               // root file
            ("/a", false),              // exist dir
            ("/a/d", true),             // non exist file
            ("/a/b/c", false),          // exist dir
            ("/a/b/c/d", true),         // truncate file
            ("/a/b/c/e", true),         // new file
            ("/a/b/c/d/e", false),      // parent is a file
            ("/a/b/c/d/e/e/e/", false), // no exist parent dir
        ];
        for (i, (input, expected)) in tests.into_iter().enumerate() {
            let res = store.create(input);
            assert_eq!(res.is_ok(), expected, "{}", i);
            if expected {
                store.assert_file_exists(input);
            }
        }
    }

    #[test]
    fn test_storage_open() {
        let store = SegmentedMemStorage::default();
        store.create("test").unwrap();
        store.mkdir_all("/a/b/c").unwrap();
        let tests = vec![
            ("/", false),
            ("/test", true),
            ("test", true),
            ("test/", true),
            ("/a", false),
            ("/a/b", false),
            ("/****", false),
        ];
        for (input, expected) in tests {
            assert_eq!(store.open(input).is_ok(), expected);
        }
    }

    #[test]
    fn test_storage_exists() {
        let store = SegmentedMemStorage::default();
        store.mkdir_all("a/b/c").unwrap();
        store.create("/a/test").unwrap();
        let tests = vec![
            ("/", true),
            ("///", true),
            ("/a/b/c/", true),
            ("a/b/c/", true),
            ("/a/b/c", true),
            ("/a", true),
            ("/a/b", true),
            ("/a/b/c/d", false),
            ("/a/test", true),
            ("test", false),
        ];
        for (input, expected) in tests {
            assert_eq!(store.exists(input), expected);
        }
    }

    #[test]
    fn test_storage_remove() {
        let store = SegmentedMemStorage::default();
        store.mkdir_all("a/b/c").unwrap();
        store.create("test").unwrap();
        store.create("/a/test").unwrap();
        store.create("/a/b/test").unwrap();
        let tests = vec![
            ("/", false),
            ("a", false),
            ("/a", false),
            ("test", true),
            ("a/test", true),
            ("a/b/test", true),
            ("hello world", false),
        ];
        for (input, expected) in tests {
            assert_eq!(store.remove(input).is_ok(), expected)
        }
    }

    #[test]
    fn test_storage_remove_dir() {
        let store = SegmentedMemStorage::default();
        // |- a
        //   |- 1
        //   |- 2
        // |- b
        // |- c
        //   |- 1
        //   |- d
        //      |- 2
        store.mkdir_all("a").unwrap();
        store.mkdir_all("b").unwrap();
        store.mkdir_all("c/d").unwrap();
        store.create("a/1").unwrap();
        store.create("a/2").unwrap();
        store.create("c/1").unwrap();
        store.create("c/d/2").unwrap();
        let tests = vec![
            ("/", false, false),
            ("a", false, false),
            ("a", true, true),
            ("b", false, true),
            ("c/1", false, false),
            ("c/1", true, false),
            ("c/d", true, true),
            ("/", true, true),
        ];
        for (input, recursively, expected) in tests {
            assert_eq!(store.remove_dir(input, recursively).is_ok(), expected);
        }
    }

    #[test]
    fn test_storage_mkdir_all() {
        let store = SegmentedMemStorage::default();
        store.assert_dir_exists("/");
        store.create("test").unwrap();
        let tests = vec![
            ("/", true),
            ("/test/a", false),
            ("a/b/c", true),
            ("a/b/c/", true), // mkdir is idempotent
        ];
        for (input, expected) in tests {
            assert_eq!(store.mkdir_all(input).is_ok(), expected);
            if expected {
                store.assert_dir_exists(input);
            }
        }
    }

    #[test]
    fn test_storage_list() {
        let store = SegmentedMemStorage::default();
        for i in 0..1000 {
            store.create(i.to_string()).unwrap();
        }
        let list = store.list("/").unwrap();
        for name in list {
            store.assert_file_exists(name);
        }
    }
    #[test]
    fn test_path_clean() {
        let tests = if cfg!(windows) {
            vec![
                ("\\path\\..\\test\\", "\\path\\test"),
                ("\\path\\.\\test\\..", "\\path\\test"),
                ("path", "\\path"),
                ("\\", "\\"),
                (r#"\\\"#, "\\"),
            ]
        } else {
            vec![
                ("/path/../test/", "/path/test"),
                ("/path/./test/..", "/path/test"),
                ("path", "/path"),
                ("/", "/"),
                ("///", "/"),
            ]
        };
        for (input, expected) in tests {
            let res = clean(input);
            assert_eq!(res.to_str().unwrap(), expected);
        }
    }

    #[test]
    fn test_reopen_file_and_read() {
        let store = SegmentedMemStorage::default();
        let mut f = store.create("test").unwrap();
        let contents = "a".repeat(1000);
        f.write(contents.as_bytes()).unwrap();
        let mut got = vec![];
        f.read_all(&mut got).unwrap();
        assert_eq!(&contents.as_bytes(), &got.as_slice());
        f.close().unwrap();
        let mut f = store.open("test").unwrap();
        let mut got = vec![];
        f.read_all(&mut got).unwrap();
        assert_eq!(&contents.as_bytes(), &got.as_slice());
    }
}
