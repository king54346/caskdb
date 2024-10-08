/// # Table
///
///数据块（Data Blocks）：表格由一个或多个数据块组成。这些是存储实际数据的基本单位。
///每个数据块包含了一系列的键值对，这些键值对是实际存储的数据。
//
// 过滤器块（Filter Block，可选）：这是一个可选的块，包含了由过滤器生成器生成的一系列过滤器数据。
// 过滤器块用于快速检查一个键是否存在于某个数据块中，而无需加载整个数据块。这可以显著提高查找效率，尤其是在数据不在表格中时。

// 元索引块（Metaindex Block）：这是一个特殊的块，用于存储表格的参数，比如过滤器块的名称和它的块句柄（Block Handle）。
// 块句柄是一个指向特定块的引用，包含了该块的位置和大小等信息。
//
// 索引块（Index Block）：这是另一个特殊的块，用于记录数据块的偏移量和长度。
// 索引块以一定的间隔（重启间隔，restart interval）组织键值对，每个键代表了其后数据块的最小键。
// 索引块使用前一个块的最后一个键、相邻块之间的较短分隔符或最后一个块的最后一个键的较短后继作为键。
//
// 表尾（Table Footer）：表格的最后部分是表尾，它包含了元索引块和索引块的位置和大小信息，以及表格格式的版本号等元数据。
///
/// ## Table data structure:
///
/// ```text
///                                                          + optional
///                                                         /
///     +--------------+--------------+--------------+------+-------+-----------------+-------------+--------+
///     | data block 1 |      ...     | data block n | filter block | metaindex block | index block | footer |
///     +--------------+--------------+--------------+--------------+-----------------+-------------+--------+
///
///    每个块 trailer 5 字节尾部包含压缩类型和校验和。
///
/// ```
///
/// ## Common Table block trailer:
///
/// ```text
///
///     +---------------------------+-------------------+
///     | compression type (1-byte) | checksum (4-byte) |
///     +---------------------------+-------------------+
///
///     The checksum is a CRC-32 computed using Castagnoli's polynomial. Compression
///     type also included in the checksum.
///
/// ```
///
/// ## Table footer:
///
/// ```text
///
///       +------------------- 40-bytes -------------------+
///      /                                                  \
///     +------------------------+--------------------+------+-----------------+
///     | metaindex block handle / index block handle / ---- | magic (8-bytes) |
///     +------------------------+--------------------+------+-----------------+
///
///     The magic are first 64-bit of SHA-1 sum of "http://code.google.com/p/leveldb/".
///
/// ```
///
/// NOTE: All fixed-length integer are little-endian.
///
///
/// # Block
///
/// 块是存储结构的基本单位，包含了一个或多个键值对入口（entries），以及一个块尾部（block trailer）。
/// 块尾部通常用于存储关于块的元数据，如压缩类型和校验和。
/// 每个键值对代表存储在块中的一条数据，包括一个键和一个值。
/// 在设计上，为了减少存储空间的占用，连续的键共享公共前缀，直到达到一个重启点。
/// 重启点共享前缀
/// 重启点是一种优化存储结构的方法。在一个块中，重启点之间的键共享公共前缀，直到下一个重启点被达到。
/// 每到达一个重启点，键的存储就从完整开始，而不是继续共享前缀。
/// 这种方法既减少了存储需求，又提高了检索效率，因为可以直接跳到某个重启点开始搜索，而不需要遍历整个块。
/// 块中应至少包含一个重启点，且第一个重启点总是0，意味着块的第一个键总是完整存储，不共享前缀。
///
/// ```text
///       + restart point                 + restart point (depends on restart interval)
///      /                               /
///     +---------------+---------------+---------------+---------------+------------------+----------------+
///     | block entry 1 | block entry 2 |      ...      | block entry n | restarts trailer | common trailer |
///     +---------------+---------------+---------------+---------------+------------------+----------------+
///
/// ```
/// Key/value entry:
///
/// ```text
///               +---- key len ----+
///              /                   \
///     +-------+-----------+-----------+---------+--------------------+--------------+----------------+
///     | 共前缀长度 (varint) | 不共享的长度 (varint) | value len (varint) | key (varlen) | value (varlen) |
///     +-------------------+---------------------+--------------------+--------------+----------------+
///
///     Block entry shares key prefix with its preceding key:
///     Conditions:
///         restart_interval=2
///         entry one  : key=deck,value=v1
///         entry two  : key=dock,value=v2
///         entry three: key=duck,value=v3
///     The entries will be encoded as follow:
///
///       + restart point (offset=0)                                                 + restart point (offset=16)
///      /                                                                          /
///     +-----+-----+-----+----------+--------+-----+-----+-----+---------+--------+-----+-----+-----+----------+--------+
///     |  0  |  4  |  2  |  "deck"  |  "v1"  |  1  |  3  |  2  |  "ock"  |  "v2"  |  0  |  4  |  2  |  "duck"  |  "v3"  |
///     +-----+-----+-----+----------+--------+-----+-----+-----+---------+--------+-----+-----+-----+----------+--------+
///      \                                   / \                                  / \                                   /
///       +----------- entry one -----------+   +----------- entry two ----------+   +---------- entry three ----------+
///
///     The block trailer will contains two restart points:
///
///     +------------+-----------+--------+
///     |     0      |    16     |   2    |
///     +------------+-----------+---+----+
///      \                      /     \
///       +-- restart points --+       + restart points length
///
/// ```
///
/// # Block restarts trailer
///
/// ```text
///
///       +-- 4-bytes --+
///      /               \
///     +-----------------+-----------------+-----------------+------------------------------+
///     | restart point 1 |       ....      | restart point n | restart points len (4-bytes) |
///     +-----------------+-----------------+-----------------+------------------------------+
///
/// ```
///
/// NOTE: All fixed-length integer are little-endian.
///
/// # Filter block
///
/// Filter block consist of one or more filter data and a filter block trailer.
/// The trailer contains 过滤器数据偏移量, 尾部偏移量 and 1字节的基数对数(过滤器参数的规模或精度级别).
///
/// Filter block data structure:
///
/// ```text
///
///       + offset 1      + offset 2      + offset n      + trailer offset
///      /               /               /               /
///     +---------------+---------------+---------------+---------+
///     | filter data 1 |      ...      | filter data n | trailer |
///     +---------------+---------------+---------------+---------+
///
/// ```
///
/// Filter block trailer:
///
/// ```text
///
///       +- 4-bytes -+
///      /             \
///     +---------------+---------------+---------------+-------------------------------+------------------+
///     | data 1 offset |      ....     | data n offset | data-offsets length (4-bytes) | base Lg (1-byte) |
///     +---------------+---------------+---------------+-------------------------------+------------------+
///
/// ```
///
/// NOTE: The filter block is not compressed
///
/// # Index block
///
/// 索引块由一个或多个kv数据和一个common tailer组成。
/// Separator key 则是介于两个数据块之间的一个键
/// block handle 表示数据块偏移和长度信息
/// ```text
///
///     +---------------+--------------+
///     |      key      |    value     |
///     +---------------+--------------+
///     | separator key | block handle |---- a block handle points a data block starting offset and the its size
///     | ...           | ...          |
///     +---------------+--------------+
///
/// ```
///
/// NOTE: All fixed-length integer are little-endian.
///
/// # Meta block
///
/// 这个元块包含一堆统计数据。关键是统计数据的名称。该值包含统计信息。
/// 对于当前的实现， Meta block仅包含过滤器元数据：
///
/// ```text
///
///     +-------------+---------------------+
///     |     key     |        value        |
///     +-------------+---------------------+
///     | filter name | filter block handle |
///     +-------------+---------------------+
///
/// ```
///
/// NOTE: All fixed-length integer are little-endian.
pub mod block;
mod filter_block;
pub mod table;

