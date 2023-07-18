// Copyright 2017 Amanieu d'Antras
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use crate::mutex::Mutex;
use crate::POINTER_WIDTH;
use once_cell::sync::Lazy;
use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Thread ID manager which allocates thread IDs. It attempts to aggressively
/// reuse thread IDs where possible to avoid cases where a ThreadLocal grows
/// indefinitely when it is used by many short-lived threads.
struct ThreadIdManager {
    free_from: usize,
    free_list: BinaryHeap<Reverse<usize>>,
}
impl ThreadIdManager {
    fn new() -> Self {
        Self {
            free_from: 0,
            free_list: BinaryHeap::new(),
        }
    }
    fn alloc(&mut self) -> usize {
        if let Some(id) = self.free_list.pop() {
            id.0
        } else {
            // `free_from` can't overflow as each thread takes up at least 2 bytes of memory and
            // thus we can't even have `usize::MAX / 2 + 1` threads.

            let id = self.free_from;
            self.free_from += 1;
            id
        }
    }
    fn free(&mut self, id: usize) {
        self.free_list.push(Reverse(id));
    }
}
static THREAD_ID_MANAGER: Lazy<Mutex<ThreadIdManager>> =
    Lazy::new(|| Mutex::new(ThreadIdManager::new()));

/// Data which is unique to the current thread while it is running.
/// A thread ID may be reused after a thread exits.
#[derive(Clone, Copy)]
pub(crate) struct Thread {
    /// The bucket this thread's local storage will be in.
    pub(crate) bucket: usize,
    /// The index into the bucket this thread's local storage is in.
    pub(crate) index: usize,
}

impl Thread {
    /// id: The thread ID obtained from the thread ID manager.
    #[inline]
    fn new(id: usize) -> Self {
        let bucket = usize::from(POINTER_WIDTH) - ((id + 1).leading_zeros() as usize) - 1;
        let bucket_size = 1 << bucket;
        let index = id - (bucket_size - 1);
        Self { bucket, index }
    }

    /// The size of the bucket this thread's local storage will be in.
    #[inline]
    pub fn bucket_size(&self) -> usize {
        1 << self.bucket
    }
}

cfg_if::cfg_if! {
    if #[cfg(feature = "nightly")] {
        use memoffset::offset_of;
        use std::ptr::null;
        use std::cell::UnsafeCell;

        // This is split into 2 thread-local variables so that we can check whether the
        // thread is initialized without having to register a thread-local destructor.
        //
        // This makes the fast path smaller.
        #[thread_local]
        static THREAD: UnsafeCell<ThreadWrapper> = UnsafeCell::new(ThreadWrapper {
            self_ptr: null(),
            thread: Thread {
                index: 0,
                bucket: 0,
            },
        });
        thread_local! { static THREAD_GUARD: ThreadGuard = const { ThreadGuard { id: Cell::new(0) } }; }

        // Guard to ensure the thread ID is released on thread exit.
        struct ThreadGuard {
            // We keep a copy of the thread ID in the ThreadGuard: we can't
            // reliably access THREAD in our Drop impl due to the unpredictable
            // order of TLS destructors.
            id: Cell<usize>,
        }

        impl Drop for ThreadGuard {
            fn drop(&mut self) {
                // Release the thread ID. Any further accesses to the thread ID
                // will go through get_slow which will either panic or
                // initialize a new ThreadGuard.
                unsafe {
                    (&mut *THREAD.get()).self_ptr = null();
                }
                THREAD_ID_MANAGER.lock().free(self.id.get());
            }
        }

        /// Data which is unique to the current thread while it is running.
        /// A thread ID may be reused after a thread exits.
        ///
        /// This wrapper exists to hide multiple accesses to the TLS data
        /// from the backend as this can lead to inefficient codegen
        /// (to be precise it can lead to multiple TLS address lookups)
        #[derive(Clone, Copy)]
        struct ThreadWrapper {
            self_ptr: *const Thread,
            thread: Thread,
        }

        impl ThreadWrapper {
            /// The thread ID obtained from the thread ID manager.
            #[inline]
            fn new(id: usize) -> Self {
                Self {
                    self_ptr: ((THREAD.get().cast_const() as usize) + offset_of!(ThreadWrapper, thread)) as *const Thread,
                    thread: Thread::new(id),
                }
            }
        }

        /// Returns a thread ID for the current thread, allocating one if needed.
        #[inline]
        pub(crate) fn get() -> Thread {
            let thread = unsafe { *THREAD.get() };
            if !thread.self_ptr.is_null() {
                unsafe { thread.self_ptr.read() }
            } else {
                get_slow()
            }
        }

        /// Out-of-line slow path for allocating a thread ID.
        #[cold]
         fn get_slow() -> Thread {
            let id = THREAD_ID_MANAGER.lock().alloc();
            let new = ThreadWrapper::new(id);
            unsafe {
                *THREAD.get() = new;
            }
            THREAD_GUARD.with(|guard| guard.id.set(id));
            new.thread
        }
    } else {
        // This is split into 2 thread-local variables so that we can check whether the
        // thread is initialized without having to register a thread-local destructor.
        //
        // This makes the fast path smaller.
        thread_local! { static THREAD: Cell<Option<Thread>> = const { Cell::new(None) }; }
        thread_local! { static THREAD_GUARD: ThreadGuard = const { ThreadGuard { id: Cell::new(0) } }; }

        // Guard to ensure the thread ID is released on thread exit.
        struct ThreadGuard {
            // We keep a copy of the thread ID in the ThreadGuard: we can't
            // reliably access THREAD in our Drop impl due to the unpredictable
            // order of TLS destructors.
            id: Cell<usize>,
        }

        impl Drop for ThreadGuard {
            fn drop(&mut self) {
                // Release the thread ID. Any further accesses to the thread ID
                // will go through get_slow which will either panic or
                // initialize a new ThreadGuard.
                let _ = THREAD.try_with(|thread| thread.set(None));
                THREAD_ID_MANAGER.lock().free(self.id.get());
            }
        }

        /// Returns a thread ID for the current thread, allocating one if needed.
        #[inline]
        pub(crate) fn get() -> Thread {
            THREAD.with(|thread| {
                if let Some(thread) = thread.get() {
                    thread
                } else {
                    get_slow(thread)
                }
            })
        }

        /// Out-of-line slow path for allocating a thread ID.
        #[cold]
        fn get_slow(thread: &Cell<Option<Thread>>) -> Thread {
            let id = THREAD_ID_MANAGER.lock().alloc();
            let new = Thread::new(id);
            thread.set(Some(new));
            THREAD_GUARD.with(|guard| guard.id.set(id));
            new
        }
    }
}

#[test]
fn test_thread() {
    let thread = Thread::new(0);
    assert_eq!(thread.bucket, 0);
    assert_eq!(thread.bucket_size(), 1);
    assert_eq!(thread.index, 0);

    let thread = Thread::new(1);
    assert_eq!(thread.bucket, 1);
    assert_eq!(thread.bucket_size(), 2);
    assert_eq!(thread.index, 0);

    let thread = Thread::new(2);
    assert_eq!(thread.bucket, 1);
    assert_eq!(thread.bucket_size(), 2);
    assert_eq!(thread.index, 1);

    let thread = Thread::new(3);
    assert_eq!(thread.bucket, 2);
    assert_eq!(thread.bucket_size(), 4);
    assert_eq!(thread.index, 0);

    let thread = Thread::new(19);
    assert_eq!(thread.bucket, 4);
    assert_eq!(thread.bucket_size(), 16);
    assert_eq!(thread.index, 4);
}
