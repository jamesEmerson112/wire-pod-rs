//! Translation of `pkg/wirepod/ttr/matchIntentSend.go`.
//!
//! Go passes the request around as `req interface{}` and reads the rest of what
//! it needs out of `vars` globals. Here the request is an [`IntentSink`], the
//! globals arrive in an [`IntentContext`], and the calls into packages this
//! crate does not depend on go through [`IntentHooks`].

use std::collections::HashMap;
use std::fmt;
use std::process::Command;

use serde::Deserialize;
use wirepod_core::intents::{CustomIntent, JsonIntent};
use wirepod_core::store::BotInfoRobot;
use wirepod_proto::chippergrpc2 as pb;

use crate::intentparam::{param_checker, prehistoric_param_checker};

/// condition, is_forecast, local_datetime, speakable_location_string,
/// temperature, temperature_unit, which is what `weatherParser` returns.
pub type Weather = (String, String, String, String, String, String);

/// Which of Go's three `vtt` request types the caller passed as `req`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Intent,
    IntentGraph,
    KnowledgeGraph,
}

/// What `req.Stream.Send` failed with, or the nil dereference Go makes when
/// `req` carries no stream of the kind being sent.
#[derive(Clone, Debug)]
pub struct SendError(pub String);

impl fmt::Display for SendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SendError {}

/// The request these functions answer on. Go writes the protobuf straight onto
/// `req.Stream`; the implementation sends it on the channel the chipper
/// service drains.
#[async_trait::async_trait]
pub trait IntentSink: Send + Sync {
    fn kind(&self) -> RequestKind;
    fn device(&self) -> &str;
    fn session(&self) -> &str;

    /// Go dereferences a nil `*vtt.IntentRequest` when the request is of
    /// another kind. That panic is this error.
    async fn send_intent(&self, _response: pb::IntentResponse) -> Result<(), SendError> {
        Err(SendError("request carries no intent stream".to_owned()))
    }

    async fn send_intent_graph(&self, _response: pb::IntentGraphResponse) -> Result<(), SendError> {
        Err(SendError(
            "request carries no intent graph stream".to_owned(),
        ))
    }
}

/// The calls these two files make into packages `wirepod-intent` does not
/// depend on. The caller implements it.
#[async_trait::async_trait]
pub trait IntentHooks: Send + Sync {
    // TODO(M3): wirepod_ttr::weather::weather_parser
    async fn weather_parser(
        &self,
        speech_text: &str,
        bot_location: &str,
        bot_units: &str,
    ) -> Weather;

    /// `go func(){ scripting.RunLuaScript(botSerial, c.LuaScript) }()`.
    ///
    /// Go's goroutine belongs to the implementor rather than to the matcher,
    /// because the Lua host sits in a crate above this one. Nothing here waits
    /// for the script, which is what the goroutine buys: a long script does not
    /// hold up the exec below it or the intent going back.
    fn run_lua_script(&self, bot_serial: &str, lua_script: &str);

    // TODO(M3): sayText from bcontrol.go, after vector.New
    async fn say_text(
        &self,
        bot_serial: &str,
        guid: &str,
        target: &str,
        text: &str,
    ) -> Result<(), String>;
}

/// One entry of Go's three parallel arrays `PluginNames`, `PluginUtterances`
/// and `PluginFunctions`, which one counter indexes together.
///
/// Nothing fills them. `ttr.LoadPlugins` reads Go `.so` files, which Rust
/// cannot open at all, so the loader is cut and the arrays stay empty; the Lua
/// host is this port's extensibility path.
pub struct Plugin {
    pub name: String,
    pub utterances: Vec<String>,
    pub function: fn(&str, &str, &str, &str) -> (String, String),
}

