//! Links init as a static executable at a fixed address, which is the
//! only kind of image the kernel starts.

fn main() {
    println!("cargo:rustc-link-arg-bins=--no-pie");
}
