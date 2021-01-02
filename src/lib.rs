// Copyright 2017 Amanieu d'Antras
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

//! Per-object thread-local storage
//!
//! This library provides the `ThreadLocal` type which allows a separate copy of
//! an object to be used for each thread. This allows for per-object
//! thread-local storage, unlike the standard library's `thread_local!` macro
//! which only allows static thread-local storage.
//!
//! Per-thread objects are not destroyed when a thread exits. Instead, objects
//! are only destroyed when the `ThreadLocal` containing them is destroyed.
//!
//! You can also iterate over the thread-local values of all thread in a
//! `ThreadLocal` object using the `iter_mut` and `into_iter` methods. This can
//! only be done if you have mutable access to the `ThreadLocal` object, which
//! guarantees that you are the only thread currently accessing it.
//!
//! A `CachedThreadLocal` type is also provided which wraps a `ThreadLocal` but
//! also uses a special fast path for the first thread that writes into it. The
//! fast path has very low overhead (<1ns per access) while keeping the same
//! performance as `ThreadLocal` for other threads.
//!
//! Note that since thread IDs are recycled when a thread exits, it is possible
//! for one thread to retrieve the object of another thread. Since this can only
//! occur after a thread has exited this does not lead to any race conditions.
//!
//! # Examples
//!
//! Basic usage of `ThreadLocal`:
//!
//! ```rust
//! use thread_local::ThreadLocal;
//! let tls: ThreadLocal<u32> = ThreadLocal::new();
//! assert_eq!(tls.get(), None);
//! assert_eq!(tls.get_or(|| 5), &5);
//! assert_eq!(tls.get(), Some(&5));
//! ```
//!
//! Combining thread-local values into a single result:
//!
//! ```rust
//! use thread_local::ThreadLocal;
//! use std::sync::Arc;
//! use std::cell::Cell;
//! use std::thread;
//!
//! let tls = Arc::new(ThreadLocal::new());
//!
//! // Create a bunch of threads to do stuff
//! for _ in 0..5 {
//!     let tls2 = tls.clone();
//!     thread::spawn(move || {
//!         // Increment a counter to count some event...
//!         let cell = tls2.get_or(|| Cell::new(0));
//!         cell.set(cell.get() + 1);
//!     }).join().unwrap();
//! }
//!
//! // Once all threads are done, collect the counter values and return the
//! // sum of all thread-local counter values.
//! let tls = Arc::try_unwrap(tls).unwrap();
//! let total = tls.into_iter().fold(0, |x, y| x + y.get());
//! assert_eq!(total, 5);
//! ```

#![warn(missing_docs)]
#![allow(clippy::mutex_atomic)]

#[macro_use]
extern crate lazy_static;

mod cached;
mod thread_id;
mod unreachable;

pub use cached::{CachedIntoIter, CachedIterMut, CachedThreadLocal};

use std::cell::UnsafeCell;
use std::fmt;
use std::marker::PhantomData;
use std::panic::UnwindSafe;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Mutex;
use unreachable::{UncheckedOptionExt, UncheckedResultExt};

/// Thread-local variable wrapper
///
/// See the [module-level documentation](index.html) for more.
pub struct ThreadLocal<T: Send> {
    // Pointer to the current top-level list
    list: AtomicPtr<List<T>>,

    // Lock used to guard against concurrent modifications. This is only taken
    // while writing to the list, not when reading from it. This also guards
    // the counter for the total number of values in the thread local.
    lock: Mutex<usize>,
}

/// A list of thread-local values.
struct List<T: Send> {
    // The thread local values in this list. If any values is `None`, it is
    // either in an earlier list or it is uninitialized.
    values: Box<[UnsafeCell<Option<T>>]>,

    // Previous list, half the size of the current one
    //
    // This cannot be a Box as that would result in the Box's pointer
    // potentially being aliased when creating a new list, which is UB.
    prev: Option<NonNull<List<T>>>,
}

impl<T: Send> Drop for List<T> {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            drop(unsafe { Box::from_raw(prev.as_ptr()) });
        }
    }
}

// ThreadLocal is always Sync, even if T isn't
unsafe impl<T: Send> Sync for ThreadLocal<T> {}

impl<T: Send> Default for ThreadLocal<T> {
    fn default() -> ThreadLocal<T> {
        ThreadLocal::new()
    }
}

