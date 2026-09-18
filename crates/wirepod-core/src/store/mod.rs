//! The persisted stores: the bot-info file, the jdocs file, and the jdocs
//! pinger's view of the first of them.

pub mod bot_info;
pub mod bot_status;
pub mod jdocs;

pub use crate::store::bot_info::{BotInfo, BotInfoRobot, BotInfoWire, RobotWire};
pub use crate::store::bot_status::{BotStatus, BotStatusKind, PingerState};
pub use crate::store::jdocs::{
    AddOutcome, BotJdoc, JDOCS_FILE_MODE, Jdoc, JdocsDecodeError, JdocsLoadOutcome, JdocsStore,
    LoadedJdocs, marshal_jdocs, parse_jdocs,
};
