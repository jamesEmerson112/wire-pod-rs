//! Where the server's files live.
//!
//! Go holds one package-level string per file. A source build leaves them at
//! the relative literals they are declared with (`vars.go:35-55`, plus
//! `config.go:13`), resolved against the working directory the process was
//! started in. A packaged build rewrites most of them in `vars.Init` under
//! `os.UserConfigDir()` joined with `PodName` (`vars.go:158-188`), which on
//! Windows is `%APPDATA%\wire-pod`. [`DataDir`] is those two layouts, and
//! [`AssetDir`] is the files Go reads relative to the working directory in
//! both of them, because the packaged wrapper changes directory into the
//! install directory before starting the server.
//!
//! Two of the rewrites are string concatenation rather than `filepath.Join`.
//! `JdocsPath` at `vars.go:170` and `BotInfoPath` at `vars.go:173` glue
//! `"/jdocs.json"` and `"/" + BotInfoName` onto a directory that
//! `filepath.Join` has already spelled with the platform separator, so on
//! Windows the running process opens `...\wire-pod\jdocs/jdocs.json`, with
//! both separators in the one path, and prints exactly that string in any log
//! line that names the file. Windows accepts either separator so nothing about
//! the file changes, but the string is observable, and the port reproduces it:
//! [`DataDir::jdocs_path`], [`DataDir::bot_info_path`] and
//! [`DataDir::session_cert_path`] hand back a `String` built the way Go builds
//! it. Every other accessor hands back a `PathBuf`, because every other path
//! is a `filepath.Join` whose result carries one separator throughout.
//!
//! Not reproduced: `WhisperModelPath` (`vars.go:175`), which is a macOS bundle
//! path built from the executable's directory rather than from the data
//! directory; the android and iOS branches; and the Linux `os.Getwd` heuristic
//! that guesses the SDK ini directory from the path the process was started in
//! (`vars.go:205-227`). The last of those is deviation 38: the port resolves
//! every path explicitly and logs what it resolved rather than inferring one
//! from the working directory.

use std::path::{Path, PathBuf};

/// Go's `PodName` (`vars.go:42`): the directory `vars.Init` creates under the
/// user config directory when the build is packaged (`vars.go:166`).
pub const POD_NAME: &str = "wire-pod";

/// Go's `BotInfoName` (`vars.go:41`), the file name `BotInfoPath` is
/// concatenated from at `vars.go:173`.
pub const BOT_INFO_NAME: &str = "botSdkInfo.json";

/// A path as the string Go would hold and print.
///
/// Go paths are bytes and Rust paths are not, so a path that is not valid
/// Unicode cannot round trip. Every path this crate builds is rooted at the
/// user config directory or at a directory the operator named, and none of
/// this machine's are, so the lossy form is the honest one: it is better to
/// log a replacement character than to refuse to name the file.
fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Which of Go's two layouts a [`DataDir`] answers with.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Root {
    /// The source layout: every path is the relative literal declared at
    /// `vars.go:35-55`, resolved by the operating system against the process's
    /// working directory, exactly as an un-packaged Go build leaves it.
    Cwd,
    /// The packaged layout: `vars.Init` rewrote every path under this
    /// directory (`vars.go:158-188`).
    Under(PathBuf),
}

/// The directory holding the server's mutable state, and one accessor per file
/// Go names inside it.
///
/// Go rewrites its path globals once at startup and every later reader takes
/// the global. Here the layout is a value the caller passes down, so a test can
/// point a whole server at a temporary directory without touching a global or
/// the live `%APPDATA%\wire-pod`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DataDir {
    root: Root,
}

impl DataDir {
    /// Go's source layout: the relative literals of `vars.go:35-55`, resolved
    /// against the process's working directory.
    ///
    /// The certificate paths escape upwards, to `../certs`, because an
    /// un-packaged Go server runs from `wire-pod/chipper` and its certificates
    /// live in `wire-pod/certs`. That is why this constructor takes no root:
    /// the layout is relative to the working directory in a way that no single
    /// root describes.
    pub fn source() -> Self {
        Self { root: Root::Cwd }
    }

    /// Go's packaged layout, rooted at `config_dir` joined with [`POD_NAME`],
    /// which is what `join(confDir, PodName)` builds at `vars.go:166` from
    /// `os.UserConfigDir()`. On Windows that directory is `%APPDATA%`, so the
    /// root is `%APPDATA%\wire-pod`.
    pub fn packaged(config_dir: &Path) -> Self {
        Self::rooted(config_dir.join(POD_NAME))
    }

