use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use crate::store::filename::{generate_filename, FileType};
use crate::storage::{File, Storage};

use log::{LevelFilter, Log, Metadata, Record};
use slog::{o, Drain, Level};

const LOG_COUNT_SUFFIX: usize = 4;
const LOG_CONTENT_LIMIT: usize = 1024*1024*500;


pub trait SuffixScheme<S: Storage> {
    fn rotate(&mut self, basepath: &Path, s: &S) -> String;

    fn log_paths(&mut self,s: &S, basepath: &Path) -> Vec<PathBuf>;
}
pub struct CountSuffix {
    max_files: usize,
}
impl CountSuffix {
    /// New CountSuffix
    pub fn new(max_files: usize) -> Self {
        Self { max_files }
    }
}

impl<S: Storage> SuffixScheme<S> for CountSuffix {
    fn rotate(&mut self, basepath: &Path,s: &S) -> String {
        /// Make sure that path(count) does not exist, by moving it to path(count+1).
        fn cascade<S: Storage>(basepath: &Path,s: &S, count: usize, max_files: usize) {
            let src = PathBuf::from(format!("{}.{}", basepath.display(), count));
            if s.exists(&src) {
                let dest = PathBuf::from(format!("{}.{}", basepath.display(), count + 1));
                if s.exists(&dest) {
                    cascade(basepath,s,count + 1, max_files);
                }
                if count >= max_files {
                    // If the file is too old (too big count), delete it,
                    //   (also if count == max_files, because then the .(max_files-1) file will be moved
                    //   to .max_files)
                    let _ = s.remove(&src).unwrap();
                } else {
                    // otherwise, rename it.
                    let _ = s.rename(src, dest);
                }
            }
        }
        cascade(basepath, s, 1, self.max_files);
        "1".to_string()
    }
    // 返回一个文件路径列表，这些路径与给定的 basepath 共享相同的文件名前缀，并且按数字后缀降序排序
    fn log_paths(&mut self,s: &S, basepath: &Path) -> Vec<PathBuf> {
        let filename_prefix = basepath
            .file_name()
            .expect("basepath.file_name()")
            .to_string_lossy();

        let parent_dir = basepath.parent().expect("basepath.parent()");
        let filepaths = s.list(parent_dir).unwrap();

        let mut numbers: Vec<usize> = filepaths.iter()
            .filter_map(|path| {
                let filename = path.file_name()?.to_string_lossy();
                if filename.starts_with(&*filename_prefix) {
                    filename.split('.').nth(1)?.parse::<usize>().ok()
                } else {
                    None
                }
            })
            .collect();

        // 降序排序
        numbers.sort_unstable_by(|x, y| y.cmp(x));

        numbers
            .iter()
            .map(|n| basepath.with_file_name(format!("{}.{n}", filename_prefix)))
            .collect::<Vec<_>>()
    }
}

pub enum ContentLimit {
    Lines(usize),
    BytesSurpassed(usize),
}

pub struct FileRotate<SS,Storage,File> {
    filepath: PathBuf,
    file: File,
    storage: Storage,
    content_limit: ContentLimit,
    count: usize, //用于比较content_limit
    suffix_scheme: SS,
    basename: PathBuf,
}

impl<SS: SuffixScheme<S>,S: Storage<F = F> + Clone,F:File> FileRotate<SS, S, F> {
    // path文件路径
    pub fn new<P: AsRef<Path>>(path: P, storage: &S, suffix_scheme: SS, content_limit: ContentLimit) -> Self {
        match content_limit {
            ContentLimit::Lines(lines) => assert!(lines > 0),
            ContentLimit::BytesSurpassed(bytes) => assert!(bytes > 0),
        };

        let basepath = path.as_ref().to_path_buf();
        let _ = storage.mkdir_all(&basepath);
        // /test/LOG
        let basename = generate_filename(basepath.to_str().unwrap(), FileType::Log, 0);
        let filepath = match &suffix_scheme {
            CountSuffix => {
                PathBuf::from(format!("{}.{}", &basename, 1))
            }
            _ => {
                PathBuf::from(&basename)
            }
        };
        let file = storage
            .create(&filepath)
            .unwrap();
        Self {
            storage: storage.clone(),
            file,
            filepath,
            basename:PathBuf::from(basename),
            content_limit,
            count: 0,
            suffix_scheme,
        }
    }