use crate::util::coding::{decode_fixed_64, put_fixed_64};
use crate::util::varint::{VarintU64, MAX_VARINT_LEN_U64};
use crate::{Error, Result};

// magic
const TABLE_MAGIC_NUMBER: u64 = 0xdb4775248b80fb57;

// 1byte compression type + 4bytes CRC
const BLOCK_TRAILER_SIZE: usize = 5;

//  BlockHandle 最大编码长度 20bytes
const MAX_BLOCK_HANDLE_ENCODE_LENGTH: usize = 2 * MAX_VARINT_LEN_U64;

// 页脚的编码长度。它由两个block handle和一个magic组成。  40+8 byte
const FOOTER_ENCODED_LENGTH: usize = 2 * MAX_BLOCK_HANDLE_ENCODE_LENGTH + 8;

/// `BlockHandle`处理块存储中的偏移和大小信息
/// 通过长度和偏移来确定每个block的位置
#[derive(Eq, PartialEq, Debug, Clone)]
pub struct BlockHandle {
    offset: u64,
    //注意: the block trailer size 是不包含的
    size: u64,
}

impl BlockHandle {
    pub fn new(offset: u64, size: u64) -> Self {
        Self { offset, size }
    }
    // 设置 offset
    #[inline]
    pub fn set_offset(&mut self, offset: u64) {
        self.offset = offset;
    }
    // 设置 size
    #[inline]
    pub fn set_size(&mut self, size: u64) {
        self.size = size
    }

    /// 将 varint 编码的 offset 和 size 附加到给定的 `dst`
    #[inline]
    pub fn encoded_to(&self, dst: &mut Vec<u8>) {
        VarintU64::put_varint(dst, self.offset);
        VarintU64::put_varint(dst, self.size);
    }

