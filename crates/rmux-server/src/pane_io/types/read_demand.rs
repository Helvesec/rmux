use super::*;

impl PaneOutputReadWake {
    pub(super) fn notify_all(&self) {
        #[cfg(windows)]
        {
            let _wake = self
                .state
                .lock()
                .expect("pane output reader wake mutex must not be poisoned");
            self.condvar.notify_all();
        }
    }
}

impl PaneOutputDemandCounter {
    pub(super) fn new(wake: Arc<PaneOutputReadWake>) -> Self {
        Self {
            count: AtomicUsize::new(0),
            notify: Notify::new(),
            wake,
        }
    }

    pub(super) fn acquire(self: &Arc<Self>) -> PaneOutputReadDemandGuard {
        self.count.fetch_add(1, Ordering::AcqRel);
        self.notify.notify_waiters();
        self.wake.notify_all();
        PaneOutputReadDemandGuard {
            counter: Arc::clone(self),
        }
    }

    pub(super) fn release(&self) {
        let previous = self.count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "pane output demand count must not underflow");
        self.notify.notify_waiters();
        self.wake.notify_all();
    }
}

impl AttachedClientReadDemand {
    pub(super) fn new(wake: Arc<PaneOutputReadWake>) -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            notify: Notify::new(),
            wake,
        }
    }

    pub(super) fn acquire(self: &Arc<Self>) -> PaneOutputAttachedClientReadDemandGuard {
        let mut clients = self
            .clients
            .lock()
            .expect("attached pane read demand mutex must not be poisoned");
        let id = loop {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            if id != 0 && !clients.contains_key(&id) {
                clients.insert(id, HashSet::new());
                break id;
            }
        };
        drop(clients);
        self.notify.notify_waiters();
        self.wake.notify_all();
        PaneOutputAttachedClientReadDemandGuard {
            clients: Arc::clone(self),
            id,
        }
    }

    pub(super) fn reader_demanded(&self, pane_id: u32) -> bool {
        let clients = self
            .clients
            .lock()
            .expect("attached pane read demand mutex must not be poisoned");
        clients.is_empty()
            || clients
                .values()
                .any(|off_panes| !off_panes.contains(&pane_id))
    }

    pub(super) fn replace_off_panes(&self, id: u64, off_panes: HashSet<u32>) {
        let mut clients = self
            .clients
            .lock()
            .expect("attached pane read demand mutex must not be poisoned");
        if let Some(existing) = clients.get_mut(&id) {
            *existing = off_panes;
        }
        drop(clients);
        self.notify.notify_waiters();
        self.wake.notify_all();
    }

    pub(super) fn set_pane_off(&self, id: u64, pane_id: u32, off: bool) {
        let mut clients = self
            .clients
            .lock()
            .expect("attached pane read demand mutex must not be poisoned");
        if let Some(off_panes) = clients.get_mut(&id) {
            if off {
                off_panes.insert(pane_id);
            } else {
                off_panes.remove(&pane_id);
            }
        }
        drop(clients);
        self.notify.notify_waiters();
        self.wake.notify_all();
    }

    pub(super) fn release(&self, id: u64) {
        let mut clients = self
            .clients
            .lock()
            .expect("attached pane read demand mutex must not be poisoned");
        let _ = clients.remove(&id);
        drop(clients);
        self.notify.notify_waiters();
        self.wake.notify_all();
    }
}

impl Default for PaneOutputReadDemandRegistry {
    fn default() -> Self {
        let wake = Arc::new(PaneOutputReadWake::default());
        Self {
            unrestricted: Arc::new(PaneOutputDemandCounter::new(Arc::clone(&wake))),
            attached_clients: Arc::new(AttachedClientReadDemand::new(wake)),
        }
    }
}

impl PaneOutputReadDemandRegistry {
    pub(crate) fn acquire_unrestricted(&self) -> PaneOutputReadDemandGuard {
        self.unrestricted.acquire()
    }

