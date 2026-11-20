use std::future::Future;
use std::pin::Pin;

// A large async match otherwise reserves construction temporaries for every
// branch in the same polling frame, even when each stored future is small.
// Construct only the selected future in its own frame, then poll it on the heap.
#[inline(never)]
pub(crate) fn boxed<F: Future>(make: impl FnOnce() -> F) -> Pin<Box<F>> {
    Box::pin(make())
}
