//! The driver itself: `gpu` sends the device its commands and makes
//! its 2D resources, `display` serves the display owner, and `log`
//! shows the log on a scanout.

#![allow(dead_code)]

mod display;
mod gpu;
mod log;
