//! Go's `pkg/servers/token/token.go`: the token gRPC service and `CreateJWT`.

use std::io;
use std::sync::Arc;

use tonic::{Request, Response, Status};
use wirepod_core::logger::COMP_TOKEN;
use wirepod_core::{
    AppState, Claims, GLOBAL_GUID, PrimaryEntry, Requestor, SessionEntry, certificate_der,
    create_token_and_hashed_token, generate_token_id, host_of, issue_token, issuer_common_name,
    read_bot_info, write_bot_info, write_token_hash,
};
use wirepod_proto::tokenpb::token_server::Token;
use wirepod_proto::tokenpb::{
    AssociatePrimaryUserRequest, AssociatePrimaryUserResponse, AssociateSecondaryClientRequest,
    AssociateSecondaryClientResponse, DisassociatePrimaryUserRequest,
    DisassociatePrimaryUserResponse, ListRevokedTokensRequest, ListRevokedTokensResponse,
    ReassociatePrimaryUserRequest, ReassociatePrimaryUserResponse, RefreshTokenRequest,
    RefreshTokenResponse, RevokeFactoryCertificateRequest, RevokeFactoryCertificateResponse,
    RevokeTokensRequest, RevokeTokensResponse, TokenBundle,
};

use crate::peer::PeerAddr;

/// Go's `TokenServer` (`token.go:24-26`), carrying the state its globals were.
pub struct TokenServer {
    state: Arc<AppState>,
}

/// Go's `peer.FromContext(ctx).Addr.String()`.
///
/// The listener puts the address in the extensions; `remote_addr` is what tonic
/// itself knows, and is the fallback for a request that never went through it.
pub(crate) fn peer_address<T>(request: &Request<T>) -> String {
    request
        .extensions()
        .get::<PeerAddr>()
        .map(|peer| peer.0)
        .or_else(|| request.remote_addr())
        .map(|addr| addr.to_string())
        .unwrap_or_default()
}

/// Go's `GetEsnFromTarget` (`token.go:58-74`), which reads the file rather than
/// the in-memory copy.
///
/// # Errors
///
/// The read's, the decode's, and `bot not found` when no robot carries `target`.
pub async fn get_esn_from_target(state: &AppState, target: &str) -> io::Result<String> {
    let robot_info = read_bot_info(state.paths().data()).await?;
    for robot in &robot_info.robots {
        if target.trim() == robot.ip_address.trim() {
            return Ok(robot.esn.clone());
        }
    }
    Err(io::Error::other("bot not found"))
}

/// Go's `SetBotGUID` (`token.go:76-97`).
///
/// # Errors
///
/// `bot not found` when no robot folds to `esn`, and whatever the write reports.
pub async fn set_bot_guid(
    state: &AppState,
    esn: &str,
    guid: &str,
    // Go declares this parameter and never reads it.
    _guid_hash: &str,
) -> io::Result<()> {
    let mut bot_info = state.bot_info_snapshot();
    let mut matched = false;
    for robot in bot_info.robots.iter_mut() {
        if esn.eq_ignore_ascii_case(&robot.esn) {
            robot.guid = guid.to_owned();
            robot.activated = true;
            tracing::info!(target: COMP_TOKEN, bot = %robot.esn, "GUID and hash written");
            matched = true;
            break;
        }
    }
    if !matched {
        return Err(io::Error::other("bot not found"));
    }
    state.set_bot_info(bot_info.clone());
    write_bot_info(state.paths().data(), &bot_info).await
}

/// Go's `ChangeGUIDInIni` (`token.go:148-178`), whose load, walk, save and log
/// lines are `SdkIniStore::update_ip_and_guid`.
pub async fn change_guid_in_ini(state: &AppState, esn: &str) {
    let bot_info = state.bot_info_snapshot();
    if let Err(err) = state.sdk_ini().update_ip_and_guid(esn, &bot_info).await {
        tracing::debug!(comp = "", "{err}");
    }
}

