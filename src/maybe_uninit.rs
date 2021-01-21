#[cfg(supports_maybe_uninit)]
pub(crate) use std::mem::MaybeUninit;

#[cfg(not(supports_maybe_uninit))]
mod polyfill {
    use std::mem::ManuallyDrop;
    use std::ptr;

    use unreachable::UncheckedOptionExt;

    /// A simple `Option`-based implementation of `MaybeUninit` for compiler versions < 1.36.0.
    pub struct MaybeUninit<T>(Option<ManuallyDrop<T>>);

    impl<T> MaybeUninit<T> {
        pub fn new(val: T) -> Self {
            MaybeUninit(Some(ManuallyDrop::new(val)))
        }
        pub fn uninit() -> Self {
            MaybeUninit(None)
        }
        pub fn as_ptr(&self) -> *const T {
            self.0
                .as_ref()
                .map(|v| &**v as *const _)
                .unwrap_or_else(ptr::null)
        }
        pub fn as_mut_ptr(&mut self) -> *mut T {
            self.0
                .as_mut()
                .map(|v| &mut **v as *mut _)
                .unwrap_or_else(ptr::null_mut)
        }
        pub unsafe fn assume_init(self) -> T {
            ManuallyDrop::into_inner(self.0.unchecked_unwrap())
        }
    }
}
#[cfg(not(supports_maybe_uninit))]
pub(crate) use self::polyfill::MaybeUninit;
