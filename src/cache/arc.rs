use std::fmt::Debug;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use crate::cache::Cache;
use crate::cache::lru::LRUCache;

/// ARC 缓存的内部状态
struct ArcCacheInner<K, V: Clone> {
    /// T1: 最近访问的条目（LRU）
    recent_set: LRUCache<K, V>,
    /// B1: 最近从 T1 驱逐的条目的 ghost 列表
    recent_evicted: LRUCache<K, ()>,
    /// T2: 频繁访问的条目（LFU）
    frequent_set: LRUCache<K, V>,
    /// B2: 最近从 T2 驱逐的条目的 ghost 列表
    frequent_evicted: LRUCache<K, ()>,
    /// 自适应参数 p：T1 的目标大小
    p: usize,
}

/// ARC (Adaptive Replacement Cache) 实现
///
/// ARC 是一种自适应缓存替换算法，结合了 LRU 和 LFU 的优点。
/// 它维护四个列表：
/// - T1: 最近访问一次的条目
/// - T2: 最近访问多次的条目
/// - B1: 最近从 T1 驱逐的条目的元数据
/// - B2: 最近从 T2 驱逐的条目的元数据
pub struct ArcCache<K, V: Clone> {
    /// 缓存的总容量
    capacity: usize,
    /// 内部状态
    inner: Arc<Mutex<ArcCacheInner<K, V>>>,
    /// 驱逐回调
    evict_hook: Option<Arc<dyn Fn(&K, &V) + Send + Sync>>,
}

impl<K: Hash + Eq + Clone, V: Clone> ArcCache<K, V> {
    /// 创建一个新的 ARC 缓存
    pub fn new(cap: usize) -> Self {
        let inner = ArcCacheInner {
            recent_set: LRUCache::new(cap),
            recent_evicted: LRUCache::new(cap),
            frequent_set: LRUCache::new(cap),
            frequent_evicted: LRUCache::new(cap),
            p: 0,
        };

        ArcCache {
            capacity: cap,
            inner: Arc::new(Mutex::new(inner)),
            evict_hook: None,
        }
    }

    /// 创建一个带有驱逐回调的 ARC 缓存
    pub fn with_evict_hook<F>(cap: usize, hook: F) -> Self
    where
        F: Fn(&K, &V) + Send + Sync + 'static,
    {
        let evict_hook = Arc::new(hook) as Arc<dyn Fn(&K, &V) + Send + Sync>;

        // 创建没有回调的 LRU 缓存
        // 我们将在 ARC 层处理驱逐回调
        let inner = ArcCacheInner {
            recent_set: LRUCache::new(cap),
            recent_evicted: LRUCache::new(cap),
            frequent_set: LRUCache::new(cap),
            frequent_evicted: LRUCache::new(cap),
            p: 0,
        };

        ArcCache {
            capacity: cap,
            inner: Arc::new(Mutex::new(inner)),
            evict_hook: Some(evict_hook),
        }
    }
}