    pub fn log_paths(&mut self) -> Vec<PathBuf> {
        self.suffix_scheme.log_paths(&self.storage,&self.basename)
    }

    fn rotate(&mut self) {
        // 生成新的文件后缀
        let suffix = self.suffix_scheme.rotate(&self.basename,&self.storage);
        let path = PathBuf::from(format!("{}.{}", self.basename.display(), suffix));

        // 关闭当前日志文件
        let _ = self.file.close();

        // 重命名当前日志文件
        let _ = self.storage.rename(&self.filepath, &path);

        // 创建新的日志文件
        let file = self.storage
            .create(&self.filepath)
            .unwrap();
        self.file = file;
        self.count = 0;
    }
}

impl<SS: SuffixScheme<S>,S: Storage<F = F> + Clone,F:File> Write for FileRotate<SS, S, F> {
    fn write(&mut self, mut buf: &[u8])-> std::result::Result<usize, std::io::Error> {
        let written = buf.len();
        match self.content_limit {
            ContentLimit::Lines(lines) => {
                while let Some((idx, _)) = buf.iter().enumerate().find(|(_, byte)| *byte == &b'\n')
                {
                    self.file.write(&buf[..idx + 1]).unwrap();
                    self.count += 1;
                    buf = &buf[idx + 1..];
                    if self.count >= lines {
                        self.rotate();
                    }
                }
                self.file.write(&buf).unwrap();
            }
            ContentLimit::BytesSurpassed(bytes) => {
                if self.count > bytes {
                    self.rotate();
                }
                self.file.write(&buf).unwrap();
                self.count += buf.len();
            }
        }
        Ok(written)
    }
    fn flush(&mut self) -> std::result::Result<(), std::io::Error> {
        Result::Ok(())
    }
}





use std::sync::Mutex;
use crate::{error, storage};
use crate::storage::mem::MemStorage;

// 是根据开发模式（开发模式或发布模式）创建合适的日志记录器
pub struct Logger {
    // 用于实际的日志记录
    inner: slog::Logger,
    // 用于过滤日志记录的级别
    // Trace
    // Debug
    // Info
    // Warn
    // Error
    level: LevelFilter,
}

impl Logger {
    /// 创建并返回一个新的 Logger 实例
    ///
    /// If `inner` is not `None`, use `inner` logger
    /// If `inner` is `None`
    ///     - In dev mode, use a std output
    ///     - In release mode, use a storage specific file with name `LOG`
    pub fn new<S: Storage + Clone + 'static >(
        inner: Option<slog::Logger>,
        level: LevelFilter,
        storage: &S,
        db_path: &str,
    ) -> Self {
        let inner = match inner {
            Some(l) => l,
            None => {
                // --release 发布模式 --debug 调试模式
                if cfg!(debug_assertions) {
                    // 使用标准输出作为日志记录器。
                    let decorator = slog_term::TermDecorator::new().build();
                    let drain = Mutex::new(slog_term::FullFormat::new(decorator).build()).fuse();
                    slog::Logger::root(drain, o!())
                } else {
                    // 并使用异步文件记录器
                    let mut write = FileRotate::new(db_path, storage, CountSuffix::new(LOG_COUNT_SUFFIX), ContentLimit::BytesSurpassed(LOG_CONTENT_LIMIT));
                    let drain = slog_async::Async::new(FileBasedDrain::new(write))
                        .build()
                        .fuse();
                    slog::Logger::root(drain, o!())
                }
            }
        };
        Self { inner, level }
    }
}

