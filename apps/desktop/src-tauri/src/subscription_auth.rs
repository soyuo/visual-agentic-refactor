use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

use crate::command_error::{CommandError, CommandErrorCode};
use crate::credentials::{self, OpenAiSubscriptionCredential, OpenAiSubscriptionStatus};

const AUTHORIZATION_ENDPOINT: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_ENDPOINT: &str = "https://auth.openai.com/api/accounts/oauth/token";
const OIDC_CONFIGURATION: &str = "https://auth.openai.com/.well-known/openid-configuration";
const RESOURCE: &str = "https://api.openai.com/v1";
const APP_NAME: &str = "Visual Agentic Refactor";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const CALLBACK_READ_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CALLBACK_REQUEST: usize = 16 * 1024;
const REQUIRED_SCOPES: [&str; 6] = [
    "openid",
    "profile",
    "email",
    "offline_access",
    "resource.invoke",
    "chatgpt.tokens.use.direct",
];
static TOKEN_LIFECYCLE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Default)]
pub struct SubscriptionLoginControl {
    active: Mutex<Option<Arc<AtomicBool>>>,
}

impl SubscriptionLoginControl {
    fn begin(&self) -> Result<Arc<AtomicBool>, CommandError> {
        let mut active = self.active.lock().map_err(|_| internal_error())?;
        if active.is_some() {
            return Err(CommandError::new(
                CommandErrorCode::InvalidInput,
                "OpenAI 브라우저 로그인이 이미 진행 중입니다.",
            ));
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        *active = Some(cancelled.clone());
        Ok(cancelled)
    }

    fn finish(&self) {
        if let Ok(mut active) = self.active.lock() {
            *active = None;
        }
    }

    fn cancel(&self) -> Result<(), CommandError> {
        let active = self.active.lock().map_err(|_| internal_error())?;
        if let Some(cancelled) = active.as_ref() {
            cancelled.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
}

#[tauri::command]
pub async fn sign_in_openai_subscription(
    app: AppHandle,
    control: State<'_, SubscriptionLoginControl>,
) -> Result<OpenAiSubscriptionStatus, CommandError> {
    let cancelled = control.begin()?;
    let result = sign_in(&app, cancelled).await;
    control.finish();
    result?;
    credentials::openai_subscription_status()
}

#[tauri::command]
pub fn cancel_openai_subscription_sign_in(
    control: State<'_, SubscriptionLoginControl>,
) -> Result<(), CommandError> {
    control.cancel()
}

#[tauri::command]
pub async fn disconnect_openai_subscription() -> Result<(), CommandError> {
    let _lifecycle_guard = TOKEN_LIFECYCLE_LOCK.lock().await;
    let revoke_result = revoke_session().await;
    credentials::delete_openai_subscription()?;
    revoke_result
}

#[tauri::command]
pub fn get_openai_subscription_status() -> Result<OpenAiSubscriptionStatus, CommandError> {
    credentials::openai_subscription_status()
}

async fn sign_in(app: &AppHandle, cancelled: Arc<AtomicBool>) -> Result<(), CommandError> {
    let host_id = credentials::load_or_create_openai_subscription_host_id()?;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|_| safe_auth_error("로그인 콜백을 시작하지 못했습니다."))?;
    let port = listener.local_addr().map_err(|_| internal_error())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");
    let existing = credentials::read_openai_subscription()?;
    let (client_id, id_token_hint, login_hint) = match existing.as_ref() {
        Some(credential) => (
            credential.issued_client_id.clone(),
            Some(credential.id_token.clone()),
            credential.email.clone(),
        ),
        None => ("dynamic_agent_client".to_owned(), None, None),
    };
    let pending = PendingAuthorization::new(
        client_id.clone(),
        redirect_uri.clone(),
        host_id,
        id_token_hint,
        login_hint,
    );
    let auth_url = pending.authorization_url()?;
    app.opener()
        .open_url(auth_url.as_str(), None::<&str>)
        .map_err(|_| safe_auth_error("시스템 브라우저를 열지 못했습니다."))?;

    let callback = wait_for_callback(&listener, &cancelled).await?;
    if cancelled.load(Ordering::SeqCst) {
        return Err(cancelled_error());
    }
    if callback.state != pending.state {
        return Err(safe_auth_error("로그인 콜백 상태를 검증하지 못했습니다."));
    }
    ensure_callback_approved(callback.error.as_deref())?;
    let code = callback
        .code
        .filter(|code| !code.is_empty() && code.len() <= 4096)
        .ok_or_else(|| safe_auth_error("로그인 콜백에 인증 코드가 없습니다."))?;
    let issued_client_id = match callback.client_id {
        Some(callback_client_id) if client_id == "dynamic_agent_client" => callback_client_id,
        Some(callback_client_id) if callback_client_id == client_id => client_id,
        Some(_) => return Err(safe_auth_error("로그인 계정 등록이 일치하지 않습니다.")),
        None if client_id != "dynamic_agent_client" => client_id,
        None => return Err(safe_auth_error("새 계정 등록 정보를 받지 못했습니다.")),
    };
    if !issued_client_id.starts_with("oaiapp_") {
        return Err(safe_auth_error(
            "발급된 OpenAI 계정 등록 정보를 확인하지 못했습니다.",
        ));
    }
    if let Some(existing) = existing.as_ref()
        && existing.issued_client_id != issued_client_id
    {
        return Err(safe_auth_error(
            "다른 ChatGPT 계정은 기존 계정을 연결 해제한 뒤 로그인해 주세요.",
        ));
    }

    let token = exchange_authorization_code(
        &issued_client_id,
        &code,
        &pending.verifier,
        &pending.redirect_uri,
    )
    .await?;
    if cancelled.load(Ordering::SeqCst) {
        return Err(cancelled_error());
    }
    let id_claims = validate_id_token(&token.id_token, &issued_client_id, &pending.nonce).await?;
    if existing
        .as_ref()
        .is_some_and(|saved| saved.subject != id_claims.sub)
    {
        return Err(safe_auth_error(
            "기존 ChatGPT 계정과 로그인한 계정이 다릅니다.",
        ));
    }
    if !token.token_type.eq_ignore_ascii_case("bearer") {
        return Err(safe_auth_error("지원하지 않는 인증 토큰 형식입니다."));
    }
    if token.access_token.is_empty() || token.refresh_token.is_empty() {
        return Err(safe_auth_error("갱신 가능한 인증 토큰을 받지 못했습니다."));
    }
    let scopes = token
        .scope
        .as_deref()
        .unwrap_or_default()
        .split_ascii_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    require_direct_scope(&scopes)?;
    let expires_at_unix = now_unix().saturating_add(token.expires_in.min(31_536_000));
    let credential = OpenAiSubscriptionCredential {
        issued_client_id,
        subject: id_claims.sub,
        email: id_claims.email,
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        id_token: token.id_token,
        expires_at_unix,
        scopes,
    };
    let _lifecycle_guard = TOKEN_LIFECYCLE_LOCK.lock().await;
    credentials::save_openai_subscription(&credential)
}

struct PendingAuthorization {
    client_id: String,
    redirect_uri: String,
    host_id: String,
    state: String,
    nonce: String,
    verifier: String,
    id_token_hint: Option<String>,
    login_hint: Option<String>,
}

impl PendingAuthorization {
    fn new(
        client_id: String,
        redirect_uri: String,
        host_id: String,
        id_token_hint: Option<String>,
        login_hint: Option<String>,
    ) -> Self {
        Self {
            client_id,
            redirect_uri,
            host_id,
            state: random_url_token(32),
            nonce: random_url_token(32),
            verifier: random_url_token(32),
            id_token_hint,
            login_hint,
        }
    }

    fn authorization_url(&self) -> Result<reqwest::Url, CommandError> {
        let mut url = reqwest::Url::parse(AUTHORIZATION_ENDPOINT).map_err(|_| internal_error())?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(self.verifier.as_bytes()));
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("client_id", &self.client_id);
            query.append_pair("response_type", "code");
            query.append_pair("redirect_uri", &self.redirect_uri);
            query.append_pair("scope", &REQUIRED_SCOPES.join(" "));
            query.append_pair("resource", RESOURCE);
            query.append_pair("state", &self.state);
            query.append_pair("nonce", &self.nonce);
            query.append_pair("code_challenge_method", "S256");
            query.append_pair("code_challenge", &challenge);
            query.append_pair("ext_agent_host_id", &self.host_id);
            if self.client_id == "dynamic_agent_client" {
                query.append_pair("agent_name_hint", APP_NAME);
            }
            if let Some(id_token) = &self.id_token_hint {
                query.append_pair("id_token_hint", id_token);
            }
            if let Some(email) = &self.login_hint {
                query.append_pair("login_hint", email);
            }
        }
        Ok(url)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct OAuthCallback {
    state: String,
    code: Option<String>,
    client_id: Option<String>,
    error: Option<String>,
}

async fn wait_for_callback(
    listener: &TcpListener,
    cancelled: &AtomicBool,
) -> Result<OAuthCallback, CommandError> {
    let deadline = tokio::time::Instant::now() + LOGIN_TIMEOUT;
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err(cancelled_error());
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(safe_auth_error("브라우저 로그인 시간이 초과되었습니다."));
        }
        let accepted = tokio::select! {
            biased;
            _ = wait_cancelled(cancelled) => return Err(cancelled_error()),
            result = tokio::time::timeout(remaining, listener.accept()) => result,
        }
        .map_err(|_| safe_auth_error("브라우저 로그인 시간이 초과되었습니다."))?
        .map_err(|_| safe_auth_error("로그인 콜백 연결을 받지 못했습니다."))?;
        let (mut stream, peer) = accepted;
        if !peer.ip().is_loopback() {
            respond(&mut stream, 403, "로그인 콜백 요청을 거부했습니다.").await;
            continue;
        }
        let request =
            match tokio::time::timeout(CALLBACK_READ_TIMEOUT, read_callback_request(&mut stream))
                .await
            {
                Ok(Ok(request)) => request,
                _ => {
                    respond(&mut stream, 400, "로그인 콜백 요청을 읽지 못했습니다.").await;
                    continue;
                }
            };
        let Some(callback) = parse_callback_request(&request) else {
            respond(&mut stream, 404, "로그인 콜백 주소가 올바르지 않습니다.").await;
            continue;
        };
        respond(
            &mut stream,
            200,
            "로그인이 처리되었습니다. 이 창을 닫아도 됩니다.",
        )
        .await;
        return Ok(callback);
    }
}

