use crate::block::Block;
use crate::cell::{Acquire, AcquireMut};
use async_lock::{RwLock, RwLockReadGuardArc, RwLockWriteGuardArc};
use std::cell::RefCell;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::Arc;

#[repr(transparent)]
#[derive(Debug, Default)]
pub struct ArcCell<T> {
    inner: Arc<RwLock<T>>,
}

impl<T> ArcCell<T> {
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(RwLock::new(value)),
        }
    }

    pub async fn acquire_async(&self) -> async_lock::RwLockReadGuard<'_, T> {
        self.inner.read().await
    }

    pub async fn acquire_mut_async(&self) -> async_lock::RwLockWriteGuard<'_, T> {
        self.inner.write().await
    }

    pub fn as_ptr(&self) -> *const T {
        Arc::as_ptr(&self.inner) as _
    }

    pub fn into_raw(self) -> *mut T {
        Arc::into_raw(self.inner) as _
    }

    pub unsafe fn from_raw(ptr: *mut T) -> Self {
        Self {
            inner: Arc::from_raw(ptr as *mut RwLock<T>),
        }
    }

    /// Acquires a read lock, which keeps this cell alive for as long as it's held.
    pub fn acquire_owned(&self) -> OwnedRef<T> {
        OwnedRef {
            guard: self.inner.read_arc_blocking(),
            cell: self.clone(),
        }
    }

    pub fn try_acquire_owned(&self) -> Option<OwnedRef<T>> {
        Some(OwnedRef {
            guard: self.inner.try_read_arc()?,
            cell: self.clone(),
        })
    }

    pub async fn acquire_owned_async(&self) -> OwnedRef<T> {
        OwnedRef {
            guard: self.inner.read_arc().await,
            cell: self.clone(),
        }
    }

    /// Acquires a write lock, which keeps this cell alive for as long as it's held.
    pub fn acquire_owned_mut(&self) -> OwnedMut<T> {
        OwnedMut {
            guard: self.inner.write_arc_blocking(),
            cell: self.clone(),
        }
    }

    pub fn try_acquire_owned_mut(&self) -> Option<OwnedMut<T>> {
        Some(OwnedMut {
            guard: self.inner.try_write_arc()?,
            cell: self.clone(),
        })
    }

    pub async fn acquire_owned_mut_async(&self) -> OwnedMut<T> {
        OwnedMut {
            guard: self.inner.write_arc().await,
            cell: self.clone(),
        }
    }
}

/// Read lock over an [ArcCell] contents, which owns a reference to that cell.
pub struct OwnedRef<T> {
    guard: RwLockReadGuardArc<T>,
    cell: ArcCell<T>,
}

impl<T> OwnedRef<T> {
    /// The cell, which contents are locked.
    pub fn cell(&self) -> &ArcCell<T> {
        &self.cell
    }
}

impl<T> Deref for OwnedRef<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.guard
    }
}

/// Write lock over an [ArcCell] contents, which owns a reference to that cell.
pub struct OwnedMut<T> {
    guard: RwLockWriteGuardArc<T>,
    cell: ArcCell<T>,
}

impl<T> OwnedMut<T> {
    /// The cell, which contents are locked.
    pub fn cell(&self) -> &ArcCell<T> {
        &self.cell
    }
}

impl<T> Deref for OwnedMut<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> DerefMut for OwnedMut<T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T> Clone for ArcCell<T> {
    fn clone(&self) -> Self {
        ArcCell {
            inner: self.inner.clone(),
        }
    }
}

pub type Ref<'a, T> = async_lock::RwLockReadGuard<'a, T>;
pub type Mut<'a, T> = async_lock::RwLockWriteGuard<'a, T>;

impl<T> Acquire for ArcCell<T> {
    type Ref<'a>
    where
        Self: 'a,
    = async_lock::RwLockReadGuard<'a, T>;

    fn acquire(&self) -> Self::Ref<'_> {
        self.inner.read_blocking()
    }

    fn try_acquire(&self) -> Option<Self::Ref<'_>> {
        self.inner.try_read()
    }
}

impl<T> AcquireMut for ArcCell<T> {
    type Mut<'a>
    where
        Self: 'a,
    = async_lock::RwLockWriteGuard<'a, T>;

    fn acquire_mut(&self) -> Self::Mut<'_> {
        self.inner.write_blocking()
    }

    fn try_acquire_mut(&self) -> Option<Self::Mut<'_>> {
        self.inner.try_write()
    }
}