    /// The packaged layout rooted at an explicit directory, which is what
    /// `--data-dir` selects. `root` is the pod directory itself, not the config
    /// directory above it.
    ///
    /// `root` is used exactly as it is given, and only the components below it
    /// are joined on. Go's `filepath.Join` would have cleaned the whole result
    /// into the platform separator, so `rooted("C:/wire-pod")` answers a path
    /// that keeps the forward slash the operator typed where Go would have
    /// rewritten it. Nothing in Go reaches this constructor to be compared
    /// with: its packaged branch is only ever handed `os.UserConfigDir()`,
    /// which is already backslashed, and `--data-dir` has no Go counterpart.
    /// The spelling an operator supplies is therefore the spelling the log line
    /// and the accessors show, which is the more useful of the two answers when
    /// the point of the flag is to say where the state went.
    pub fn rooted(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Root::Under(root.into()),
        }
    }

    /// The directory every path is rooted at, or `None` in the source layout,
    /// where the root is whatever working directory the process was started in.
    pub fn root(&self) -> Option<&Path> {
        match &self.root {
            Root::Cwd => None,
            Root::Under(root) => Some(root),
        }
    }

    /// Joins the packaged layout's components under the root, or answers the
    /// source layout's literal.
    ///
    /// The two spellings sit side by side at every call below so that a reader
    /// can check both against the Go line the accessor cites. `packaged` is
    /// split into components rather than written as one `"a/b"` string because
    /// Go's `filepath.Join` cleans its result into the platform separator and
    /// `Path::join` does not.
    fn resolve(&self, packaged: &[&str], source: &str) -> PathBuf {
        match &self.root {
            Root::Cwd => PathBuf::from(source),
            Root::Under(root) => {
                let mut path = root.clone();
                path.extend(packaged);
                path
            }
        }
    }

    /// `vars.JdocsDir`: `join(podDir, "./jdocs")` (`vars.go:169`), or
    /// `"./jdocs"` (`vars.go:37`). Go creates it at `vars.go:185`.
    pub fn jdocs_dir(&self) -> PathBuf {
        self.resolve(&["jdocs"], "./jdocs")
    }

    /// `vars.JdocsPath`: `JdocsDir + "/jdocs.json"` (`vars.go:170`), or
    /// `"./jdocs/jdocs.json"` (`vars.go:36`).
    ///
    /// A `String` because of the concatenation: in the packaged layout on
    /// Windows this carries the directory's backslashes and then a forward
    /// slash before the file name. In the source layout the concatenation
    /// reproduces the declared literal exactly, so one construction serves both
    /// layouts. Go writes this file at `vars.go:317`.
    pub fn jdocs_path(&self) -> String {
        format!("{}/jdocs.json", text(&self.jdocs_dir()))
    }

    /// `vars.BotInfoPath`: `JdocsDir + "/" + BotInfoName` (`vars.go:173`), or
    /// `"./jdocs/botSdkInfo.json"` (`vars.go:40`). A `String` for the same
    /// reason as [`DataDir::jdocs_path`].
    pub fn bot_info_path(&self) -> String {
        format!("{}/{}", text(&self.jdocs_dir()), BOT_INFO_NAME)
    }

    /// `vars.CustomIntentsPath`: `join(podDir, "./customIntents.json")`
    /// (`vars.go:171`), or `"./customIntents.json"` (`vars.go:38`).
    pub fn custom_intents_path(&self) -> PathBuf {
        self.resolve(&["customIntents.json"], "./customIntents.json")
    }

    /// `vars.BotConfigsPath`: `join(podDir, "./botConfig.json")`
    /// (`vars.go:172`), or `"./botConfig.json"` (`vars.go:39`).
    pub fn bot_configs_path(&self) -> PathBuf {
        self.resolve(&["botConfig.json"], "./botConfig.json")
    }

    /// `vars.ApiConfigPath`: `join(podDir, "./apiConfig.json")`
    /// (`vars.go:176`), or `"./apiConfig.json"` (`config.go:13`). Go rewrites
    /// this file at every boot (`config.go:155`).
    pub fn api_config_path(&self) -> PathBuf {
        self.resolve(&["apiConfig.json"], "./apiConfig.json")
    }

    /// `vars.SessionCertPath`: `join(podDir, "./session-certs/")`
    /// (`vars.go:181`), or `"./session-certs/"` (`vars.go:45`). Go creates it
    /// at `vars.go:186` and lists it at `vars.go:371`.
    ///
    /// The trailing slash survives in the source layout because that is how the
    /// literal is declared; `filepath.Join` cleans it away in the packaged one.
    /// Nothing about the directory depends on it, since every reader either
    /// joins onto it or concatenates a separator of its own, but it is part of
    /// the string Go prints when it announces the directory, and it is why
    /// [`DataDir::session_cert_path`] doubles the slash in that layout.
    pub fn session_cert_dir(&self) -> PathBuf {
        self.resolve(&["session-certs"], "./session-certs/")
    }

    /// One robot's session certificate, `SessionCertPath + "/" + esn`, which is
    /// how both writers spell it (`jdocs/botInfoStorer.go:99` with mode `0777`,
    /// `jdocs/server.go:123` with mode `0755`).
    ///
    /// A `String`, because the separator is concatenated at those call sites
    /// rather than joined: in the packaged layout on Windows the result mixes
    /// separators the way [`DataDir::jdocs_path`] does, and in the source
    /// layout it carries the doubled slash that the directory's own trailing
    /// slash produces. A third reader, `config-ws/webserver.go:511`, builds the
    /// same file with `path.Join`, which spells it with forward slashes on
    /// every platform; the two forms name the same file.
    pub fn session_cert_path(&self, esn: &str) -> String {
        format!("{}/{}", text(&self.session_cert_dir()), esn)
    }

    /// `vars.Certs`: `join(podDir, "./certs")` (`vars.go:180`), or `"../certs"`
    /// (`vars.go:54`). Go creates it at `vars.go:187`.
    pub fn certs_dir(&self) -> PathBuf {
        self.resolve(&["certs"], "../certs")
    }

    /// `vars.CertPath`: `join(podDir, "./certs/cert.crt")` (`vars.go:177`), or
    /// `"../certs/cert.crt"` (`vars.go:51`).
    pub fn cert_path(&self) -> PathBuf {
        self.resolve(&["certs", "cert.crt"], "../certs/cert.crt")
    }

    /// `vars.KeyPath`: `join(podDir, "./certs/cert.key")` (`vars.go:178`), or
    /// `"../certs/cert.key"` (`vars.go:52`).
    pub fn key_path(&self) -> PathBuf {
        self.resolve(&["certs", "cert.key"], "../certs/cert.key")
    }

    /// `vars.ServerConfigPath`: `join(podDir, "./certs/server_config.json")`
    /// (`vars.go:179`), or `"../certs/server_config.json"` (`vars.go:53`).
    pub fn server_config_path(&self) -> PathBuf {
        self.resolve(
            &["certs", "server_config.json"],
            "../certs/server_config.json",
        )
    }

    /// `vars.VoskModelPath`: `join(podDir, "./vosk/models/")` (`vars.go:174`),
    /// or `"../vosk/models/"` (`vars.go:43`). `filepath.Join` cleans both the
    /// interior separator and the trailing one, so the packaged form is two
    /// joined components rather than one.
    pub fn vosk_model_dir(&self) -> PathBuf {
        self.resolve(&["vosk", "models"], "../vosk/models/")
    }
}

