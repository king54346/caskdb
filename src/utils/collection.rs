pub type DefaultHashBuilder = std::hash::BuildHasherDefault<murmurhash::MurmurHasher64>;
pub type HashMap<K, V> = std::collections::HashMap<K, V, DefaultHashBuilder>;
pub type HashSet<K> = std::collections::HashSet<K, DefaultHashBuilder>;
