//! Panics that are caught where they happen. The panic hook (in `main.rs`) runs before
//! a panic reaches the code that catches it, so it cannot tell a panic that ends the
//! app from one that is handled; code that catches panics on a thread says so here.
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

thread_local! {
    /// How many [`catch`] calls the current thread is inside.
    static CATCHING: Cell<u32> = const { Cell::new(0) };
}

/// Runs `f`, catching a panic in it (like `catch_unwind`), and lets the panic hook know
/// meanwhile that a panic here does not end the app.
pub fn catch<R>(f: impl FnOnce() -> R) -> std::thread::Result<R> {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            CATCHING.with(|n| n.set(n.get().saturating_sub(1)));
        }
    }
    CATCHING.with(|n| n.set(n.get() + 1));
    let _leave = Leave;
    catch_unwind(AssertUnwindSafe(f))
}

/// True if a panic on this thread now would be caught by a [`catch`] around it.
pub fn is_caught() -> bool {
    CATCHING.with(|n| n.get() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_inside_catch_is_known_to_be_caught() {
        assert!(!is_caught());
        let seen = catch(is_caught).expect("no panic");
        assert!(seen);
        let result = catch(|| {
            panic!("test: caught");
        });
        assert!(result.is_err());
        assert!(!is_caught(), "left again after the panic");
    }
}
