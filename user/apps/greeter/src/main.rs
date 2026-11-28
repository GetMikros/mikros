use elegance::prelude::*;

#[component]
fn Screen() -> impl IntoView {
    elg! {
        VStack(spacing: 16) {
            Text("Mikros").font(Font::system(56.0, .bold))
        }
    }
}

fn main() -> Result<std::process::ExitCode, Error> {
    App::new()
        .window(WindowDesc::new("Greeter").size(800, 600), Screen)
        .run()
}
