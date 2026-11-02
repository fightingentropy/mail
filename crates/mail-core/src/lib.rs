//! Mail's engine: Google sign-in, Gmail and iCloud sync, the local SQLite
//! store and search.
//!
//! The command line and the desktop window are thin layers over this crate.
//! Writing ends in Gmail's Drafts; the only send, `compose::send`, is called
//! by the window after you confirm.

pub mod auth;
pub mod compose;
pub mod gmail;
pub mod icloud;
pub mod model;
pub mod paths;
pub mod store;
pub mod sync;
