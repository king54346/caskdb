use std::fmt::Debug;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use crate::cache::Cache;
use crate::cache::lru::LRUCache;

pub struct ArcCacheInner<K, V: Clone> {
    recent_set:    LRUCache<K, V>,    // 最近访问集合（热集合)
    recent_evicted:  LRUCache<K, ()>, // 最近访问但被驱逐的集合（冷集合）
    frequent_set:  LRUCache<K, V>,    // 频繁访问集合（热集合）
    frequent_evicted:  LRUCache<K, ()>,// 频繁访问但被驱逐的集合（冷集合）
    p: usize,         // 平衡因子，用于调节 recent 和 frequent 集合的大小 p 表示 t1 的目标大小。t2 的大小则相应地为 capacity - p。
}

pub struct ArcCache<K, V: Clone> {
    // 缓存的容量
    capacity: usize,
    inner: Arc<Mutex<ArcCacheInner<K, V>>>,
    // 删除kv的回调
    evict_hook: Option<Box<dyn Fn(&K, &V)>>,
}

impl<K :Hash + Eq + Clone, V: Clone> ArcCache<K, V> {
    pub fn new(cap: usize) -> Self {
        let l = ArcCacheInner {
            recent_set: LRUCache::new(cap),
            recent_evicted: LRUCache::new(cap),
            frequent_set: LRUCache::new(cap),
            frequent_evicted:LRUCache::new(cap),
            p: 0,
        };
        ArcCache {
            capacity: cap,
            inner: Arc::new(Mutex::new(l)),
            evict_hook: None,
        }
    }
}

impl<K, V> ArcCache<K, V>
    where
        K: Send + Sync + Hash + Eq + Debug + Clone,
        V: Send + Sync + Clone,
{
    // p 表示的是 t1 的目标大小
    fn adjust_p(&self, inner: &mut ArcCacheInner<K, V>, from_b1: bool)
        where K: Send + Sync + Hash + Eq + Debug, V: Send + Sync + Clone  {
        let recent_evicted_len = inner.recent_evicted.total_charge();
        let frequent_evicted_len = inner.frequent_evicted.total_charge();
        let delta = if from_b1 {
            if frequent_evicted_len > recent_evicted_len {
                frequent_evicted_len / recent_evicted_len
            } else {
                1
            }
        } else {
            if recent_evicted_len > frequent_evicted_len {
                recent_evicted_len / frequent_evicted_len
            } else {
                1
            }
        };
        if from_b1 {
            if delta <= self.capacity - inner.p {
                inner.p += delta;
            } else {
                inner.p = self.capacity;
            }
        } else {
            if inner.p > delta {
                inner.p -= delta;
            } else {
                inner.p = 0;
            }
        }
    }
    // 在缓存容量达到上限时，通过移除 recent_set 或 frequent_set 中的最少使用的元素，来为新的缓存项腾出空间
    fn replace(&self, inner: &mut ArcCacheInner<K, V>, frequent_evicted_contains_key: bool) {
        let recent_set_len = inner.recent_set.total_charge();
        /// 如果 recent_set 中有元素且其大小大于 p，或者其大小等于 p 且 frequent_evicted_contains_key 为真
        if recent_set_len > 0 && recent_set_len>inner.p || (recent_set_len == inner.p && frequent_evicted_contains_key)  {
            // 从 recent_set 中移除最少使用的元素（LRU）
            if let Some((key, _,charge)) = inner.recent_set.erase_lru() {
                inner.recent_evicted.insert(key, (),charge);
            }
            // 否则从 frequent_set 中移除最少使用的元素（LRU）
        }else if let Some((key, _,charge)) = inner.frequent_set.erase_lru() {
            inner.frequent_evicted.insert(key, (),charge);
        }
    }
}

