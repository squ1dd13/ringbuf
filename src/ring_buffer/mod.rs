mod owning;
mod storage;
mod traits;

pub use owning::*;
pub use storage::*;
pub use traits::*;

use crate::{
    counter::{AtomicCounter, Counter},
    utils::uninit_array,
};
use core::mem::MaybeUninit;

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

impl<T, S: Counter, const N: usize> Default for OwningRingBuffer<T, [MaybeUninit<T>; N], S> {
    fn default() -> Self {
        unsafe { Self::from_raw_parts(uninit_array(), 0, 0) }
    }
}

#[cfg(feature = "alloc")]
impl<T, S: Counter> OwningRingBuffer<T, Vec<MaybeUninit<T>>, S> {
    /// Creates a new instance of a ring buffer.
    ///
    /// *Panics if `capacity` is zero.*
    pub fn new(capacity: usize) -> Self {
        let mut data = Vec::new();
        data.resize_with(capacity, MaybeUninit::uninit);
        unsafe { Self::from_raw_parts(data, 0, 0) }
    }
}

/// Stack-allocated ring buffer with static capacity.
///
/// Capacity must be greater that zero.
pub type StaticRingBuffer<T, const N: usize> =
    OwningRingBuffer<T, [MaybeUninit<T>; N], AtomicCounter>;

/// Heap-allocated ring buffer.
#[cfg(feature = "alloc")]
pub type HeapRingBuffer<T> = OwningRingBuffer<T, Vec<MaybeUninit<T>>, AtomicCounter>;
