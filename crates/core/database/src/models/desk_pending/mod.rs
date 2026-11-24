//! Zrzur Desk pending actions (`desk_pending`): a changing desk command does
//! nothing at first. The server stores what was asked here, shows a
//! confirmation card, and acts only when the same person presses its
//! Confirm button within ten minutes, once.

mod model;
mod ops;

pub use model::*;
pub use ops::*;
