use crate::cache::Cache;
use crate::utils::collection::HashMap;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::mem;
use std::mem::MaybeUninit;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// 指向键的原始指针包装
#[derive(Copy, Clone)]
struct KeyRef<K> {
    k: *const K,
}

impl<K: Hash> Hash for KeyRef<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        unsafe { (*self.k).hash(state) }
    }
}

impl<K: PartialEq> PartialEq for KeyRef<K> {
    fn eq(&self, other: &KeyRef<K>) -> bool {
        unsafe { (*self.k).eq(&*other.k) }
    }
}

impl<K: Eq> Eq for KeyRef<K> {}

impl<K> Default for KeyRef<K> {
    fn default() -> Self {
        KeyRef { k: ptr::null() }
    }
}

// LRU 缓存条目
struct LRUEntry<K, V> {
    key: MaybeUninit<K>,
    value: MaybeUninit<V>,
    prev: *mut LRUEntry<K, V>,
    next: *mut LRUEntry<K, V>,
    charge: usize,
}

impl<K, V> LRUEntry<K, V> {
    fn new(key: K, value: V, charge: usize) -> Self {
        LRUEntry {
            key: MaybeUninit::new(key),
            value: MaybeUninit::new(value),
            charge,
            next: ptr::null_mut(),
            prev: ptr::null_mut(),
        }
    }

    fn new_empty() -> Self {
        LRUEntry {
            key: MaybeUninit::uninit(),
            value: MaybeUninit::uninit(),
            charge: 0,
            next: ptr::null_mut(),
            prev: ptr::null_mut(),
        }
    }
}

// LRU 缓存主结构
pub struct LRUCache<K, V: Clone> {
    capacity: usize,
    inner: Arc<Mutex<LRUInner<K, V>>>,
    usage: Arc<AtomicUsize>,
    evict_hook: Option<Arc<dyn Fn(&K, &V) + Send + Sync>>,
}

// 内部数据结构
struct LRUInner<K, V> {
    table: HashMap<KeyRef<K>, Box<LRUEntry<K, V>>>,
    // head.next 是最新的条目
    head: *mut LRUEntry<K, V>,
    // tail.prev 是最旧的条目
    tail: *mut LRUEntry<K, V>,
}

impl<K, V> LRUInner<K, V> {
    // 从链表中分离节点
    fn detach(&mut self, n: *mut LRUEntry<K, V>) {
        unsafe {
            (*(*n).next).prev = (*n).prev;
            (*(*n).prev).next = (*n).next;
        }
    }

    // 将节点附加到链表头部
    fn attach(&mut self, n: *mut LRUEntry<K, V>) {
        unsafe {
            (*n).next = (*self.head).next;
            (*n).prev = self.head;
            (*self.head).next = n;
            (*(*n).next).prev = n;
        }
    }

    // 移动节点到头部（标记为最近使用）
    fn touch(&mut self, n: *mut LRUEntry<K, V>) {
        self.detach(n);
        self.attach(n);
    }
}

impl<K: Hash + Eq + Clone, V: Clone> LRUCache<K, V> {
    pub fn new(cap: usize) -> Self {
        let head = Box::into_raw(Box::new(LRUEntry::new_empty()));
        let tail = Box::into_raw(Box::new(LRUEntry::new_empty()));

        unsafe {
            (*head).next = tail;
            (*tail).prev = head;
        }

        let inner = LRUInner {
            table: HashMap::default(),
            head,
            tail,
        };

        LRUCache {
            usage: Arc::new(AtomicUsize::new(0)),
            capacity: cap,
            inner: Arc::new(Mutex::new(inner)),
            evict_hook: None,
        }
    }

    pub fn with_evict_hook<F>(cap: usize, hook: F) -> Self
    where
        F: Fn(&K, &V) + Send + Sync + 'static,
    {
        let head = Box::into_raw(Box::new(LRUEntry::new_empty()));
        let tail = Box::into_raw(Box::new(LRUEntry::new_empty()));

        unsafe {
            (*head).next = tail;
            (*tail).prev = head;
        }

        let inner = LRUInner {
            table: HashMap::default(),
            head,
            tail,
        };

        LRUCache {
            usage: Arc::new(AtomicUsize::new(0)),
            capacity: cap,
            inner: Arc::new(Mutex::new(inner)),
            evict_hook: Some(Arc::new(hook) as Arc<dyn Fn(&K, &V) + Send + Sync>),
        }
    }

    pub fn set_evict_hook<F>(&mut self, hook: F)
    where
        F: Fn(&K, &V) + Send + Sync + 'static,
    {
        self.evict_hook = Some(Arc::new(hook) as Arc<dyn Fn(&K, &V) + Send + Sync>);
    }

