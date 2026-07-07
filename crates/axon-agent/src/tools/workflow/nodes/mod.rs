//! Per-node executors for the workflow engine.
//!
//! Each submodule owns one node type's execution logic; the dispatch in
//! `super::execute_node_by_type` is just a flat list of one-line delegations to
//! these. Adding a new node type = add a file here + one arm in that match,
//! instead of growing the engine file. Shared helpers (expression engine,
//! value/condition utils, the JS sandbox) stay in the parent `workflow` module
//! and are reached via `crate::tools::workflow::*`.
//!
//! NOTE: the `javascript` node still lives inline in the parent module because
//! its boa sandbox is shared with the expression engine; extracting that
//! subsystem cleanly is a separate follow-up.

pub(crate) mod aggregate;
pub(crate) mod classifier;
pub(crate) mod compression;
pub(crate) mod condition;
pub(crate) mod convert_to_file;
pub(crate) mod cortex;
pub(crate) mod crypto;
pub(crate) mod database;
pub(crate) mod date_time;
pub(crate) mod discord;
pub(crate) mod engram;
pub(crate) mod extract_from_file;
pub(crate) mod facebook;
pub(crate) mod filter;
pub(crate) mod fovea;
pub(crate) mod github;
pub(crate) mod homeostasis;
pub(crate) mod html_extract;
pub(crate) mod iterate;
pub(crate) mod markdown;
pub(crate) mod mcp;
pub(crate) mod merge;
pub(crate) mod respond_to_webhook;
pub(crate) mod rss;
pub(crate) mod shell;
pub(crate) mod slack;
pub(crate) mod soma;
pub(crate) mod sort_limit;
pub(crate) mod split_out;
pub(crate) mod subflow;
pub(crate) mod synapse;
pub(crate) mod trigger;
pub(crate) mod wait;
pub(crate) mod xml;
