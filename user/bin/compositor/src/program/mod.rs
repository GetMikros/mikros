//! The compositor on Mikros. So far it makes the Vulkan device of the
//! CPU driver that it will compose with.

mod render;

pub fn main() {
    if let Err(e) = render::Renderer::new() {
        eprintln!("compositor: no Vulkan device: {e:?}");
        std::process::exit(1);
    }
}