/// The `vars` globals these two files read.
pub struct IntentContext<'a> {
    /// `vars.APIConfig.STT.Language`.
    pub language: &'a str,
    /// `vars.APIConfig.Knowledge.IntentGraph`.
    pub intent_graph: bool,
    /// `vars.APIConfig.Weather.Enable`.
    pub weather_enable: bool,
    /// `vars.VoskGrammerEnable`.
    pub vosk_grammer_enable: bool,
    /// `vars.CustomIntents`; `None` is `vars.CustomIntentsExist == false`.
    pub custom_intents: Option<&'a [CustomIntent]>,
    /// `vars.BotInfo.Robots`.
    pub robots: &'a [BotInfoRobot],
    /// Empty until M5 loads the plugins.
    pub plugins: &'a [Plugin],
    /// `DefaultLocation` out of the robot's `vic.RobotSettings` jdoc, which
    /// [`crate::intentparam::bot_location_and_units`] reads.
    pub bot_location: &'a str,
    /// `F` or `C`, from `TempIsFahrenheit` in the same jdoc.
    pub bot_units: &'a str,
    pub hooks: &'a dyn IntentHooks,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SystemIntentResponseStruct {
    status: String,
    #[serde(rename = "returnIntent")]
    return_intent: String,
}

/// What `IntentPass` sent, which is Go's `*vtt.IntentResponse` or
/// `*vtt.IntentGraphResponse`.
#[derive(Clone, Debug)]
pub enum IntentPassOutcome {
    Intent(pb::IntentResponse),
    IntentGraph(pb::IntentGraphResponse),
}

// Go's `fmt.Sprint` of a `map[string]string`, which prints `map[k:v k2:v2]`
// with the keys sorted.
fn sprint_map(params: &HashMap<String, String>) -> String {
    let mut keys: Vec<&String> = params.keys().collect();
    keys.sort();
    let pairs: Vec<String> = keys
        .iter()
        .map(|key| format!("{key}:{}", params[*key]))
        .collect();
    format!("map[{}]", pairs.join(" "))
}

/// Go's `map[string]string{intentParam: intentParamValue}`. An empty
/// `intentParam` makes a map with one empty-string key, and that key reaches
/// the robot, so it is kept.
pub(crate) fn one_param(param: &str, value: &str) -> HashMap<String, String> {
    HashMap::from([(param.to_owned(), value.to_owned())])
}

pub async fn intent_pass(
    sink: &dyn IntentSink,
    ctx: &IntentContext<'_>,
    intent_thing: &str,
    speech_text: &str,
    intent_params: HashMap<String, String>,
    is_param: bool,
) -> Result<IntentPassOutcome, SendError> {
    let esn = sink.device();
    let is_intent_graph = sink.kind() == RequestKind::IntentGraph;
    let mut intent_thing = intent_thing.to_owned();

    // intercept if not intent graph but intent graph is enabled
    if !is_intent_graph && ctx.intent_graph && intent_thing == "intent_system_unmatched" {
        intent_thing = "intent_greeting_hello".to_owned();
    }

    let intent_result = if is_param {
        pb::IntentResult {
            query_text: speech_text.to_owned(),
            action: intent_thing.clone(),
            parameters: intent_params.clone(),
            ..Default::default()
        }
    } else {
        pb::IntentResult {
            query_text: speech_text.to_owned(),
            action: intent_thing.clone(),
            ..Default::default()
        }
    };
    tracing::info!(
        comp = "intent",
        bot = esn,
        "matched {intent_thing}, text '{speech_text}'"
    );
    if is_param {
        tracing::info!(
            comp = "intent",
            bot = esn,
            "params {}",
            sprint_map(&intent_params)
        );
    }
    let intent = pb::IntentResponse {
        is_final: true,
        intent_result: Some(intent_result.clone()),
        ..Default::default()
    };
    let intent_graph_send = pb::IntentGraphResponse {
        response_type: pb::IntentGraphMode::Intent as i32,
        is_final: true,
        intent_result: Some(intent_result),
        command_type: pb::RobotMode::VoiceCommand.as_str_name().to_owned(),
        ..Default::default()
    };
    if !is_intent_graph {
        sink.send_intent(intent.clone()).await?;
        tracing::info!(comp = "intent", bot = esn, "intent sent: {intent_thing}");
        if is_param {
            tracing::debug!(
                comp = "intent",
                bot = esn,
                "params sent: {}",
                sprint_map(&intent_params)
            );
        } else {
            tracing::debug!(comp = "intent", bot = esn, "no params sent");
        }
        Ok(IntentPassOutcome::Intent(intent))
    } else {
        sink.send_intent_graph(intent_graph_send.clone()).await?;
        tracing::info!(comp = "intent", bot = esn, "intent sent: {intent_thing}");
        if is_param {
            tracing::debug!(
                comp = "intent",
                bot = esn,
                "params sent: {}",
                sprint_map(&intent_params)
            );
        } else {
            tracing::debug!(comp = "intent", bot = esn, "no params sent");
        }
        Ok(IntentPassOutcome::IntentGraph(intent_graph_send))
    }
}

