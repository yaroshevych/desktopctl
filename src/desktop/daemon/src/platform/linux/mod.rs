//! Linux (Ubuntu 26.04 / GNOME 50 Wayland) backend subsystems.
//!
//! See `tmp/ubuntu-spec.md` for the full design. Each submodule is implemented
//! independently and wired into the capability facades (`platform::capture`,
//! `platform::windowing`, etc.) at integration time.

pub mod capture;
pub mod coords;
pub mod extension;
pub mod input;
pub mod portal;