impl<T: Send> Drop for ThreadLocal<T> {
    fn drop(&mut self) {
        unsafe {
            Box::from_raw(*self.list.get_mut());
        }
    }
}

impl<T: Send> ThreadLocal<T> {
    /// Creates a new empty `ThreadLocal`.
    pub fn new() -> ThreadLocal<T> {
        ThreadLocal::with_capacity(2)
    }

    /// Creates a new `ThreadLocal` with an initial capacity. If less than the capacity threads
    /// access the thread local it will never reallocate.
    pub fn with_capacity(capacity: usize) -> ThreadLocal<T> {
        let list = List {
            values: (0..capacity)
                .map(|_| UnsafeCell::new(None))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            prev: None,
        };
        ThreadLocal {
            list: AtomicPtr::new(Box::into_raw(Box::new(list))),
            lock: Mutex::new(0),
        }
    }

    /// Returns the element for the current thread, if it exists.
    pub fn get(&self) -> Option<&T> {
        let id = thread_id::get();
        self.get_fast(id)
    }

    /// Returns the element for the current thread, or creates it if it doesn't
    /// exist.
    pub fn get_or<F>(&self, create: F) -> &T
    where
        F: FnOnce() -> T,
    {
        unsafe {
            self.get_or_try(|| Ok::<T, ()>(create()))
                .unchecked_unwrap_ok()
        }
    }

    /// Returns the element for the current thread, or creates it if it doesn't
    /// exist. If `create` fails, that error is returned and no element is
    /// added.
    pub fn get_or_try<F, E>(&self, create: F) -> Result<&T, E>
    where
        F: FnOnce() -> Result<T, E>,
    {
        let id = thread_id::get();
        match self.get_fast(id) {
            Some(x) => Ok(x),
            None => Ok(self.insert(id, create()?, true)),
        }
    }

    // Fast path: try to find our thread in the top-level list
    fn get_fast(&self, id: usize) -> Option<&T> {
        let list = unsafe { &*self.list.load(Ordering::Acquire) };
        list.values
            .get(id)
            .and_then(|cell| unsafe { &*cell.get() }.as_ref())
            .or_else(|| self.get_slow(id, list))
    }

    // Slow path: try to find our thread in the other lists, and then move it to
    // the top-level list.
    #[cold]
    fn get_slow(&self, id: usize, list_top: &List<T>) -> Option<&T> {
        let mut current = list_top.prev;
        while let Some(list) = current {
            let list = unsafe { list.as_ref() };

            match list.values.get(id) {
                Some(value) => {
                    let value_option = unsafe { &mut *value.get() };
                    if value_option.is_some() {
                        let value = unsafe { value_option.take().unchecked_unwrap() };
                        return Some(self.insert(id, value, false));
                    }
                }
                None => break,
            }
            current = list.prev;
        }
        None
    }

    #[cold]
    fn insert(&self, id: usize, data: T, new: bool) -> &T {
        let list_raw = self.list.load(Ordering::Relaxed);
        let list = unsafe { &*list_raw };

        // Lock the Mutex to ensure only a single thread is adding new lists at
        // once
        let mut count = self.lock.lock().unwrap();
        if new {
            *count += 1;
        }

        // If there isn't space for this thread's local, add a new list.
        let list = if id >= list.values.len() {
            let new_list = Box::into_raw(Box::new(List {
                values: (0..std::cmp::max(list.values.len() * 2, id + 1))
                    // Values will be lazily moved into the top-level list, so
                    // it starts out empty
                    .map(|_| UnsafeCell::new(None))
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
                prev: Some(unsafe { NonNull::new_unchecked(list_raw) }),
            }));
            self.list.store(new_list, Ordering::Release);
            unsafe { &*new_list }
        } else {
            list
        };

        // We are no longer adding new lists, so we don't need the guard
        drop(count);

        // Insert the new element into the top-level list
        unsafe {
            let value_ptr = list.values.get_unchecked(id).get();
            *value_ptr = Some(data);
            (&*value_ptr).as_ref().unchecked_unwrap()
        }
    }

    fn raw_iter(&mut self) -> RawIter<T> {
        RawIter {
            remaining: *self.lock.get_mut().unwrap(),
            index: 0,
            list: *self.list.get_mut(),
        }
    }

