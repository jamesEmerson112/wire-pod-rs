//! Go's `pkg/servers/jdocs/server.go`: the jdocs gRPC service.

use std::sync::Arc;

use tonic::{Request, Response, Status};
use wirepod_core::logger::COMP_JDOCS;
use wirepod_core::{
    AppState, Jdoc, SecondaryEntry, cert_file_path, create_token_and_hashed_token, host_of,
    write_bot_info, write_session_cert, write_token_hash,
};
use wirepod_proto::jdocspb::jdocs_server::Jdocs;
use wirepod_proto::jdocspb::{
    DeleteDocReq, DeleteDocResp, Jdoc as JdocMsg, PurgeAccountDocsReq, PurgeAccountDocsResp,
    ReadDocsReq, ReadDocsResp, ViewAccountDocsReq, ViewDocsResp, WriteDocReq, WriteDocResp,
    read_docs_req, read_docs_resp, write_doc_resp,
};

use crate::token::{peer_address, set_bot_guid};

/// The `vic.AppTokens` document wire-pod hands a robot it cannot place, whose
/// hash is the one the global GUID verifies against (`server.go:63`).
const GLOBAL_GUID_HASH: &str = concat!(
    r#"{"client_tokens":[{"hash":"J5TAnJTPRCioMExFo5KzH2fHOAXyM5fuO8YRbQSamIsNzymnJ8KDIerFxuJV4qBN","#,
    r#""client_name":"","app_id":"","issued_at":"2022-11-26T18:23:08Z","is_primary":true}]}"#
);

/// Go's `JdocServer` (`server.go:17-19`), carrying the state its globals were.
pub struct JdocServer {
    state: Arc<AppState>,
}

/// Go's `esnOf` (`server.go:21-23`).
fn esn_of(thing: &str) -> &str {
    thing.strip_prefix("vic:").unwrap_or(thing)
}

