// Batch是对数据库的批量操作。
// 如果readonly为true，则只能通过Get方法从batch中获取数据。
// 如果尝试使用Put或Delete方法，将会返回错误。
//
// 如果readonly为false，则可以使用Put和Delete方法将数据写入批次。
// 调用Commit方法时，数据将被写入数据库。
//
// 批处理不是事务，它不保证隔离性。
// 但它可以保证原子性、一致性和持久性（如果 Sync 选项为 true）。
//
// 必须调用 Commit 方法来提交批次，否则 DB 将被锁定。


use snowflake::ProcessUniqueId;

struct Batch {
    db: Option<Arc<DB>>,
    pending_writes: Vec<LogRecord>,
    pending_writes_map: HashMap<u64, Vec<usize>>,
    options: BatchOptions,
    committed: bool,
    rollbacked: bool,
    batch_id: Option<ProcessUniqueId>,
    buffers: Vec<ByteBuffer>,
}

impl Batch {

}