async fn custom_intent_handler(
    sink: &dyn IntentSink,
    ctx: &IntentContext<'_>,
    voice_text: &str,
    bot_serial: &str,
) -> bool {
    let mut success_matched = false;
    if let Some(custom_intents) = ctx.custom_intents {
        for c in custom_intents {
            for v in &c.utterances {
                // Check whether the custom sentence is either at the end of the spoken text or space-separated...
                let seek_text = v.trim().to_lowercase();
                // System intents can also match any utterances (*)
                if (c.is_system_intent && seek_text.starts_with('*'))
                    || voice_text.contains(&seek_text)
                {
                    tracing::debug!(
                        comp = "intent",
                        bot = bot_serial,
                        "custom intent matched: {} - {} - {}",
                        c.name,
                        c.description,
                        c.intent
                    );
                    let mut intent_params: HashMap<String, String> = HashMap::new();
                    let mut is_param = false;
                    if !c.params.param_value.is_empty() {
                        tracing::debug!(
                            comp = "intent",
                            bot = bot_serial,
                            "custom intent parameter: {} - {}",
                            c.params.param_name,
                            c.params.param_value
                        );
                        intent_params = one_param(&c.params.param_name, &c.params.param_value);
                        is_param = true;
                    }

                    if !c.lua_script.is_empty() {
                        ctx.hooks.run_lua_script(bot_serial, &c.lua_script);
                    }

                    let mut args: Vec<String> = Vec::new();
                    for arg in &c.exec_args {
                        let arg = match arg.as_str() {
                            "!botSerial" => bot_serial.to_owned(),
                            "!speechText" => format!("\"{voice_text}\""),
                            "!intentName" => c.name.clone(),
                            "!locale" => ctx.language.to_owned(),
                            other => other.to_owned(),
                        };
                        args.push(arg);
                    }
                    let run = if args.is_empty() {
                        tracing::debug!(comp = "intent", bot = bot_serial, "executing: {}", c.exec);
                        Command::new(&c.exec).output()
                    } else {
                        tracing::debug!(
                            comp = "intent",
                            bot = bot_serial,
                            "executing: {} {}",
                            c.exec,
                            args.join(" ")
                        );
                        Command::new(&c.exec).args(&args).output()
                    };
                    // Go prints the run error together with the captured
                    // stderr and carries on with whatever stdout it has.
                    let out = match run {
                        Ok(out) => {
                            if !out.status.success() {
                                println!(
                                    "{}: {}",
                                    out.status,
                                    String::from_utf8_lossy(&out.stderr)
                                );
                            }
                            out.stdout
                        }
                        Err(err) => {
                            println!("{err}: ");
                            Vec::new()
                        }
                    };
                    tracing::debug!(
                        comp = "intent",
                        bot = bot_serial,
                        "custom intent exec output: {}",
                        String::from_utf8_lossy(&out).trim()
                    );

                    if c.is_system_intent {
                        // A system intent returns its output in json format
                        if let Ok(resp) = serde_json::from_slice::<SystemIntentResponseStruct>(&out)
                            && resp.status == "ok"
                        {
                            tracing::debug!(
                                comp = "intent",
                                bot = bot_serial,
                                "system intent parsed and executed successfully"
                            );
                            let _ = intent_pass(
                                sink,
                                ctx,
                                &resp.return_intent,
                                voice_text,
                                intent_params,
                                is_param,
                            )
                            .await;
                            success_matched = true;
                        }
                    } else {
                        let _ =
                            intent_pass(sink, ctx, &c.intent, voice_text, intent_params, is_param)
                                .await;
                        success_matched = true;
                    }
                    break;
                }
                // Go checks here too, which it can only reach with
                // successMatched still false, because the branch above breaks.
                if success_matched {
                    break;
                }
            }
            if success_matched {
                break;
            }
        }
    }
    success_matched
}

