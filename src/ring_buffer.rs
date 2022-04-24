use alloc::{sync::Arc, vec::Vec};
use cache_padded::CachePadded;
use core::{
    cell::UnsafeCell,
    cmp,
    convert::{AsMut, AsRef},
    marker::PhantomData,
    mem::MaybeUninit,
    ptr, slice,
    sync::atomic::{AtomicUsize, Ordering},
};

use crate::{
    consumer::{ArcConsumer, RefConsumer},
    producer::{ArcProducer, RefProducer},
};

pub trait Container<U>: AsRef<[U]> + AsMut<[U]> {}
impl<U, C> Container<U> for C where C: AsRef<[U]> + AsMut<[U]> {}

struct Storage<U, C: Container<U>> {
    len: usize,
    container: UnsafeCell<C>,
    phantom: PhantomData<U>,
}

unsafe impl<U, C: Container<U>> Sync for Storage<U, C> {}

impl<U, C> Storage<U, C>
where
    C: AsRef<[U]> + AsMut<[U]>,
{
    pub fn new(mut container: C) -> Self {
        Self {
            len: container.as_mut().len(),
            container: UnsafeCell::new(container),
            phantom: PhantomData,
        }
    }

    pub fn into_inner(self) -> C {
        self.container.into_inner()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub unsafe fn as_slice(&self) -> &[U] {
        (&*self.container.get()).as_ref()
    }

    #[warn(clippy::mut_from_ref)]
    pub unsafe fn as_mut_slice(&self) -> &mut [U] {
        (&mut *self.container.get()).as_mut()
    }
}

pub trait RingBufferBase<T> {
    /// The capacity of the ring buffer.
    ///
    /// This value does not change.
    fn capacity(&self) -> usize;

    /// The number of elements stored in the buffer at the moment.
    ///
    /// *Actual value may change at any time if there is a concurring activity of producer or consumer.*
    fn occupied_len(&self) -> usize;

    /// The number of vacant places in the buffer at the moment.
    ///
    /// *Actual value may change at any time if there is a concurring activity of producer or consumer.*
    fn vacant_len(&self) -> usize;

    /// Checks if the ring buffer is empty.
    ///
    /// *The result is relevant until producer put an element.*
    fn is_empty(&self) -> bool {
        self.occupied_len() == 0
    }

    /// Checks if the ring buffer is full.
    ///
    /// *The result is relevant until consumer take an element.*
    fn is_full(&self) -> bool {
        self.vacant_len() == 0
    }
}

pub trait RingBufferHead<T>: RingBufferBase<T> {
    /// Move ring buffer **head** pointer by `count` elements forward.
    ///
    /// *Panics if `count` is greater than number of elements in the ring buffer.*
    ///
    /// # Safety
    ///
    /// First `count` elements in occupied area must be initialized before this call.
    unsafe fn move_head(&self, count: usize);

    /// Returns a pair of slices which contain, in order, the contents of the `RingBuffer`.
    ///
    /// All elements in slices are guaranteed to be *initialized*.
    ///
    /// *The slices may not include elements pushed to the buffer by concurring producer after the method call.*
    unsafe fn occupied_slices(&self) -> (&mut [MaybeUninit<T>], &mut [MaybeUninit<T>]);
}

pub trait RingBufferTail<T>: RingBufferBase<T> {
    /// Move ring buffer **tail** pointer by `count` elements forward.
    ///
    /// *Panics if `count` is greater than number of vacant places in the ring buffer.*
    ///
    /// # Safety
    ///
    /// First `count` elements in vacant area must be deinitialized (dropped) before this call.
    unsafe fn move_tail(&self, count: usize);

    /// All elements in slices are guaranteed to be *uninitialized*.
    unsafe fn vacant_slices(&self) -> (&mut [MaybeUninit<T>], &mut [MaybeUninit<T>]);
}

pub struct RingBuffer<T, C: Container<MaybeUninit<T>>> {
    data: Storage<MaybeUninit<T>, C>,
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
}

impl<T, C: Container<MaybeUninit<T>>> RingBuffer<T, C> {
    fn head(&self) -> usize {
        self.head.load(Ordering::Acquire)
    }
    fn tail(&self) -> usize {
        self.head.load(Ordering::Acquire)
    }
    fn modulus(&self) -> usize {
        2 * self.capacity()
    }

    pub unsafe fn from_raw_parts(container: C, head: usize, tail: usize) -> Self {
        Self {
            data: Storage::new(container),
            head: CachePadded::new(AtomicUsize::new(head)),
            tail: CachePadded::new(AtomicUsize::new(tail)),
        }
    }

    /// Splits ring buffer into producer and consumer.
    pub fn split(self) -> (ArcProducer<T, Self>, ArcConsumer<T, Self>) {
        let arc = Arc::new(self);
        (ArcProducer::new(arc.clone()), ArcConsumer::new(arc))
    }

    pub fn split_ref(&mut self) -> (RefProducer<'_, T, Self>, RefConsumer<'_, T, Self>) {
        (RefProducer::new(self), RefConsumer::new(self))
    }
}

impl<T, C: Container<MaybeUninit<T>>> RingBufferBase<T> for RingBuffer<T, C> {
    fn capacity(&self) -> usize {
        self.data.len()
    }

    fn occupied_len(&self) -> usize {
        (self.modulus() + self.tail() - self.head()) % self.modulus()
    }
    fn vacant_len(&self) -> usize {
        (self.modulus() + self.head() - self.tail() - self.capacity()) % self.modulus()
    }
}

impl<T, C: Container<MaybeUninit<T>>> RingBufferHead<T> for RingBuffer<T, C> {
    unsafe fn move_head(&self, count: usize) {
        assert!(count <= self.occupied_len());
        self.head
            .store((self.head() + count) % self.modulus(), Ordering::Release);
    }

    unsafe fn occupied_slices(&self) -> (&mut [MaybeUninit<T>], &mut [MaybeUninit<T>]) {
        let head = self.head();
        let tail = self.tail();
        let len = self.data.len();

        let ranges = match head.cmp(&tail) {
            cmp::Ordering::Less => ((head % len)..(tail % len), 0..0),
            cmp::Ordering::Greater => ((head % len)..len, 0..(tail % len)),
            cmp::Ordering::Equal => (0..0, 0..0),
        };

        let ptr = self.data.as_mut_slice().as_mut_ptr();
        (
            slice::from_raw_parts_mut(ptr.add(ranges.0.start), ranges.0.len()),
            slice::from_raw_parts_mut(ptr.add(ranges.1.start), ranges.1.len()),
        )
    }
}

impl<T, C: Container<MaybeUninit<T>>> RingBufferTail<T> for RingBuffer<T, C> {
    unsafe fn move_tail(&self, count: usize) {
        assert!(count <= self.vacant_len());
        self.tail
            .store((self.tail() + count) % self.modulus(), Ordering::Release);
    }

    unsafe fn vacant_slices(&self) -> (&mut [MaybeUninit<T>], &mut [MaybeUninit<T>]) {
        let head = self.head();
        let tail = self.tail();
        let len = self.data.len();

        let ranges = match head.cmp(&tail) {
            cmp::Ordering::Less => ((tail % len)..len, 0..(head % len)),
            cmp::Ordering::Greater => ((tail % len)..(head % len), 0..0),
            cmp::Ordering::Equal => (0..0, 0..0),
        };

        let ptr = self.data.as_mut_slice().as_mut_ptr();
        (
            slice::from_raw_parts_mut(ptr.add(ranges.0.start), ranges.0.len()),
            slice::from_raw_parts_mut(ptr.add(ranges.1.start), ranges.1.len()),
        )
    }
}

impl<T, C: Container<MaybeUninit<T>>> Drop for RingBuffer<T, C> {
    fn drop(&mut self) {
        let (left, right) = unsafe { self.occupied_slices() };
        for elem in left.iter_mut().chain(right.iter_mut()) {
            unsafe { ptr::drop_in_place(elem.as_mut_ptr()) };
        }
    }
}

impl<T> RingBuffer<T, Vec<MaybeUninit<T>>> {
    pub fn new(capacity: usize) -> Self {
        let mut data = Vec::new();
        data.resize_with(capacity, MaybeUninit::uninit);
        unsafe { Self::from_raw_parts(data, 0, 0) }
    }
}

impl<T, const N: usize> Default for RingBuffer<T, [MaybeUninit<T>; N]> {
    fn default() -> Self {
        let uninit = MaybeUninit::<[T; N]>::uninit();
        let array = unsafe { (&uninit as *const _ as *const [MaybeUninit<T>; N]).read() };
        unsafe { Self::from_raw_parts(array, 0, 0) }
    }
}

/*
/// Moves at most `count` items from the `src` consumer to the `dst` producer.
/// Consumer and producer may be of different buffers as well as of the same one.
///
/// `count` is the number of items being moved, if `None` - as much as possible items will be moved.
///
/// Returns number of items been moved.
pub fn move_items<T>(src: &mut Consumer<T>, dst: &mut Producer<T>, count: Option<usize>) -> usize {
    unsafe {
        src.pop_access(|src_left, src_right| -> usize {
            dst.push_access(|dst_left, dst_right| -> usize {
                let n = count.unwrap_or_else(|| {
                    min(
                        src_left.len() + src_right.len(),
                        dst_left.len() + dst_right.len(),
                    )
                });
                let mut m = 0;
                let mut src = (SlicePtr::new(src_left), SlicePtr::new(src_right));
                let mut dst = (SlicePtr::new(dst_left), SlicePtr::new(dst_right));

                loop {
                    let k = min(n - m, min(src.0.len, dst.0.len));
                    if k == 0 {
                        break;
                    }
                    copy(src.0.ptr, dst.0.ptr, k);
                    if src.0.len == k {
                        src.0 = src.1;
                        src.1 = SlicePtr::null();
                    } else {
                        src.0.shift(k);
                    }
                    if dst.0.len == k {
                        dst.0 = dst.1;
                        dst.1 = SlicePtr::null();
                    } else {
                        dst.0.shift(k);
                    }
                    m += k
                }

                m
            })
        })
    }
}
*/
