//! The persisted stores: the bot-info file and the jdocs pinger's view of it.

pub mod bot_info;
pub mod bot_status;

pub use crate::store::bot_info::{BotInfo, BotInfoRobot, BotInfoWire, RobotWire};
pub use crate::store::bot_status::{BotStatus, BotStatusKind, PingerState};
