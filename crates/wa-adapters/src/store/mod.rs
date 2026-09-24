//! Storage adapters.
//!
//! [`conformance`] is the executable definition of the `KvStore` contract;
//! every adapter's test module runs it.

pub mod conformance;
#[cfg(feature = "memory")]
mod memory_kv;

#[cfg(feature = "memory")]
pub use memory_kv::MemoryKvStore;
