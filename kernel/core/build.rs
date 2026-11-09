//! Turns on the lock order check in the fuzz build of the kernel. With
//! MIKROS_LOCK_DISCOVER set in the environment, any kernel build checks
//! and lists the violations instead of panicking, to find the order.
//! Host builds never check: their tests share one mock processor across
//! threads, so the locks it records held are not one thread's.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(lock_check)");
    println!("cargo::rustc-check-cfg=cfg(lock_discover)");
    println!("cargo::rerun-if-env-changed=MIKROS_LOCK_DISCOVER");
    let fuzz = std::env::var_os("CARGO_FEATURE_FUZZ").is_some();
    let kernel = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "none");
    let discover = kernel && std::env::var_os("MIKROS_LOCK_DISCOVER").is_some();
    if kernel && fuzz || discover {
        println!("cargo::rustc-cfg=lock_check");
    }
    if discover {
        println!("cargo::rustc-cfg=lock_discover");
    }
}
