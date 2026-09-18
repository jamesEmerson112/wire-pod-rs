//! Path resolution, asserted string for string against the Go literals.
//!
//! Every assertion here compares strings rather than `Path` values on purpose.
//! `PathBuf`'s `PartialEq` compares component by component, so it reads
//! `a\b/c`, `a/b/c` and `a\b\c\` as one value and would not notice a separator
//! changing or a trailing one disappearing. Those are exactly the differences
//! the Go server can print into a log line, so they are the differences these
//! tests exist to pin.

use std::path::{MAIN_SEPARATOR, Path};

use wirepod_core::paths::{AssetDir, BOT_INFO_NAME, DataDir, POD_NAME, sdk_ini_dir};

/// A path as the string the server would print, failing the test rather than
/// replacing anything if it is somehow not Unicode.
fn text(path: &Path) -> String {
    path.to_str()
        .unwrap_or_else(|| panic!("{path:?} is not valid Unicode"))
        .to_owned()
}

#[test]
fn the_go_constants_are_the_go_spellings() {
    assert_eq!(POD_NAME, "wire-pod");
    assert_eq!(BOT_INFO_NAME, "botSdkInfo.json");
}

/// The layout the production server actually runs in: packaged, on Windows,
/// under `%APPDATA%`.
#[cfg(windows)]
#[test]
fn the_packaged_windows_layout_matches_go_string_for_string() {
    let data = DataDir::packaged(Path::new(r"C:\Users\tester\AppData\Roaming"));
    let root = r"C:\Users\tester\AppData\Roaming\wire-pod";

    assert_eq!(
        text(data.root().expect("a packaged layout has a root")),
        root
    );

    // filepath.Join, so one separator throughout.
    assert_eq!(text(&data.jdocs_dir()), format!(r"{root}\jdocs"));
    assert_eq!(
        text(&data.custom_intents_path()),
        format!(r"{root}\customIntents.json")
    );
    assert_eq!(
        text(&data.bot_configs_path()),
        format!(r"{root}\botConfig.json")
    );
    assert_eq!(
        text(&data.api_config_path()),
        format!(r"{root}\apiConfig.json")
    );
    assert_eq!(
        text(&data.session_cert_dir()),
        format!(r"{root}\session-certs")
    );
    assert_eq!(text(&data.certs_dir()), format!(r"{root}\certs"));
    assert_eq!(text(&data.cert_path()), format!(r"{root}\certs\cert.crt"));
    assert_eq!(text(&data.key_path()), format!(r"{root}\certs\cert.key"));
    assert_eq!(
        text(&data.server_config_path()),
        format!(r"{root}\certs\server_config.json")
    );
    assert_eq!(text(&data.vosk_model_dir()), format!(r"{root}\vosk\models"));

    // String concatenation onto a joined directory, so both separators, which
    // is what the running Go process opens and prints.
    assert_eq!(data.jdocs_path(), format!(r"{root}\jdocs/jdocs.json"));
    assert_eq!(
        data.bot_info_path(),
        format!(r"{root}\jdocs/botSdkInfo.json")
    );
    assert_eq!(
        data.session_cert_path("00000000"),
        format!(r"{root}\session-certs/00000000")
    );

    for mixed in [
        data.jdocs_path(),
        data.bot_info_path(),
        data.session_cert_path("00000000"),
    ] {
        assert!(
            mixed.contains('\\') && mixed.contains('/'),
            "{mixed} lost one of Go's two separators"
        );
    }
}

/// The same layout on Linux, where Go's `filepath.Join` and its concatenated
/// forward slash agree and nothing is mixed.
#[cfg(unix)]
#[test]
fn the_packaged_unix_layout_matches_go_string_for_string() {
    let data = DataDir::packaged(Path::new("/home/tester/.config"));
    let root = "/home/tester/.config/wire-pod";

    assert_eq!(
        text(data.root().expect("a packaged layout has a root")),
        root
    );

    assert_eq!(text(&data.jdocs_dir()), format!("{root}/jdocs"));
    assert_eq!(
        text(&data.custom_intents_path()),
        format!("{root}/customIntents.json")
    );
    assert_eq!(
        text(&data.bot_configs_path()),
        format!("{root}/botConfig.json")
    );
    assert_eq!(
        text(&data.api_config_path()),
        format!("{root}/apiConfig.json")
    );
    assert_eq!(
        text(&data.session_cert_dir()),
        format!("{root}/session-certs")
    );
    assert_eq!(text(&data.certs_dir()), format!("{root}/certs"));
    assert_eq!(text(&data.cert_path()), format!("{root}/certs/cert.crt"));
    assert_eq!(text(&data.key_path()), format!("{root}/certs/cert.key"));
    assert_eq!(
        text(&data.server_config_path()),
        format!("{root}/certs/server_config.json")
    );
    assert_eq!(text(&data.vosk_model_dir()), format!("{root}/vosk/models"));

    assert_eq!(data.jdocs_path(), format!("{root}/jdocs/jdocs.json"));
    assert_eq!(
        data.bot_info_path(),
        format!("{root}/jdocs/botSdkInfo.json")
    );
    assert_eq!(
        data.session_cert_path("00000000"),
        format!("{root}/session-certs/00000000")
    );
}

