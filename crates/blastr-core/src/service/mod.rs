//! The engine process: the engine's side of the messages the window and the
//! analysis engine exchange.
//!
//! The window analyses nothing itself. It starts `blastr-engine`, writes
//! [`blastr_model::protocol::Request`]s to its standard input and reads
//! [`blastr_model::protocol::Response`]s from its standard output, one message
//! per frame; [`server`] is what the engine runs on the other end. Keeping
//! rust-analyzer in a console process of its own means the programs it starts
//! share that process's invisible console instead of each opening a window of
//! its own.

pub mod server;
