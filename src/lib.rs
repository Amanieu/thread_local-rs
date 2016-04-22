// Copyright 2016 Amanieu d'Antras
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
//! A `CachedThreadLocal` type is also provided which wraps a `ThreadLocal` but
//! also uses a special fast path for the first thread that writes into it. The
//! fast path has very low overhead (<1ns per access) while keeping the same
//! performance as `ThreadLocal` for other threads.
//!
//! Note that since thread IDs are recycled when a thread exits, it is possible
//! for one thread to retrieve the object of another thread. Since this can only
//! occur after a thread has exited this does not lead to any race conditions.
//!
//! # Example
//!
//! ```rust
//! use thread_local::ThreadLocal;
//! let tls: ThreadLocal<u32> = ThreadLocal::new();
//! assert_eq!(tls.get(), None);
//! assert_eq!(tls.get_or(|| Box::new(5)), &5);
//! assert_eq!(tls.get(), Some(&5));
//! ```

#![warn(missing_docs)]

extern crate thread_id;

use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::marker::PhantomData;
use std::cell::UnsafeCell;
use std::mem;

// Option::unchecked_unwrap
trait UncheckedOptionExt<T> {
    unsafe fn unchecked_unwrap(self) -> T;
}
impl<T> UncheckedOptionExt<T> for Option<T> {
    unsafe fn unchecked_unwrap(self) -> T {
        unsafe fn unreachable() -> ! {
            enum Void {}
            match *(1 as *const Void) {}
        }
        match self {
            Some(x) => x,
            None => unreachable(),
        }
    }
}

// BoxExt::{from_raw,into_raw}
trait BoxExt<T: ?Sized> {
    fn into_raw(b: Self) -> *mut T;
    unsafe fn from_raw(raw: *mut T) -> Self;
}
impl<T: ?Sized> BoxExt<T> for Box<T> {
    fn into_raw(mut b: Self) -> *mut T {
        let ptr: *mut T = &mut *b;
        mem::forget(b);
        ptr
    }
    unsafe fn from_raw(raw: *mut T) -> Self {
        mem::transmute(raw)
    }
}

/// Thread-local variable wrapper
///
/// See the [module-level documentation](index.html) for more.
pub struct ThreadLocal<T: ?Sized + Send> {
    // Pointer to the current top-level hash table
    table: AtomicPtr<Table<T>>,

    // Lock used to guard against concurrent modifications. This is only taken
    // while writing to the table, not when reading from it.
    lock: Mutex<usize>,

    // PhantomData to indicate that we logically own T
    marker: PhantomData<T>,
}

struct Table<T: ?Sized + Send> {
    // Hash entries for the table
    entries: Box<[TableEntry<T>]>,

    // Number of bits used for the hash function
    hash_bits: usize,

    // Previous table, half the size of the current one
    prev: Option<Box<Table<T>>>,
}

struct TableEntry<T: ?Sized + Send> {
    // Current owner of this entry, or 0 if this is an empty entry
    owner: AtomicUsize,

    // The object associated with this entry. This is only ever accessed by the
    // owner of the entry.
    data: UnsafeCell<Option<Box<T>>>,
}

// ThreadLocal is always Sync, even if T isn't
unsafe impl<T: ?Sized + Send> Sync for ThreadLocal<T> {}

impl<T: ?Sized + Send> Default for ThreadLocal<T> {
    fn default() -> ThreadLocal<T> {
        ThreadLocal::new()
    }
}

impl<T: ?Sized + Send> Drop for ThreadLocal<T> {
    fn drop(&mut self) {
        unsafe {
            let _: Box<Table<T>> = BoxExt::from_raw(self.table.load(Ordering::Relaxed));
        }
    }
}

// Implementation of Clone for TableEntry, needed to make vec![] work
impl<T: ?Sized + Send> Clone for TableEntry<T> {
    fn clone(&self) -> TableEntry<T> {
        TableEntry {
            owner: AtomicUsize::new(0),
            data: UnsafeCell::new(None),
        }
    }
}

// Hash function for the thread id
#[cfg(target_pointer_width = "32")]
#[inline]
fn hash(id: usize, bits: usize) -> usize {
    id.wrapping_mul(0x9E3779B9) >> (32 - bits)
}
#[cfg(target_pointer_width = "64")]
#[inline]
fn hash(id: usize, bits: usize) -> usize {
    id.wrapping_mul(0x9E3779B97F4A7C15) >> (64 - bits)
}