/// The directory holding the files that ship with the server rather than the
/// files it writes, and one accessor per file Go reads out of it.
///
/// Go has no such variable. It reads each of these through a literal relative
/// to the working directory, and the packaged wrapper changes directory into
/// the install directory before starting the server, so in practice they all
/// resolve beside the executable. The port names the directory instead and logs
/// what it resolved, which is the reporting half of deviation 38.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetDir {
    root: PathBuf,
}

impl AssetDir {
    /// The asset directory rooted at `root`, which is what `--asset-dir`
    /// selects and what Go leaves as the working directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The escape-pod certificate, `"./epod/ep.crt"`
    /// (`initwirepod/startserver.go:161`).
    pub fn epod_cert_path(&self) -> PathBuf {
        self.root.join("epod").join("ep.crt")
    }

    /// The escape-pod key, `"./epod/ep.key"`
    /// (`initwirepod/startserver.go:162`).
    pub fn epod_key_path(&self) -> PathBuf {
        self.root.join("epod").join("ep.key")
    }

    /// The web UI's document root, `"./webroot"`
    /// (`config-ws/webserver.go:437`).
    pub fn webroot_dir(&self) -> PathBuf {
        self.root.join("webroot")
    }

    /// The SDK app's document root, `"./webroot/sdkapp"`
    /// (`sdkapp/server.go:26`).
    pub fn sdk_app_dir(&self) -> PathBuf {
        self.webroot_dir().join("sdkapp")
    }

