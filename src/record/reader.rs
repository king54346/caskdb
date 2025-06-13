use crate::record::reader::ReaderError::{BadRecord, EOF};
use crate::record::{RecordType, BLOCK_SIZE, HEADER_SIZE};
use crate::storage::File;
use crate::utils::coding::decode_fixed_32;
use crate::utils::crc32::{hash, unmask};
use std::io::SeekFrom;

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug)]
enum ReaderError {
    // * 我们遇到了内部读取文件的错误
    // * 我们达到了日志块的末尾
    // * 我们得到了一个大于 BLOCK_SIZE 的记录
    EOF,
    // 表示我们发现了一个无效的物理记录。
    // 目前有三种情况会发生这种错误：
    // * 记录具有无效的 CRC（ReadPhysicalRecord 报告了一个丢弃）
    // * 记录是一个0长度的记录（不会报告丢弃）
    // * 记录低于构造函数的 initial_offset（不会报告丢弃）
    BadRecord,
}

// 代表一条记录
#[derive(Debug, Clone)]
struct Record {
    t: RecordType,
    data: Vec<u8>,
}

/// 用于报告日志读取过程中检测到的腐败情况
pub trait Reporter {
    /// bytes 因腐败而丢失的大约字节数,reason: 腐败的原因
    fn corruption(&mut self, bytes: u64, reason: &str);
}

/// `Reader` 用于从日志文件中读取记录。
/// `Reader` 总是从 `file` 的 `initial_offset` 处开始读取记录。
pub struct Reader<F: File> {
    // NOTICE: we probably mutate the underlying file in the FilePtr by calling `seek()` and this is not thread safe
    file: F,
    reporter: Option<Box<dyn Reporter>>,
    // 是否进行校验和检查
    checksum: bool,
    // 是否已到达文件末尾
    eof: bool,
    // 最后一次读取记录的偏移量
    last_record_offset: u64,
    // 缓冲区结束位置的偏移量，在文件中的当前位置
    end_of_buffer_offset: u64,
    // 当前读取块的缓存
    buf: Vec<u8>,
    // 缓存中有效数据的长度，已经读取的数据长度
    buf_length: usize,
    // 开始读取记录的初始偏移量，文件开始读取的位置
    initial_offset: u64,

    // see the test case 'test_skip_into_multi_record'
    // 是否需要重新同步到第一个有效的完整记录,如果为 true，将快进到First record or Full record
    resyncing: bool,
}

impl<F: File> Reader<F> {
    pub fn new(
        file: F,
        reporter: Option<Box<dyn Reporter>>,
        checksum: bool,
        initial_offset: u64,
    ) -> Self {
        Reader {
            file,
            reporter,
            checksum,
            buf: vec![0; BLOCK_SIZE],
            buf_length: 0,
            eof: false,
            last_record_offset: 0,
            end_of_buffer_offset: 0,
            initial_offset,
            resyncing: initial_offset > 0,
        }
    }

    /// Deliver the file's ownership
    #[inline]
    pub fn into_file(self) -> F {
        self.file
    }