    // 检查键是否存在（不更新 LRU 顺序）
    pub fn contains_key(&self, key: &K) -> bool {
        let key_ref = KeyRef { k: key as *const K };
        let l = self.inner.lock().unwrap();
        l.table.contains_key(&key_ref)
    }

    // 查找条目并返回克隆的键值对（更新 LRU 顺序）
    pub fn lookup(&self, key: &K) -> Option<(K, V, usize)> {
        let key_ref = KeyRef { k: key as *const K };
        let mut l = self.inner.lock().unwrap();

        if let Some(node) = l.table.get_mut(&key_ref) {
            let p = node.as_mut() as *mut LRUEntry<K, V>;
            l.touch(p);

            unsafe {
                Some((
                    (*(*p).key.as_ptr()).clone(),
                    (*(*p).value.as_ptr()).clone(),
                    (*p).charge,
                ))
            }
        } else {
            None
        }
    }

    // 驱逐最少使用的条目
    pub fn evict_lru(&self) -> Option<(K, V, usize)> {
        let mut l = self.inner.lock().unwrap();

        // 检查是否为空
        unsafe {
            if (*l.head).next == l.tail {
                return None;
            }
        }

        // 获取最旧的条目
        let oldest = unsafe { (*l.tail).prev };
        let key_ref = KeyRef {
            k: unsafe { (*oldest).key.as_ptr() },
        };

        // 从表中移除
        if let Some(mut entry) = l.table.remove(&key_ref) {
            let charge = entry.charge;
            self.usage.fetch_sub(charge, Ordering::Release);
            l.detach(entry.as_mut());

            // 提取键值对
            let (key, value) = unsafe {
                let k = ptr::read(entry.key.as_ptr());
                let v = ptr::read(entry.value.as_ptr());
                (k, v)
            };

            // 在锁外调用回调
            drop(l);
            if let Some(ref hook) = self.evict_hook {
                hook(&key, &value);
            }

            Some((key, value, charge))
        } else {
            None
        }
    }

    // 清空缓存中的所有条目
    pub fn clear(&self) {
        let mut l = self.inner.lock().unwrap();

        // 收集所有需要调用回调的键值对
        let mut to_call_hooks = Vec::new();

        // 遍历并移除所有条目
        for (_, mut entry) in l.table.drain() {
            self.usage.fetch_sub(entry.charge, Ordering::Release);

            // 安全地提取键值对
            unsafe {
                let k = ptr::read(entry.key.as_ptr());
                let v = ptr::read(entry.value.as_ptr());
                to_call_hooks.push((k, v));
            }
        }

        // 重置链表，只保留哨兵节点
        unsafe {
            (*l.head).next = l.tail;
            (*l.tail).prev = l.head;
        }

        // 在锁外调用所有回调
        drop(l);
        if let Some(ref hook) = self.evict_hook {
            for (k, v) in to_call_hooks {
                hook(&k, &v);
            }
        }
    }

    // 内部驱逐方法，必须在持有锁的情况下调用
    fn evict_to_make_room_locked(&self, l: &mut LRUInner<K, V>, needed: usize) -> bool {
        let current_usage = self.usage.load(Ordering::Acquire);

        if current_usage + needed <= self.capacity {
            return true;
        }

        let mut evicted = 0;
        let to_evict = current_usage + needed - self.capacity;
        let mut to_call_hooks = Vec::new();

        while evicted < to_evict {
            unsafe {
                if (*l.head).next == l.tail {
                    // 恢复已移除的条目并调用钩子
                    for (k, v) in to_call_hooks {
                        if let Some(ref hook) = self.evict_hook {
                            hook(&k, &v);
                        }
                    }
                    return false; // 缓存已空
                }
            }

            let oldest = unsafe { (*l.tail).prev };
            let key_ref = KeyRef {
                k: unsafe { (*oldest).key.as_ptr() },
            };

            if let Some(mut entry) = l.table.remove(&key_ref) {
                evicted += entry.charge;
                self.usage.fetch_sub(entry.charge, Ordering::Release);
                l.detach(entry.as_mut());

                // 安全地释放内存
                unsafe {
                    let k = ptr::read(entry.key.as_ptr());
                    let v = ptr::read(entry.value.as_ptr());
                    to_call_hooks.push((k, v));
                }
            }
        }

        // 在方法结束时调用所有钩子
        for (k, v) in to_call_hooks {
            if let Some(ref hook) = self.evict_hook {
                hook(&k, &v);
            }
        }

        true
    }
}