impl<T: ?Sized + Send> ThreadLocal<T> {
    /// Creates a new empty `ThreadLocal`.
    pub fn new() -> ThreadLocal<T> {
        let entry = TableEntry {
            owner: AtomicUsize::new(0),
            data: UnsafeCell::new(None),
        };
        let table = Table {
            entries: vec![entry; 2].into_boxed_slice(),
            hash_bits: 1,
            prev: None,
        };
        ThreadLocal {
            table: AtomicPtr::new(BoxExt::into_raw(Box::new(table))),
            lock: Mutex::new(0),
            marker: PhantomData,
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
        where F: FnOnce() -> Box<T>
    {
        let id = thread_id::get();
        match self.get_fast(id) {
            Some(x) => x,
            None => self.insert(id, create(), true),
        }
    }

    // Simple hash table lookup function
    fn lookup(id: usize, table: &Table<T>) -> Option<&UnsafeCell<Option<Box<T>>>> {
        // Because we use a Mutex to prevent concurrent modifications (but not
        // reads) of the hash table, we can avoid any memory barriers here. No
        // elements between our hash bucket and our value can have been modified
        // since we inserted our thread-local value into the table.
        for entry in table.entries.iter().cycle().skip(hash(id, table.hash_bits)) {
            let owner = entry.owner.load(Ordering::Relaxed);
            if owner == id {
                return Some(&entry.data);
            }
            if owner == 0 {
                return None;
            }
        }
        unreachable!();
    }

    // Fast path: try to find our thread in the top-level hash table
    fn get_fast(&self, id: usize) -> Option<&T> {
        let table = unsafe { &*self.table.load(Ordering::Relaxed) };
        match Self::lookup(id, table) {
            Some(x) => unsafe { Some((*x.get()).as_ref().unchecked_unwrap()) },
            None => self.get_slow(id, table),
        }
    }

    // Slow path: try to find our thread in the other hash tables, and then
    // move it to the top-level hash table.
    #[cold]
    fn get_slow(&self, id: usize, table_top: &Table<T>) -> Option<&T> {
        let mut current = &table_top.prev;
        while let Some(ref table) = *current {
            if let Some(x) = Self::lookup(id, table) {
                let data = unsafe { (*x.get()).take().unchecked_unwrap() };
                return Some(self.insert(id, data, false));
            }
            current = &table.prev;
        }
        None
    }

    #[cold]
    fn insert(&self, id: usize, data: Box<T>, new: bool) -> &T {
        // Lock the Mutex to ensure only a single thread is modify the hash
        // table at once.
        let mut count = self.lock.lock().unwrap();
        if new {
            *count += 1;
        }
        let table_raw = self.table.load(Ordering::Relaxed);
        let table = unsafe { &*table_raw };

        // If the current top-level hash table is more than 75% full, add a new
        // level with 2x the capacity. Elements will be moved up to the new top
        // level table as they are accessed.
        let table = if *count > table.entries.len() * 3 / 4 {
            let entry = TableEntry {
                owner: AtomicUsize::new(0),
                data: UnsafeCell::new(None),
            };
            let new_table = BoxExt::into_raw(Box::new(Table {
                entries: vec![entry; table.entries.len() * 2].into_boxed_slice(),
                hash_bits: table.hash_bits + 1,
                prev: unsafe { Some(BoxExt::from_raw(table_raw)) },
            }));
            self.table.store(new_table, Ordering::Release);
            unsafe { &*new_table }
        } else {
            table
        };

        // Insert the new element into the top-level hash table
        for entry in table.entries.iter().cycle().skip(hash(id, table.hash_bits)) {
            let owner = entry.owner.load(Ordering::Relaxed);
            if owner == 0 {
                unsafe {
                    entry.owner.store(id, Ordering::Relaxed);
                    *entry.data.get() = Some(data);
                    return (*entry.data.get()).as_ref().unchecked_unwrap();
                }
            }
            if owner == id {
                // This can happen if create() inserted a value into this
                // ThreadLocal between our calls to get_fast() and insert(). We
                // just return the existing value and drop the newly-allocated
                // Box.
                unsafe {
                    return (*entry.data.get()).as_ref().unchecked_unwrap();
                }
            }
        }
        unreachable!();
    }
}

impl<T: Send + Default> ThreadLocal<T> {
    /// Returns the element for the current thread, or creates a default one if
    /// it doesn't exist.
    pub fn get_default(&self) -> &T {
        self.get_or(|| Box::new(T::default()))
    }
}

/// Wrapper around `ThreadLocal` which adds a fast path for a single thread.
///
/// This has the same API as `ThreadLocal`, but will register the first thread
/// that sets a value as its owner. All accesses by the owner will go through
/// a special fast path which is much faster than the normal `ThreadLocal` path.
pub struct CachedThreadLocal<T: ?Sized + Send> {
    owner: AtomicUsize,
    local: UnsafeCell<Option<Box<T>>>,
    global: ThreadLocal<T>,
}

// CachedThreadLocal is always Sync, even if T isn't
unsafe impl<T: ?Sized + Send> Sync for CachedThreadLocal<T> {}

impl<T: ?Sized + Send> Default for CachedThreadLocal<T> {
    fn default() -> CachedThreadLocal<T> {
        CachedThreadLocal::new()
    }
}

impl<T: ?Sized + Send> CachedThreadLocal<T> {
    /// Creates a new empty `CachedThreadLocal`.
    pub fn new() -> CachedThreadLocal<T> {
        CachedThreadLocal {
            owner: AtomicUsize::new(0),
            local: UnsafeCell::new(None),
            global: ThreadLocal::new(),
        }
    }

    /// Returns the element for the current thread, if it exists.
    pub fn get(&self) -> Option<&T> {
        let id = thread_id::get();
        let owner = self.owner.load(Ordering::Relaxed);
        if owner == id {
            return unsafe { Some((*self.local.get()).as_ref().unchecked_unwrap()) };
        }
        if owner == 0 {
            return None;
        }
        self.global.get_fast(id)
    }

    /// Returns the element for the current thread, or creates it if it doesn't
    /// exist.
    #[inline(always)]
    pub fn get_or<F>(&self, create: F) -> &T
        where F: FnOnce() -> Box<T>
    {
        let id = thread_id::get();
        let owner = self.owner.load(Ordering::Relaxed);
        if owner == id {
            return unsafe { (*self.local.get()).as_ref().unchecked_unwrap() };
        }
        self.get_or_slow(id, owner, create)
    }

    #[cold]
    #[inline(never)]
    fn get_or_slow<F>(&self, id: usize, owner: usize, create: F) -> &T
        where F: FnOnce() -> Box<T>
    {
        if owner == 0 && self.owner.compare_and_swap(0, id, Ordering::Relaxed) == 0 {
            unsafe {
                (*self.local.get()) = Some(create());
                return (*self.local.get()).as_ref().unchecked_unwrap();
            }
        }
        match self.global.get_fast(id) {
            Some(x) => x,
            None => self.global.insert(id, create(), true),
        }
    }
}

impl<T: Send + Default> CachedThreadLocal<T> {
    /// Returns the element for the current thread, or creates a default one if
    /// it doesn't exist.
    pub fn get_default(&self) -> &T {
        self.get_or(|| Box::new(T::default()))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering::Relaxed;
    use std::thread;
    use super::{ThreadLocal, CachedThreadLocal};

    fn make_create() -> Arc<Fn() -> Box<usize> + Send + Sync> {
        let count = AtomicUsize::new(0);
        Arc::new(move || Box::new(count.fetch_add(1, Relaxed)))
    }

    #[test]
    fn same_thread() {
        let create = make_create();
        let tls = ThreadLocal::new();
        assert_eq!(None, tls.get());
        assert_eq!(0, *tls.get_or(|| create()));
        assert_eq!(Some(&0), tls.get());
        assert_eq!(0, *tls.get_or(|| create()));
        assert_eq!(Some(&0), tls.get());
        assert_eq!(0, *tls.get_or(|| create()));
        assert_eq!(Some(&0), tls.get());
    }

    #[test]
    fn same_thread_cached() {
        let create = make_create();
        let tls = CachedThreadLocal::new();
        assert_eq!(None, tls.get());
        assert_eq!(0, *tls.get_or(|| create()));
        assert_eq!(Some(&0), tls.get());
        assert_eq!(0, *tls.get_or(|| create()));
        assert_eq!(Some(&0), tls.get());
        assert_eq!(0, *tls.get_or(|| create()));
        assert_eq!(Some(&0), tls.get());
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
    fn is_sync() {
        fn foo<T: Sync>() {}
        foo::<ThreadLocal<String>>();
        foo::<ThreadLocal<RefCell<String>>>();
        foo::<CachedThreadLocal<String>>();
        foo::<CachedThreadLocal<RefCell<String>>>();
    }
}
