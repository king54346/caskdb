use crate::cache::lru::LRUCache;
use crate::cache::{Cache, ShardedCache};
use crate::logger::Logger;
use crate::storage::{File, Storage};
use std::sync::Arc;
use log::{LevelFilter, Log};

const DEFAULT_CACHE_SHARDS: usize = 8;


/// Options to control the behavior of a database (passed to `DB::Open`)
#[derive(Clone)]
pub struct Options{

    /// 日志记录
    /// 在开发模式下，默认使用std输出
    /// 在release模式下，默认使用文件`LOG.x`进行输出
    pub logger: Option<slog::Logger>,

    /// 最大日志级别
    pub logger_level: LevelFilter,
}

impl Options{
    // 通过限制某些标志的范围、应用自定义记录器等来初始化选项。
    pub(crate) fn initialize<O: File + 'static, S: Storage<F = O> + Clone + 'static >(
        &mut self,
        db_path: &str,
        storage: &S,
    ) {
        self.apply_logger(storage, db_path);
        // if self.block_cache.is_none() {
        //     let mut shards = vec![];
        //     for _ in 0..DEFAULT_CACHE_SHARDS {
        //         shards.push(LRUCache::new(8 << 20));
        //     }
        //     self.block_cache = Some(Arc::new(ShardedCache::new(shards)))
        // }
    }

    fn apply_logger<S: Storage + Clone + 'static >(&mut self, storage: &S, db_path: &str) {
        let user_logger = std::mem::replace(&mut self.logger, None);
        let logger = Logger::new(user_logger, self.logger_level, storage, db_path);
        let static_logger: &'static dyn Log = Box::leak(Box::new(logger));
        let _ = log::set_logger(static_logger); // global logger could be set
        log::set_max_level(self.logger_level);
        info!("Logger initialized: [level {:?}]", &self.logger_level);
    }

    fn clip_range<N: PartialOrd + Eq + Copy>(n: N, min: N, max: N) -> N {
        let mut r = n;
        if n > max {
            r = max
        }
        if n < min {
            r = min
        }
        r
    }
}

impl Default for Options {
    fn default() -> Self {
        Options {
            logger: None,
            logger_level: LevelFilter::Warn,
        }
    }
}

/// Options that control read operations
#[derive(Clone, Copy)]
pub struct ReadOptions {
    /// If true, all data read from underlying storage will be
    /// verified against corresponding checksums.
    pub verify_checksums: bool,

    /// Should the data read for this iteration be cached in memory?
    /// Callers may wish to set this field to false for bulk scans.
    pub fill_cache: bool,

}

impl Default for ReadOptions {
    fn default() -> Self {
        ReadOptions {
            verify_checksums: false,
            fill_cache: true,
        }
    }
}

/// Options that control write operations
#[derive(Default)]
pub struct WriteOptions {
    /// If true, the write will be flushed from the operating system
    /// buffer cache before the write is considered complete.
    /// If this flag is true, writes will be slower.
    ///
    /// If this flag is false, and the machine crashes, some recent
    /// writes may be lost.  Note that if it is just the process that
    /// crashes (i.e., the machine does not reboot), no writes will be
    /// lost even if sync==false.
    ///
    /// In other words, a DB write with sync==false has similar
    /// crash semantics as the "write()" system call.  A DB write
    /// with sync==true has similar crash semantics to a "write()"
    /// system call followed by "fsync()".
    pub sync: bool,
}