    /// 返回编码后的 BlockHandle 的字节数组
    #[inline]
    pub fn encoded(&self) -> Vec<u8> {
        let mut v = vec![];
        self.encoded_to(&mut v);
        v
    }

    /// 从字节数组中解码一个 BlockHandle
    ///
    /// # Error
    ///
    /// If varint decoding fails, return `Status::Corruption` with relative messages
    #[inline]
    pub fn decode_from(src: &[u8]) -> Result<(Self, usize)> {
        // 从字节数组中尝试读取第一个VarintU64值 为offset
        if let Some((offset, n)) = VarintU64::read(src) {
            // 基于第一个读取的结果，计算剩余的字节数组，并尝试读取第二个VarintU64值
            if let Some((size, m)) = VarintU64::read(&src[n..]) {
                Ok((Self::new(offset, size), m + n))
            } else {
                Err(Error::Corruption("bad block handle".to_owned()))
            }
        } else {
            Err(Error::Corruption("bad block handle".to_owned()))
        }
    }
}

///` Footer `用于封装存储在每个sstable文件末尾的固定信息 408byte(meta_index_handle+index_handle)+8byte(magic)
#[derive(Debug)]
pub struct Footer {
    meta_index_handle: BlockHandle,
    index_handle: BlockHandle,
}

impl Footer {
    #[inline]
    pub fn new(meta_index_handle: BlockHandle, index_handle: BlockHandle) -> Self {
        Self {
            meta_index_handle,
            index_handle,
        }
    }

    // 从字节数组中解码 Footer，并返回解码的长度
    ///
    /// # Error
    ///
    /// Returns `Status::Corruption` when decoding meta index or index handle fails
    ///
    pub fn decode_from(src: &[u8]) -> Result<(Self, usize)> {
        // (40,48]
        let magic = decode_fixed_64(&src[FOOTER_ENCODED_LENGTH - 8..]);
        if magic != TABLE_MAGIC_NUMBER {
            return Err(Error::Corruption(
                "not an sstable (bad magic number)".to_owned(),
            ));
        };
        let (meta_index_handle, n) = BlockHandle::decode_from(src)?;
        let (index_handle, m) = BlockHandle::decode_from(&src[n..])?;
        Ok((
            Self {
                meta_index_handle,
                index_handle,
            },
            m + n,
        ))
    }

    // 编码 Footer 并返回编码后的字节数组
    pub fn encoded(&self) -> Vec<u8> {
        let mut v = vec![];
        // 编码 meta index handle
        self.meta_index_handle.encoded_to(&mut v);
        // 编码 index handle
        self.index_handle.encoded_to(&mut v);
        v.resize(2 * MAX_BLOCK_HANDLE_ENCODE_LENGTH, 0);
        // 添加魔数
        put_fixed_64(&mut v, TABLE_MAGIC_NUMBER);
        assert_eq!(
            v.len(),
            FOOTER_ENCODED_LENGTH,
            "[footer] the length of encoded footer is {}, expect {}",
            v.len(),
            FOOTER_ENCODED_LENGTH
        );
        v
    }
}

#[cfg(test)]
mod test_footer {
    use crate::sstable::{BlockHandle, Footer};

    #[test]
    fn test_footer_corruption() {
        let footer = Footer::new(BlockHandle::new(300, 100), BlockHandle::new(401, 1000));
        let mut encoded = footer.encoded();
        let last = encoded.last_mut().unwrap();
        *last += 1;
        let r1 = Footer::decode_from(&encoded);
        assert!(r1.is_err());
        let e1 = r1.unwrap_err();
        assert_eq!(
            e1.to_string(),
            "data corruption: not an sstable (bad magic number)"
        );
    }

    #[test]
    fn test_encode_decode() {
        let footer = Footer::new(BlockHandle::new(300, 100), BlockHandle::new(401, 1000));
        let encoded = footer.encoded();
        let (footer, _) = Footer::decode_from(&encoded).expect("footer decoding should work");
        assert_eq!(footer.index_handle, BlockHandle::new(401, 1000));
        assert_eq!(footer.meta_index_handle, BlockHandle::new(300, 100));
    }
}

#[cfg(test)]
mod tests {
    use crate::db::format::{
        InternalKey, InternalKeyComparator, ParsedInternalKey, ValueType, MAX_KEY_SEQUENCE,
        VALUE_TYPE_FOR_SEEK,
    };
    use crate::db::{WickDB, WickDBIterator, DB};
    use crate::iterator::Iterator;
    use crate::mem::{MemTable, MemTableIterator};
    use crate::options::{Options, ReadOptions};
    use crate::sstable::block::*;
    use crate::sstable::table::*;
    use crate::storage::mem::{FileNode, MemStorage};
    use crate::storage::{File, Storage};
    use crate::util::collection::HashSet;
    use crate::util::comparator::{BytewiseComparator, Comparator};
    use crate::{Error, Result};
    use crate::{WriteBatch, WriteOptions};
    use rand::prelude::ThreadRng;
    use rand::Rng;
    use std::cell::Cell;
    use std::cmp::Ordering;
    use std::sync::Arc;

