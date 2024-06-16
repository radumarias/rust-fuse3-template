pub(crate) mod fs_model;
pub(crate) mod fs;
pub(crate) mod stream_util;
pub mod mount;

#[allow(unreachable_code)]
#[must_use]
pub const fn is_debug() -> bool {
    #[cfg(debug_assertions)]
    {
        return true;
    }
    false
}
