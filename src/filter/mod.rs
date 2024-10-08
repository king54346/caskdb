pub mod bloom;

/// `FilterPolicy` is an algorithm for probabilistically encoding a set of keys.
/// The canonical implementation is a Bloom filter.
///
/// Every `FilterPolicy` has a name. This names the algorithm itself, not any one
/// particular instance. Aspects specific to a particular instance, such as the
/// set of keys or any other parameters, will be encoded in the byte filter
/// returned by `new_filter_writer`.
///
/// The name may be written to files on disk, along with the filter data. To use
/// these filters, the `FilterPolicy` name at the time of writing must equal the
/// name at the time of reading. If they do not match, the filters will be
/// ignored, which will not affect correctness but may affect performance.
pub trait FilterPolicy: Send + Sync {
    /// Return the name of this policy.  Note that if the filter encoding
    /// changes in an incompatible way, the name returned by this method
    /// must be changed.  Otherwise, old incompatible filters may be
    /// passed to methods of this type.
    fn name(&self) -> &str;

    /// `MayContain` returns whether the encoded filter may contain given key.
    /// False positives are possible, where it returns true for keys not in the
    /// original set.
    fn may_contain(&self, filter: &[u8], key: &[u8]) -> bool;

    /// Creates a filter based on given keys
    fn create_filter(&self, keys: &Vec<&[u8]>) -> Vec<u8>;
}