impl<K, V> Cache<K, V> for LRUCache<K, V>
where
    K: Send + Sync + Hash + Eq + Clone + Debug,
    V: Send + Sync + Clone,
{
    fn insert(&self, key: K, value: V, charge: usize) -> Option<V> {
        if self.capacity == 0 || charge > self.capacity {
            return None;
        }

        let mut l = self.inner.lock().unwrap();
        let key_ref = KeyRef { k: &key as *const K };

        // 检查键是否已存在
        if let Some(node) = l.table.get_mut(&key_ref) {
            let p = node.as_mut() as *mut LRUEntry<K, V>;

            // 更新值并获取旧值
            let old_value = unsafe {
                let old_charge = (*p).charge;
                let old_v = ptr::read((*p).value.as_ptr());
                ptr::write((*p).value.as_mut_ptr(), value);

                // 更新 charge 并调整 usage
                if old_charge != charge {
                    (*p).charge = charge;
                    if charge > old_charge {
                        let increase = charge - old_charge;
                        // 检查是否需要驱逐以腾出空间
                        if self.usage.load(Ordering::Acquire) + increase > self.capacity {
                            // 需要驱逐，但当前项正在使用，所以暂时移除它
                            l.detach(p);
                            self.usage.fetch_sub(old_charge, Ordering::Release);

                            // 驱逐其他项
                            if !self.evict_to_make_room_locked(&mut l, charge) {
                                // 无法腾出足够空间，恢复原值
                                ptr::write((*p).value.as_mut_ptr(), old_v.clone());
                                (*p).charge = old_charge;
                                l.attach(p);
                                self.usage.fetch_add(old_charge, Ordering::Release);
                                return None;
                            }

                            // 重新附加并更新使用量
                            l.attach(p);
                            self.usage.fetch_add(charge, Ordering::Release);
                        } else {
                            self.usage.fetch_add(increase, Ordering::Release);
                        }
                    } else {
                        self.usage.fetch_sub(old_charge - charge, Ordering::Release);
                    }
                }

                old_v
            };

            l.touch(p);

            // 在锁外调用回调
            drop(l);
            if let Some(ref hook) = self.evict_hook {
                hook(&key, &old_value);
            }

            Some(old_value)
        } else {
            // 新插入的情况
            // 确保有足够空间
            if !self.evict_to_make_room_locked(&mut l, charge) {
                return None;
            }

            let mut entry = Box::new(LRUEntry::new(key, value, charge));
            self.usage.fetch_add(charge, Ordering::Release);
            l.attach(entry.as_mut());

            l.table.insert(
                KeyRef { k: entry.key.as_ptr() },
                entry,
            );

            None
        }
    }

    fn get(&self, key: &K) -> Option<V> {
        let key_ref = KeyRef { k: key as *const K };
        let mut l = self.inner.lock().unwrap();

        if let Some(node) = l.table.get_mut(&key_ref) {
            let p = node.as_mut() as *mut LRUEntry<K, V>;
            l.touch(p);
            Some(unsafe { (*(*p).value.as_ptr()).clone() })
        } else {
            None
        }
    }

    fn erase(&self, key: &K) {
        let key_ref = KeyRef { k: key as *const K };
        let mut l = self.inner.lock().unwrap();

        if let Some(mut entry) = l.table.remove(&key_ref) {
            self.usage.fetch_sub(entry.charge, Ordering::Release);
            l.detach(entry.as_mut());

            // 安全地提取键值
            let (k, v) = unsafe {
                let k = ptr::read(entry.key.as_ptr());
                let v = ptr::read(entry.value.as_ptr());
                (k, v)
            };

            // 在锁外调用回调
            drop(l);
            if let Some(ref hook) = self.evict_hook {
                hook(&k, &v);
            }
        }
    }

    #[inline]
    fn total_charge(&self) -> usize {
        self.usage.load(Ordering::Acquire)
    }

    fn clear(&self) {
        self.clear();
    }
}

impl<K, V: Clone> Drop for LRUCache<K, V> {
    fn drop(&mut self) {
        let mut l = self.inner.lock().unwrap();

        // 清空哈希表并释放所有条目
        for (_, mut entry) in l.table.drain() {
            unsafe {
                ptr::drop_in_place(entry.key.as_mut_ptr());
                ptr::drop_in_place(entry.value.as_mut_ptr());
            }
        }

        // 释放哨兵节点
        unsafe {
            let _ = Box::from_raw(l.head);
            let _ = Box::from_raw(l.tail);
        }
    }
}

