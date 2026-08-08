//! The one `unsafe` call in this workspace: macOS's `sandbox_init`.
//!
//! ADR-0002 forbids `unsafe` in our own code, and [ADR-0017] carves out exactly
//! this crate. The argument is short: macOS confines a process by calling
//! `sandbox_init`, that is a C function, there is no safe wrapper for it on
//! crates.io, and a renderer nobody confines is a worse outcome than one
//! `unsafe` call in a crate that does nothing else. Linux needed no exception
//! (`seccompiler`) and neither did Windows (`rappct`, ADR-0014); this platform
//! has no equivalent, which is why it is the only one that gets one.
//!
//! # What is in here, and what is deliberately not
//!
//! Mechanism only. This crate knows how to call one C function and how to turn
//! its error into a `String`. It does not know what a renderer is, what should
//! be denied, or what the profile says — that is policy, it lives in
//! `sandbox::confine` with the Linux and Windows policies, and it is written in
//! a crate where `unsafe` is still forbidden.
//!
//! The split is the point. An exception is only as narrow as the code inside
//! it, so the code inside it is a page long and takes a `&str`.
//!
//! # Why `sandbox_init` at all, given Apple deprecated it
//!
//! It has carried a deprecation warning since macOS 10.7 and remains what
//! actually confines a process from inside it. Chromium and Firefox both still
//! call it. The replacement Apple points to is App Sandbox entitlements, which
//! are applied to a signed, bundled application at launch — not something a
//! process can do to itself, and not available to a program run from a
//! terminal. For the shape this browser needs, which is "the child restricts
//! itself before reading its first frame", this is the interface that exists.
//!
//! [ADR-0017]: ../../../docs/adr/0017-one-unsafe-crate-for-macos.md

#![cfg(target_os = "macos")]

use std::ffi::{CStr, CString, c_char, c_int};

// The two entry points, as libSystem declares them in `sandbox.h`:
//
//     int  sandbox_init(const char *profile, uint64_t flags, char **errorbuf);
//     void sandbox_free_error(char *errorbuf);
//
// Both live in libSystem, which every macOS binary links already, so there is
// no build script and nothing to find at link time.
unsafe extern "C" {
    fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
    fn sandbox_free_error(errorbuf: *mut c_char);
}

/// Passing a profile written out in full rather than naming a built-in one.
///
/// `sandbox_init` reads its first argument two ways depending on this: with
/// `SANDBOX_NAMED` it is the name of one of Apple's canned profiles, and with
/// zero it is the profile itself. The canned ones are both coarser and even
/// more deprecated than the call, so this is always zero.
const PROFILE_IS_LITERAL: u64 = 0;

/// Applies a sandbox profile to the calling process, permanently.
///
/// There is no way to undo it and no way to check it afterwards from outside,
/// which is why `sandbox::confine`'s self-test applies one and then tries the
/// things it is supposed to prevent.
///
/// # Errors
///
/// The message from `sandbox_init` where there is one, and a plain statement
/// that it failed where there is not. A profile that does not parse fails here,
/// which is the failure worth being loud about: a profile with a typo denies
/// nothing rather than denying everything.
pub fn confine(profile: &str) -> Result<(), String> {
    // A NUL in the middle would truncate the profile silently at the C
    // boundary, and a truncated profile is a *valid* profile that allows more
    // than it was meant to. Refused rather than trimmed.
    let profile =
        CString::new(profile).map_err(|_| "the profile contains a NUL byte".to_owned())?;

    let mut error: *mut c_char = std::ptr::null_mut();
    // SAFETY: `profile` is a `CString` alive for this call, so the pointer is
    // valid and NUL-terminated for the duration. `error` is a live local, so
    // the out-pointer is valid and writable. `sandbox_init` either leaves it
    // untouched (on success) or stores a pointer it owns, which is released
    // below with the deallocator libSystem provides for it.
    let status = unsafe { sandbox_init(profile.as_ptr(), PROFILE_IS_LITERAL, &mut error) };
    if status == 0 {
        return Ok(());
    }

    if error.is_null() {
        // Documented to be set on failure; handled anyway, because a null
        // dereference here would turn "the sandbox did not install" into a
        // crash in the code reporting that it did not install.
        return Err(format!("sandbox_init failed with status {status}"));
    }
    // SAFETY: non-null, and `sandbox_init` returned a NUL-terminated C string
    // it owns. Copied into a `String` before anything is freed, so the owned
    // value does not borrow from memory released on the next line.
    let message = unsafe { CStr::from_ptr(error) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: `error` came from `sandbox_init` and has not been freed. This is
    // the deallocator libSystem pairs with it; `free` is not documented to be
    // correct here and is not used.
    unsafe { sandbox_free_error(error) };
    Err(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_with_a_nul_is_refused_rather_than_truncated() {
        // The failure mode worth a test: C would stop at the NUL, and a
        // profile cut short is not a broken profile, it is a *weaker* one that
        // installs successfully. Nothing about that is visible afterwards.
        let error = confine("(version 1)\0(deny default)").expect_err("refused");
        assert!(error.contains("NUL"), "{error}");
    }

    #[test]
    fn a_profile_that_does_not_parse_is_reported() {
        // Also weakening rather than strengthening if it were swallowed: a
        // process whose profile failed to install is a process with no sandbox,
        // and the only sign is this error being returned.
        //
        // Applied to the test process, which is safe *because* it fails: a
        // profile that does not parse confines nothing.
        let error = confine("(this is not a profile").expect_err("refused");
        assert!(!error.is_empty());
    }
}
