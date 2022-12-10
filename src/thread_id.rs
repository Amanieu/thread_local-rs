// Copyright 2017 Amanieu d'Antras
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use crate::POINTER_WIDTH;
use cfg_if::cfg_if;
use once_cell::sync::Lazy;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::mem::transmute;
use std::sync::Mutex;
use std::usize;

/// Thread ID manager which allocates thread IDs. It attempts to aggressively
/// reuse thread IDs where possible to avoid cases where a ThreadLocal grows
/// indefinitely when it is used by many short-lived threads.
struct ThreadIdManager {
    free_from: usize,
    free_list: BinaryHeap<Reverse<usize>>,
}
impl ThreadIdManager {
    fn new() -> ThreadIdManager {
        ThreadIdManager {
            free_from: 0,
            free_list: BinaryHeap::new(),
        }
    }
    fn alloc(&mut self) -> usize {
        if let Some(id) = self.free_list.pop() {
            id.0
        } else {
            let id = self.free_from;
            self.free_from = self
                .free_from
                .checked_add(1)
                .expect("Ran out of thread IDs");
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
    /// The thread ID obtained from the thread ID manager.
    pub(crate) id: usize,
    /// The bucket this thread's local storage will be in.
    pub(crate) bucket: usize,
    /// The size of the bucket this thread's local storage will be in.
    pub(crate) bucket_size: usize,
    /// The index into the bucket this thread's local storage is in.
    pub(crate) index: usize,
}
impl Thread {
    fn new(id: usize) -> Thread {
        let bucket = usize::from(POINTER_WIDTH) - id.leading_zeros() as usize;
        let bucket_size = 1 << bucket.saturating_sub(1);
        let index = if id != 0 { id ^ bucket_size } else { 0 };

        Thread {
            id,
            bucket,
            bucket_size,
            index,
        }
    }
}

/// Wrapper around `Thread` that allocates and deallocates the ID.
#[derive(Clone)]
pub(crate) struct ThreadHolder<const OWNER: bool>(Thread);

impl<const OWNER: bool> ThreadHolder<OWNER> {
    #[inline(always)]
    pub(crate) fn into_inner(self) -> Thread {
        self.0
    }
}

cfg_if! {
    if #[cfg(feature = "nightly")] {
        impl ThreadHolder<true> {
            pub(crate) fn new() -> Self {
                // we have to initialize `THREAD_HOLDER_GUARD` in order for it to protect
                // `THREAD_HOLDER` when it gets initialized
                THREAD_HOLDER_GUARD.with(|_| {});
                ThreadHolder(Thread::new(THREAD_ID_MANAGER.lock().unwrap().alloc()))
            }
        }
    } else {
        impl ThreadHolder<true> {
            fn new() -> Self {
                ThreadHolder(Thread::new(THREAD_ID_MANAGER.lock().unwrap().alloc()))
            }
        }
    }
}

impl<const OWNER: bool> Drop for ThreadHolder<OWNER> {
    fn drop(&mut self) {
        if OWNER {
            THREAD_ID_MANAGER.lock().unwrap().free(self.0.id);
        }
    }
}

cfg_if! {
    if #[cfg(feature = "nightly")] {
        #[thread_local]
        static mut THREAD_HOLDER: Option<ThreadHolder<true>> = None;

        thread_local! { static THREAD_HOLDER_GUARD: ThreadHolderGuard = const { ThreadHolderGuard }; }

        struct ThreadHolderGuard;

        impl Drop for ThreadHolderGuard {
            fn drop(&mut self) {
                // SAFETY: this is safe because we know that we (the current thread)
                // are the only one who can be accessing our `THREAD_HOLDER` and thus
                // it's safe for us to access and drop it.
                unsafe { THREAD_HOLDER.take(); }
            }
        }

        #[inline]
        pub(crate) fn try_get_thread_holder() -> Option<ThreadHolder<false>> {
            // SAFETY: this is safe as the only two possibilities for updates
            // are when this thread gets stopped or when the thread holder
            // gets first set (which is no problem for this as it can't happen
            // during this function call and after the clone we don't
            // care about how the data we return is used)
            // the transmute is safe because the only thing we are changing
            // with it is the const generic parameter to a more restrictive
            // one which is safe
            unsafe { transmute(THREAD_HOLDER.clone()) }
        }

        #[inline]
        pub(crate) fn set_thread_holder(thread_holder: ThreadHolder<true>) {
            // SAFETY: this is safe because we know that there are no references
            // to `THREAD_HOLDER` alive when this function gets called
            // and thus we don't have to care about potential unsafety
            // because of references, because there are none
            // also the data is thread local which means that
            // it's impossible for data races to occur
            unsafe { THREAD_HOLDER = Some(thread_holder); }
        }

    } else {
        thread_local!(static THREAD_HOLDER: ThreadHolder<true> = ThreadHolder::new());

        /// Get the current thread.
        #[inline]
        pub(crate) fn get() -> Thread {
            THREAD_HOLDER.with(|holder| holder.0)
        }
    }
}

#[test]
fn test_thread() {
    let thread = Thread::new(0);
    assert_eq!(thread.id, 0);
    assert_eq!(thread.bucket, 0);
    assert_eq!(thread.bucket_size, 1);
    assert_eq!(thread.index, 0);

    let thread = Thread::new(1);
    assert_eq!(thread.id, 1);
    assert_eq!(thread.bucket, 1);
    assert_eq!(thread.bucket_size, 1);
    assert_eq!(thread.index, 0);

    let thread = Thread::new(2);
    assert_eq!(thread.id, 2);
    assert_eq!(thread.bucket, 2);
    assert_eq!(thread.bucket_size, 2);
    assert_eq!(thread.index, 0);

    let thread = Thread::new(3);
    assert_eq!(thread.id, 3);
    assert_eq!(thread.bucket, 2);
    assert_eq!(thread.bucket_size, 2);
    assert_eq!(thread.index, 1);

    let thread = Thread::new(19);
    assert_eq!(thread.id, 19);
    assert_eq!(thread.bucket, 5);
    assert_eq!(thread.bucket_size, 16);
    assert_eq!(thread.index, 3);
}