// 确保线程安全
unsafe impl<K: Send, V: Send + Clone> Send for LRUCache<K, V> {}
unsafe impl<K: Sync, V: Sync + Clone> Sync for LRUCache<K, V> {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::sync::{Arc as StdArc, Mutex as StdMutex};

    const CACHE_SIZE: usize = 100;

    struct CacheTest {
        cache: LRUCache<u32, u32>,
        deleted_kv: StdArc<StdMutex<Vec<(u32, u32)>>>,
    }

    impl CacheTest {
        fn new(cap: usize) -> Self {
            let deleted_kv = StdArc::new(StdMutex::new(vec![]));
            let cloned = deleted_kv.clone();
            let cache = LRUCache::with_evict_hook(cap, move |k: &u32, v: &u32| {
                cloned.lock().unwrap().push((*k, *v));
            });
            Self { cache, deleted_kv }
        }

        fn get(&self, key: u32) -> Option<u32> {
            self.cache.get(&key)
        }

        fn insert(&self, key: u32, value: u32) {
            self.cache.insert(key, value, 1);
        }

        fn insert_with_charge(&self, key: u32, value: u32, charge: usize) {
            self.cache.insert(key, value, charge);
        }

        fn erase(&self, key: u32) {
            self.cache.erase(&key);
        }

        fn assert_deleted_kv(&self, index: usize, (key, val): (u32, u32)) {
            assert_eq!((key, val), self.deleted_kv.lock().unwrap()[index]);
        }

        fn assert_get(&self, key: u32, want: u32) -> u32 {
            let h = self.cache.get(&key).unwrap();
            assert_eq!(want, h);
            h
        }
    }

    #[test]
    fn test_hit_and_miss() {
        let cache = CacheTest::new(CACHE_SIZE);
        assert_eq!(None, cache.get(100));
        cache.insert(100, 101);
        assert_eq!(Some(101), cache.get(100));
        assert_eq!(None, cache.get(200));
        assert_eq!(None, cache.get(300));

        cache.insert(200, 201);
        assert_eq!(Some(101), cache.get(100));
        assert_eq!(Some(201), cache.get(200));
        assert_eq!(None, cache.get(300));

        cache.insert(100, 102);
        assert_eq!(Some(102), cache.get(100));
        assert_eq!(Some(201), cache.get(200));
        assert_eq!(None, cache.get(300));

        assert_eq!(1, cache.deleted_kv.lock().unwrap().len());
        cache.assert_deleted_kv(0, (100, 101));
    }

    #[test]
    fn test_erase() {
        let cache = CacheTest::new(CACHE_SIZE);
        cache.erase(200);
        assert_eq!(0, cache.deleted_kv.lock().unwrap().len());

        cache.insert(100, 101);
        cache.insert(200, 201);
        cache.erase(100);

        assert_eq!(None, cache.get(100));
        assert_eq!(Some(201), cache.get(200));
        assert_eq!(1, cache.deleted_kv.lock().unwrap().len());
        cache.assert_deleted_kv(0, (100, 101));

        cache.erase(100);
        assert_eq!(None, cache.get(100));
        assert_eq!(Some(201), cache.get(200));
        assert_eq!(1, cache.deleted_kv.lock().unwrap().len());
    }

    #[test]
    fn test_entries_are_pinned() {
        let cache = CacheTest::new(CACHE_SIZE);
        cache.insert(100, 101);
        let v1 = cache.assert_get(100, 101);
        assert_eq!(v1, 101);
        cache.insert(100, 102);
        let v2 = cache.assert_get(100, 102);
        assert_eq!(1, cache.deleted_kv.clone().lock().unwrap().len());
        cache.assert_deleted_kv(0, (100, 101));
        assert_eq!(v1, 101);
        assert_eq!(v2, 102);

        cache.erase(100);
        assert_eq!(v1, 101);
        assert_eq!(v2, 102);
        assert_eq!(None, cache.get(100));
        assert_eq!(
            vec![(100, 101), (100, 102)],
            cache.deleted_kv.lock().unwrap().clone()
        );
    }

    #[test]
    fn test_eviction_policy() {
        let cache = CacheTest::new(CACHE_SIZE);
        cache.insert(100, 101);
        cache.insert(200, 201);
        cache.insert(300, 301);

        // frequently used entry must be kept around
        for i in 0..(CACHE_SIZE + 100) as u32 {
            cache.insert(1000 + i, 2000 + i);
            assert_eq!(Some(2000 + i), cache.get(1000 + i));
            assert_eq!(Some(101), cache.get(100));
        }
        assert!(cache.cache.inner.lock().unwrap().table.len() <= CACHE_SIZE);
        assert_eq!(Some(101), cache.get(100));
        assert_eq!(None, cache.get(200));
        assert_eq!(None, cache.get(300));
    }

