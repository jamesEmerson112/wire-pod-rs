//! The persisted stores: the bot-info file, the jdocs file, the jdocs pinger's
//! view of the first of them, the session certificates under
//! `session-certs/`, and the SDK's own `sdk_config.ini`.

pub mod bot_info;
pub mod bot_status;
pub mod jdocs;
pub mod sdk_ini;
pub mod session_certs;

pub use crate::store::bot_info::{
    BOT_INFO_FILE_MODE, BotInfo, BotInfoRobot, BotInfoWire, GLOBAL_GUID, RobotWire, bot_info_gate,
    marshal_bot_info, read_bot_info, write_bot_info,
};
pub use crate::store::bot_status::{BotStatus, BotStatusKind, PingerState};
pub use crate::store::jdocs::{
    AddOutcome, BotJdoc, JDOCS_FILE_MODE, Jdoc, JdocsDecodeError, JdocsLoadOutcome, JdocsStore,
    LoadedJdocs, marshal_jdocs, parse_jdocs,
};
pub use crate::store::sdk_ini::{
    DEFAULT_SECTION, IniEdit, IniError, IniFile, IniKey, IniSection, LINE_BREAK,
    SDK_CERT_FILE_MODE, SDK_CONFIG_FILE, SDK_INI_DIR_MODE, SDK_INI_FILE_MODE, SdkIniStore,
    SecondaryOutcome, cert_file_path, cert_value, sdk_config_path,
};
pub use crate::store::session_certs::{
    LoadedSessionCerts, PLACEHOLDER_NAME, ReadSessionCertsOutcome, RecurringInfo,
    RecurringInfoLoad, SESSION_CERT_FILE_MODE, SessionCertStore, certificate_der,
    issuer_common_name, read_session_certs, session_cert_gate, session_cert_read_path,
    write_session_cert,
};