impl<K, V> ArcCache<K, V>
where
    K: Send + Sync + Hash + Eq + Debug + Clone,
    V: Send + Sync + Clone,
{
    /// 调整自适应参数 p
    ///
    /// - from_b1 = true: 命中 B1，增加 p（增大 T1）
    /// - from_b1 = false: 命中 B2，减少 p（增大 T2）
    fn adjust_p(&self, inner: &mut ArcCacheInner<K, V>, from_b1: bool) {
        let b1_len = inner.recent_evicted.total_charge();
        let b2_len = inner.frequent_evicted.total_charge();

        if b1_len == 0 && b2_len == 0 {
            return;
        }

        let delta = if from_b1 {
            // 命中 B1：增加 p
            if b2_len > 0 {
                std::cmp::max(1, b2_len / b1_len.max(1))
            } else {
                1
            }
        } else {
            // 命中 B2：减少 p
            if b1_len > 0 {
                std::cmp::max(1, b1_len / b2_len.max(1))
            } else {
                1
            }
        };

        if from_b1 {
            inner.p = (inner.p + delta).min(self.capacity);
        } else {
            inner.p = inner.p.saturating_sub(delta);
        }
    }

    /// 替换策略：当缓存满时决定从哪个集合驱逐
    fn replace(&self, inner: &mut ArcCacheInner<K, V>, hit_in_b2: bool) {
        let t1_len = inner.recent_set.total_charge();
        let t2_len = inner.frequent_set.total_charge();

        if t1_len + t2_len == 0 {
            return;
        }

        // 决定从哪个集合驱逐
        let evict_from_t1 = if t1_len > inner.p {
            true
        } else if t1_len < inner.p {
            false
        } else {
            // t1_len == p 时，如果命中 B2 则从 T1 驱逐，否则从 T2 驱逐
            hit_in_b2
        };

        if evict_from_t1 && t1_len > 0 {
            // 从 T1 驱逐到 B1
            if let Some((key, value, charge)) = inner.recent_set.evict_lru() {
                // 调用驱逐回调
                if let Some(ref hook) = self.evict_hook {
                    hook(&key, &value);
                }
                inner.recent_evicted.insert(key, (), charge);
            }
        } else if t2_len > 0 {
            // 从 T2 驱逐到 B2
            if let Some((key, value, charge)) = inner.frequent_set.evict_lru() {
                // 调用驱逐回调
                if let Some(ref hook) = self.evict_hook {
                    hook(&key, &value);
                }
                inner.frequent_evicted.insert(key, (), charge);
            }
        } else if t1_len > 0 {
            // 如果 T2 为空，还是从 T1 驱逐
            if let Some((key, value, charge)) = inner.recent_set.evict_lru() {
                // 调用驱逐回调
                if let Some(ref hook) = self.evict_hook {
                    hook(&key, &value);
                }
                inner.recent_evicted.insert(key, (), charge);
            }
        }
    }

    /// 确保 ghost 列表不超过其容量限制
    fn maintain_ghost_lists(&self, inner: &mut ArcCacheInner<K, V>) {
        // B1 的大小不应超过 c - p
        let b1_target = self.capacity.saturating_sub(inner.p);
        while inner.recent_evicted.total_charge() > b1_target {
            inner.recent_evicted.evict_lru();
        }

        // B2 的大小不应超过 p
        while inner.frequent_evicted.total_charge() > inner.p {
            inner.frequent_evicted.evict_lru();
        }
    }

    /// 获取缓存的当前状态（用于调试）
    #[allow(dead_code)]
    pub fn stats(&self) -> (usize, usize, usize, usize, usize) {
        let inner = self.inner.lock().unwrap();
        (
            inner.recent_set.total_charge(),
            inner.recent_evicted.total_charge(),
            inner.frequent_set.total_charge(),
            inner.frequent_evicted.total_charge(),
            inner.p,
        )
    }

    fn ensure_space(&self, inner: &mut ArcCacheInner<K, V>, additional: usize, hit_in_b2: bool) {
        let mut current_total = inner.recent_set.total_charge() + inner.frequent_set.total_charge();
        while current_total + additional > self.capacity {
            // 如果缓存为空无法继续驱逐
            if inner.recent_set.total_charge()==0 && inner.recent_set.total_charge()==0 {
                break;
            }
            self.replace(inner, hit_in_b2);
            current_total = inner.recent_set.total_charge() + inner.frequent_set.total_charge();
        }
    }

}