/// The un-packaged layout, which is the set of literals the Go variables are
/// declared with. Every one of them is written with forward slashes in the Go
/// source and neither Go nor Rust rewrites a literal, so this holds on both
/// platforms.
#[test]
fn the_source_layout_matches_the_go_literals() {
    let data = DataDir::source();

    assert_eq!(
        data.root(),
        None,
        "the source layout is relative to the cwd"
    );

    assert_eq!(text(&data.jdocs_dir()), "./jdocs");
    assert_eq!(data.jdocs_path(), "./jdocs/jdocs.json");
    assert_eq!(data.bot_info_path(), "./jdocs/botSdkInfo.json");
    assert_eq!(text(&data.custom_intents_path()), "./customIntents.json");
    assert_eq!(text(&data.bot_configs_path()), "./botConfig.json");
    assert_eq!(text(&data.api_config_path()), "./apiConfig.json");
    assert_eq!(text(&data.session_cert_dir()), "./session-certs/");
    assert_eq!(text(&data.certs_dir()), "../certs");
    assert_eq!(text(&data.cert_path()), "../certs/cert.crt");
    assert_eq!(text(&data.key_path()), "../certs/cert.key");
    assert_eq!(
        text(&data.server_config_path()),
        "../certs/server_config.json"
    );
    assert_eq!(text(&data.vosk_model_dir()), "../vosk/models/");

    // The directory literal ends in a slash and the writers concatenate one of
    // their own, so Go really does open a doubled slash here.
    assert_eq!(
        data.session_cert_path("00000000"),
        "./session-certs//00000000"
    );
}

#[test]
fn packaged_is_rooted_at_the_pod_directory_under_the_config_directory() {
    let config = Path::new("base");
    assert_eq!(
        DataDir::packaged(config),
        DataDir::rooted(config.join(POD_NAME))
    );
}

#[test]
fn the_asset_files_sit_where_go_reads_them_relative_to_its_working_directory() {
    let assets = AssetDir::new("base");
    let separator = MAIN_SEPARATOR;

    assert_eq!(text(assets.root()), "base");
    assert_eq!(
        text(&assets.epod_cert_path()),
        format!("base{separator}epod{separator}ep.crt")
    );
    assert_eq!(
        text(&assets.epod_key_path()),
        format!("base{separator}epod{separator}ep.key")
    );
    assert_eq!(
        text(&assets.webroot_dir()),
        format!("base{separator}webroot")
    );
    assert_eq!(
        text(&assets.sdk_app_dir()),
        format!("base{separator}webroot{separator}sdkapp")
    );
    assert_eq!(
        text(&assets.intent_data_path("en-US")),
        format!("base{separator}intent-data{separator}en-US.json")
    );
    assert_eq!(
        text(&assets.weather_map_path()),
        format!("base{separator}weather-map.json")
    );
    assert_eq!(
        text(&assets.version_file()),
        format!("base{separator}version")
    );
}

/// Go concatenates the SDK directory onto the home directory, so on Windows it
/// mixes separators and on both platforms it keeps a trailing slash that the
/// callers rely on.
#[cfg(windows)]
#[test]
fn the_sdk_ini_directory_hangs_off_the_home_directory_with_a_forward_slash() {
    let directory = sdk_ini_dir(Path::new(r"C:\Users\tester"));
    assert_eq!(directory, r"C:\Users\tester/.anki_vector/");
    assert!(directory.ends_with('/'), "the callers concatenate onto it");
    assert_eq!(
        format!("{directory}sdk_config.ini"),
        r"C:\Users\tester/.anki_vector/sdk_config.ini"
    );
}

#[cfg(unix)]
#[test]
fn the_sdk_ini_directory_hangs_off_the_home_directory_with_a_forward_slash() {
    let directory = sdk_ini_dir(Path::new("/home/tester"));
    assert_eq!(directory, "/home/tester/.anki_vector/");
    assert!(directory.ends_with('/'), "the callers concatenate onto it");
    assert_eq!(
        format!("{directory}sdk_config.ini"),
        "/home/tester/.anki_vector/sdk_config.ini"
    );
}
