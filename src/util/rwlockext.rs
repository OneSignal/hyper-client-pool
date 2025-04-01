use std::sync::{RwLock, RwLockWriteGuard};

pub trait RwLockExt<T> {
    fn write_ignore_poison(&self) -> RwLockWriteGuard<T>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    fn write_ignore_poison(&self) -> RwLockWriteGuard<T> {
        self.write().unwrap_or_else(|e| e.into_inner())
    }
}
