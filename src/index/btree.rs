use std::collections::{Bound, BTreeMap};
use std::ops::RangeInclusive;
use crate::index::{Indexer, KeyDirEntry};
use std::error::Error;
use std::sync::{Arc, Mutex};

// mutex 需要Sync才能实现Send
// Arc::new(BTreeIndexer).clone()和clone如果只有Arc是一样的,因为clone会派生调用inner的clone，
// 当有其他字段的时候，arc只增加计数不会去clone其他字段，clone会调用每个字段的clone方法
#[derive(Clone)]
pub struct BTreeIndexer {
    inner: Arc<Mutex<BTreeMap<Vec<u8>, KeyDirEntry>>>,
}

impl BTreeIndexer {
    pub fn new() -> Self {
        BTreeIndexer {
            inner: Arc::new(Mutex::new(BTreeMap::new()))
        }
    }
}

//  btreeIndexer 支持范围查询功能
impl Indexer for BTreeIndexer {
    fn put(& self, key: &[u8], position: KeyDirEntry) -> Option<KeyDirEntry> {
        let mut map = self.inner.lock().unwrap();
        map.insert(key.to_vec(), position)
    }

    fn get(&self, key: &[u8]) -> Option<KeyDirEntry> {
        let map = self.inner.lock().unwrap();
        map.get(key).copied()
    }

    fn delete(& self, key: &[u8]) -> Option<KeyDirEntry> {
        let mut map = self.inner.lock().unwrap();
        map.remove(key)
    }

    fn size(&self) -> usize {
        let map = self.inner.lock().unwrap();
        map.len()
    }

    fn ascend<F>(&self, mut handle_fn: F) -> Result<(), Box<dyn Error>>
        where
            F: FnMut(&[u8], &KeyDirEntry) -> Result<bool, Box<dyn Error>>,
    {
        let map = self.inner.lock().unwrap();
        for (key, pos) in map.iter() {
            if !handle_fn(key, pos)? {
                break;
            }
        }
        Ok(())
    }

    fn ascend_range<F>(&self, range: RangeInclusive<&[u8]>, mut handle_fn: F) -> Result<(), Box<dyn Error>>
        where
            F: FnMut(&[u8], &KeyDirEntry) -> Result<bool, Box<dyn Error>>,
    {
        let map = self.inner.lock().unwrap();
        let (start, end) = range.into_inner();
        let range = (
            Bound::Included(start.to_vec()),
            Bound::Included(end.to_vec()),
        );
        for (key, pos) in map.range(range) {
            if !handle_fn(key, pos)? {
                break;
            }
        }
        Ok(())
    }

    fn ascend_greater_or_equal<F>(&self, key: &[u8], mut handle_fn: F) -> Result<(), Box<dyn Error>>
        where
            F: FnMut(&[u8], &KeyDirEntry) -> Result<bool, Box<dyn Error>>,
    {
        let map = self.inner.lock().unwrap();
        let range = (Bound::Included(key.to_vec()), Bound::Unbounded);
        for (k, pos) in map.range(range) {
            if !handle_fn(k, pos)? {
                break;
            }
        }
        Ok(())
    }

    fn descend<F>(&self, mut handle_fn: F) -> Result<(), Box<dyn Error>>
        where
            F: FnMut(&[u8], &KeyDirEntry) -> Result<bool, Box<dyn Error>>,
    {
        let map = self.inner.lock().unwrap();
        for (key, pos) in map.iter().rev() {
            if !handle_fn(key, pos)? {
                break;
            }
        }
        Ok(())
    }

    fn descend_range<F>(&self, range: RangeInclusive<&[u8]>, mut handle_fn: F) -> Result<(), Box<dyn Error>>
        where
            F: FnMut(&[u8], &KeyDirEntry) -> Result<bool, Box<dyn Error>>,
    {
        let map = self.inner.lock().unwrap();
        let (start, end) = range.into_inner();
        let range = (
            Bound::Included(start.to_vec()),
            Bound::Included(end.to_vec()),
        );
        for (key, pos) in map.range(range).rev() {
            if !handle_fn(key, pos)? {
                break;
            }
        }
        Ok(())
    }