    // Return the reverse of given key
    fn reverse(key: &[u8]) -> Vec<u8> {
        let mut v = Vec::from(key);
        let length = v.len();
        for i in 0..length / 2 {
            v.swap(i, length - i - 1)
        }
        v
    }

    #[derive(Default, Clone, Copy)]
    struct ReverseComparator {
        cmp: BytewiseComparator,
    }

    impl Comparator for ReverseComparator {
        fn compare(&self, a: &[u8], b: &[u8]) -> Ordering {
            self.cmp.compare(&reverse(a), &reverse(b))
        }

        fn name(&self) -> &str {
            "wickdb.ReverseBytewiseComparator"
        }

        fn separator(&self, a: &[u8], b: &[u8]) -> Vec<u8> {
            let s = self.cmp.separator(&reverse(a), &reverse(b));
            reverse(&s)
        }

        fn successor(&self, key: &[u8]) -> Vec<u8> {
            let s = self.cmp.successor(&reverse(key));
            reverse(&s)
        }
    }

    #[derive(Clone, Copy)]
    enum TestComparator {
        Normal(BytewiseComparator),
        Reverse(ReverseComparator),
    }

    impl TestComparator {
        fn new(is_reversed: bool) -> Self {
            match is_reversed {
                true => TestComparator::Reverse(ReverseComparator::default()),
                false => TestComparator::Normal(BytewiseComparator::default()),
            }
        }
    }
    impl Default for TestComparator {
        fn default() -> Self {
            TestComparator::Normal(BytewiseComparator::default())
        }
    }

    impl Comparator for TestComparator {
        fn compare(&self, a: &[u8], b: &[u8]) -> Ordering {
            match &self {
                TestComparator::Normal(c) => c.compare(a, b),
                TestComparator::Reverse(c) => c.compare(a, b),
            }
        }

        fn name(&self) -> &str {
            match &self {
                TestComparator::Normal(c) => c.name(),
                TestComparator::Reverse(c) => c.name(),
            }
        }

        fn separator(&self, a: &[u8], b: &[u8]) -> Vec<u8> {
            match &self {
                TestComparator::Normal(c) => c.separator(a, b),
                TestComparator::Reverse(c) => c.separator(a, b),
            }
        }

        fn successor(&self, key: &[u8]) -> Vec<u8> {
            match &self {
                TestComparator::Normal(c) => c.successor(key),
                TestComparator::Reverse(c) => c.successor(key),
            }
        }
    }

    // 用于为BlockBuilder/TableBuilder 和 Block/Table 测试提供一个统一的接口
    trait Constructor {
        type Iter: Iterator;

        fn new(is_reversed: bool) -> Self;

        // Write key/value pairs in `data` into inner data structure
        fn finish(
            &mut self,
            options: Arc<Options<TestComparator>>,
            storage: &MemStorage,
            data: &[(Vec<u8>, Vec<u8>)],
        ) -> Result<()>;

        // Returns a iterator for inner data structure
        fn iter(&self) -> Self::Iter;
    }

    struct BlockConstructor {
        block: Block,
        is_reversed: bool,
    }

    impl Constructor for BlockConstructor {
        type Iter = BlockIterator<TestComparator>;

        fn new(is_reversed: bool) -> Self {
            Self {
                block: Block::default(),
                is_reversed,
            }
        }

        fn finish(
            &mut self,
            options: Arc<Options<TestComparator>>,
            _storage: &MemStorage,
            data: &[(Vec<u8>, Vec<u8>)],
        ) -> Result<()> {
            let mut builder = BlockBuilder::new(
                options.block_restart_interval,
                TestComparator::new(self.is_reversed),
            );
            for (key, value) in data {
                builder.add(key.as_slice(), value.as_slice())
            }
            let data = builder.finish();
            let block = Block::new(Vec::from(data))?;
            self.block = block;
            Ok(())
        }

        fn iter(&self) -> Self::Iter {
            self.block.iter(TestComparator::new(self.is_reversed))
        }
    }

    struct TableConstructor {
        table: Option<Arc<Table<FileNode>>>,
        cmp: TestComparator,
    }

    impl Constructor for TableConstructor {
        type Iter = TableIterator<TestComparator, FileNode>;

        fn new(is_reversed: bool) -> Self {
            Self {
                table: None,
                cmp: TestComparator::new(is_reversed),
            }
        }