async fn wait_cancelled(cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn read_callback_request(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut request = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    while request.len() < MAX_CALLBACK_REQUEST {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    if request.len() >= MAX_CALLBACK_REQUEST
        || !request.windows(4).any(|window| window == b"\r\n\r\n")
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "callback request is too large or incomplete",
        ));
    }
    String::from_utf8(request)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid request"))
}

fn parse_callback_request(request: &str) -> Option<OAuthCallback> {
    let request_line = request.lines().next()?;
    let mut parts = request_line.split_ascii_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    if !target.starts_with("/auth/callback?") {
        return None;
    }
    let url = reqwest::Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    let mut values = std::collections::HashMap::new();
    for (key, value) in url.query_pairs() {
        if values
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return None;
        }
    }
    let state = values.remove("state")?;
    if state.is_empty() || state.len() > 512 {
        return None;
    }
    Some(OAuthCallback {
        state,
        code: values.remove("code"),
        client_id: values.remove("client_id"),
        error: values.remove("error"),
    })
}

async fn respond(stream: &mut TcpStream, status: u16, message: &str) {
    let body =
        format!("<!doctype html><meta charset=\"utf-8\"><title>로그인</title><p>{message}</p>");
    let response = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        if status == 200 { "OK" } else { "Bad Request" },
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    id_token: String,
    expires_in: u64,
    token_type: String,
    scope: Option<String>,
}

