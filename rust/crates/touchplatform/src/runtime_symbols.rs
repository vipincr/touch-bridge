//! Apple symbols resolved by name at run time: the private calls the headers do not declare, and the
//! few public calls the generated bindings lack. Each is looked up in the current process the first
//! time it is used, and a private one can be absent on a macOS version, so every wrapper hands out an
//! `Option`.

use std::ffi::CStr;
use std::sync::OnceLock;

/// Resolves `name` into `cell` if that has not happened yet, and returns the function if this
/// operating system has it.
///
/// # Safety
///
/// `T` must be exactly the C signature of the symbol called `name`. A wrong signature turns every
/// later call into undefined behaviour.
pub unsafe fn lookup<T: Copy>(name: &CStr, cell: &OnceLock<Option<T>>) -> Option<T> {
    *cell.get_or_init(|| {
        // SAFETY: guaranteed by the caller. `dlsym` is thread-safe, and a null result only means the
        // symbol is not exported here.
        unsafe {
            let address = libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr());
            (!address.is_null()).then(|| std::mem::transmute_copy::<*mut std::ffi::c_void, T>(&address))
        }
    })
}