async fn plugin_function_handler(
    sink: &dyn IntentSink,
    ctx: &IntentContext<'_>,
    voice_text: &str,
    bot_serial: &str,
) -> bool {
    let mut matched = false;
    let is_igr = sink.kind() == RequestKind::IntentGraph;
    for plugin in ctx.plugins {
        for s in &plugin.utterances {
            if voice_text.contains(s.as_str()) || s == "*" {
                tracing::debug!(
                    comp = "intent",
                    bot = bot_serial,
                    "matched plugin {}, executing function",
                    plugin.name
                );
                let mut guid = String::new();
                let mut target = String::new();
                for bot in ctx.robots {
                    if bot.esn == bot_serial {
                        guid = bot.guid.clone();
                        target = format!("{}:443", bot.ip_address);
                    }
                }
                let (mut intent, plugin_response) =
                    (plugin.function)(voice_text, bot_serial, &guid, &target);
                if intent.is_empty() && plugin_response.is_empty() {
                    break;
                }
                if intent.is_empty() {
                    intent = "intent_imperative_praise".to_owned();
                }
                tracing::debug!(
                    comp = "intent",
                    bot = bot_serial,
                    "plugin {}, response {plugin_response}",
                    plugin.name
                );
                if !plugin_response.is_empty() && is_igr {
                    let response = pb::IntentGraphResponse {
                        session: sink.session().to_owned(),
                        device_id: sink.device().to_owned(),
                        response_type: pb::IntentGraphMode::KnowledgeGraph as i32,
                        spoken_text: plugin_response,
                        query_text: voice_text.to_owned(),
                        is_final: true,
                        ..Default::default()
                    };
                    let _ = sink.send_intent_graph(response).await;
                } else if !plugin_response.is_empty() {
                    // TODO(M4): KGSim(botSerial, pluginResponse)
                } else {
                    let _ =
                        intent_pass(sink, ctx, &intent, voice_text, HashMap::new(), false).await;
                }
                matched = true;
                break;
            }
        }
        if matched {
            break;
        }
    }
    matched
}