async fn exchange_authorization_code(
    client_id: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse, CommandError> {
    let client = secure_http_client()?;
    let response = client
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
            ("resource", RESOURCE),
        ])
        .send()
        .await
        .map_err(|_| safe_auth_error("OpenAI 로그인 토큰을 교환하지 못했습니다."))?;
    if !response.status().is_success() {
        return Err(safe_auth_error("OpenAI 로그인 토큰을 교환하지 못했습니다."));
    }
    response
        .json::<TokenResponse>()
        .await
        .map_err(|_| safe_auth_error("OpenAI 로그인 토큰 응답을 확인하지 못했습니다."))
}

#[derive(Deserialize)]
struct OidcConfiguration {
    issuer: String,
    jwks_uri: String,
    revocation_endpoint: Option<String>,
}

#[derive(Deserialize)]
struct JsonWebKeySet {
    keys: Vec<JsonWebKey>,
}

#[derive(Deserialize)]
struct JsonWebKey {
    kid: String,
    kty: String,
    alg: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

#[derive(Deserialize)]
struct IdTokenClaims {
    sub: String,
    email: Option<String>,
    nonce: String,
}

async fn validate_id_token(
    id_token: &str,
    client_id: &str,
    nonce: &str,
) -> Result<IdTokenClaims, CommandError> {
    let client = secure_http_client()?;
    let configuration = client
        .get(OIDC_CONFIGURATION)
        .send()
        .await
        .map_err(|_| safe_auth_error("OpenAI 인증 서버를 확인하지 못했습니다."))?
        .error_for_status()
        .map_err(|_| safe_auth_error("OpenAI 인증 서버를 확인하지 못했습니다."))?
        .json::<OidcConfiguration>()
        .await
        .map_err(|_| safe_auth_error("OpenAI 인증 서버 응답을 확인하지 못했습니다."))?;
    if configuration.issuer != "https://auth.openai.com"
        || !configuration
            .jwks_uri
            .starts_with("https://auth.openai.com/")
    {
        return Err(safe_auth_error(
            "OpenAI 인증 서버 주소가 올바르지 않습니다.",
        ));
    }
    let header = decode_header(id_token)
        .map_err(|_| safe_auth_error("OpenAI ID 토큰을 확인하지 못했습니다."))?;
    if header.alg != Algorithm::RS256 {
        return Err(safe_auth_error("지원하지 않는 OpenAI ID 토큰 서명입니다."));
    }
    let kid = header
        .kid
        .ok_or_else(|| safe_auth_error("OpenAI ID 토큰 서명 키를 확인하지 못했습니다."))?;
    let jwks = client
        .get(&configuration.jwks_uri)
        .send()
        .await
        .map_err(|_| safe_auth_error("OpenAI ID 토큰 서명 키를 불러오지 못했습니다."))?
        .error_for_status()
        .map_err(|_| safe_auth_error("OpenAI ID 토큰 서명 키를 불러오지 못했습니다."))?
        .json::<JsonWebKeySet>()
        .await
        .map_err(|_| safe_auth_error("OpenAI ID 토큰 서명 키를 확인하지 못했습니다."))?;
    let key = jwks
        .keys
        .into_iter()
        .find(|key| {
            key.kid == kid
                && key.kty == "RSA"
                && key.alg.as_deref().is_none_or(|alg| alg == "RS256")
        })
        .ok_or_else(|| safe_auth_error("OpenAI ID 토큰 서명 키를 찾지 못했습니다."))?;
    let decoding_key = DecodingKey::from_rsa_components(
        key.n
            .as_deref()
            .ok_or_else(|| safe_auth_error("OpenAI 서명 키가 올바르지 않습니다."))?,
        key.e
            .as_deref()
            .ok_or_else(|| safe_auth_error("OpenAI 서명 키가 올바르지 않습니다."))?,
    )
    .map_err(|_| safe_auth_error("OpenAI 서명 키를 확인하지 못했습니다."))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[configuration.issuer.as_str()]);
    validation.set_audience(&[client_id]);
    validation.set_required_spec_claims(&["iss", "aud", "exp", "sub"]);
    let claims = decode::<IdTokenClaims>(id_token, &decoding_key, &validation)
        .map_err(|_| safe_auth_error("OpenAI ID 토큰 서명이나 만료 정보를 확인하지 못했습니다."))?
        .claims;
    if claims.sub.trim().is_empty() || claims.nonce != nonce {
        return Err(safe_auth_error(
            "OpenAI ID 토큰 계정이나 nonce가 일치하지 않습니다.",
        ));
    }
    Ok(claims)
}