    /// 读取下一条完整的记录到给定的缓冲区中
    /// 如果成功读取返回 true，否则返回 false
    pub fn read_record(&mut self, buf: &mut Vec<u8>) -> bool {
        // 检查初始偏移量并跳过到该位置
        if self.last_record_offset < self.initial_offset && !self.skip_to_initial_block() {
            return false;
        }
        // 当前是否正在处理被分成多个片段的记录
        let mut in_fragmented_record = false;
        // 用于记录逻辑记录的起始偏移量。
        let mut prospective_record_offset = 0;
        // 循环读取物理记录并处理不同类型的记录
        loop {
            match self.read_physical_record() {
                Ok(mut record) => {
                    // 同步到下一个完整记录
                    if self.resyncing {
                        // 跳过 Middle 和 Last 类型的记录，并根据需要更新 resyncing 状态。
                        match record.t {
                            RecordType::Middle => continue,
                            RecordType::Last => {
                                self.resyncing = false;
                                continue;
                            }
                            _ => self.resyncing = false,
                        }
                    }

                    let fragment_size = record.data.len() as u64;
                    // 当前读取记录的起始偏移量
                    let physical_record_offset = self.end_of_buffer_offset
                        - self.buf_length as u64
                        - HEADER_SIZE as u64
                        - fragment_size;
                    match record.t {
                        RecordType::Full => {
                            if in_fragmented_record {
                                self.report_drop(
                                    buf.len() as u64,
                                    "partial record without end(1) for reading a new Full record",
                                );
                            }
                            // 更新last_record_offset
                            self.last_record_offset = physical_record_offset;
                            buf.clear();
                            buf.append(&mut record.data);
                            return true;
                        }
                        RecordType::First => {
                            if in_fragmented_record {
                                self.report_drop(
                                    buf.len() as u64,
                                    "partial record without end(2) for reading a new First record",
                                );
                            }
                            prospective_record_offset = physical_record_offset;

                            // 清除buf
                            buf.clear();
                            buf.append(&mut record.data);
                            in_fragmented_record = true;
                        }
                        RecordType::Middle => {
                            if !in_fragmented_record {
                                self.report_drop(
                                    fragment_size,
                                    format!(
                                        "missing start of fragmented record({:?})",
                                        RecordType::Middle
                                    )
                                    .as_str(),
                                );
                            // 继续读取
                            } else {
                                buf.append(&mut record.data);
                            }
                        }
                        RecordType::Last => {
                            if !in_fragmented_record {
                                self.report_drop(
                                    fragment_size,
                                    format!(
                                        "missing start of fragmented record({:?})",
                                        RecordType::Last
                                    )
                                    .as_str(),
                                );
                            } else {
                                buf.extend(record.data);
                                // last_record_offset 只有在完整读取到逻辑记录的最后一部分 (Last 类型的物理记录) 时才会更新，而不是first
                                self.last_record_offset = prospective_record_offset;
                                return true;
                            }
                        }
                        RecordType::Zero => {
                            /* Zero类型记录被认为是不相关的并且永远不应该被读出 */
                        }
                    }
                }
                Err(e) => {
                    match e {
                        // 缓冲区长度小于记录头的大小且文件已经结束，意味着缓冲区内没有完整的记录头
                        // 读取的数据量少于一个块大小
                        // 在解析头部之后，记录长度超过缓冲区长度且文件已经结束,这种情况表示文件在写入过程中可能未完全写入记录
                        // 读取失败EOF
                        // 读取的数据量少于一个块大小（BLOCK_SIZE）
                        ReaderError::EOF => {
                            if in_fragmented_record {
                                // 再写入record的时候崩溃或停止，导致记录不完整
                                buf.clear();
                            }
                            return false;
                        }
                        // 记录长度超过缓冲区长度且文件未结束
                        // 记录类型为 0 且数据长度为 0
                        // CRC 校验不通过
                        // 记录在 initial_offset 之前
                        ReaderError::BadRecord => {
                            if in_fragmented_record {
                                self.report_drop(
                                    buf.len() as u64,
                                    "bad record read in middle of record",
                                );
                                in_fragmented_record = false;
                                buf.clear();
                            }
                        }
                    }
                }
            }
        }
    }