/// Go's `itemsToStr` (`server.go:25-31`).
fn items_to_str(items: &[read_docs_req::Item]) -> String {
    items
        .iter()
        .map(|item| item.doc_name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `strings.Split(thing, ":")[1]`, which Go indexes without checking.
fn esn_of_thing(thing: &str) -> Option<&str> {
    thing.split(':').nth(1)
}

/// What a `thing` with no colon answers, where Go takes the process down.
fn no_esn() -> Status {
    Status::invalid_argument("thing carries no colon")
}

/// The IP-change loop both RPCs run (`server.go:47-54`, `:72-79`).
async fn update_ip(state: &AppState, esn: &str, ip_addr: &str) {
    let mut bot_info = state.bot_info_snapshot();
    for index in 0..bot_info.robots.len() {
        if bot_info.robots[index].esn == esn && bot_info.robots[index].ip_address != ip_addr {
            tracing::info!(target: COMP_JDOCS, bot = %esn, "IP changed to {ip_addr}");
            bot_info.robots[index].ip_address = ip_addr.to_owned();
            state.set_bot_info(bot_info.clone());
            if let Err(err) = write_bot_info(state.paths().data(), &bot_info).await {
                tracing::warn!(target: COMP_JDOCS, bot = %esn, "write bot info: {err}");
            }
        }
    }
}

/// The four fields Go copies one at a time into a `vars.AJdoc`.
fn to_jdoc(msg: JdocMsg) -> Jdoc {
    Jdoc {
        doc_version: msg.doc_version,
        fmt_version: msg.fmt_version,
        client_metadata: msg.client_metadata,
        json_doc: msg.json_doc,
        ..Jdoc::default()
    }
}

/// The same four fields in the other direction.
fn to_msg(jdoc: Jdoc) -> JdocMsg {
    JdocMsg {
        doc_version: jdoc.doc_version,
        fmt_version: jdoc.fmt_version,
        client_metadata: jdoc.client_metadata,
        json_doc: jdoc.json_doc,
    }
}

/// One `ReadDocsResp_Item` with `CHANGED` and a document.
fn changed(doc: JdocMsg) -> read_docs_resp::Item {
    read_docs_resp::Item {
        status: read_docs_resp::Status::Changed as i32,
        doc: Some(doc),
    }
}

#[tonic::async_trait]
impl Jdocs for JdocServer {
    async fn write_doc(
        &self,
        request: Request<WriteDocReq>,
    ) -> Result<Response<WriteDocResp>, Status> {
        let ip_addr = host_of(&peer_address(&request)).to_owned();
        let req = request.into_inner();
        tracing::info!(target: COMP_JDOCS, bot = %esn_of(&req.thing), "WriteDoc {}", req.doc_name);
        // Go reads four fields off a nil `req.Doc` and takes the process down.
        let doc = req
            .doc
            .ok_or_else(|| Status::invalid_argument("request carries no doc"))?;
        let outcome = self
            .state
            .jdocs()
            .add_jdoc(&req.thing, &req.doc_name, to_jdoc(doc))
            .await;
        if let Err(err) = outcome.written {
            tracing::warn!(target: COMP_JDOCS, "write jdocs: {err}");
        }

        let esn = esn_of_thing(&req.thing).ok_or_else(no_esn)?;
        update_ip(&self.state, esn, &ip_addr).await;

        Ok(Response::new(WriteDocResp {
            status: write_doc_resp::Status::Accepted as i32,
            latest_doc_version: outcome.latest_version,
        }))
    }

    async fn read_docs(
        &self,
        request: Request<ReadDocsReq>,
    ) -> Result<Response<ReadDocsResp>, Status> {
        let peer_addr = peer_address(&request);
        let ip_addr = host_of(&peer_addr).to_owned();
        let req = request.into_inner();
        tracing::debug!(
            target: COMP_JDOCS,
            bot = %esn_of(&req.thing),
            "ReadDocs: {}",
            items_to_str(&req.items)
        );
        let esn = esn_of_thing(&req.thing).ok_or_else(no_esn)?.to_owned();
        let is_already_known = self.state.with_bot_info(|info| info.is_bot_in_info(&esn));

        update_ip(&self.state, &esn, &ip_addr).await;

        if self.state.tokens().session_holds(&ip_addr)
            && let Err(err) = self.state.jdocs().delete_data(&req.thing).await
        {
            tracing::warn!(target: COMP_JDOCS, bot = %esn, "delete data: {err}");
        }

        // Go indexes `req.Items[0]` without checking the length.
        let first = req
            .items
            .first()
            .ok_or_else(|| Status::invalid_argument("request carries no items"))?;
        if first.doc_name.contains("vic.AppTokens") {
            let mut bot_info = self.state.bot_info_snapshot();
            if bot_info.store_bot_info(&peer_addr, &req.thing) {
                self.state.set_bot_info(bot_info.clone());
                if let Err(err) = write_bot_info(self.state.paths().data(), &bot_info).await {
                    tracing::warn!(target: COMP_JDOCS, bot = %esn, "write bot info: {err}");
                }
            }
            let token_exists = self
                .state
                .jdocs()
                .get_jdoc(&req.thing, "vic.AppTokens")
                .is_some();
            if !token_exists {
                tracing::debug!(
                    target: COMP_JDOCS,
                    bot = %esn,
                    "App tokens jdoc not found for this bot, trying bots in TokenHashStore"
                );
                let walk = self.state.tokens().take_primary_matches(&ip_addr);
                for entry in &walk.matches {
                    let lowered = esn.trim().to_ascii_lowercase();
                    if let Err(err) = write_token_hash(
                        self.state.jdocs(),
                        &lowered,
                        &entry.guid_hash,
                        self.state.wall().as_ref(),
                    )
                    .await
                    {
                        tracing::error!(
                            target: COMP_JDOCS,
                            bot = %esn,
                            "Error writing token hash to vic.AppTokens: {err}"
                        );
                    }
                    if let Err(err) =
                        set_bot_guid(&self.state, &esn, &entry.guid, &entry.guid_hash).await
                    {
                        tracing::error!(
                            target: COMP_JDOCS,
                            bot = %esn,
                            "Error writing token hash to {}: {err}",
                            self.state.paths().data().bot_info_path()
                        );
                    }
                    tracing::debug!(
                        target: COMP_JDOCS,
                        bot = %esn,
                        "matched with IP {ip_addr} in token store"
                    );
                }
                let matched = walk.matched();
                let bot_guid = walk.bot_guid().to_owned();

                let mut session_matched = false;
                if let Some(found) = self.state.tokens().find_session_match(&ip_addr) {
                    session_matched = true;
                    let full_path =
                        cert_file_path(self.state.sdk_ini().dir(), &found.entry.name, &esn);
                    tracing::debug!(
                        target: COMP_JDOCS,
                        bot = %esn,
                        "Outputting session cert to {}",
                        full_path.display()
                    );
                    if let Err(err) = self
                        .state
                        .sdk_ini()
                        .write_cert(&found.entry.name, &esn, found.entry.cert.clone())
                        .await
                    {
                        tracing::warn!(target: COMP_JDOCS, bot = %esn, "write sdk cert: {err}");
                    }
                    if let Err(err) = write_session_cert(
                        self.state.paths().data(),
                        &esn,
                        found.entry.cert.clone(),
                    )
                    .await
                    {
                        tracing::warn!(target: COMP_JDOCS, bot = %esn, "write session cert: {err}");
                    }
                    if let Err(err) = self
                        .state
                        .sdk_ini()
                        .write_to_ini_primary(&found.entry.name, &esn, &bot_guid, &ip_addr)
                        .await
                    {
                        tracing::warn!(target: COMP_JDOCS, bot = %esn, "write sdk ini: {err}");
                    }
                    self.state
                        .session_certs()
                        .add_to_r_info(&esn, &found.entry.name, &ip_addr);
                    self.state.tokens().remove_from_session_store(found.index);
                    tracing::debug!(
                        target: COMP_JDOCS,
                        bot = %esn,
                        "Session certificate successfully output"
                    );
                }
                tracing::info!(target: COMP_JDOCS, bot = %esn, "New bot associated, IP: {ip_addr}");
                if !matched {
                    if !is_already_known {
                        tracing::debug!(
                            target: COMP_JDOCS,
                            bot = %esn,
                            "Bot was not known to wire-pod, creating token and hash (in ReadDocs)"
                        );
                        match create_token_and_hashed_token() {
                            Ok(pair) => {
                                self.state.tokens().add_secondary(SecondaryEntry {
                                    esn: esn.clone(),
                                    target: ip_addr.clone(),
                                    guid: pair.guid.clone(),
                                    guid_hash: pair.guid_hash.clone(),
                                });
                                if let Err(err) = write_token_hash(
                                    self.state.jdocs(),
                                    &esn,
                                    &pair.guid_hash,
                                    self.state.wall().as_ref(),
                                )
                                .await
                                {
                                    tracing::error!(target: COMP_JDOCS, bot = %esn, "write token hash: {err}");
                                }
                                if !session_matched
                                    && let Err(err) = self
                                        .state
                                        .sdk_ini()
                                        .write_to_ini_secondary(&esn, &pair.guid, &ip_addr)
                                        .await
                                {
                                    tracing::warn!(target: COMP_JDOCS, bot = %esn, "write sdk ini: {err}");
                                }
                            }
                            Err(err) => {
                                tracing::error!(target: COMP_JDOCS, bot = %esn, "create token: {err}");
                            }
                        }
                        // `vic.AppToken`, not `vic.AppTokens`: Go looks up a
                        // name nothing ever writes, so this is always blank.
                        let token_jdoc = self
                            .state
                            .jdocs()
                            .get_jdoc(&req.thing, "vic.AppToken")
                            .unwrap_or_default();
                        let last = self.state.tokens().secondary_len().wrapping_sub(1);
                        self.state.tokens().remove_from_second_store(last);
                        return Ok(Response::new(ReadDocsResp {
                            items: vec![changed(to_msg(token_jdoc))],
                        }));
                    }
                    tracing::debug!(
                        target: COMP_JDOCS,
                        bot = %esn,
                        "Bot not found in any store, providing global GUID"
                    );
                    return Ok(Response::new(ReadDocsResp {
                        items: vec![changed(JdocMsg {
                            doc_version: 1,
                            fmt_version: 1,
                            client_metadata: "placeholder".to_owned(),
                            json_doc: GLOBAL_GUID_HASH.to_owned(),
                        })],
                    }));
                }
            }
        }

        let mut return_items = Vec::new();
        for item in &req.items {
            match self.state.jdocs().get_jdoc(&req.thing, &item.doc_name) {
                Some(gotten) => return_items.push(changed(to_msg(gotten))),
                None => return_items.push(changed(JdocMsg {
                    doc_version: 0,
                    fmt_version: 0,
                    client_metadata: "wirepod-noexist".to_owned(),
                    json_doc: String::new(),
                })),
            }
        }
        Ok(Response::new(ReadDocsResp {
            items: return_items,
        }))
    }

    async fn delete_doc(
        &self,
        _request: Request<DeleteDocReq>,
    ) -> Result<Response<DeleteDocResp>, Status> {
        Err(Status::unimplemented("method DeleteDoc not implemented"))
    }

    async fn purge_account_docs(
        &self,
        _request: Request<PurgeAccountDocsReq>,
    ) -> Result<Response<PurgeAccountDocsResp>, Status> {
        Err(Status::unimplemented(
            "method PurgeAccountDocs not implemented",
        ))
    }

    async fn view_account_docs(
        &self,
        _request: Request<ViewAccountDocsReq>,
    ) -> Result<Response<ViewDocsResp>, Status> {
        Err(Status::unimplemented(
            "method ViewAccountDocs not implemented",
        ))
    }

    async fn view_account_docs_with_pii(
        &self,
        _request: Request<ViewAccountDocsReq>,
    ) -> Result<Response<ViewDocsResp>, Status> {
        Err(Status::unimplemented(
            "method ViewAccountDocsWithPII not implemented",
        ))
    }
}

/// Go's `NewJdocsServer` (`server.go:198-200`).
pub fn new_jdocs_server(state: Arc<AppState>) -> JdocServer {
    JdocServer { state }
}