pub async fn access_token_for_inference() -> Result<String, CommandError> {
    let _lifecycle_guard = TOKEN_LIFECYCLE_LOCK.lock().await;
    let mut credential = credentials::read_openai_subscription()?
        .ok_or_else(|| safe_auth_error("ChatGPT 구독 로그인이 필요합니다."))?;
    require_direct_scope(&credential.scopes)?;
    if credential.expires_at_unix > now_unix().saturating_add(60) {
        return Ok(credential.access_token);
    }
    let client = secure_http_client()?;
    let response = client
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", credential.issued_client_id.as_str()),
            ("refresh_token", credential.refresh_token.as_str()),
            ("resource", RESOURCE),
        ])
        .send()
        .await
        .map_err(|_| safe_auth_error("ChatGPT 로그인이 만료되었습니다. 다시 로그인해 주세요."))?;
    if !response.status().is_success() {
        return Err(safe_auth_error(
            "ChatGPT 로그인이 만료되었습니다. 다시 로그인해 주세요.",
        ));
    }
    #[derive(Deserialize)]
    struct RefreshResponse {
        access_token: String,
        refresh_token: Option<String>,
        expires_in: u64,
        scope: Option<String>,
        token_type: String,
    }
    let refreshed = response
        .json::<RefreshResponse>()
        .await
        .map_err(|_| safe_auth_error("ChatGPT 토큰 갱신 응답을 확인하지 못했습니다."))?;
    if !refreshed.token_type.eq_ignore_ascii_case("bearer") || refreshed.access_token.is_empty() {
        return Err(safe_auth_error(
            "ChatGPT 토큰 갱신 응답이 올바르지 않습니다.",
        ));
    }
    let scopes = refreshed
        .scope
        .as_deref()
        .map(|scopes| scopes.split_ascii_whitespace().map(str::to_owned).collect())
        .unwrap_or_else(|| credential.scopes.clone());
    if require_direct_scope(&scopes).is_err() {
        return Err(safe_auth_error(
            "ChatGPT 요금제 사용 권한이 더 이상 유효하지 않습니다.",
        ));
    }
    credential.access_token = refreshed.access_token;
    credential.refresh_token = refreshed.refresh_token.unwrap_or(credential.refresh_token);
    credential.expires_at_unix = now_unix().saturating_add(refreshed.expires_in.min(31_536_000));
    credential.scopes = scopes;
    let access_token = credential.access_token.clone();
    credentials::save_openai_subscription(&credential)?;
    Ok(access_token)
}