impl<K, V> Cache<K, V> for ArcCache<K, V>
where
    K: Send + Sync + Hash + Eq + Debug + Clone,
    V: Send + Sync + Clone,
{
    fn insert(&self, key: K, value: V, charge: usize) -> Option<V> {
        if charge > self.capacity {
            return None;
        }

        let mut inner = self.inner.lock().unwrap();

        // 情况 1: 键在 T2 (frequent_set) 中
        if let Some((_, old_value, old_charge)) = inner.frequent_set.lookup(&key) {
            // 如果 charge 改变了，需要调整容量
            if old_charge != charge {
                inner.frequent_set.erase(&key);

                // 确保有足够空间
                self.ensure_space(&mut inner, charge, false);
                inner.frequent_set.insert(key.clone(), value, charge);

                // 调用驱逐回调
                if let Some(ref hook) = self.evict_hook {
                    hook(&key, &old_value);
                }
                return Some(old_value);
            } else {
                // charge 没变，直接更新
                inner.frequent_set.insert(key, value, charge);
                return Some(old_value);
            }
        }

        // 情况 2: 键在 T1 (recent_set) 中
        if let Some((k, old_value, old_charge)) = inner.recent_set.lookup(&key) {
            // 从 T1 移到 T2（提升为频繁访问）
            inner.recent_set.erase(&key);

            // 确保有足够空间
            self.ensure_space(&mut inner, charge, false);
            inner.frequent_set.insert(k, value, charge);
            return Some(old_value);
        }

        // 情况 3: 键在 B2 (frequent_evicted) 中
        if inner.frequent_evicted.contains_key(&key) {
            // 命中 B2：减少 p（增大 T2 的目标大小）
            self.adjust_p(&mut inner, false);

            // 确保有足够空间
            self.ensure_space(&mut inner, charge, true);
            // 从 B2 移除并插入到 T2
            inner.frequent_evicted.erase(&key);
            inner.frequent_set.insert(key, value, charge);

            // 维护 ghost 列表大小
            self.maintain_ghost_lists(&mut inner);
            return None;
        }

        // 情况 4: 键在 B1 (recent_evicted) 中
        if inner.recent_evicted.contains_key(&key) {
            // 命中 B1：增加 p（增大 T1 的目标大小）
            self.adjust_p(&mut inner, true);

            // 确保有足够空间
            self.ensure_space(&mut inner, charge, false);
            // 从 B1 移除并插入到 T2（注意：根据 ARC 算法，从 B1 恢复的项直接进入 T2）
            inner.recent_evicted.erase(&key);
            inner.frequent_set.insert(key, value, charge);

            // 维护 ghost 列表大小
            self.maintain_ghost_lists(&mut inner);
            return None;
        }

        // 情况 5: 完全缓存未命中
        self.ensure_space(&mut inner, charge, false);
        // 新条目总是先进入 T1
        let result = inner.recent_set.insert(key, value, charge);

        // 维护 ghost 列表大小
        self.maintain_ghost_lists(&mut inner);

        result
    }

    fn get(&self, key: &K) -> Option<V> {
        let mut inner = self.inner.lock().unwrap();

        // 检查 T1 (recent_set)
        if let Some((k, v, charge)) = inner.recent_set.lookup(&key) {
            // 将条目从 T1 移到 T2（提升为频繁访问）
            inner.recent_set.erase(&key);
            inner.frequent_set.insert(k, v.clone(), charge);
            return Some(v);
        }

        // 检查 T2 (frequent_set)
        if let Some(value) = inner.frequent_set.get(&key) {
            return Some(value);
        }

        None
    }

    fn erase(&self, key: &K) {
        let mut inner = self.inner.lock().unwrap();

        // 如果在 recent_set 或 frequent_set 中，需要调用驱逐回调
        if let Some((_, value, _)) = inner.recent_set.lookup(key) {
            if let Some(ref hook) = self.evict_hook {
                hook(key, &value);
            }
        } else if let Some((_, value, _)) = inner.frequent_set.lookup(key) {
            if let Some(ref hook) = self.evict_hook {
                hook(key, &value);
            }
        }

        // 从所有列表中删除
        inner.recent_set.erase(key);
        inner.frequent_set.erase(key);
        inner.recent_evicted.erase(key);
        inner.frequent_evicted.erase(key);
    }

    fn total_charge(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.recent_set.total_charge() + inner.frequent_set.total_charge()
    }

    fn clear(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.recent_set.clear();
        inner.frequent_set.clear();
        inner.recent_evicted.clear();
        inner.frequent_evicted.clear();
    }
}

// 确保线程安全
unsafe impl<K: Send, V: Send + Clone> Send for ArcCache<K, V> {}
unsafe impl<K: Sync, V: Sync + Clone> Sync for ArcCache<K, V> {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Cache;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;

    #[test]
    fn test_basic_arc_operations() {
        let cache = ArcCache::new(3);

        // 插入三个元素
        assert_eq!(cache.insert("a", 1, 1), None);
        assert_eq!(cache.insert("b", 2, 1), None);
        assert_eq!(cache.insert("c", 3, 1), None);

        // 验证都能获取到
        assert_eq!(cache.get(&"a"), Some(1));
        assert_eq!(cache.get(&"b"), Some(2));
        assert_eq!(cache.get(&"c"), Some(3));

        // 现在 a, b, c 都在 T2 中（因为 get 操作）

        // 插入第四个元素
        assert_eq!(cache.insert("d", 4, 1), None);

        // 应该还能获取到所有元素（因为它们都被访问过）
        assert!(cache.get(&"a").is_some() || cache.get(&"b").is_some() ||
            cache.get(&"c").is_some() || cache.get(&"d").is_some());
    }

