thread_local! {
    static OBSERVER: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

pub(crate) fn install(observer: impl FnOnce() + 'static) {
    OBSERVER.with_borrow_mut(|slot| {
        assert!(
            slot.is_none(),
            "previous initial renderer observer not consumed"
        );
        *slot = Some(Box::new(observer));
    });
}

pub(super) fn observe() {
    let observer = OBSERVER.with_borrow_mut(Option::take);
    if let Some(observer) = observer {
        observer();
    }
}
