#[cfg(feature = "sync")]
pub mod sync;

use std::cell::RefCell;
use std::rc::Rc;

pub trait Acquire {
    type Ref<'a>
    where
        Self: 'a;

    fn acquire(&self) -> Self::Ref<'_>;

    fn try_acquire(&self) -> Option<Self::Ref<'_>>;
}

pub trait AcquireMut: Acquire {
    type Mut<'a>
    where
        Self: 'a;

    fn acquire_mut(&self) -> Self::Mut<'_>;

    fn try_acquire_mut(&self) -> Option<Self::Mut<'_>>;
}

#[repr(transparent)]
#[derive(Debug, Default)]
pub struct RcCell<T> {
    inner: Rc<RefCell<T>>,
}

impl<T> RcCell<T> {
    pub fn new(inner: T) -> Self {
        Self {
            inner: Rc::new(RefCell::new(inner)),
        }
    }

    pub fn as_ptr(&self) -> *const T {
        Rc::as_ptr(&self.inner) as _
    }

    pub fn into_raw(self) -> *mut T {
        Rc::into_raw(self.inner) as _
    }

    pub unsafe fn from_raw(ptr: *mut T) -> Self {
        Self {
            inner: Rc::from_raw(ptr as *mut RefCell<T>),
        }
    }
}

impl<T> Clone for RcCell<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Acquire for RcCell<T> {
    type Ref<'a>
    where
        Self: 'a,
    = std::cell::Ref<'a, T>;

    fn acquire(&self) -> Self::Ref<'_> {
        self.inner.borrow()
    }

    fn try_acquire(&self) -> Option<Self::Ref<'_>> {
        self.inner.try_borrow().ok()
    }
}

impl<T> AcquireMut for RcCell<T> {
    type Mut<'a>
    where
        Self: 'a,
    = std::cell::RefMut<'a, T>;

    fn acquire_mut(&self) -> Self::Mut<'_> {
        self.inner.borrow_mut()
    }

    fn try_acquire_mut(&self) -> Option<Self::Mut<'_>> {
        self.inner.try_borrow_mut().ok()
    }
}