    #[test]
    fn test_arc_adaptation() {
        let cache = ArcCache::new(4);

        // 模拟扫描模式：连续插入多个只访问一次的元素
        for i in 0..8 {
            cache.insert(i, i * 10, 1);
        }

        // 现在缓存中应该有最近的 4 个元素
        assert_eq!(cache.get(&7), Some(70));
        assert_eq!(cache.get(&6), Some(60));
        assert_eq!(cache.get(&5), Some(50));
        assert_eq!(cache.get(&4), Some(40));

        // 早期的元素应该被驱逐
        assert_eq!(cache.get(&0), None);
        assert_eq!(cache.get(&1), None);

        // 多次访问某些元素，使它们成为频繁访问
        for _ in 0..3 {
            assert_eq!(cache.get(&7), Some(70));
            assert_eq!(cache.get(&6), Some(60));
        }

        // 插入新元素，频繁访问的元素应该被保留
        cache.insert(10, 100, 1);
        cache.insert(11, 110, 1);

        // 频繁访问的元素应该还在
        assert_eq!(cache.get(&7), Some(70));
        assert_eq!(cache.get(&6), Some(60));
    }

    #[test]
    fn test_ghost_list_hit() {
        let cache = ArcCache::new(2);

        // 插入两个元素
        cache.insert("a", 1, 1);
        cache.insert("b", 2, 1);

        // 插入第三个，"a" 被驱逐到 B1
        cache.insert("c", 3, 1);
        assert_eq!(cache.get(&"a"), None);

        // 再次插入 "a"，应该命中 B1 并调整 p
        cache.insert("a", 4, 1);
        assert_eq!(cache.get(&"a"), Some(4));

        // "a" 现在应该在 T2 中（从 ghost 恢复的项进入 T2）
        let (t1, b1, t2, b2, p) = cache.stats();
        assert!(t2 > 0);
    }

    #[test]
    fn test_update_existing() {
        let cache = ArcCache::new(3);

        // 插入一个值
        assert_eq!(cache.insert("key", "value1", 1), None);
        assert_eq!(cache.get(&"key"), Some("value1"));

        // 更新值
        assert_eq!(cache.insert("key", "value2", 1), Some("value1"));
        assert_eq!(cache.get(&"key"), Some("value2"));

        // 再次更新（现在在 T2 中）
        assert_eq!(cache.insert("key", "value3", 1), Some("value2"));
        assert_eq!(cache.get(&"key"), Some("value3"));
    }

    #[test]
    fn test_erase() {
        let cache = ArcCache::new(3);

        cache.insert("a", 1, 1);
        cache.insert("b", 2, 1);
        cache.insert("c", 3, 1);

        // 删除一个存在的键
        cache.erase(&"b");
        assert_eq!(cache.get(&"b"), None);

        // 确保其他键还在
        assert_eq!(cache.get(&"a"), Some(1));
        assert_eq!(cache.get(&"c"), Some(3));

        // 删除不存在的键不应该出错
        cache.erase(&"d");
    }

    #[test]
    fn test_charge_aware() {
        let cache = ArcCache::new(4);

        // 插入一个大条目
        assert_eq!(cache.insert("big", "large", 3), None);
        assert_eq!(cache.total_charge(), 3);

        // 插入小条目
        assert_eq!(cache.insert("small", "tiny", 1), None);
        assert_eq!(cache.total_charge(), 4);

        // 再插入一个小条目，应该正好填满
        assert_eq!(cache.insert("another", "tiny", 1), None);

        // 大条目应该被驱逐
        assert_eq!(cache.get(&"big"), None);
        assert_eq!(cache.get(&"small"), Some("tiny"));
    }

