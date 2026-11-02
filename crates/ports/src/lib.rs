//! Port traits. `app` depends only on these and on `domain`; adapters
//! implement them and are injected (plan §2.1).
//!
//! Changing a trait here is a cross-slice change: adapters, `app` and the
//! fixture adapters all follow.

pub mod account;
pub mod agent;
pub mod clock;
pub mod code_intel;
pub mod command;
pub mod device_login;
pub mod git;
pub mod host;
pub mod issues;
pub mod secrets;
pub mod space_agent_tools;
pub mod space_tools;
pub mod store;
pub mod terminal;
pub mod worktree_lease;

pub use account::*;
pub use agent::*;
pub use clock::*;
pub use code_intel::*;
pub use command::*;
pub use device_login::*;
pub use git::*;
pub use host::*;
pub use issues::*;
pub use secrets::*;
pub use space_agent_tools::*;
pub use space_tools::*;
pub use store::*;
pub use terminal::*;
pub use worktree_lease::*;
