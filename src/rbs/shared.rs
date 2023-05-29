use super::{
    init::rb_impl_init,
    utils::{modulus, ranges},
};
#[cfg(feature = "alloc")]
use crate::storage::Heap;
use crate::{
    consumer::Consumer,
    halves::cached::{CachedCons, CachedProd},
    producer::Producer,
    storage::{Shared, Static, Storage},
    traits::{ring_buffer::Split, Observer, RingBuffer},
};
#[cfg(feature = "alloc")]
use alloc::sync::Arc;
use core::{
    mem::{ManuallyDrop, MaybeUninit},
    num::NonZeroUsize,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};
use crossbeam_utils::CachePadded;

/// Ring buffer that could be shared between threads.
pub struct SharedRb<S: Storage> {
    storage: Shared<S>,
    read: CachePadded<AtomicUsize>,
    write: CachePadded<AtomicUsize>,
}

impl<S: Storage> SharedRb<S> {
    /// Constructs ring buffer from storage and indices.
    ///
    /// # Safety
    ///
    /// The items in storage inside `read..write` range must be initialized, items outside this range must be uninitialized.
    /// `read` and `write` positions must be valid (see [`RbBase`](`crate::ring_buffer::RbBase`)).
    pub unsafe fn from_raw_parts(storage: S, read: usize, write: usize) -> Self {
        Self {
            storage: Shared::new(storage),
            read: CachePadded::new(AtomicUsize::new(read)),
            write: CachePadded::new(AtomicUsize::new(write)),
        }
    }
    /// Destructures ring buffer into underlying storage and `read` and `write` indices.
    ///
    /// # Safety
    ///
    /// Initialized contents of the storage must be properly dropped.
    pub unsafe fn into_raw_parts(self) -> (S, usize, usize) {
        let this = ManuallyDrop::new(self);
        (
            ptr::read(&this.storage).into_inner(),
            this.read.load(Ordering::Acquire),
            this.write.load(Ordering::Acquire),
        )
    }
}

impl<S: Storage> Observer for SharedRb<S> {
    type Item = S::Item;

    #[inline]
    fn capacity(&self) -> NonZeroUsize {
        self.storage.len()
    }

    fn occupied_len(&self) -> usize {
        let modulus = modulus(self);
        (modulus.get() + self.write.load(Ordering::Acquire) - self.read.load(Ordering::Acquire)) % modulus
    }
    fn vacant_len(&self) -> usize {
        let modulus = modulus(self);
        (self.capacity().get() + self.read.load(Ordering::Acquire) - self.write.load(Ordering::Acquire)) % modulus
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.read.load(Ordering::Acquire) == self.write.load(Ordering::Acquire)
    }
}

impl<S: Storage> Producer for SharedRb<S> {
    #[inline]
    unsafe fn advance_write_index(&self, count: usize) {
        self.write
            .store((self.write.load(Ordering::Acquire) + count) % modulus(self), Ordering::Release);
    }

    #[inline]
    fn vacant_slices(&self) -> (&[MaybeUninit<S::Item>], &[MaybeUninit<S::Item>]) {
        let (first, second) = unsafe {
            self.unsafe_slices(
                self.write.load(Ordering::Acquire),
                self.read.load(Ordering::Acquire) + self.capacity().get(),
            )
        };
        (first as &_, second as &_)
    }
    #[inline]
    fn vacant_slices_mut(&mut self) -> (&mut [MaybeUninit<S::Item>], &mut [MaybeUninit<S::Item>]) {
        unsafe {
            self.unsafe_slices(
                self.write.load(Ordering::Acquire),
                self.read.load(Ordering::Acquire) + self.capacity().get(),
            )
        }
    }
}

impl<S: Storage> Consumer for SharedRb<S> {
    #[inline]
    unsafe fn advance_read_index(&self, count: usize) {
        self.read
            .store((self.read.load(Ordering::Acquire) + count) % modulus(self), Ordering::Release);
    }

    #[inline]
    fn occupied_slices(&self) -> (&[MaybeUninit<S::Item>], &[MaybeUninit<S::Item>]) {
        let (first, second) = unsafe { self.unsafe_slices(self.read.load(Ordering::Acquire), self.write.load(Ordering::Acquire)) };
        (first as &_, second as &_)
    }
    #[inline]
    unsafe fn occupied_slices_mut(&mut self) -> (&mut [MaybeUninit<S::Item>], &mut [MaybeUninit<S::Item>]) {
        self.unsafe_slices(self.read.load(Ordering::Acquire), self.write.load(Ordering::Acquire))
    }
}

impl<S: Storage> RingBuffer for SharedRb<S> {
    fn read_index(&self) -> usize {
        self.read.load(Ordering::Acquire)
    }
    fn write_index(&self) -> usize {
        self.write.load(Ordering::Acquire)
    }

    unsafe fn set_read_index(&self, value: usize) {
        self.read.store(value, Ordering::Release);
    }
    unsafe fn set_write_index(&self, value: usize) {
        self.write.store(value, Ordering::Release);
    }

    unsafe fn unsafe_slices(&self, start: usize, end: usize) -> (&mut [MaybeUninit<S::Item>], &mut [MaybeUninit<S::Item>]) {
        let (first, second) = ranges(self.capacity(), start, end);
        (self.storage.slice(first), self.storage.slice(second))
    }
}

impl<S: Storage> Drop for SharedRb<S> {
    fn drop(&mut self) {
        self.clear();
    }
}

impl<'a, S: Storage + 'a> Split for &'a mut SharedRb<S> {
    type Prod = CachedProd<&'a SharedRb<S>>;
    type Cons = CachedCons<&'a SharedRb<S>>;

    fn split(self) -> (Self::Prod, Self::Cons) {
        unsafe { (CachedProd::new(self), CachedCons::new(self)) }
    }
}
#[cfg(feature = "alloc")]
impl<S: Storage> Split for SharedRb<S> {
    type Prod = CachedProd<Arc<Self>>;
    type Cons = CachedCons<Arc<Self>>;

    fn split(self) -> (Self::Prod, Self::Cons) {
        let rc = Arc::new(self);
        unsafe { (CachedProd::new(rc.clone()), CachedCons::new(rc)) }
    }
}
impl<S: Storage> SharedRb<S> {
    #[cfg(feature = "alloc")]
    pub fn split(self) -> (CachedProd<Arc<Self>>, CachedCons<Arc<Self>>) {
        Split::split(self)
    }
    pub fn split_ref(&mut self) -> (CachedProd<&Self>, CachedCons<&Self>) {
        Split::split(self)
    }
}

rb_impl_init!(SharedRb);
