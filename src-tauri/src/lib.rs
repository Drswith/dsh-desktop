//! DSH Launcher: a menu bar shell that installs the dsh runtime, supervises the
//! local `dsh web` service, and opens its UI in the default browser.

pub mod app;
pub mod core;

pub use app::run;
