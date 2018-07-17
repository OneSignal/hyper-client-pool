use std::sync::{RwLock, RwLockWriteGuard, RwLockReadGuard};

pub trait RwLockExt<T> {
    fn write_ignore_poison(&self) -> RwLockWriteGuard<T>;
    fn read_ignore_poison(&self) -> RwLockReadGuard<T>;
}

impl< T> RwLockExt<T> for RwLock<T> {
    fn write_ignore_poison(&self) -> RwLockWriteGuard<T> {
        self.write().unwrap_or_else(|e| e.into_inner())
    }

    fn read_ignore_poison(&self) -> RwLockReadGuard<T> {
        self.read().unwrap_or_else(|e| e.into_inner())
    }
}
