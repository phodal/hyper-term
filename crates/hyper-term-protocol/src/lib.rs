//! Renderer-independent contracts shared by `hyperd`, desktop clients, and tests.

mod block;
mod domain;
mod ids;
mod wire;

pub use block::*;
pub use domain::*;
pub use ids::*;
pub use wire::*;

pub const PROTOCOL_VERSION: u16 = 1;
pub const EVENT_SCHEMA_VERSION: u16 = 1;
pub const BLOCK_SCHEMA_VERSION: u16 = 1;