    fn descend_less_or_equal<F>(&self, key: &[u8], mut handle_fn: F) -> Result<(), Box<dyn Error>>
        where
            F: FnMut(&[u8], &KeyDirEntry) -> Result<bool, Box<dyn Error>>,
    {
        let map = self.inner.lock().unwrap();
        let range = (Bound::Unbounded, Bound::Included(key.to_vec()));
        for (k, pos) in map.range(range).rev() {
            if !handle_fn(k, pos)? {
                break;
            }
        }
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::thread;
    use crate::index::btree::BTreeIndexer;
    use crate::index::{Indexer, KeyDirEntry};

    #[test]
    fn test_btree_index() {
        let mut indexer = BTreeIndexer::new();
        let pos = KeyDirEntry {
            segment_id: 1,
            offset: 100,
            size: 50,
        };

        indexer.put(b"key1", pos);
        indexer.put(b"key2", pos);

        if let Some(position) = indexer.get(b"key1") {
            println!("{:?}", position);
        }

        indexer.ascend(|key, pos| {
            println!("{:?}: {:?}", key, pos);
            Ok(true)
        }).unwrap();
    }
    #[test]
    fn test_memory_btree_put_get() {
        let mut mt = BTreeIndexer::new();

        let key = b"testKey";
        let chunk_position = KeyDirEntry {
            segment_id: 1,
            offset: 100,
            size: 50,
        };

        // Test Put
        let old_pos = mt.put(key, chunk_position);
        assert!(old_pos.is_none(), "expected nil, got {:?}", old_pos);

        // Test Get
        let got_pos = mt.get(key).unwrap();
        assert_eq!(chunk_position.offset, got_pos.offset, "expected {:?}, got {:?}", chunk_position, got_pos);
    }

    #[test]
    fn test_memory_btree_delete() {
        let mut mt = BTreeIndexer::new();

        let key = b"testKey";
        let chunk_position = KeyDirEntry {
            segment_id: 1,
            offset: 100,
            size: 50,
        };

        mt.put(key, chunk_position);

        // Test Delete
        let del_pos = mt.delete(key).unwrap();
        assert_eq!(chunk_position.offset, del_pos.offset, "expected {:?}, got {:?}", chunk_position, del_pos);

        // Ensure the key is deleted
        assert!(mt.get(key).is_none(), "expected nil, got value");
    }

    #[test]
    fn test_memory_btree_size() {
        let mut mt = BTreeIndexer::new();

        assert_eq!(mt.size(), 0, "expected size to be 0, got {}", mt.size());

        let key = b"testKey";
        let chunk_position = KeyDirEntry {
            segment_id: 1,
            offset: 100,
            size: 50,
        };

        mt.put(key, chunk_position);

        assert_eq!(mt.size(), 1, "expected size to be 1, got {}", mt.size());
    }

    #[test]
    fn test_memory_btree_ascend_descend() {
        let mut mt = BTreeIndexer::new();

        let data = vec![
            ("apple", KeyDirEntry { segment_id: 1, offset: 100, size: 50 }),
            ("banana", KeyDirEntry { segment_id: 1, offset: 200, size: 50 }),
            ("cherry", KeyDirEntry { segment_id: 1, offset: 300, size: 50 }),
        ];

        for (key, pos) in data.iter() {
            mt.put(key.as_bytes(), *pos);
        }

        // Test Ascend
        let mut prev_key = vec![];
        mt.ascend(|key, pos| {
            if !prev_key.is_empty() && prev_key >= Vec::from(key) {
                return Err("items are not in ascending order".into());
            }
            prev_key = key.to_vec();
            Ok(true)
        }).unwrap();

        // Test Descend
        prev_key = b"zzzzzz".to_vec();
        mt.descend(|key, pos| {
            if prev_key <= Vec::from(key) {
                return Err("items are not in descending order".into());
            }
            prev_key = key.to_vec();
            Ok(true)
        }).unwrap();
    }

    #[test]
    fn test_memory_btree_ascend_range_descend_range() {
        let mut mt = BTreeIndexer::new();

        let data = vec![
            ("apple", KeyDirEntry { segment_id: 1, offset: 100, size: 50 }),
            ("banana", KeyDirEntry { segment_id: 1, offset: 200, size: 50 }),
            ("cherry", KeyDirEntry { segment_id: 1, offset: 300, size: 50 }),
            ("date", KeyDirEntry { segment_id: 1, offset: 400, size: 50 }),
            ("grape", KeyDirEntry { segment_id: 1, offset: 500, size: 50 }),
        ];

        for (key, pos) in data.iter() {
            mt.put(key.as_bytes(), *pos);
        }

        // Test AscendRange
        println!("Testing AscendRange:");
        mt.ascend_range(b"banana".as_ref()..=b"grape".as_ref(), |key, pos| {
            println!("Key: {}, Position: {:?}", std::str::from_utf8(key).unwrap(), pos);
            Ok(true)
        }).unwrap();

        // Test DescendRange
        println!("Testing DescendRange:");
        mt.descend_range(b"cherry".as_ref()..=b"date".as_ref(), |key, pos| {
            println!("Key: {}, Position: {:?}", std::str::from_utf8(key).unwrap(), pos);
            Ok(true)
        }).unwrap();
    }

    #[test]
    fn test_memory_btree_ascend_greater_or_equal_descend_less_or_equal() {
        let mut mt = BTreeIndexer::new();

        let data = vec![
            ("apple", KeyDirEntry { segment_id: 1, offset: 100, size: 50 }),
            ("banana", KeyDirEntry { segment_id: 1, offset: 200, size: 50 }),
            ("cherry", KeyDirEntry { segment_id: 1, offset: 300, size: 50 }),
            ("date", KeyDirEntry { segment_id: 1, offset: 400, size: 50 }),
            ("grape", KeyDirEntry { segment_id: 1, offset: 500, size: 50 }),
        ];

        for (key, pos) in data.iter() {
            mt.put(key.as_bytes(), *pos);
        }

        // Test AscendGreaterOrEqual
        println!("Testing AscendGreaterOrEqual:");
        mt.ascend_greater_or_equal(b"cherry", |key, pos| {
            println!("Key: {}, Position: {:?}", std::str::from_utf8(key).unwrap(), pos);
            Ok(true)
        }).unwrap();

        // Test DescendLessOrEqual
        println!("Testing DescendLessOrEqual:");
        mt.descend_less_or_equal(b"date", |key, pos| {
            println!("Key: {}, Position: {:?}", std::str::from_utf8(key).unwrap(), pos);
            Ok(true)
        }).unwrap();
    }
    #[test]
    fn test_concurrent_access() {
        let indexer = BTreeIndexer::new();

        let handles: Vec<_> = (0..10)
            .map(|i| {
                let indexer = indexer.clone();
                thread::spawn(move || {
                    let key = format!("key{}", i).into_bytes();
                    let position = KeyDirEntry {
                        segment_id: i,
                        offset: i as u64 * 100,
                        size: 50,
                    };

                    indexer.put(&key, position);
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }
        // Verify the results
        for i in 0..10 {
            let key = format!("key{}", i).into_bytes();
            let position = KeyDirEntry {
                segment_id: i,
                offset: i as u64 * 100,
                size: 50,
            };

            let stored_position = indexer.get(&key).unwrap();
            assert_eq!(stored_position, position, "expected {:?}, got {:?}", position, stored_position);
        }
    }
}