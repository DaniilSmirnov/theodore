//! GitHub HostAdapter: GraphQL (cynic) + REST, notifications polling.

mod adapter;
mod anchor;
pub mod device_flow;
mod graphql;
mod logs;
mod pagination;
mod rest;
pub mod testkit;
mod time;

pub use adapter::GithubAdapter;
pub use device_flow::GithubDeviceLogin;
