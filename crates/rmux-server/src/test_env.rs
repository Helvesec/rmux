use std::sync::OnceLock;

use tokio::sync::{Mutex, MutexGuard};

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) async fn lock_async() -> MutexGuard<'static, ()> {
    ENV_LOCK.get_or_init(|| Mutex::new(())).lock().await
}

pub(crate) fn lock_blocking() -> MutexGuard<'static, ()> {
    ENV_LOCK.get_or_init(|| Mutex::new(())).blocking_lock()
}

pub(crate) struct EnvVarGuard {
    name: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    pub(crate) fn set(name: &'static str, value: Option<&str>) -> Self {
        let previous = std::env::var(name).ok();
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
        Self { name, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}