    /// Returns a mutable iterator over the local values of all threads in
    /// unspecified order.
    ///
    /// Since this call borrows the `ThreadLocal` mutably, this operation can
    /// be done safely---the mutable borrow statically guarantees no other
    /// threads are currently accessing their associated values.
    pub fn iter_mut(&mut self) -> IterMut<T> {
        IterMut {
            raw: self.raw_iter(),
            marker: PhantomData,
        }
    }

    /// Removes all thread-specific values from the `ThreadLocal`, effectively
    /// reseting it to its original state.
    ///
    /// Since this call borrows the `ThreadLocal` mutably, this operation can
    /// be done safely---the mutable borrow statically guarantees no other
    /// threads are currently accessing their associated values.
    pub fn clear(&mut self) {
        *self = ThreadLocal::new();
    }
}

impl<T: Send> IntoIterator for ThreadLocal<T> {
    type Item = T;
    type IntoIter = IntoIter<T>;

    fn into_iter(mut self) -> IntoIter<T> {
        IntoIter {
            raw: self.raw_iter(),
            _thread_local: self,
        }
    }
}

impl<'a, T: Send + 'a> IntoIterator for &'a mut ThreadLocal<T> {
    type Item = &'a mut T;
    type IntoIter = IterMut<'a, T>;

    fn into_iter(self) -> IterMut<'a, T> {
        self.iter_mut()
    }
}

impl<T: Send + Default> ThreadLocal<T> {
    /// Returns the element for the current thread, or creates a default one if
    /// it doesn't exist.
    pub fn get_or_default(&self) -> &T {
        self.get_or(Default::default)
    }
}

impl<T: Send + fmt::Debug> fmt::Debug for ThreadLocal<T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ThreadLocal {{ local_data: {:?} }}", self.get())
    }
}

impl<T: Send + UnwindSafe> UnwindSafe for ThreadLocal<T> {}

struct RawIter<T: Send> {
    remaining: usize,
    index: usize,
    list: *const List<T>,
}

impl<T: Send> Iterator for RawIter<T> {
    type Item = *mut Option<T>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        loop {
            let values = &*unsafe { &*self.list }.values;

            while self.index < values.len() {
                let val = values[self.index].get();
                self.index += 1;
                if unsafe { (*val).is_some() } {
                    self.remaining -= 1;
                    return Some(val);
                }
            }
            self.index = 0;
            self.list = unsafe { (*self.list).prev.as_ref().unchecked_unwrap().as_ref() };
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

/// Mutable iterator over the contents of a `ThreadLocal`.
pub struct IterMut<'a, T: Send + 'a> {
    raw: RawIter<T>,
    marker: PhantomData<&'a mut ThreadLocal<T>>,
}

impl<'a, T: Send + 'a> Iterator for IterMut<'a, T> {
    type Item = &'a mut T;

    fn next(&mut self) -> Option<&'a mut T> {
        self.raw
            .next()
            .map(|x| unsafe { &mut *(*x).as_mut().unchecked_unwrap() })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.raw.size_hint()
    }
}

impl<'a, T: Send + 'a> ExactSizeIterator for IterMut<'a, T> {}

/// An iterator that moves out of a `ThreadLocal`.
pub struct IntoIter<T: Send> {
    raw: RawIter<T>,
    _thread_local: ThreadLocal<T>,
}

impl<T: Send> Iterator for IntoIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.raw
            .next()
            .map(|x| unsafe { (*x).take().unchecked_unwrap() })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.raw.size_hint()
    }
}

impl<T: Send> ExactSizeIterator for IntoIter<T> {}

#[cfg(test)]
mod tests {
    use super::{CachedThreadLocal, ThreadLocal};
    use std::cell::RefCell;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::Arc;
    use std::thread;

    fn make_create() -> Arc<dyn Fn() -> usize + Send + Sync> {
        let count = AtomicUsize::new(0);
        Arc::new(move || count.fetch_add(1, Relaxed))
    }

    #[test]
    fn same_thread() {
        let create = make_create();
        let mut tls = ThreadLocal::new();
        assert_eq!(None, tls.get());
        assert_eq!("ThreadLocal { local_data: None }", format!("{:?}", &tls));
        assert_eq!(0, *tls.get_or(|| create()));
        assert_eq!(Some(&0), tls.get());
        assert_eq!(0, *tls.get_or(|| create()));
        assert_eq!(Some(&0), tls.get());
        assert_eq!(0, *tls.get_or(|| create()));
        assert_eq!(Some(&0), tls.get());
        assert_eq!("ThreadLocal { local_data: Some(0) }", format!("{:?}", &tls));
        tls.clear();
        assert_eq!(None, tls.get());
    }

