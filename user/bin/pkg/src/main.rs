//! pkg, which installs, removes, lists, shows and makes `.mika` packages
//! (MIKSPEC-0101.R28). `request` reads the arguments, `order` puts the
//! packages to install after those they need, `show` makes the output,
//! and `pack` makes packages from folders; the calls to the install
//! service follow.

#![allow(dead_code)]

mod order;
mod pack;
mod request;
mod show;

fn main() {}