        fn finish(
            &mut self,
            options: Arc<Options<TestComparator>>,
            storage: &MemStorage,
            data: &[(Vec<u8>, Vec<u8>)],
        ) -> Result<()> {
            let file_name = "test_table";
            let file = storage.create(file_name)?;
            let mut builder = TableBuilder::new(file, self.cmp, &options);
            for (key, value) in data {
                builder.add(key.as_slice(), value.as_slice()).unwrap();
            }
            builder.finish(false).unwrap();
            let file = storage.open(file_name)?;
            let file_len = file.len()?;
            let table = Table::open(file, 0, file_len, options, self.cmp)?;
            self.table = Some(Arc::new(table));
            Ok(())
        }

        fn iter(&self) -> Self::Iter {
            let t = self.table.as_ref().unwrap();
            new_table_iterator(self.cmp, t.clone(), ReadOptions::default())
        }
    }

    // A helper struct to convert user key into lookup key for inner iterator
    struct KeyConvertingIterator<I: Iterator> {
        inner: I,
        err: Cell<Option<Error>>,
    }

    impl<I: Iterator> KeyConvertingIterator<I> {
        fn new(iter: I) -> Self {
            Self {
                inner: iter,
                err: Cell::new(None),
            }
        }
    }

    impl<I: Iterator> Iterator for KeyConvertingIterator<I> {
        fn valid(&self) -> bool {
            self.inner.valid()
        }

        fn seek_to_first(&mut self) {
            self.inner.seek_to_first()
        }

        fn seek_to_last(&mut self) {
            self.inner.seek_to_last()
        }

        fn seek(&mut self, target: &[u8]) {
            let ikey = InternalKey::new(target, MAX_KEY_SEQUENCE, VALUE_TYPE_FOR_SEEK);
            self.inner.seek(ikey.data());
        }

        fn next(&mut self) {
            self.inner.next()
        }

        fn prev(&mut self) {
            self.inner.prev()
        }

        fn key(&self) -> &[u8] {
            match ParsedInternalKey::decode_from(self.inner.key()) {
                Some(parsed_ikey) => parsed_ikey.user_key,
                None => {
                    self.err
                        .set(Some(Error::Corruption("malformed internal key".to_owned())));
                    "corrupted key".as_bytes()
                }
            }
        }

        fn value(&self) -> &[u8] {
            self.inner.value()
        }

        fn status(&mut self) -> Result<()> {
            let err = self.err.take();
            if err.is_none() {
                self.err.set(err);
                self.inner.status()
            } else {
                Err(err.unwrap())
            }
        }
    }

    // A simple wrapper for entries collected in a Vec
    struct EntryIterator {
        current: usize,
        data: Vec<(Vec<u8>, Vec<u8>)>,
        cmp: TestComparator,
    }

    impl EntryIterator {
        fn new(is_reversed: bool, data: Vec<(Vec<u8>, Vec<u8>)>) -> Self {
            let cmp = TestComparator::new(is_reversed);
            Self {
                current: data.len(),
                data,
                cmp,
            }
        }
    }

    impl Iterator for EntryIterator {
        fn valid(&self) -> bool {
            self.current < self.data.len()
        }

        fn seek_to_first(&mut self) {
            self.current = 0
        }

        fn seek_to_last(&mut self) {
            if self.data.is_empty() {
                self.current = 0
            } else {
                self.current = self.data.len() - 1
            }
        }

        fn seek(&mut self, target: &[u8]) {
            for (i, (key, _)) in self.data.iter().enumerate() {
                if self.cmp.compare(key.as_slice(), target) != Ordering::Less {
                    self.current = i;
                    return;
                }
            }
            self.current = self.data.len()
        }

        fn next(&mut self) {
            assert!(self.valid());
            self.current += 1
        }

        fn prev(&mut self) {
            assert!(self.valid());
            if self.current == 0 {
                self.current = self.data.len()
            } else {
                self.current -= 1
            }
        }

        fn key(&self) -> &[u8] {
            assert!(self.valid());
            self.data[self.current].0.as_slice()
        }

        fn value(&self) -> &[u8] {
            assert!(self.valid());
            self.data[self.current].1.as_slice()
        }

        fn status(&mut self) -> Result<()> {
            Ok(())
        }
    }

    struct MemTableConstructor {
        inner: MemTable<TestComparator>,
    }

    impl Constructor for MemTableConstructor {
        type Iter = KeyConvertingIterator<MemTableIterator<TestComparator>>;

        fn new(is_reversed: bool) -> Self {
            let icmp = InternalKeyComparator::new(TestComparator::new(is_reversed));
            Self {
                inner: MemTable::new(1 << 32, icmp),
            }
        }

