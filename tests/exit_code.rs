//! LOCK-001: the CLI edge maps a contended lock to `EX_TEMPFAIL` (75) while every
//! other failure keeps the historical exit code 1. The seam is a pure
//! `phora::cli::exit_code(&Error) -> i32` that `main` consults.

use phora::cli::exit_code;
use phora::error::Error;
use phora::store::StoreError;

#[test]
fn contended_lock_maps_to_ex_tempfail() {
    let err = Error::StoreCtx(StoreError::Lock(
        "another phora process is running for this project (state.lock held)".to_owned(),
    ));

    assert_eq!(
        exit_code(&err),
        75,
        "a contended StoreError::Lock must surface as EX_TEMPFAIL (75) so callers \
         can distinguish 'busy, retry' from a hard failure"
    );
}

#[test]
fn other_errors_keep_exit_code_one() {
    let err = Error::Config("read phora.toml: no such file".to_owned());

    assert_eq!(
        exit_code(&err),
        1,
        "non-lock failures must keep the historical exit code 1"
    );
}

#[test]
fn store_registry_error_is_not_treated_as_tempfail() {
    let err = Error::StoreCtx(StoreError::Registry("corrupt record".to_owned()));

    assert_eq!(
        exit_code(&err),
        1,
        "only the Lock variant is EX_TEMPFAIL; a registry error is a hard failure"
    );
}