impl<K, V> Cache<K, V> for ArcCache<K, V>
    where
        K: Send + Sync + Hash + Eq + Debug + Clone,
        V: Send + Sync + Clone,
{

    // 如果key在T1中，移除key并插入到T2的MRU（Most Recently Used）端
    // 如果key在T2中，更新key为T2的MRU端
    // 如果key在B1中，表示key最近从T1中被移除
    // 如果key在B2中，表示key最近从T2中被移除
    // 如果key不在缓存和B1或B2中
    fn insert(&self, key: K, value: V, charge: usize) -> Option<V> {
        let mut inner = self.inner.lock().unwrap();
        // 缓存命中处理
        // t2频繁列表命中
        if inner.frequent_set.contains_key(&key){
            //  移动频繁使用列表
            return inner.frequent_set.insert(key, value,charge)
        }
        // t1最近使用列表命中
        if inner.recent_set.contains_key(&key) {
            inner.recent_set.erase(&key);
            //  移动到lfu
            return inner.frequent_set.insert(key, value,charge)
        }
        // 缓存ghost列表 b2 frequent_evicted
        if inner.frequent_evicted.contains_key(&key){
            // 当命中 b1 p 增大,b2时 p减小
            // 调整 p 的值,将 p 增加到 capacity 的最大值。将元素从 b1 移动到 t2 frequent_set。
            self.adjust_p(&mut inner,false);
            //如果当前缓存（recent_set和frequent_set的总长度）已达到容量上限
            if inner.recent_set.total_charge() + inner.frequent_set.total_charge()  >= self.capacity {
                self.replace(&mut inner,true);
            }
            //然后从frequent_evicted中删除该key，并将其插入到frequent_set中，返回true。
            inner.frequent_evicted.erase(&key);
            return inner.frequent_set.insert(key, value,charge)
        }
        //  b1 recent_evicted 调整 p 的值，增加 t2 的容量。将 p 增加到 capacity 的最大值。将元素从 b1 移动到 t2
        if inner.recent_evicted.contains_key(&key) {
            // 当命中b1的时候，说明t1太小了，t1的长度会增加1，t2会减少1
            self.adjust_p(&mut inner,true);
            if inner.recent_set.total_charge() + inner.frequent_set.total_charge() >= self.capacity {
                self.replace(&mut inner,false);
            }

            inner.recent_evicted.erase(&key);
            return inner.frequent_set.insert(key, value,charge)
        }
        
        // 未命中缓存
        // 如果缓存已满，根据 ARC 算法进行替换
        if inner.recent_set.total_charge() + inner.frequent_set.total_charge() >= self.capacity {
            self.replace(&mut inner,false);
        }
        // 如果lru_e的大小超过阈值，移除条目
        if inner.recent_evicted.total_charge() > self.capacity - inner.p {
            inner.recent_evicted.erase_lru();
        }
        // 如果lfu_e的大小超过阈值，移除条目
        if inner.frequent_evicted.total_charge() > inner.p {
            inner.frequent_evicted.erase_lru();
        }
        // 将新元素插入到 t1 中。
        inner.recent_set.insert(key, value, charge)
    }

    fn get(&self, key: &K) -> Option<V> {
        let mut inner = self.inner.lock().unwrap();

        // 在 recent_set 中查找键
        if let Some((k,v,charge)) = inner.recent_set.lookup(key) {
            // 将键值对移动到 frequent_set
            let value_cloned = v.clone();
            inner.recent_set.erase(key);
            inner.frequent_set.insert(k, v,charge);
            return Some(value_cloned);
        }

        // 在 frequent_set 中查找键
        if let Some(value) = inner.frequent_set.get(key) {
            // 将键值对移动到 frequent_set 的前面
            inner.frequent_set.get(key);
            return Some(value.clone());
        }

        // 未命中，返回 None
        None
    }

    fn erase(&self, key: &K) {
        let mut inner = self.inner.lock().unwrap();
        inner.frequent_set.erase(key);
        inner.recent_set.erase(key);
        inner.frequent_evicted.erase(key);
        inner.recent_evicted.erase(key);

    }

    fn total_charge(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.recent_set.total_charge() + inner.frequent_set.total_charge()
    }
}

// 线程安全
unsafe impl<K: Send, V: Send + Clone> Send for ArcCache<K, V> {}
unsafe impl<K: Sync, V: Sync + Clone> Sync for ArcCache<K, V> {}

// 如果命中，且lfu中没有，数据放入lfu
// 当lru和lfu都满的时候，一个数据进入缓存，lru淘汰到ghost，命中ghost调整p，lru+1，lfu-1
// lfu同理，但是ghost链表中淘汰就真淘汰
#[cfg(test)]
mod tests {
    use crate::cache::arc::ArcCache;
    use crate::cache::Cache;