        fn finish(
            &mut self,
            _options: Arc<Options<TestComparator>>,
            _storage: &MemStorage,
            data: &[(Vec<u8>, Vec<u8>)],
        ) -> Result<()> {
            for (seq, (key, value)) in data.iter().enumerate() {
                self.inner.add(
                    seq as u64 + 1,
                    ValueType::Value,
                    key.as_slice(),
                    value.as_slice(),
                );
            }
            Ok(())
        }

        fn iter(&self) -> Self::Iter {
            KeyConvertingIterator::new(self.inner.iter())
        }
    }

    struct DBConstructor {
        inner: WickDB<MemStorage, TestComparator>,
    }

    struct DBIterWrapper {
        inner: WickDBIterator<MemStorage, TestComparator>,
        key_buf: Vec<u8>,
        value_buf: Vec<u8>,
    }
    impl DBIterWrapper {
        // fill the kv buffer from inner key-value
        fn fill_entry(&mut self) {
            if self.valid() {
                self.key_buf = self.inner.key().to_vec();
                self.value_buf = self.inner.value().to_vec();
            }
        }
    }

    impl Iterator for DBIterWrapper {
        fn valid(&self) -> bool {
            self.inner.valid()
        }

        fn seek_to_first(&mut self) {
            self.inner.seek_to_first();
            self.fill_entry();
        }

        fn seek_to_last(&mut self) {
            self.inner.seek_to_last();
            self.fill_entry();
        }

        fn seek(&mut self, target: &[u8]) {
            self.inner.seek(target);
            self.fill_entry();
        }

        fn next(&mut self) {
            self.inner.next();
            self.fill_entry();
        }

        fn prev(&mut self) {
            self.inner.prev();
            self.fill_entry();
        }

        fn key(&self) -> &[u8] {
            &self.key_buf
        }

        fn value(&self) -> &[u8] {
            &self.value_buf
        }

        fn status(&mut self) -> Result<()> {
            self.inner.status()
        }
    }

    impl Constructor for DBConstructor {
        type Iter = DBIterWrapper;

        fn new(is_reversed: bool) -> Self {
            let mut options = Options::<TestComparator>::default();
            let env = MemStorage::default();
            options.write_buffer_size = 10000; // Something small to force merging
            options.error_if_exists = true;
            options.comparator = TestComparator::new(is_reversed);
            let db = WickDB::open_db(options, "table_testdb", env).expect("could not open db");
            Self { inner: db }
        }

        fn finish(
            &mut self,
            _options: Arc<Options<TestComparator>>,
            _storage: &MemStorage,
            data: &[(Vec<u8>, Vec<u8>)],
        ) -> Result<()> {
            for (key, value) in data.iter() {
                let mut batch = WriteBatch::default();
                batch.put(key.as_slice(), value.as_slice());
                self.inner
                    .write(WriteOptions::default(), batch)
                    .expect("write batch should work")
            }
            Ok(())
        }

        fn iter(&self) -> Self::Iter {
            DBIterWrapper {
                inner: self.inner.iter(ReadOptions::default()).unwrap(),
                key_buf: vec![],
                value_buf: vec![],
            }
        }
    }

    struct CommonConstructor<C: Constructor> {
        storage: MemStorage,
        constructor: C,
        // key&value pairs in order
        data: Vec<(Vec<u8>, Vec<u8>)>,
        keys: HashSet<Vec<u8>>,
    }

    impl<C: Constructor> CommonConstructor<C> {
        fn new(storage: MemStorage, constructor: C) -> Self {
            Self {
                storage,
                constructor,
                data: vec![],
                keys: HashSet::default(),
            }
        }
        fn add(&mut self, key: &[u8], value: &[u8]) {
            if !self.keys.contains(key) {
                self.data.push((Vec::from(key), Vec::from(value)));
                self.keys.insert(Vec::from(key));
            }
        }

        // Finish constructing the data structure with all the keys that have
        // been added so far.  Returns the keys in sorted order and stores the
        // key/value pairs in `data`
        fn finish(&mut self, options: Arc<Options<TestComparator>>) -> Vec<Vec<u8>> {
            let cmp = options.comparator.clone();
            // Sort the data
            self.data.sort_by(|(a, _), (b, _)| cmp.compare(&a, &b));
            let mut res = vec![];
            for (key, _) in self.data.iter() {
                res.push(key.clone())
            }
            self.constructor
                .finish(options, &self.storage, &self.data)
                .expect("constructor finish should be ok");
            res
        }
    }

    struct TestHarness<C: Constructor> {
        options: Arc<Options<TestComparator>>,
        reverse_cmp: bool,
        inner: CommonConstructor<C>,
        rand: ThreadRng,
    }

