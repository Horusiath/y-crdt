use crate::block::Block;
use crate::cell::{Acquire, AcquireMut};
use async_lock::RwLock;
use std::cell::RefCell;
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
}

impl<T> Clone for ArcCell<T> {
    fn clone(&self) -> Self {
        ArcCell {
            inner: self.inner.clone(),
        }
    }
}

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