    #[test]
    fn test_concurrent_access() {
        let cache = StdArc::new(ArcCache::<i32, i32>::new(100));
        let hit_count = StdArc::new(AtomicUsize::new(0));
        let mut handles = vec![];

        // 多个线程并发访问
        for i in 0..10 {
            let cache_clone = cache.clone();
            let hit_count_clone = hit_count.clone();

            let handle = std::thread::spawn(move || {
                for j in 0..100 {
                    let key = (i * 10 + j) % 50;
                    cache_clone.insert(key, key * 2, 1);

                    if cache_clone.get(&key).is_some() {
                        hit_count_clone.fetch_add(1, Ordering::Relaxed);
                    }
                }
            });
            handles.push(handle);
        }

        // 等待所有线程完成
        for handle in handles {
            handle.join().unwrap();
        }

        // 验证缓存状态
        assert!(cache.total_charge() <= 100);
        assert!(hit_count.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn test_evict_hook() {
        let evicted = StdArc::new(Mutex::new(Vec::new()));
        let evicted_clone = evicted.clone();

        let cache = ArcCache::with_evict_hook(2, move |k: &String, v: &i32| {
            evicted_clone.lock().unwrap().push((k.to_string(), *v));
        });

        // 插入三个元素，第一个应该被驱逐
        cache.insert("a".parse().unwrap(), 1, 1);
        cache.insert("b".parse().unwrap(), 2, 1);
        cache.insert("c".parse().unwrap(), 3, 1);

        // 检查驱逐记录
        let evicted_items = evicted.lock().unwrap();
        assert!(!evicted_items.is_empty());
        assert!(evicted_items.iter().any(|(k, _)| k == "a"));
    }

    #[test]
    fn test_basic_insert_get() {
        let cache = ArcCache::new(4);
        let key = "test_key";
        let value = "test_value";

        // 测试插入后能正确获取
        assert_eq!(cache.insert(key, value, 1), None);
        assert_eq!(cache.get(&key), Some(value));
    }

    #[test]
    fn test_insert_overwrite() {
        let cache = ArcCache::new(4);
        let key = "key";

        // 首次插入
        assert_eq!(cache.insert(key, "value1", 1), None);
        // 覆盖插入应返回旧值
        assert_eq!(cache.insert(key, "value2", 1), Some("value1"));
        // 验证新值
        assert_eq!(cache.get(&key), Some("value2"));
    }

    // #[test]
    // fn test_erase() {
    //     let cache = ArcCache::new(4);
    //     let key = "to_erase";
    // 
    //     cache.insert(key, "value", 1);
    //     cache.erase(&key);
    // 
    //     // 删除后应获取不到
    //     assert_eq!(cache.get(&key), None);
    // }

    #[test]
    fn test_total_charge_basic() {
        let cache = ArcCache::new(30);

        cache.insert("k1", "v1", 10);
        cache.insert("k2", "v2", 20);

        // 验证总charge
        assert_eq!(cache.total_charge(), 30);
    }

    #[test]
    fn test_total_charge_after_eviction() {
        let cache = ArcCache::new(30);

        cache.insert("k1", "v1", 10);
        cache.insert("k2", "v2", 15);
        cache.insert("k3", "v3", 10); // 应触发淘汰(k1)

        // 验证淘汰后的总charge
        assert_eq!(cache.total_charge(), 25); // 15+10
    }

    #[test]
    fn test_clear() {
        let cache = ArcCache::new(4);

        cache.insert("k1", "v1", 5);
        cache.insert("k2", "v2", 5);
        cache.clear();

        // 清空后charge为0
        assert_eq!(cache.total_charge(), 0);
        // 所有键都应不存在
        assert_eq!(cache.get(&"k1"), None);
        assert_eq!(cache.get(&"k2"), None);
    }

    #[test]
    fn test_concurrent_access2() {
        use std::thread;
        let cache = StdArc::new(ArcCache::<i32, i32>::new(100));
        let mut handles = vec![];

        // 多线程并发插入
        for i in 0..10 {
            let cache_clone = cache.clone();
            handles.push(thread::spawn(move || {
                cache_clone.insert(i, i*10, 1);
            }));
        }

        // 等待所有线程完成
        for handle in handles {
            handle.join().unwrap();
        }

        // 验证所有插入的值
        for i in 0..10 {
            assert_eq!(cache.get(&i), Some(i*10));
        }
        assert_eq!(cache.total_charge(), 10);
    }
}