    //返回最后一个记录的偏移量。
    // 用于测试。
    #[inline]
    #[allow(dead_code)]
    pub(super) fn last_record_offset(&self) -> u64 {
        self.last_record_offset
    }
    // 从文件中读取一个物理记录
    fn read_physical_record(&mut self) -> Result<Record, ReaderError> {
        loop {
            // 如果当前缓冲区的长度小于记录头的大小，意味着缓冲区内没有完整的记录头
            if self.buf_length < HEADER_SIZE {
                //清空缓冲区并尝试读取一个块
                self.clear_buf();
                if !self.eof {
                    //  尝试从文件读取数据到缓冲区
                    match self.file.read(&mut self.buf) {
                        Ok(read) => {
                            // 更新缓冲区长度和结束偏移量
                            self.end_of_buffer_offset += read as u64;
                            self.buf_length = read;
                            // 如果读取的数据量少于一个块大小（BLOCK_SIZE）通常表明已经到达文件的末尾，则设置 eof 为真，表示文件结束
                            // 返回 EOF error
                            if read < BLOCK_SIZE as usize {
                                self.eof = true;
                            }
                        }
                        Err(e) => {
                            // 如果读取失败，报告错误并返回 EOF 错误。
                            self.report_drop(BLOCK_SIZE as u64, &e.to_string());
                            self.eof = true;
                            return Err(ReaderError::EOF);
                        }
                    }
                    continue;
                } else {
                    // 缓冲区非空：这意味着在文件结束时，缓冲区中仍有数据，但不足以构成一个完整的记录头。
                    // 截断头部：可能是因为writer在写入记录头部时崩溃或停止，导致记录头部不完整。
                    return Err(ReaderError::EOF);
                }
            }
            // 解析头部
            let header = &self.buf[0..HEADER_SIZE];
            let record_type = *header.last().unwrap();
            let data_length =
                ((header[4] as usize & 0xff) | ((header[5] as usize & 0xff) << 8)) as usize;
            // 当前记录的长度，包括头部和数据部分
            let record_length = HEADER_SIZE + data_length;
            // 检查记录长度是否超过缓冲区长度
            if record_length > self.buf_length {
                let drop_size = self.buf_length;
                self.clear_buf();
                // 如果文件未结束，报告错误并返回 BadRecord
                if !self.eof {
                    self.report_drop(drop_size as u64, "bad record length");
                    return Err(BadRecord);
                }
                // 如果文件结束，返回 EOF, 这种情况表示文件在写入过程中可能未完全写入记录
                return Err(EOF);
            }

            // 处理空记录 记录类型为0且数据长度为0
            if record_type == 0 && data_length == 0 {
                self.clear_buf();
                self.report_drop(self.buf.len() as u64, "empty length record");
                return Err(BadRecord);
            }

            // 校验CRC
            if self.checksum {
                let expected = unmask(decode_fixed_32(header));
                // HEADER_SIZE - 1 to included the record type
                let actual = hash(&self.buf[HEADER_SIZE - 1..record_length]);
                //如果不匹配，清空缓冲区并报告错误。
                if expected != actual {
                    let drop_size = self.buf_length;
                    self.clear_buf();
                    self.report_drop(drop_size as u64, "checksum mismatch");
                    return Err(BadRecord);
                }
            }
            // 处理读取的数据
            let mut data = self.buf.drain(0..record_length).collect::<Vec<u8>>();
            self.buf_length -= data.len();

            // 检查记录是否在 initial_offset 之前，如果是则返回 BadRecord
            //  self.initial_offset + self.buf_length as u64 + record_length 当前记录的结束位置
            //  end_of_buffer_offset已经读取到的文件位置
            if self.end_of_buffer_offset
                < self.initial_offset + self.buf_length as u64 + record_length as u64
            {
                return Err(BadRecord);
            }

            // 去除头
            data.drain(0..HEADER_SIZE);
            return Ok(Record {
                t: RecordType::from(record_type as usize),
                data,
            });
        }
    }
    // 向reporter 对象报告数据丢失的信息
    fn report_drop(&mut self, bytes: u64, reason: &str) {
        if let Some(reporter) = self.reporter.as_mut() {
            // end_of_buffer_offset - bytes >= initial_offset 确保字节数不会超出初始偏移量
            // end_of_buffer_offset == 0 表示第一次读取块时遇到读取错误
            if self.end_of_buffer_offset == 0
                || self.end_of_buffer_offset - bytes >= self.initial_offset
            {
                reporter.corruption(bytes, reason);
            }
        }
    }


    // clear `buf` and reset `buf_length`
    fn clear_buf(&mut self) {
        self.buf = vec![0; BLOCK_SIZE];
        self.buf_length = 0;
    }

    /// 用于跳过所有在 `initial_offset` 之前的完整块,并将文件指针移动到 `initial_offset` 对应的块开始位置
    /// 返回一个布尔值，表示是否成功跳过这些块
    /// 例如 initial_offset 28 BLOCK_SIZE 16 offset_in_block为12，表明读取的是trailer，则需要跳下个块
    fn skip_to_initial_block(&mut self) -> bool {
        // 计算initial_offset在块内的偏移量offset_in_block
        let offset_in_block = self.initial_offset % BLOCK_SIZE as u64;
        //计算块的起始位置偏移
        let mut block_start_location = self.initial_offset - offset_in_block;

        // 处理尾部（trailer）情况 检查offset_in_block是否超过了块大小减去6的值，true跳到下一个数据块的起始位置
        // 确保不会从数据块的尾部开始读取数据，直接跳到下一个完整的数据块从该块的起始位置开始读取数据
        if offset_in_block > BLOCK_SIZE as u64 - 6 {
            block_start_location += BLOCK_SIZE as u64;
        }
        //更新缓冲区结束位置偏移量
        self.end_of_buffer_offset = block_start_location;
        //移动文件指针
        if block_start_location > 0 {
            if let Err(e) = self.file.seek(SeekFrom::Start(block_start_location)) {
                // 如果移动文件指针过程中发生错误，捕获错误并调用report_drop方法报告错误
                self.report_drop(block_start_location, &e.to_string());
                return false;
            }
        }
        true
    }
}