    #[test]
    fn same_thread_cached() {
        let create = make_create();
        let mut tls = CachedThreadLocal::new();
        assert_eq!(None, tls.get());
        assert_eq!("ThreadLocal { local_data: None }", format!("{:?}", &tls));
        assert_eq!(0, *tls.get_or(|| create()));
        assert_eq!(Some(&0), tls.get());
        assert_eq!(0, *tls.get_or(|| create()));
        assert_eq!(Some(&0), tls.get());
        assert_eq!(0, *tls.get_or(|| create()));
        assert_eq!(Some(&0), tls.get());
        assert_eq!("ThreadLocal { local_data: Some(0) }", format!("{:?}", &tls));
        tls.clear();
        assert_eq!(None, tls.get());
    }

    #[test]
    fn different_thread() {
        let create = make_create();
        let tls = Arc::new(ThreadLocal::new());
        assert_eq!(None, tls.get());
        assert_eq!(0, *tls.get_or(|| create()));
        assert_eq!(Some(&0), tls.get());

        let tls2 = tls.clone();
        let create2 = create.clone();
        thread::spawn(move || {
            assert_eq!(None, tls2.get());
            assert_eq!(1, *tls2.get_or(|| create2()));
            assert_eq!(Some(&1), tls2.get());
        })
        .join()
        .unwrap();

        assert_eq!(Some(&0), tls.get());
        assert_eq!(0, *tls.get_or(|| create()));
    }

    #[test]
    fn different_thread_cached() {
        let create = make_create();
        let tls = Arc::new(CachedThreadLocal::new());
        assert_eq!(None, tls.get());
        assert_eq!(0, *tls.get_or(|| create()));
        assert_eq!(Some(&0), tls.get());

        let tls2 = tls.clone();
        let create2 = create.clone();
        thread::spawn(move || {
            assert_eq!(None, tls2.get());
            assert_eq!(1, *tls2.get_or(|| create2()));
            assert_eq!(Some(&1), tls2.get());
        })
        .join()
        .unwrap();

        assert_eq!(Some(&0), tls.get());
        assert_eq!(0, *tls.get_or(|| create()));
    }

    #[test]
    fn iter() {
        let tls = Arc::new(ThreadLocal::new());
        tls.get_or(|| Box::new(1));

        let tls2 = tls.clone();
        thread::spawn(move || {
            tls2.get_or(|| Box::new(2));
            let tls3 = tls2.clone();
            thread::spawn(move || {
                tls3.get_or(|| Box::new(3));
            })
            .join()
            .unwrap();
            drop(tls2);
        })
        .join()
        .unwrap();

        let mut tls = Arc::try_unwrap(tls).unwrap();
        let mut v = tls.iter_mut().map(|x| **x).collect::<Vec<i32>>();
        v.sort_unstable();
        assert_eq!(vec![1, 2, 3], v);
        let mut v = tls.into_iter().map(|x| *x).collect::<Vec<i32>>();
        v.sort_unstable();
        assert_eq!(vec![1, 2, 3], v);
    }

    #[test]
    fn iter_cached() {
        let tls = Arc::new(CachedThreadLocal::new());
        tls.get_or(|| Box::new(1));

        let tls2 = tls.clone();
        thread::spawn(move || {
            tls2.get_or(|| Box::new(2));
            let tls3 = tls2.clone();
            thread::spawn(move || {
                tls3.get_or(|| Box::new(3));
            })
            .join()
            .unwrap();
            drop(tls2);
        })
        .join()
        .unwrap();

        let mut tls = Arc::try_unwrap(tls).unwrap();
        let mut v = tls.iter_mut().map(|x| **x).collect::<Vec<i32>>();
        v.sort_unstable();
        assert_eq!(vec![1, 2, 3], v);
        let mut v = tls.into_iter().map(|x| *x).collect::<Vec<i32>>();
        v.sort_unstable();
        assert_eq!(vec![1, 2, 3], v);
    }

    #[test]
    fn is_sync() {
        fn foo<T: Sync>() {}
        foo::<ThreadLocal<String>>();
        foo::<ThreadLocal<RefCell<String>>>();
        foo::<CachedThreadLocal<String>>();
        foo::<CachedThreadLocal<RefCell<String>>>();
    }
}
