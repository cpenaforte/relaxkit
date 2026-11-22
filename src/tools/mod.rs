//! The tool install engine: detects each tool on each host, adopts what is already there, installs the rest
//! from the recipe, and records the result in the lockfile.

pub mod detect;
pub mod engine;
pub mod handlers;

pub use detect::Route;
pub use engine::{Engine, HandlerFn, Outcome, handler};
