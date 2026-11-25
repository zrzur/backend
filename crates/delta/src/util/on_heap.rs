//! Large futures off the stack.
//!
//! In a debug build every future an `async fn` creates gets its own place
//! in that function's poll frame, and those places are never shared. A
//! future built inline therefore costs its full size on the stack for as
//! long as the caller is being polled, also when it is boxed right after
//! (`Box::pin(f())` builds `f()` in the caller's frame first). Along a long
//! chain of calls these frames add up: confirming a desk suspension once
//! needed more than the 2 MiB of a tokio worker thread and aborted the API.
//!
//! [`on_heap`] builds the future in a frame of its own, which has ended
//! before anything is polled: the caller keeps only the box.

use std::future::Future;

use futures::future::BoxFuture;

/// `make()`'s future, built in this function's frame and moved to the heap
#[inline(never)]
pub fn on_heap<'a, F, Fut>(make: F) -> BoxFuture<'a, Fut::Output>
where
    F: FnOnce() -> Fut,
    Fut: Future + Send + 'a,
{
    Box::pin(make())
}