    impl<C: Constructor> TestHarness<C> {
        fn new(reverse_cmp: bool, restart_interval: usize) -> Self {
            let mut options = Options::<TestComparator>::default();
            options.block_restart_interval = restart_interval;
            // Use shorter block size for tests to exercise block boundary
            // conditions more
            options.block_size = 256;
            options.paranoid_checks = true;
            options.comparator = TestComparator::new(reverse_cmp);
            let constructor = C::new(reverse_cmp);
            let storage = MemStorage::default();
            TestHarness {
                inner: CommonConstructor::new(storage, constructor),
                reverse_cmp,
                rand: rand::thread_rng(),
                options: Arc::new(options),
            }
        }

        fn add(&mut self, key: &[u8], value: &[u8]) {
            self.inner.add(key, value)
        }

        fn test_forward_scan(&self, expected: &[(Vec<u8>, Vec<u8>)]) {
            let mut iter = self.inner.constructor.iter();
            assert!(
                !iter.valid(),
                "iterator should be invalid after being initialized"
            );
            iter.seek_to_first();
            for (key, value) in expected.iter() {
                assert_eq!(format_kv(key.clone(), value.clone()), format_entry(&iter));
                iter.next();
            }
            assert!(
                !iter.valid(),
                "iterator should be invalid after yielding all entries"
            );
        }

        fn test_backward_scan(&self, expected: &[(Vec<u8>, Vec<u8>)]) {
            let mut iter = self.inner.constructor.iter();
            assert!(
                !iter.valid(),
                "iterator should be invalid after being initialized"
            );
            iter.seek_to_last();
            for (key, value) in expected.iter().rev() {
                assert_eq!(format_kv(key.clone(), value.clone()), format_entry(&iter));
                iter.prev();
            }
            assert!(
                !iter.valid(),
                "iterator should be invalid after yielding all entries"
            );
        }

        fn test_random_access(&mut self, keys: &[Vec<u8>], expected: Vec<(Vec<u8>, Vec<u8>)>) {
            let mut iter = self.inner.constructor.iter();
            assert!(
                !iter.valid(),
                "iterator should be invalid after being initialized"
            );
            let mut expected_iter = EntryIterator::new(self.reverse_cmp, expected);
            for _ in 0..1000 {
                match self.rand.gen_range(0, 5) {
                    // case for `next`
                    0 => {
                        if iter.valid() {
                            iter.next();
                            expected_iter.next();
                            if iter.valid() {
                                assert_eq!(format_entry(&iter), format_entry(&expected_iter));
                            } else {
                                assert_eq!(iter.valid(), expected_iter.valid());
                            }
                        }
                    }
                    // case for `seek_to_first`
                    1 => {
                        iter.seek_to_first();
                        expected_iter.seek_to_first();
                        if iter.valid() {
                            assert_eq!(format_entry(&iter), format_entry(&expected_iter));
                        } else {
                            assert_eq!(iter.valid(), expected_iter.valid());
                        }
                    }
                    // case for `seek`
                    2 => {
                        let rkey = random_seek_key(keys, self.reverse_cmp);
                        let key = rkey.as_slice();
                        iter.seek(key);
                        expected_iter.seek(key);
                        if iter.valid() {
                            assert_eq!(format_entry(&iter), format_entry(&expected_iter));
                        } else {
                            assert_eq!(iter.valid(), expected_iter.valid());
                        }
                    }
                    // case for `prev`
                    3 => {
                        if iter.valid() {
                            iter.prev();
                            expected_iter.prev();
                            if iter.valid() {
                                assert_eq!(format_entry(&iter), format_entry(&expected_iter));
                            } else {
                                assert_eq!(iter.valid(), expected_iter.valid());
                            }
                        }
                    }
                    // case for `seek_to_last`
                    4 => {
                        iter.seek_to_last();
                        expected_iter.seek_to_last();
                        if iter.valid() {
                            assert_eq!(format_entry(&iter), format_entry(&expected_iter));
                        } else {
                            assert_eq!(iter.valid(), expected_iter.valid());
                        }
                    }
                    _ => { /* ignore */ }
                }
            }
        }

        fn do_test(&mut self) {
            let keys = self.inner.finish(self.options.clone());
            let expected = self.inner.data.clone();
            self.test_forward_scan(&expected);
            self.test_backward_scan(&expected);
            self.test_random_access(&keys, expected);
        }
    }

    #[inline]
    fn format_kv(key: Vec<u8>, value: Vec<u8>) -> String {
        format!("'{:?}->{:?}'", key, value)
    }

    // Return a String represents current entry of the given iterator
    #[inline]
    fn format_entry(iter: &dyn Iterator) -> String {
        format!("'{:?}->{:?}'", iter.key(), iter.value())
    }

