//! Renderer-independent contracts shared by `hyperd`, desktop clients, and tests.

mod artifact;
mod block;
mod domain;
mod execution_context;
mod ids;
mod mcp;
mod sandbox;
mod terminal_web;
mod wire;

pub use artifact::*;
pub use block::*;
pub use domain::*;
pub use execution_context::*;
pub use ids::*;
pub use mcp::*;
pub use sandbox::*;
pub use terminal_web::*;
pub use wire::*;

pub const PROTOCOL_VERSION: u16 = 9;
pub const EVENT_SCHEMA_VERSION: u16 = 1;
pub const BLOCK_SCHEMA_VERSION: u16 = 3;
