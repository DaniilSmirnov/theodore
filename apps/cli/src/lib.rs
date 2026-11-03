//! `theodore` CLI internals, split out as a library so integration tests can
//! drive each command's testable core directly (with a fake `AgentPort`)
//! instead of going through `main`.

pub mod deps;
pub mod difffile;
pub mod endpoint;
pub mod eval;
pub mod graph_check;
pub mod group;
pub mod links;
pub mod mcp_bridge;
pub mod sandbox;