    pub(crate) fn acquire_attached_client(&self) -> PaneOutputAttachedClientReadDemandGuard {
        self.attached_clients.acquire()
    }

    pub(super) fn unrestricted_count(&self) -> usize {
        self.unrestricted.count.load(Ordering::Acquire)
    }

    pub(super) fn attached_client_reader_demanded(&self, pane_id: u32) -> bool {
        self.attached_clients.reader_demanded(pane_id)
    }

    pub(super) fn reader_wake(&self) -> Arc<PaneOutputReadWake> {
        Arc::clone(&self.unrestricted.wake)
    }
}

impl Drop for PaneOutputReadDemandGuard {
    fn drop(&mut self) {
        self.counter.release();
    }
}

impl PaneOutputAttachedClientReadDemandGuard {
    pub(crate) fn replace_off_panes(&self, off_panes: HashSet<u32>) {
        self.clients.replace_off_panes(self.id, off_panes);
    }

    pub(crate) fn set_pane_off(&self, pane_id: u32, off: bool) {
        self.clients.set_pane_off(self.id, pane_id, off);
    }
}

impl Drop for PaneOutputAttachedClientReadDemandGuard {
    fn drop(&mut self) {
        self.clients.release(self.id);
    }
}

impl PaneOutputSender {
    #[cfg(all(test, windows))]
    pub(crate) fn reader_probe_for_test(&self) -> Arc<windows_reader_probe::WindowsReaderProbe> {
        Arc::clone(&self.inner.reader_probe)
    }

    pub(crate) fn acquire_read_demand(&self) -> PaneOutputReadDemandGuard {
        self.inner.read_demand.acquire()
    }

    pub(super) fn reader_demanded(&self, pane_id: u32) -> bool {
        self.inner.read_demand.count.load(Ordering::Acquire) > 0
            || self.inner.global_read_demand.unrestricted_count() > 0
            || self
                .inner
                .global_read_demand
                .attached_client_reader_demanded(pane_id)
    }

    pub(super) fn reader_generation_active(&self, generation: Option<u64>) -> bool {
        !self.inner.reader_retired.load(Ordering::Acquire)
            && generation_matches(self.current_generation(), generation)
    }

    pub(super) fn notify_reader_lifecycle(&self) {
        self.inner.reader_lifecycle.notify_waiters();
        self.inner.reader_wake.notify_all();
    }

    pub(crate) async fn wait_for_reader_demand(
        &self,
        pane_id: u32,
        generation: Option<u64>,
    ) -> bool {
        loop {
            let local = self.inner.read_demand.notify.notified();
            let unrestricted = self.inner.global_read_demand.unrestricted.notify.notified();
            let attached_clients = self
                .inner
                .global_read_demand
                .attached_clients
                .notify
                .notified();
            let lifecycle = self.inner.reader_lifecycle.notified();
            if !self.reader_generation_active(generation) {
                return false;
            }
            if self.reader_demanded(pane_id) {
                return true;
            }
            tokio::select! {
                _ = local => {}
                _ = unrestricted => {}
                _ = attached_clients => {}
                _ = lifecycle => {}
            }
        }
    }

    #[cfg(windows)]
    pub(crate) fn wait_for_reader_demand_blocking(
        &self,
        pane_id: u32,
        generation: Option<u64>,
    ) -> bool {
        let mut wake = self
            .inner
            .reader_wake
            .state
            .lock()
            .expect("pane output reader wake mutex must not be poisoned");
        loop {
            if !self.reader_generation_active(generation) {
                return false;
            }
            if self.inner.reader_draining.load(Ordering::Acquire) || self.reader_demanded(pane_id) {
                return true;
            }
            #[cfg(test)]
            let parked = self.inner.reader_probe.enter(true);
            wake = self
                .inner
                .reader_wake
                .condvar
                .wait(wake)
                .expect("pane output reader wake mutex must not be poisoned");
            #[cfg(test)]
            drop(parked);
        }
    }
}
