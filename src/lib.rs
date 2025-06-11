mod store;
// mod segment;
// mod utils;

mod storage;
// mod store;

#[macro_use]
mod error;
mod logger;
mod segment;
mod utils;
mod record;
// mod options;
mod cache;
mod options;
mod index;
// mod batch;

#[macro_use]
extern crate log;

pub use log::{LevelFilter, Log};
pub use error::{Error, Result};
#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
