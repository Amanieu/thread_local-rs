use crossbeam_utils::Backoff;
use std::cell::UnsafeCell;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};

/// A mutex optimized for little contention.
pub(crate) struct Mutex<T> {
    guard: AtomicBool,
    data: UnsafeCell<T>,
}

impl<T> Mutex<T> {
    #[inline]
    pub const fn new(val: T) -> Self {
        Self {
            guard: AtomicBool::new(false),
            data: UnsafeCell::new(val),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        if self
            .guard
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return MutexGuard(self);
        }

        let backoff = Backoff::new();
        while self
            .guard
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            backoff.snooze();
        }
        MutexGuard(self)
    }

    #[inline]
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        if self
            .guard
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        Some(MutexGuard(self))
    }
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Sync> Sync for Mutex<T> {}

pub(crate) struct MutexGuard<'a, T>(&'a Mutex<T>);

impl<'a, T> Deref for MutexGuard<'a, T> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.0.data.get() }
    }
}

impl<'a, T> DerefMut for MutexGuard<'a, T> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.0.data.get() }
    }
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    #[inline]
    fn drop(&mut self) {
        self.0.guard.store(false, Ordering::Release);
    }
}
