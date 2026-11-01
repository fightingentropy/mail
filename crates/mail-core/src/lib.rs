//! Mail's engine: Google sign-in, Gmail sync, the local SQLite store and search.
//!
//! The command line and the desktop window are thin layers over this crate.
//! Nothing here sends email: writing ends in Gmail's Drafts.

pub mod auth;
pub mod compose;
pub mod gmail;
pub mod model;
pub mod paths;
pub mod store;
pub mod sync;
