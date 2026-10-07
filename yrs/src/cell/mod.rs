#[cfg(feature = "sync")]
pub mod sync;

use std::cell::{Ref, RefCell, RefMut};
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
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

    /// Acquires a shared borrow, which keeps this cell alive for as long as it's held.
    pub fn acquire_owned(&self) -> OwnedRef<T>
    where
        T: 'static,
    {
        self.try_acquire_owned()
            .expect("RcCell is already mutably borrowed")
    }

    pub fn try_acquire_owned(&self) -> Option<OwnedRef<T>>
    where
        T: 'static,
    {
        let cell = self.clone();
        let guard = cell.inner.try_borrow().ok()?;
        // SAFETY: the borrowed RefCell lives in the Rc allocation, which `cell` keeps alive until
        // after the guard is released (see Drop impl).
        let guard: Ref<'static, T> = unsafe { std::mem::transmute(guard) };
        Some(OwnedRef {
            guard: ManuallyDrop::new(guard),
            cell,
        })
    }

    /// Acquires an exclusive borrow, which keeps this cell alive for as long as it's held.
    pub fn acquire_owned_mut(&self) -> OwnedMut<T>
    where
        T: 'static,
    {
        self.try_acquire_owned_mut()
            .expect("RcCell is already borrowed")
    }

    pub fn try_acquire_owned_mut(&self) -> Option<OwnedMut<T>>
    where
        T: 'static,
    {
        let cell = self.clone();
        let guard = cell.inner.try_borrow_mut().ok()?;
        // SAFETY: see [RcCell::try_acquire_owned].
        let guard: RefMut<'static, T> = unsafe { std::mem::transmute(guard) };
        Some(OwnedMut {
            guard: ManuallyDrop::new(guard),
            cell,
        })
    }
}

/// Shared borrow of a [RcCell] contents, which owns a reference to that cell.
pub struct OwnedRef<T: 'static> {
    /// Must be released before `cell`.
    guard: ManuallyDrop<Ref<'static, T>>,
    cell: RcCell<T>,
}

impl<T: 'static> OwnedRef<T> {
    /// The cell, which contents are borrowed.
    pub fn cell(&self) -> &RcCell<T> {
        &self.cell
    }
}

impl<T: 'static> Deref for OwnedRef<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T: 'static> Drop for OwnedRef<T> {
    fn drop(&mut self) {
        unsafe { ManuallyDrop::drop(&mut self.guard) }
    }
}

/// Exclusive borrow of a [RcCell] contents, which owns a reference to that cell.
pub struct OwnedMut<T: 'static> {
    /// Must be released before `cell`.
    guard: ManuallyDrop<RefMut<'static, T>>,
    cell: RcCell<T>,
}

impl<T: 'static> OwnedMut<T> {
    /// The cell, which contents are borrowed.
    pub fn cell(&self) -> &RcCell<T> {
        &self.cell
    }
}

impl<T: 'static> Deref for OwnedMut<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T: 'static> DerefMut for OwnedMut<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T: 'static> Drop for OwnedMut<T> {
    fn drop(&mut self) {
        unsafe { ManuallyDrop::drop(&mut self.guard) }
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
