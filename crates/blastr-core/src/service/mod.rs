//! The engine process: the messages the window and the analysis engine
//! exchange, and the engine's side of them.
//!
//! The window analyses nothing itself. It starts `blastr-engine`, writes
//! [`protocol::Request`]s to its standard input and reads
//! [`protocol::Response`]s from its standard output, one message per frame;
//! [`server`] is what the engine runs on the other end. Keeping rust-analyzer
//! in a console process of its own means the programs it starts share that
//! process's invisible console instead of each opening a window of its own.

pub mod protocol;
pub mod server;
