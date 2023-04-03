use crate::{
    raw::{RawRb, RawStorage},
    utils::{slice_assume_init_mut, slice_assume_init_ref},
    Observer,
};
use core::{cmp, iter::Chain, iter::ExactSizeIterator, mem::MaybeUninit, ops::Deref, slice};

/// Consumer part of ring buffer.
///
/// # Mode
///
/// It can operate in immediate (by default) or postponed mode.
/// Mode could be switched using [`Self::postponed`]/[`Self::into_postponed`] and [`Self::into_immediate`] methods.
///
/// + In immediate mode removed and inserted items are automatically synchronized with the other end.
/// + In postponed mode synchronization occurs only when [`Self::sync`] or [`Self::into_immediate`] is called or when `Self` is dropped.
///   The reason to use postponed mode is that multiple subsequent operations are performed faster due to less frequent cache synchronization.
pub trait Consumer: Observer {
    /// Provides a direct access to the ring buffer occupied memory.
    /// The difference from [`Self::as_slices`] is that this method provides slices of [`MaybeUninit`], so items may be moved out of slices.  
    ///
    /// Returns a pair of slices of stored items, the second one may be empty.
    /// Elements with lower indices in slice are older. First slice contains older items that second one.
    ///
    /// # Safety
    ///
    /// All items are initialized. Elements must be removed starting from the beginning of first slice.
    /// When all items are removed from the first slice then items must be removed from the beginning of the second slice.
    ///
    /// *This method must be followed by [`Self::advance_read`] call with the number of items being removed previously as argument.*
    /// *No other mutating calls allowed before that.*
    #[inline]
    fn occupied_slices(&self) -> (&[MaybeUninit<Self::Item>], &[MaybeUninit<Self::Item>]) {
        let (left, right) = unsafe { self.as_raw().occupied_slices() };
        (left as &[_], right as &[_])
    }

    /// Provides a direct mutable access to the ring buffer occupied memory.
    ///
    /// Same as [`Self::occupied_slices`].
    ///
    /// # Safety
    ///
    /// When some item is replaced with uninitialized value then it must immediately consumed by [`Self::advance_read`].
    #[inline]
    unsafe fn occupied_slices_mut(
        &mut self,
    ) -> (
        &mut [MaybeUninit<Self::Item>],
        &mut [MaybeUninit<Self::Item>],
    ) {
        self.as_raw().occupied_slices()
    }

    /// Moves `read` pointer by `count` places forward.
    ///
    /// # Safety
    ///
    /// First `count` items in occupied memory must be moved out or dropped.
    #[inline]
    unsafe fn advance_read(&mut self, count: usize) {
        self.as_raw().move_read_end(count);
    }

    /// Returns a pair of slices which contain, in order, the contents of the ring buffer.
    #[inline]
    fn as_slices(&self) -> (&[Self::Item], &[Self::Item]) {
        unsafe {
            let (left, right) = self.occupied_slices();
            (slice_assume_init_ref(left), slice_assume_init_ref(right))
        }
    }

    /// Returns a pair of mutable slices which contain, in order, the contents of the ring buffer.
    #[inline]
    fn as_mut_slices(&mut self) -> (&mut [Self::Item], &mut [Self::Item]) {
        unsafe {
            let (left, right) = self.occupied_slices_mut();
            (slice_assume_init_mut(left), slice_assume_init_mut(right))
        }
    }

    /// Removes latest item from the ring buffer and returns it.
    ///
    /// Returns `None` if the ring buffer is empty.
    fn try_pop(&mut self) -> Option<Self::Item> {
        if !self.is_empty() {
            let elem = unsafe { self.occupied_slices().0.get_unchecked(0).assume_init_read() };
            unsafe { self.advance_read(1) };
            Some(elem)
        } else {
            None
        }
    }

