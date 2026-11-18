//! The state of the window, with no drawing code.
//!
//! Everything in here can be tested without opening a window: the views read
//! it and call its transitions, and the background jobs report into it.

pub mod commits;
pub mod loadable;
pub mod outline;
pub mod search;
pub mod selection;
pub mod session;
pub mod tasks;
pub mod worker;
