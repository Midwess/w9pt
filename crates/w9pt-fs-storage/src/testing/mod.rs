//! Deterministic target implementation and reusable test support.

use core::{future::Future, task::Poll};
use std::{sync::Arc, task::Wake};

mod conformance;
mod memory_store;
mod repository_conformance;

pub use conformance::{
    TargetConformanceError, check_target_conformance, check_target_operations,
    check_target_pair_conformance, check_target_pair_operations,
};
pub use memory_store::{
    FailureTiming, MemoryTarget, MemoryTargetError, TargetTraceEvent, TracePhase,
};
pub use repository_conformance::{
    RepositoryConformanceError, check_repository_conformance, check_repository_method_conformance,
};

/// Drives one future to completion without choosing an async runtime.
///
/// This small executor is intended for deterministic conformance tests. It parks
/// the current thread when a future is pending and unparks it through the waker.
pub fn block_on<F: Future>(future: F) -> F::Output {
    struct ThreadWake(std::thread::Thread);

    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Arc::new(ThreadWake(std::thread::current())).into();
    let mut context = core::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::park(),
        }
    }
}