    /// One locale's intent file, `"./" + "intent-data/" + language + ".json"`
    /// (`vars.go:292-294`). `language` is the configured STT language, which is
    /// the file's stem.
    pub fn intent_data_path(&self, language: &str) -> PathBuf {
        self.root
            .join("intent-data")
            .join(format!("{language}.json"))
    }

    /// The weather condition map, `"./weather-map.json"`
    /// (`ttr/weather.go:220`).
    pub fn weather_map_path(&self) -> PathBuf {
        self.root.join("weather-map.json")
    }

    /// The warm-up sample every speech engine transcribes at startup,
    /// `"./stttest.pcm"` (`stt/vosk/Vosk.go:116`).
    pub fn stttest_path(&self) -> PathBuf {
        self.root.join("stttest.pcm")
    }

    /// The version file the web UI reads, `"./version"` (`vars.go:46`, read at
    /// `config-ws/webserver.go:358`).
    ///
    /// It belongs here rather than on [`DataDir`] because `vars.Init` never
    /// rewrites it on a desktop platform: the android branch at `vars.go:183`
    /// is the only one that touches it, so a packaged Windows server reads it
    /// out of the directory the wrapper changed into, beside the assets.
    pub fn version_file(&self) -> PathBuf {
        self.root.join("version")
    }
}

/// The SDK ini directory the port falls back to when nobody resolved one:
/// `./.anki_vector/`, under whatever working directory the process was started
/// in.
///
/// Not a Go path. Go's `SDKIniPath` is absolute in every branch it can take
/// (`vars.go:207-227`): the user's home directory on Windows and macOS
/// (`:208-209`), the Android data directory (`:211`), and a path rebuilt from
/// `os.Getwd` on Linux (`:217-225`). None of the three is relative and none of
/// them is this, so a state that ends up here corresponds to no Go mode at all.
///
/// It is relative on purpose. A default rooted at the user's home directory
/// would put an unconfigured test's writes into the real `~/.anki_vector/`,
/// beside the ini file the production server and the Python SDK share, which is
/// a worse failure than a fallback that is obviously nobody's directory. The
/// boot path passes the home directory explicitly, and a builder that reaches
/// this constant says so in a warning
/// ([`crate::state::AppStateBuilder::build`]).
pub const DEFAULT_SDK_INI_DIR: &str = "./.anki_vector/";

/// Go's `SDKIniPath` on Windows and macOS: the user's home directory with
/// `"/.anki_vector/"` concatenated onto it (`vars.go:207-209`).
///
/// A `String` with a trailing slash, both faithfully. The concatenation means
/// that on Windows the value mixes separators exactly the way
/// [`DataDir::jdocs_path`] does, and Go prints it at `vars.go:228`.
///
/// The trailing slash is load bearing because Go's readers disagree about how
/// they attach a file name to it, and on Windows the two spellings differ.
/// Everything that touches `sdk_config.ini` concatenates, with no separator of
/// its own: the ini file itself at `jdocs/botInfoStorer.go:32`, `:60`, `:70`,
/// `:127` and `token/token.go:177`, and the `cert` value stored inside it at
/// `jdocs/botInfoStorer.go:43`, `:55`, `:83` and `:106`. The certificate file
/// that value names is the exception: `jdocs/server.go:114` builds its path
/// with `filepath.Join`, which cleans every separator to a backslash, and
/// writes it at `:121`. So the path recorded in the ini and the path of the
/// file on disk are the same file spelled two ways, and a caller writing the
/// ini has to reproduce the concatenated spelling rather than the joined one.
///
/// The Linux branch (`vars.go:212-227`), which reconstructs the path from
/// `os.Getwd`, is deviation 38 and is not reproduced; the caller passes the
/// home directory on every platform.
pub fn sdk_ini_dir(home_dir: &Path) -> String {
    format!("{}/.anki_vector/", text(home_dir))
}
