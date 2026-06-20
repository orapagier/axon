pub mod client;
pub mod inprocess;
pub use client::{McpClient, McpManager, McpResult};
pub use inprocess::InProcessMcp;
