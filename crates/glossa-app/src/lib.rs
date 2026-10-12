//! Application state machine, orchestration traits, and daemon actor.

pub mod error;
pub mod level;
pub mod machine;
pub mod ports;
pub mod services;

pub use error::AppError;
pub use machine::{reduce, Action, Decision};
pub use services::app_actor::{ActorExit, AppActor, AppDependencies};
pub use services::command_router::AppHandle;
pub use services::status_store::StatusStore;