    fn random_seek_key(keys: &[Vec<u8>], reverse_cmp: bool) -> Vec<u8> {
        if keys.is_empty() {
            b"foo".to_vec()
        } else {
            let mut rnd = rand::thread_rng();
            let result = keys.get(rnd.gen_range(0, keys.len())).unwrap();
            match rnd.gen_range(0, 3) {
                1 => {
                    // Attempt to return something smaller than an existing key
                    let mut cloned = result.clone();
                    if !cloned.is_empty() && *cloned.last().unwrap() > 0u8 {
                        let last = cloned.last_mut().unwrap();
                        *last -= 1
                    }
                    cloned
                }
                2 => {
                    // Return something larger than an existing key
                    let mut cloned = result.clone();
                    if reverse_cmp {
                        cloned.insert(0, 0)
                    } else {
                        cloned.push(0);
                    }
                    cloned
                }
                _ => result.clone(), // Return an existing key
            }
        }
    }

    enum TestType {
        Table,
        Block,
        Memtable,
        #[allow(dead_code)]
        DB, // TODO: Enable DB test util fundamental components are stable
    }

    fn tests() -> Vec<(TestType, bool, usize)> {
        vec![
            (TestType::Table, false, 16),
            (TestType::Table, false, 1),
            (TestType::Table, false, 1024),
            (TestType::Table, true, 16),
            (TestType::Table, true, 1),
            (TestType::Table, true, 1024),
            (TestType::Block, false, 16),
            (TestType::Block, false, 1),
            (TestType::Block, false, 1024),
            (TestType::Block, true, 16),
            (TestType::Block, true, 1),
            (TestType::Block, true, 1024),
            // Restart interval does not matter for memtables
            (TestType::Memtable, false, 16),
            (TestType::Memtable, true, 16),
            // Do not bother with restart interval variations for DB
            (TestType::DB, false, 16),
            (TestType::DB, true, 16),
        ]
    }

    fn random_key(length: usize) -> Vec<u8> {
        let chars = vec![
            '0', '1', 'a', 'b', 'c', 'd', 'e', '\u{00fd}', '\u{00fe}', '\u{00ff}',
        ];
        let mut rnd = rand::thread_rng();
        let mut result = vec![];
        for _ in 0..length {
            let i = rnd.gen_range(0, chars.len());
            let v = chars.get(i).unwrap();
            let mut buf = vec![0; v.len_utf8()];
            v.encode_utf8(&mut buf);
            result.append(&mut buf);
        }
        result
    }

    fn random_value(length: usize) -> Vec<u8> {
        let mut result = vec![0u8; length];
        let mut rnd = rand::thread_rng();
        for i in 0..length {
            let v = rnd.gen_range(0, 96);
            result[i] = v as u8;
        }
        result
    }

    macro_rules! test_harness {
        ($kv:expr) => {
            tests()
                .into_iter()
                .for_each(|(tp, is_reversed, restart_interval)| match tp {
                    TestType::Memtable => {
                        let mut t =
                            TestHarness::<MemTableConstructor>::new(is_reversed, restart_interval);
                        for (k, v) in $kv.clone() {
                            t.add(k, v);
                        }
                        t.do_test();
                    }
                    TestType::Block => {
                        let mut t =
                            TestHarness::<BlockConstructor>::new(is_reversed, restart_interval);
                        for (k, v) in $kv.clone() {
                            t.add(k, v);
                        }
                        t.do_test();
                    }
                    TestType::Table => {
                        let mut t =
                            TestHarness::<TableConstructor>::new(is_reversed, restart_interval);
                        for (k, v) in $kv.clone() {
                            t.add(k, v);
                        }
                        t.do_test();
                    }
                    TestType::DB => {
                        let mut t =
                            TestHarness::<DBConstructor>::new(is_reversed, restart_interval);
                        for (k, v) in $kv.clone() {
                            t.add(k, v);
                        }
                        t.do_test();
                    }
                })
        };
    }

    #[test]
    fn test_empty_harness() {
        test_harness!(vec![]);
    }

    #[test]
    fn test_simple_empty_key() {
        test_harness!(vec![(b"", b"v")]);
    }

    #[test]
    fn test_single_key() {
        test_harness!(vec![(b"abc", b"v")]);
    }

    #[test]
    fn test_mutiple_key() {
        let kv: Vec<(&[u8], &[u8])> = vec![(b"abc", b"v"), (b"abcd", b"v"), (b"ac", b"v2")];
        test_harness!(kv);
    }

    #[test]
    fn test_special_key() {
        test_harness!(vec![(b"\xff\xff", b"v")]);
    }

    #[test]
    fn test_randomized_key() {
        let mut rnd = rand::thread_rng();
        let mut kv = vec![];
        for _ in 0..1000 {
            let key = random_key(rnd.gen_range(1, 10));
            let value = random_value(rnd.gen_range(1, 5));
            kv.push((key, value));
        }
        test_harness!(kv.iter());
    }
}