    #[test]
    fn test_arc_cache() {
        let cache = ArcCache::new(3); // 假设缓存容量为3
        // 插入三个元素
        assert_eq!(cache.insert("a", 1, 1), None);
        assert_eq!(cache.insert("b", 2, 1), None);
        assert_eq!(cache.insert("c", 3, 1), None);

        // 检查缓存命中
        assert_eq!(cache.get(&"a"), Some(1));
        assert_eq!(cache.get(&"b"), Some(2));
        assert_eq!(cache.get(&"c"), Some(3));

        // 插入第四个元素，应该触发替换
        assert_eq!(cache.insert("d", 4, 1), None);

        // "a" 应该被淘汰
        assert_eq!(cache.get(&"a"), None);
        cache.insert(&"a",5,1);
        assert_eq!(cache.get(&"b"), Some(2));
        assert_eq!(cache.get(&"c"), Some(3));
        // assert_eq!(cache.get(&"d"), Some(4));

        // 再次访问 "b" 和 "c" 使其变为频繁使用
        assert_eq!(cache.get(&"b"), Some(2));

        assert_eq!(cache.get(&"a"), Some(5));
        assert_eq!(cache.insert(&"b",5,1), Some(2));
        // 插入第五个元素
        assert_eq!(cache.insert("e", 5, 1), None);

        // "d" 应该被淘汰，因为 "b" 和 "c" 是频繁使用的
        assert_eq!(cache.get(&"d"), None);
        assert_eq!(cache.get(&"b"), Some(5));
        assert_eq!(cache.get(&"c"), None);
        assert_eq!(cache.get(&"e"), Some(5));
    }
    #[test]
    fn test_charge_usage() {
        let cache = ArcCache::new(3);

        // 插入值并指定消耗
        assert_eq!(cache.insert("key1", "value1", 2), None);
        assert_eq!(cache.get(&"key1"), Some("value1"));

        // 插入第二个值，消耗超过容量
        assert_eq!(cache.insert("key2", "value2", 2), None);
        assert_eq!(cache.get(&"key1"), Some("value1")); // key1 应该被移除
        assert_eq!(cache.get(&"key2"), Some("value2"));
    }
    #[test]
    fn test_eviction_policy() {
        let cache = ArcCache::new(2);

        // 插入两个值
        assert_eq!(cache.insert("key1", "value1", 1), None);
        assert_eq!(cache.insert("key2", "value2", 1), None);

        // 插入第三个值，应该触发驱逐
        assert_eq!(cache.insert("key3", "value3", 1), None);
        assert_eq!(cache.get(&"key1"), None); // key1 应该被驱逐
        assert_eq!(cache.get(&"key2"), Some("value2"));
        assert_eq!(cache.get(&"key3"), Some("value3"));
    }
    #[test]
    fn test_insert_and_get() {
        let cache = ArcCache::new(3);

        // 插入并获取单个值
        assert_eq!(cache.insert("key1", "value1", 1), None);
        assert_eq!(cache.get(&"key1"), Some("value1"));

        // 更新已存在的值
        assert_eq!(cache.insert("key1", "value2", 1), Some("value1"));
        assert_eq!(cache.get(&"key1"), Some("value2"));
        assert_eq!(cache.insert("key1", "value2", 1), Some("value2"));
        // 插入多个值并获取
        assert_eq!(cache.insert("key2", "value3", 1), None);
        assert_eq!(cache.insert("key3", "value4", 1), None);
        assert_eq!(cache.get(&"key2"), Some("value3"));
        assert_eq!(cache.get(&"key3"), Some("value4"));

        // 检查缓存容量限制
        cache.insert("key4", "value5", 1);
        assert_eq!(cache.get(&"key1"), None); // key1 应该被移除，因为缓存容量为 3
        assert_eq!(cache.get(&"key2"), Some("value3"));
        assert_eq!(cache.get(&"key3"), Some("value4"));
        assert_eq!(cache.get(&"key4"), Some("value5"));
    }
    #[test]
    fn test_adjust_p_decrease() {
        let cache = ArcCache::new(3);

        // 插入三个值，填满缓存
        assert_eq!(cache.insert("key1", "value1", 1), None);
        assert_eq!(cache.insert("key2", "value2", 1), None);
        assert_eq!(cache.insert("key3", "value3", 1), None);

        // 强制驱逐 key1 到 frequent_evicted
        assert_eq!(cache.insert("key4", "value4", 1), None);
        assert_eq!(cache.get(&"key1"), None); // key1 应该被驱逐

        // 插入 key1 到 frequent_evicted 中
        assert_eq!(cache.insert("key1", "value5", 1), None);

        // 现在 key1 在 frequent_evicted 中，再次插入 key1，触发 adjust_p 的逻辑
        // 这将调整 p 的值并将 key1 移动到 frequent_set 中
        assert_eq!(cache.insert("key1", "value6", 1), Some("value5"));

        // 验证 key1 已被移到 frequent_set 中
        assert_eq!(cache.get(&"key1"), Some("value6"));
    }
}