    /// Returns an iterator that removes items one by one from the ring buffer.
    ///
    /// Iterator provides only items that are available for consumer at the moment of `pop_iter` call, it will not contain new items added after it was created.
    ///
    /// *Information about removed items is commited to the buffer only when iterator is destroyed.*
    fn pop_iter(&mut self) -> PopIter<'_, Self::Raw> {
        unsafe { PopIter::new(self.as_raw()) }
    }

    /// Returns a front-to-back iterator containing references to items in the ring buffer.
    ///
    /// This iterator does not remove items out of the ring buffer.
    fn iter(&self) -> Iter<'_, Self::Raw> {
        let (left, right) = self.as_slices();
        left.iter().chain(right.iter())
    }

    /// Returns a front-to-back iterator that returns mutable references to items in the ring buffer.
    ///
    /// This iterator does not remove items out of the ring buffer.
    fn iter_mut(&mut self) -> IterMut<'_, Self::Raw> {
        let (left, right) = self.as_mut_slices();
        left.iter_mut().chain(right.iter_mut())
    }

    /// Removes at most `n` and at least `min(n, Self::len())` items from the buffer and safely drops them.
    ///
    /// If there is no concurring producer activity then exactly `min(n, Self::len())` items are removed.
    ///
    /// Returns the number of deleted items.
    ///
    #[cfg_attr(
        feature = "alloc",
        doc = r##"
```ignore
# extern crate ringbuf;
# use ringbuf::HeapRb;
# fn main() {
let target = HeapRb::<i32>::new(8);
let (mut prod, mut cons) = target.split();

assert_eq!(prod.push_iter(&mut (0..8)), 8);

assert_eq!(cons.skip(4), 4);
assert_eq!(cons.skip(8), 4);
assert_eq!(cons.skip(8), 0);
# }
```
"##
    )]
    fn skip(&mut self, count: usize) -> usize {
        let count = cmp::min(count, self.occupied_len());
        assert_eq!(unsafe { self.as_raw().skip(Some(count)) }, count);
        count
    }

    /// Removes all items from the buffer and safely drops them.
    ///
    /// Returns the number of deleted items.
    fn clear(&mut self) -> usize {
        unsafe { self.as_raw().skip(None) }
    }
}

/// An iterator that removes items from the ring buffer.
pub struct PopIter<'a, R: RawRb> {
    target: &'a R,
    slices: (&'a [MaybeUninit<R::Item>], &'a [MaybeUninit<R::Item>]),
    initial_len: usize,
}

impl<'a, R: RawRb> PopIter<'a, R> {
    unsafe fn new(target: &'a R) -> Self {
        let slices = unsafe { target.occupied_slices() };
        Self {
            target,
            initial_len: slices.0.len() + slices.1.len(),
            slices: (slices.0, slices.1),
        }
    }
}

impl<'a, R: RawRb> Iterator for PopIter<'a, R> {
    type Item = R::Item;
    #[inline]
    fn next(&mut self) -> Option<R::Item> {
        match self.slices.0.len() {
            0 => None,
            n => {
                let item = unsafe { self.slices.0.get_unchecked(0).assume_init_read() };
                if n == 1 {
                    (self.slices.0, self.slices.1) = (self.slices.1, &[]);
                } else {
                    self.slices.0 = unsafe { self.slices.0.get_unchecked(1..n) };
                }
                Some(item)
            }
        }
    }
    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.len(), Some(self.len()))
    }
}

impl<'a, R: RawRb> ExactSizeIterator for PopIter<'a, R> {
    fn len(&self) -> usize {
        self.slices.0.len() + self.slices.1.len()
    }
}

impl<'a, R: RawRb> Drop for PopIter<'a, R> {
    fn drop(&mut self) {
        unsafe { self.target.move_read_end(self.initial_len - self.len()) };
    }
}

/// Iterator over ring buffer contents.
///
/// *Please do not rely on actual type, it may change in future.*
#[allow(type_alias_bounds)]
pub type Iter<'a, R: RawRb> = Chain<slice::Iter<'a, R::Item>, slice::Iter<'a, R::Item>>;

/// Mutable iterator over ring buffer contents.
///
/// *Please do not rely on actual type, it may change in future.*
#[allow(type_alias_bounds)]
pub type IterMut<'a, R: RawRb> = Chain<slice::IterMut<'a, R::Item>, slice::IterMut<'a, R::Item>>;

pub struct Wrap<R> {
    raw: R,
}

impl<R> Wrap<R>
where
    R: Sized,
{
    /// # Safety
    ///
    /// There must be no more than one consumer wrapper.
    pub unsafe fn new(raw: R) -> Self {
        Self { raw }
    }
}

impl<R: Deref> Observer for Wrap<R>
where
    R::Target: RawRb + Sized,
{
    type Item = <R::Target as RawStorage>::Item;
    type Raw = R::Target;
    fn as_raw(&self) -> &Self::Raw {
        &self.raw
    }
}

impl<R: Deref> Consumer for Wrap<R> where R::Target: RawRb + Sized {}
