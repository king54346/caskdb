### Plan & Progress

#### The basic shape of LevelDB

- [x] Fundamental components
  - [x] Arena
  - [x] Skiplist
  - [x] Cache
  - [x] Record
  - [x] Batch
  - [x] Block
  - [x] Table
  - [x] Version
  - [x] VersionEdit
  - [x] VersionSet
  - [x] Storage
  - [x] DB
- [x] Compaction implementation
- [x] Scheduling

#### [ongoing] Test cases & Benches

- Adding more test cases. The progress is tracked by this [issue](https://github.com/Fullstop000/wickdb/issues/3).
- Adding benchmarks. The progress is tracked by this [issue](https://github.com/Fullstop000/wickdb/issues/21).

### Developing

`wickdb` is built using the latest version of `stable` Rust, using [the 2018 edition](https://doc.rust-lang.org/edition-guide/rust-2018/).

In order to have your PR merged running the following must finish without error otherwise the CI will fail:

```bash
cargo test --all && \
cargo clippy && \
cargo fmt --all -- --check
```

You may optionally want to install `cargo-watch` to allow for automated rebuilding while editing:

```bash
cargo watch -s "cargo check --tests"
```

There're so many `TODO`s in current implementation and you can pick either of them to do something.

This crate is still at early stage so any PRs or issues are welcomed!.

## License

[![FOSSA Status](https://app.fossa.com/api/projects/git%2Bgithub.com%2FFullstop000%2Fwickdb.svg?type=large)](https://app.fossa.io/projects/git%2Bgithub.com%2FFullstop000%2Fwickdb?ref=badge_large)


https://github.com/rosedblabs/mini-bitcask-rs
https://github.com/szuwgh/nikidb/tree/main boltdb
https://github.com/erikgrinaker/toydb/tree/master
https://github.com/kawasin73/prsqlite
https://github.com/erikgrinaker/toydb/tree/master
https://github.com/balloonwj/CppGuide/blob/master/articles/leveldb%E6%BA%90%E7%A0%81%E5%88%86%E6%9E%90/leveldb%E6%BA%90%E7%A0%81%E5%88%86%E6%9E%9017.md
