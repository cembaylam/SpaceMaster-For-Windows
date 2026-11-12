//! SpaceMaster core: the parallel size scanner, safety rules and the UI-independent browser logic.
//! The Direct2D front end lives in the `spacemaster` binary.

pub mod browser;
pub mod enumerate;
pub mod format;
pub mod fsops;
pub mod listing;
pub mod model;
pub mod node;
pub mod paths;
pub mod safety;
pub mod scanner;
pub mod settings;
pub mod volumes;