    #[test]
    fn test_use_exceeds_cache_size() {
        let cache = CacheTest::new(CACHE_SIZE);
        let extra = 100;
        let total = CACHE_SIZE + extra;
        // overfill the cache, keeping handles on all inserted entries
        for i in 0..total as u32 {
            cache.insert(1000 + i, 2000 + i)
        }

        // check that all the entries can be found in the cache
        for i in 0..total as u32 {
            if i < extra as u32 {
                assert_eq!(None, cache.get(1000 + i))
            } else {
                assert_eq!(Some(2000 + i), cache.get(1000 + i))
            }
        }
    }

    #[test]
    fn test_heavy_entries() {
        let cache = CacheTest::new(CACHE_SIZE);
        let light = 1;
        let heavy = 10;
        let mut added = 0;
        let mut index = 0;
        while added < 2 * CACHE_SIZE {
            let weight = if index & 1 == 0 { light } else { heavy };
            cache.insert_with_charge(index, 1000 + index, weight);
            added += weight;
            index += 1;
        }
        let mut cache_weight = 0;
        for i in 0..index {
            let weight = if i & 1 == 0 { light } else { heavy };
            if let Some(val) = cache.get(i) {
                cache_weight += weight;
                assert_eq!(1000 + i, val);
            }
        }
        assert!(cache_weight <= CACHE_SIZE);
    }

    #[test]
    fn test_zero_size_cache() {
        let cache = CacheTest::new(0);
        cache.insert(100, 101);
        assert_eq!(None, cache.get(100));
    }

    #[test]
    fn test_evict_lru() {
        let cache = CacheTest::new(4);
        cache.insert(100, 101);
        cache.insert(101, 102);
        cache.insert(102, 103);
        cache.insert(103, 104);

        let evicted = cache.cache.evict_lru();
        assert_eq!(evicted, Some((100, 101, 1)));

        assert_eq!(None, cache.get(100));
        assert_eq!(Some(102), cache.get(101));
        assert_eq!(Some(103), cache.get(102));
        assert_eq!(Some(104), cache.get(103));
    }

    #[test]
    fn test_evict_lru_with_vec_keys() {
        let cache = LRUCache::new(2);
        let vec1 = vec![1, 2, 3];
        let vec2 = vec![4, 5, 6];

        cache.insert(vec1.clone(), 123, 1);
        cache.insert(vec2.clone(), 456, 1);

        let evicted = cache.evict_lru();
        assert_eq!(evicted, Some((vec1.clone(), 123, 1)));

        assert_eq!(None, cache.get(&vec1));
        assert_eq!(Some(456), cache.get(&vec2));
    }

    #[test]
    fn test_charge_updates() {
        let cache = CacheTest::new(10);

        // Insert with charge 3
        cache.insert_with_charge(1, 100, 3);
        assert_eq!(cache.cache.total_charge(), 3);

        // Update with smaller charge
        cache.insert_with_charge(1, 101, 1);
        assert_eq!(cache.cache.total_charge(), 1);
        assert_eq!(cache.get(1), Some(101));

        // Update with larger charge
        cache.insert_with_charge(1, 102, 5);
        assert_eq!(cache.cache.total_charge(), 5);
        assert_eq!(cache.get(1), Some(102));
    }

    #[test]
    fn test_concurrent_access() {
        let cache = StdArc::new(LRUCache::new(1000));
        let mut handles = vec![];

        // Spawn multiple threads that read and write concurrently
        for i in 0..10 {
            let cache_clone = cache.clone();
            let handle = thread::spawn(move || {
                for j in 0..100 {
                    let key = (i * 100 + j) % 200;
                    cache_clone.insert(key, key * 2, 1);
                    cache_clone.get(&key);
                }
            });
            handles.push(handle);
        }

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify cache is still in a valid state
        assert!(cache.total_charge() <= 1000);
    }

    #[test]
    fn test_contains_key_no_touch() {
        let cache = CacheTest::new(3);
        cache.insert(1, 10);
        cache.insert(2, 20);
        cache.insert(3, 30);

        // contains_key should not affect LRU order
        assert!(cache.cache.contains_key(&1));

        // Insert a new item, should evict 1 if contains_key didn't touch it
        cache.insert(4, 40);
        assert_eq!(cache.get(1), None);
        assert_eq!(cache.get(2), Some(20));
        assert_eq!(cache.get(3), Some(30));
        assert_eq!(cache.get(4), Some(40));
    }
}