pub async fn process_text_all(
    sink: &dyn IntentSink,
    ctx: &IntentContext<'_>,
    voice_text: &str,
    intents: &[JsonIntent],
    is_opus: bool,
) -> bool {
    let bot_serial = sink.device().to_owned();
    // Go carries a `matched` flag and an `intentNum` counter to leave the outer
    // loop; `intentNum` is never read, and the flag is a labelled break here.
    let mut success_matched = false;
    let voice_text = voice_text.to_lowercase();
    let plugin_matched = plugin_function_handler(sink, ctx, &voice_text, &bot_serial).await;
    let custom_intent_matched = custom_intent_handler(sink, ctx, &voice_text, &bot_serial).await;
    if !custom_intent_matched && !plugin_matched {
        tracing::debug!(
            comp = "intent",
            bot = bot_serial.as_str(),
            "not a custom intent"
        );
        // Look for a perfect match first
        'perfect: for b in intents {
            for c in &b.keyphrases {
                if voice_text == c.to_lowercase() {
                    tracing::debug!(
                        comp = "intent",
                        bot = bot_serial.as_str(),
                        "perfect match for intent {} ({})",
                        b.name,
                        c.to_lowercase()
                    );
                    if is_opus {
                        param_checker(sink, ctx, &b.name, &voice_text, &bot_serial).await;
                    } else {
                        prehistoric_param_checker(sink, ctx, &b.name, &voice_text).await;
                    }
                    success_matched = true;
                    break 'perfect;
                }
            }
        }
        // Not found? Then let's be happy with a bare substring search
        if !success_matched {
            'partial: for b in intents {
                for c in &b.keyphrases {
                    if voice_text.contains(&c.to_lowercase()) && !b.require_exact_match {
                        tracing::debug!(
                            comp = "intent",
                            bot = bot_serial.as_str(),
                            "partial match for intent {} ({})",
                            b.name,
                            c.to_lowercase()
                        );
                        if is_opus {
                            param_checker(sink, ctx, &b.name, &voice_text, &bot_serial).await;
                        } else {
                            prehistoric_param_checker(sink, ctx, &b.name, &voice_text).await;
                        }
                        success_matched = true;
                        break 'partial;
                    }
                }
            }
        }
    } else {
        tracing::debug!(
            comp = "intent",
            bot = bot_serial.as_str(),
            "this is a custom intent or plugin"
        );
        success_matched = true;
    }
    success_matched
}

