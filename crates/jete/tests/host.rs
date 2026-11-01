//! Smoke test for the development loop: this compiles on the Mac and must
//! run on hemlock, through the cargo runner in `.cargo/config.toml`.

use std::path::Path;

#[test]
fn runs_on_a_linux_kvm_host() {
    assert_eq!(
        std::env::consts::OS,
        "linux",
        "test binary is not running on Linux"
    );
    assert!(
        Path::new("/dev/kvm").exists(),
        "/dev/kvm is missing: this test is not running on a KVM host"
    );
}