async fn revoke_session() -> Result<(), CommandError> {
    let Some(credential) = credentials::read_openai_subscription()? else {
        return Ok(());
    };
    let client = secure_http_client()?;
    let configuration_response = client
        .get(OIDC_CONFIGURATION)
        .send()
        .await
        .map_err(|_| safe_auth_error("OpenAI 세션 해제 상태를 확인하지 못했습니다."))?
        .error_for_status()
        .map_err(|_| safe_auth_error("OpenAI 세션 해제 상태를 확인하지 못했습니다."))?;
    let configuration = configuration_response
        .json::<OidcConfiguration>()
        .await
        .map_err(|_| safe_auth_error("OpenAI 세션 해제 상태를 확인하지 못했습니다."))?;
    let endpoint = configuration
        .revocation_endpoint
        .ok_or_else(|| safe_auth_error("OpenAI 세션 해제 주소를 확인하지 못했습니다."))?;
    if !endpoint.starts_with("https://auth.openai.com/") {
        return Err(safe_auth_error(
            "OpenAI 세션 해제 주소를 확인하지 못했습니다.",
        ));
    }
    client
        .post(endpoint)
        .form(&[
            ("token", credential.refresh_token.as_str()),
            ("token_type_hint", "refresh_token"),
            ("client_id", credential.issued_client_id.as_str()),
        ])
        .send()
        .await
        .map_err(|_| safe_auth_error("OpenAI 서버 세션을 해제하지 못했습니다."))?
        .error_for_status()
        .map_err(|_| safe_auth_error("OpenAI 서버 세션을 해제하지 못했습니다."))?;
    Ok(())
}