pub async fn knowledge_graph_response_ig(
    sink: &dyn IntentSink,
    spoken_text: &str,
    query_text: &str,
) -> Result<(), SendError> {
    let intent_result = pb::IntentResult {
        query_text: query_text.to_owned(),
        action: "intent_knowledge_response_extend_bypass".to_owned(),
        ..Default::default()
    };

    let intent_graph_send = pb::IntentGraphResponse {
        response_type: pb::IntentGraphMode::KnowledgeGraph as i32,
        is_final: true,
        intent_result: Some(intent_result),
        spoken_text: spoken_text.to_owned(),
        query_text: query_text.to_owned(),
        command_type: pb::RobotMode::VoiceCommand.as_str_name().to_owned(),
        ..Default::default()
    };
    sink.send_intent_graph(intent_graph_send).await
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;

    use super::*;

    /// An [`IntentSink`] that keeps what was sent instead of writing a stream.
    pub(crate) struct FakeSink {
        pub kind: RequestKind,
        pub sent: Mutex<Vec<pb::IntentResult>>,
        pub graph: Mutex<Vec<pb::IntentGraphResponse>>,
    }

    impl FakeSink {
        pub(crate) fn new(kind: RequestKind) -> Self {
            Self {
                kind,
                sent: Mutex::new(Vec::new()),
                graph: Mutex::new(Vec::new()),
            }
        }

        pub(crate) fn last(&self) -> pb::IntentResult {
            self.sent.lock().unwrap().last().cloned().unwrap()
        }
    }

    #[async_trait::async_trait]
    impl IntentSink for FakeSink {
        fn kind(&self) -> RequestKind {
            self.kind
        }

        fn device(&self) -> &str {
            "00303f28"
        }

        fn session(&self) -> &str {
            "session"
        }

        async fn send_intent(&self, response: pb::IntentResponse) -> Result<(), SendError> {
            self.sent
                .lock()
                .unwrap()
                .push(response.intent_result.unwrap_or_default());
            Ok(())
        }

        async fn send_intent_graph(
            &self,
            response: pb::IntentGraphResponse,
        ) -> Result<(), SendError> {
            self.sent
                .lock()
                .unwrap()
                .push(response.intent_result.clone().unwrap_or_default());
            self.graph.lock().unwrap().push(response);
            Ok(())
        }
    }

    pub(crate) struct FakeHooks;

    #[async_trait::async_trait]
    impl IntentHooks for FakeHooks {
        async fn weather_parser(
            &self,
            _speech_text: &str,
            bot_location: &str,
            bot_units: &str,
        ) -> Weather {
            (
                "Sunny".to_owned(),
                "false".to_owned(),
                "2026-09-20T12:00:00".to_owned(),
                bot_location.to_owned(),
                "20".to_owned(),
                bot_units.to_owned(),
            )
        }

        fn run_lua_script(&self, _bot_serial: &str, _lua_script: &str) {}

        async fn say_text(
            &self,
            _bot_serial: &str,
            _guid: &str,
            _target: &str,
            _text: &str,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    pub(crate) fn context<'a>(hooks: &'a FakeHooks) -> IntentContext<'a> {
        IntentContext {
            language: "en-US",
            intent_graph: false,
            weather_enable: false,
            vosk_grammer_enable: false,
            custom_intents: None,
            robots: &[],
            plugins: &[],
            bot_location: "San Francisco",
            bot_units: "F",
            hooks,
        }
    }

    fn intent(name: &str, keyphrases: &[&str], exact: bool) -> JsonIntent {
        JsonIntent {
            name: name.to_owned(),
            keyphrases: keyphrases.iter().map(|k| (*k).to_owned()).collect(),
            require_exact_match: exact,
        }
    }

    #[tokio::test]
    async fn intent_pass_rewrites_unmatched_and_keeps_an_empty_parameter_key() {
        let hooks = FakeHooks;
        let mut ctx = context(&hooks);
        ctx.intent_graph = true;

        let sink = FakeSink::new(RequestKind::Intent);
        intent_pass(
            &sink,
            &ctx,
            "intent_system_unmatched",
            "hello",
            one_param("", ""),
            false,
        )
        .await
        .unwrap();
        assert_eq!(sink.last().action, "intent_greeting_hello");

        // On the intent graph path the rewrite does not happen, and the empty
        // key survives into the parameters the robot receives.
        let graph = FakeSink::new(RequestKind::IntentGraph);
        intent_pass(
            &graph,
            &ctx,
            "intent_system_unmatched",
            "hello",
            one_param("", ""),
            true,
        )
        .await
        .unwrap();
        assert_eq!(graph.last().action, "intent_system_unmatched");
        assert_eq!(graph.last().parameters.get(""), Some(&String::new()));
        assert_eq!(graph.graph.lock().unwrap()[0].command_type, "VOICE_COMMAND");

        // A knowledge graph request carries neither stream, which is where Go
        // dereferences a nil pointer.
        struct KnowledgeGraphSink;

        #[async_trait::async_trait]
        impl IntentSink for KnowledgeGraphSink {
            fn kind(&self) -> RequestKind {
                RequestKind::KnowledgeGraph
            }

            fn device(&self) -> &str {
                "00303f28"
            }

            fn session(&self) -> &str {
                "session"
            }
        }

        assert!(
            intent_pass(
                &KnowledgeGraphSink,
                &ctx,
                "intent_greeting_hello",
                "hello",
                HashMap::new(),
                false
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn process_text_all_prefers_an_exact_match_and_honours_requires_exact() {
        let hooks = FakeHooks;
        let ctx = context(&hooks);
        let intents = [
            intent("intent_greeting_hello", &["hello"], false),
            intent("intent_imperative_praise", &["good robot"], true),
            intent("intent_names_username_extend", &["my name is"], false),
        ];

        for (text, want) in [
            ("Hello", Some("intent_greeting_hello")),
            ("my name is james", Some("intent_names_username_extend")),
            ("you are a good robot", None),
            ("what is the airspeed of a swallow", None),
        ] {
            let sink = FakeSink::new(RequestKind::Intent);
            let matched = process_text_all(&sink, &ctx, text, &intents, true).await;
            assert_eq!(matched, want.is_some(), "{text}");
            if let Some(want) = want {
                assert_eq!(sink.last().action, want, "{text}");
            } else {
                assert!(sink.sent.lock().unwrap().is_empty(), "{text}");
            }
        }
    }
}