/// Go's `CreateJWT` (`token.go:185-270`).
pub async fn create_jwt(
    state: &Arc<AppState>,
    peer_addr: &str,
    skip_guid: bool,
    is_primary: bool,
) -> TokenBundle {
    let mut skip_guid = skip_guid;
    let mut requestor = Requestor::Unknown;
    let mut client_token = GLOBAL_GUID.to_owned();
    let mut bundle = TokenBundle::default();
    let mut secondary = false;
    let mut secondary_guid = String::new();
    let mut secondary_hash = String::new();

    // Go reads the clock here, before it knows the requestor or the token id,
    // and logs both claims; the two fields are filled in below where Go assigns
    // its own locals.
    let mut claims = Claims::new(&Requestor::Unknown, String::new(), state.wall().as_ref());
    tracing::debug!(target: COMP_TOKEN, "Current time: {}", claims.iat);
    tracing::debug!(target: COMP_TOKEN, "Token expires: {}", claims.expires);

    let ip_addr = host_of(peer_addr).trim().to_owned();
    let esn = get_esn_from_target(state, &ip_addr).await;

    if let Ok(esn) = &esn
        && let Some(entry) = state.tokens().take_secondary_match(esn)
    {
        skip_guid = true;
        secondary = true;
        secondary_guid = entry.guid;
        secondary_hash = entry.guid_hash;
    }

    match &esn {
        Ok(esn) if !is_primary => {
            tracing::info!(target: COMP_TOKEN, bot = %esn, "matched target {ip_addr}");
            requestor = Requestor::Robot(esn.clone());
            if !skip_guid {
                match create_token_and_hashed_token() {
                    Ok(pair) => {
                        if let Err(err) = write_token_hash(
                            state.jdocs(),
                            esn,
                            &pair.guid_hash,
                            state.wall().as_ref(),
                        )
                        .await
                        {
                            tracing::error!(target: COMP_TOKEN, bot = %esn, "write token hash: {err}");
                        }
                        if let Err(err) =
                            set_bot_guid(state, esn, &pair.guid, &pair.guid_hash).await
                        {
                            tracing::error!(target: COMP_TOKEN, bot = %esn, "set bot guid: {err}");
                        }
                        change_guid_in_ini(state, esn).await;
                        client_token = pair.guid;
                    }
                    Err(err) => tracing::error!(target: COMP_TOKEN, "create token: {err}"),
                }
            }
        }
        _ => {
            tracing::debug!(
                target: COMP_TOKEN,
                "ESN not found in store or this is an associate primary user request, act as if this is a new robot"
            );
            if !skip_guid {
                tracing::debug!(target: COMP_TOKEN, "Adding {ip_addr} to TokenHashStore");
                match create_token_and_hashed_token() {
                    Ok(pair) => {
                        state.tokens().add_primary(PrimaryEntry {
                            target: ip_addr.clone(),
                            guid: pair.guid.clone(),
                            guid_hash: pair.guid_hash,
                        });
                        client_token = pair.guid;
                    }
                    Err(err) => tracing::error!(target: COMP_TOKEN, "create token: {err}"),
                }
            }
        }
    }
    if !skip_guid {
        bundle.client_token = client_token;
    }

    if secondary {
        let esn = esn.unwrap_or_default();
        if let Err(err) = set_bot_guid(state, &esn, &secondary_guid, &secondary_hash).await {
            tracing::error!(target: COMP_TOKEN, bot = %esn, "set bot guid: {err}");
        }
        bundle.client_token = secondary_guid.clone();
        tracing::debug!(target: COMP_TOKEN, "Secondary client: {secondary_guid}");
    }

    let request_uuid = generate_token_id().unwrap_or_default();
    tracing::debug!(target: COMP_TOKEN, "UUID for this token request: {request_uuid}");

    claims.requestor_id = requestor.id();
    claims.token_id = request_uuid;
    bundle.token = issue_token(&claims).unwrap_or_default();
    bundle
}

#[tonic::async_trait]
impl Token for TokenServer {
    async fn associate_primary_user(
        &self,
        request: Request<AssociatePrimaryUserRequest>,
    ) -> Result<Response<AssociatePrimaryUserResponse>, Status> {
        tracing::debug!(target: COMP_TOKEN, "Incoming Associate Primary User request");
        let peer_addr = peer_address(&request);
        let req = request.into_inner();
        // Go dereferences `pem.Decode`'s nil result for a request carrying no
        // certificate and takes the process down.
        let Some(der) = certificate_der(&req.session_certificate) else {
            return Err(Status::invalid_argument(
                "session certificate could not be decoded",
            ));
        };
        let name = issuer_common_name(&der).unwrap_or_default();
        self.state.tokens().add_session(SessionEntry {
            peer_addr: peer_addr.clone(),
            name,
            cert: req.session_certificate,
        });
        Ok(Response::new(AssociatePrimaryUserResponse {
            data: Some(create_jwt(&self.state, &peer_addr, false, true).await),
        }))
    }

    async fn reassociate_primary_user(
        &self,
        _request: Request<ReassociatePrimaryUserRequest>,
    ) -> Result<Response<ReassociatePrimaryUserResponse>, Status> {
        Err(Status::unimplemented(
            "method ReassociatePrimaryUser not implemented",
        ))
    }

    async fn associate_secondary_client(
        &self,
        request: Request<AssociateSecondaryClientRequest>,
    ) -> Result<Response<AssociateSecondaryClientResponse>, Status> {
        tracing::debug!(target: COMP_TOKEN, "Incoming Associate Secondary Client request");
        let peer_addr = peer_address(&request);
        Ok(Response::new(AssociateSecondaryClientResponse {
            data: Some(create_jwt(&self.state, &peer_addr, false, false).await),
        }))
    }

    async fn disassociate_primary_user(
        &self,
        _request: Request<DisassociatePrimaryUserRequest>,
    ) -> Result<Response<DisassociatePrimaryUserResponse>, Status> {
        Err(Status::unimplemented(
            "method DisassociatePrimaryUser not implemented",
        ))
    }

    async fn refresh_token(
        &self,
        request: Request<RefreshTokenRequest>,
    ) -> Result<Response<RefreshTokenResponse>, Status> {
        tracing::debug!(target: COMP_TOKEN, "Incoming Refresh Token request");
        let peer_addr = peer_address(&request);
        Ok(Response::new(RefreshTokenResponse {
            data: Some(create_jwt(&self.state, &peer_addr, false, false).await),
        }))
    }

    async fn list_revoked_tokens(
        &self,
        _request: Request<ListRevokedTokensRequest>,
    ) -> Result<Response<ListRevokedTokensResponse>, Status> {
        Err(Status::unimplemented(
            "method ListRevokedTokens not implemented",
        ))
    }

    async fn revoke_factory_certificate(
        &self,
        _request: Request<RevokeFactoryCertificateRequest>,
    ) -> Result<Response<RevokeFactoryCertificateResponse>, Status> {
        Err(Status::unimplemented(
            "method RevokeFactoryCertificate not implemented",
        ))
    }

    async fn revoke_tokens(
        &self,
        _request: Request<RevokeTokensRequest>,
    ) -> Result<Response<RevokeTokensResponse>, Status> {
        Err(Status::unimplemented("method RevokeTokens not implemented"))
    }
}

/// Go's `NewTokenServer` (`token.go:298-300`).
pub fn new_token_server(state: Arc<AppState>) -> TokenServer {
    TokenServer { state }
}