fn require_direct_scope(scopes: &[String]) -> Result<(), CommandError> {
    if scopes
        .iter()
        .any(|scope| scope == "chatgpt.tokens.use.direct")
    {
        Ok(())
    } else {
        Err(safe_auth_error(
            "ChatGPT 요금제 사용 권한이 승인되지 않았습니다.",
        ))
    }
}

fn ensure_callback_approved(error: Option<&str>) -> Result<(), CommandError> {
    if error.is_some() {
        Err(safe_auth_error(
            "OpenAI 로그인이 승인되지 않았거나 거부되었습니다.",
        ))
    } else {
        Ok(())
    }
}

fn secure_http_client() -> Result<reqwest::Client, CommandError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|_| safe_auth_error("보안 연결을 초기화하지 못했습니다."))
}

fn random_url_token(length: usize) -> String {
    let mut bytes = vec![0; length];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn safe_auth_error(message: &str) -> CommandError {
    CommandError::new(CommandErrorCode::Internal, message)
}

fn cancelled_error() -> CommandError {
    CommandError::new(
        CommandErrorCode::Cancelled,
        "OpenAI 브라우저 로그인이 취소되었습니다.",
    )
}

fn internal_error() -> CommandError {
    CommandError::new(
        CommandErrorCode::Internal,
        "OpenAI 로그인 상태를 확인하지 못했습니다.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorization_url_uses_loopback_s256_and_direct_scope() {
        let pending = PendingAuthorization::new(
            "dynamic_agent_client".into(),
            "http://127.0.0.1:1455/auth/callback".into(),
            "urn:uuid:host".into(),
            None,
            None,
        );
        let url = pending.authorization_url().unwrap();
        let query = url
            .query_pairs()
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(url.path(), "/api/accounts/authorize");
        assert_eq!(
            query.get("redirect_uri").unwrap(),
            "http://127.0.0.1:1455/auth/callback"
        );
        assert_eq!(query.get("code_challenge_method").unwrap(), "S256");
        assert!(
            query
                .get("scope")
                .unwrap()
                .contains("chatgpt.tokens.use.direct")
        );
        assert_eq!(query.get("client_id").unwrap(), "dynamic_agent_client");
        assert_eq!(query.get("agent_name_hint").unwrap(), APP_NAME);
        assert_ne!(pending.state, pending.nonce);
    }

    #[test]
    fn callback_parser_rejects_duplicate_state_and_wrong_path() {
        assert!(parse_callback_request("GET /wrong?state=s HTTP/1.1\r\n\r\n").is_none());
        assert!(
            parse_callback_request("GET /auth/callback?state=a&state=b&code=c HTTP/1.1\r\n\r\n")
                .is_none()
        );
        assert_eq!(
            parse_callback_request(
                "GET /auth/callback?state=s&code=c&client_id=oaiapp_x HTTP/1.1\r\n\r\n"
            ),
            Some(OAuthCallback {
                state: "s".into(),
                code: Some("c".into()),
                client_id: Some("oaiapp_x".into()),
                error: None,
            })
        );
    }

    #[test]
    fn direct_inference_scope_is_required_before_persisting_or_refreshing() {
        assert!(require_direct_scope(&[]).is_err());
        assert!(require_direct_scope(&["profile".to_owned()]).is_err());
        assert!(require_direct_scope(&["chatgpt.tokens.use.direct".to_owned()]).is_ok());
    }

    #[test]
    fn callback_errors_are_parsed_without_exposing_provider_text() {
        let callback = parse_callback_request(
            "GET /auth/callback?state=s&error=access_denied&error_description=private HTTP/1.1\r\n\r\n",
        ).expect("callback parsed");
        let error = ensure_callback_approved(callback.error.as_deref()).unwrap_err();
        assert_eq!(error.code, CommandErrorCode::Internal);
        assert_eq!(
            error.message,
            "OpenAI 로그인이 승인되지 않았거나 거부되었습니다."
        );
        assert!(!error.message.contains("private"));
    }
}
