//! One more than a count, without `+`.
//!
//! Declared hold at `usize::MAX`: a line number cannot get there in a file
//! that fits in memory (`rust-checked-arithmetic` RCA-DECL-4).

/// One more than `count`, holding at `usize::MAX`.
// See `gate::run` on `redundant_pub_crate` versus `unreachable_pub`.
#[allow(clippy::redundant_pub_crate)]
pub(crate) const fn next(count: usize) -> usize {
    match count.checked_add(1) {
        Some(next) => next,
        None => usize::MAX,
    }
}