impl Log for Logger {
    // 日志记录是否启用
    // 取决于记录的级别是否低于或等于 Logger 的级别过滤器。
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.level
    }

    #[allow(unused_must_use)]
    fn log(&self, r: &Record) {
        if self.enabled(r.metadata()) {
            // 将 log crate 的日志级别转换为 slog 的日志级别。
            let level = log_to_slog_level(r.metadata().level());
            let args = r.args();
            let target = r.target();
            let module = r.module_path_static().unwrap_or("");
            let file = r.file_static().unwrap_or("");
            let line = r.line().unwrap_or(0);

            // 创建 slog::Record 实例并记录日志。
            let s = slog::RecordStatic {
                location: &slog::RecordLocation {
                    file,
                    line,
                    column: 0,
                    function: "",
                    module,
                },
                level,
                tag: target,
            };
            // inner中选择std out 还是 文件存储
            if cfg!(debug_assertions) {
                let meta_info = format!("{}:{}", file, line);
                self.inner.log(&slog::Record::new(
                    &s,
                    args,
                    slog::b!("[location]" => meta_info),
                ))
            } else {
                self.inner.log(&slog::Record::new(&s, args, slog::b!()))
            }
        }
    }

    fn flush(&self) {}
}

fn log_to_slog_level(level: log::Level) -> Level {
    match level {
        log::Level::Trace => Level::Trace,
        log::Level::Debug => Level::Debug,
        log::Level::Info => Level::Info,
        log::Level::Warn => Level::Warning,
        log::Level::Error => Level::Error,
    }
}
// 用于将日志记录写入文件 该结构体和实现基于 slog 日志库
struct FileBasedDrain<F:  Write> {
    inner: Mutex<F>,
}

impl<F:  Write> FileBasedDrain<F> {
    fn new(f: F) -> Self {
        FileBasedDrain {
            inner: Mutex::new(f),
        }
    }
}

impl<F: Write> Drain for FileBasedDrain<F> {
    type Ok = ();
    type Err = slog::Never;

    fn log(
        &self,
        record: &slog::Record,
        values: &slog::OwnedKVList,
    ) -> Result<Self::Ok, Self::Err> {
        // 日志记录格式为 [日志级别] : 日志消息 键值对列表 \n
        // 键值对列表 [location]: src\logger.rs:353 信息仅用于debug
        let _ = self.inner.lock().unwrap().write(
            format!(
                "[{}] : {:?} {:?} \n",
                record.level(),
                record.msg(),
                values
            )
                .as_bytes(),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::storage::mem::MemStorage;

    use std::thread;
    use std::time::Duration;
    use crate::storage::file::FileStorage;

    #[test]
    fn test_default_logger() {
        let s = FileStorage::default();
        let db_path = "test";
        let logger = Logger::new(None, LevelFilter::Debug, &s, db_path);
        //泄漏的 logger的生命周期与整个程序的生命周期相同
        let _ = log::set_logger(Box::leak(Box::new(logger)));
        log::set_max_level(LevelFilter::Debug);
        info!("Hello World");
        info!("Hello World");
        info!("Hello World");
        info!("Hello World");
        info!("Hello World");
        info!("Hello World");
        // Wait for the async logger print the result
        thread::sleep(Duration::from_millis(100));
    }
    #[test]
    fn test_mem() {
        let s = MemStorage::default();
        let mut f =FileRotate::new("/test/sdfsd/sdfsd", &s, CountSuffix::new(4), ContentLimit::Lines(4));
        write!(f, "a\nb\nc\nd\ne\nf\ng\nh\ni\n").unwrap();
        println!("{:?}", f.log_paths());
    }
    #[test]
    fn test_sysfile() {
        let mut s = FileStorage::default();
        let mut f =FileRotate::new("/test", &s, CountSuffix::new(4), ContentLimit::Lines(4));
        write!(f, "a\nb\nc\nd\ne\nf\ng\nh\ni\n").unwrap();
        println!("{:?}", f.log_paths());
    }
    #[test]
    fn test_mem_bytes_surpassed() {
        let mut s = MemStorage::default();
        let mut f =FileRotate::new("/test", &s, CountSuffix::new(4), ContentLimit::BytesSurpassed(30));
        write!(f, "aaaaa\nb\nc\nd\ne\nf\ng\nh\ni\n").unwrap();
        write!(f, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nb\nc\nd\ne\nf\ng\nh\ni\n").unwrap();
        println!("{:?}", f.log_paths());
    }
}
