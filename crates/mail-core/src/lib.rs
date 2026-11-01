//! Mail's engine: Google sign-in, Gmail sync, the local SQLite store and search.
//!
//! The command line (and later the desktop window) are thin layers over this
//! crate. Nothing here sends email.

pub mod auth;
pub mod gmail;
pub mod model;
pub mod paths;
pub mod store;
pub mod sync;
