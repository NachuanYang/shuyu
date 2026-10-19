use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io,
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{AppHandle, Manager, State};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{watch, Mutex as AsyncMutex},
};
use uuid::Uuid;
use zeroize::Zeroize;

use crate::siwc_store::{
    valid_issued_client_id, CredentialStore, RegistrationMetadata, TokenCredentials,
};

const ISSUER: &str = "https://auth.openai.com";
const OPENID_CONFIGURATION_URL: &str = "https://auth.openai.com/.well-known/openid-configuration";
const API_RESOURCE: &str = "https://api.openai.com/v1";
const CALLBACK_PATH: &str = "/auth/callback";
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_CALLBACK_BYTES: usize = 16_384;
const ID_TOKEN_FUTURE_CLOCK_SKEW_SECONDS: i64 = 60;

#[derive(PartialEq, Eq)]
struct AuthorizationCallback {
    code: String,
    client_id: String,
}

impl Drop for AuthorizationCallback {
    fn drop(&mut self) {
        self.code.zeroize();
    }
}

#[derive(Clone, Debug, Deserialize)]
struct OpenIdConfiguration {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    revocation_endpoint: Option<String>,
}

struct AuthorizationAttempt {
    state: String,
    nonce: String,
    verifier: String,
    redirect_uri: String,
}

impl Drop for AuthorizationAttempt {
    fn drop(&mut self) {
        self.state.zeroize();
        self.nonce.zeroize();
        self.verifier.zeroize();
        self.redirect_uri.zeroize();
    }
}

#[derive(Deserialize)]
struct JwtHeader {
    alg: String,
    kid: String,
    #[serde(default)]
    crit: Option<Vec<String>>,
    #[serde(default)]
    b64: Option<bool>,
}

#[derive(Deserialize)]
struct JsonWebKeySet {
    keys: Vec<JsonWebKey>,
}

#[derive(Deserialize)]
struct JsonWebKey {
    kid: String,
    kty: String,
    #[serde(default)]
    alg: Option<String>,
    #[serde(rename = "use", default)]
    public_key_use: Option<String>,
    #[serde(default)]
    key_ops: Vec<String>,
    #[serde(default)]
    crv: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
}

#[derive(Deserialize)]
struct IdentityClaims {
    iss: String,
    sub: String,
    aud: Value,
    exp: i64,
    iat: i64,
    #[serde(default)]
    azp: Option<String>,
    nonce: Option<String>,
    #[serde(default)]
    nbf: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatGptConnectionStatus {
    connected: bool,
    plan_usage_authorized: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatGptDisconnectResult {
    local_credentials_removed: bool,
    remote_revocation_confirmed: bool,
    message: String,
}

#[derive(Default)]
pub(crate) struct ChatGptAuthManager {
    active: Mutex<Option<watch::Sender<bool>>>,
    refresh_lock: AsyncMutex<()>,
}

impl ChatGptAuthManager {
    fn begin(&self) -> Result<watch::Receiver<bool>, String> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| "ChatGPT授权状态暂时不可用，请重启书隅后重试。".to_string())?;
        if active.is_some() {
            return Err("已有一个ChatGPT授权流程正在进行，请先完成或取消该流程。".into());
        }
        let (sender, receiver) = watch::channel(false);
        *active = Some(sender);
        Ok(receiver)
    }

    fn finish(&self) {
        if let Ok(mut active) = self.active.lock() {
            *active = None;
        }
    }

    fn cancel(&self) -> Result<bool, String> {
        let active = self
            .active
            .lock()
            .map_err(|_| "ChatGPT授权状态暂时不可用，请重启书隅后重试。".to_string())?;
        Ok(active
            .as_ref()
            .is_some_and(|sender| sender.send(true).is_ok()))
    }
}

pub fn granted_scope_contains_direct_plan_usage(scopes: &str) -> bool {
    scopes
        .split_ascii_whitespace()
        .any(|scope| scope == "chatgpt.tokens.use.direct")
}

fn pkce_s256_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn build_authorization_url(
    client_id: &str,
    first_registration: bool,
    host_id: &str,
    redirect_uri: &str,
    state: &str,
    nonce: &str,
    code_challenge: &str,
) -> Result<Url, String> {
    if client_id.trim().is_empty()
        || client_id.len() > 512
        || (first_registration && client_id != "dynamic_agent_client")
        || (!first_registration && client_id == "dynamic_agent_client")
        || (!first_registration && !valid_issued_client_id(client_id))
        || host_id.trim().is_empty()
        || host_id.len() > 256
        || state.len() < 16
        || nonce.len() < 16
        || code_challenge.len() != 43
    {
        return Err("ChatGPT授权参数无效。".into());
    }
    let redirect = Url::parse(redirect_uri).map_err(|_| "ChatGPT授权回调地址无效。".to_string())?;
    if redirect.scheme() != "http"
        || redirect.host_str() != Some("127.0.0.1")
        || redirect.path() != "/auth/callback"
        || redirect.port().is_none()
        || redirect.username() != ""
        || redirect.password().is_some()
        || redirect.query().is_some()
        || redirect.fragment().is_some()
    {
        return Err("ChatGPT授权回调必须绑定本机127.0.0.1地址。".into());
    }
    let mut url = Url::parse("https://auth.openai.com/api/accounts/authorize")
        .map_err(|_| "无法创建ChatGPT授权地址。".to_string())?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("client_id", client_id)
            .append_pair("ext_agent_host_id", host_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect.as_str())
            .append_pair(
                "scope",
                "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
            )
            .append_pair("resource", "https://api.openai.com/v1")
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", code_challenge);
        if first_registration {
            query.append_pair("agent_name_hint", "书隅");
        }
    }
    Ok(url)
}

fn parse_authorization_callback(
    query: &str,
    expected_state: &str,
    issued_client_id: Option<&str>,
) -> Result<AuthorizationCallback, String> {
    if query.len() > 16_384 || expected_state.is_empty() {
        return Err("ChatGPT授权回调内容无效或过长。".into());
    }
    let callback_url = Url::parse(&format!("http://127.0.0.1/?{query}"))
        .map_err(|_| "无法解析ChatGPT授权回调。".to_string())?;
    let mut code = None;
    let mut state = None;
    let mut returned_client_id = None;
    let mut oauth_error = None;
    for (key, value) in callback_url.query_pairs() {
        let value = value.into_owned();
        let target = match key.as_ref() {
            "code" => Some(&mut code),
            "state" => Some(&mut state),
            "client_id" => Some(&mut returned_client_id),
            "error" => Some(&mut oauth_error),
            _ => None,
        };
        if let Some(target) = target {
            if target.replace(value).is_some() {
                return Err("ChatGPT授权回调包含重复参数。".into());
            }
        }
    }
    if state.as_deref() != Some(expected_state) {
        return Err("ChatGPT授权校验失败，请重新发起连接。".into());
    }
    if oauth_error.is_some() {
        return Err("ChatGPT授权未完成，请在浏览器中同意授权后重试。".into());
    }
    let code = code
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "ChatGPT授权回调缺少授权码。".to_string())?;
    if code.len() > 4096 || code.chars().any(char::is_whitespace) {
        return Err("ChatGPT返回的授权码格式无效。".into());
    }
    let client_id = match (issued_client_id, returned_client_id) {
        (Some(saved), Some(returned)) if saved != returned => {
            return Err("ChatGPT返回的客户端注册与当前账号不一致。".into());
        }
        (Some(saved), _) => saved.to_owned(),
        (None, Some(returned)) if valid_issued_client_id(&returned) => returned,
        (None, _) => return Err("首次ChatGPT注册未返回客户端编号。".into()),
    };
    Ok(AuthorizationCallback { code, client_id })
}

fn validate_discovery_document(value: &Value) -> Result<OpenIdConfiguration, String> {
    let configuration: OpenIdConfiguration = serde_json::from_value(value.clone())
        .map_err(|_| "OpenAI账号服务返回的OIDC发现文档格式无效。".to_string())?;
    if configuration.issuer != ISSUER
        || configuration.authorization_endpoint != "https://auth.openai.com/api/accounts/authorize"
        || configuration.token_endpoint != "https://auth.openai.com/api/accounts/oauth/token"
        || configuration.jwks_uri != "https://auth.openai.com/.well-known/jwks.json"
    {
        return Err("OpenAI账号服务的OIDC发现文档端点不符合已固定的官方地址。".into());
    }
    if let Some(endpoint) = &configuration.revocation_endpoint {
        let url =
            Url::parse(endpoint).map_err(|_| "OpenAI账号服务的撤销端点格式无效。".to_string())?;
        if url.scheme() != "https"
            || url.host_str() != Some("auth.openai.com")
            || url.port().is_some()
            || url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.path().starts_with("/api/accounts/oauth/")
        {
            return Err("OpenAI账号服务的撤销端点不在受信任的官方授权域名内。".into());
        }
    }
    Ok(configuration)
}

fn parse_token_response(
    response: &Value,
    previous: Option<&TokenCredentials>,
    now_unix: i64,
    saved_at: &str,
) -> Result<TokenCredentials, String> {
    let access_token = response
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "OpenAI授权响应缺少access token。".to_string())?
        .to_owned();
    let refresh_token = response
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| previous.map(|tokens| tokens.refresh_token.clone()))
        .ok_or_else(|| "OpenAI授权响应缺少refresh token，无法安全保存可续期连接。".to_string())?;
    let id_token = response
        .get("id_token")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| previous.map(|tokens| tokens.id_token.clone()))
        .ok_or_else(|| "OpenAI授权响应缺少ID token，无法验证账号身份。".to_string())?;
    let expires_in = response
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|seconds| *seconds > 0)
        .ok_or_else(|| "OpenAI授权响应缺少有效的令牌有效期。".to_string())?;
    let expires_at_unix = now_unix
        .checked_add(expires_in)
        .ok_or_else(|| "OpenAI授权响应中的令牌有效期超出支持范围。".to_string())?;
    let token_type = response
        .get("token_type")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| previous.map(|tokens| tokens.token_type.clone()))
        .unwrap_or_else(|| "Bearer".into());
    if !token_type.eq_ignore_ascii_case("bearer") {
        return Err("OpenAI授权返回了不支持的令牌类型。".into());
    }
    let scopes = response
        .get("scope")
        .and_then(Value::as_str)
        .map(|scope| scope.split_ascii_whitespace().map(str::to_owned).collect())
        .or_else(|| previous.map(|tokens| tokens.scopes.clone()))
        .unwrap_or_default();
    Ok(TokenCredentials {
        id_token,
        access_token,
        refresh_token,
        expires_at_unix,
        token_type,
        scopes,
        saved_at: saved_at.to_owned(),
    })
}

fn classify_oauth_error(status: u16, body: &str) -> String {
    let code = serde_json::from_str::<Value>(body).ok().and_then(|value| {
        value
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    match code.as_deref() {
        Some("invalid_grant")
        | Some("invalid_refresh_token")
        | Some("token_expired")
        | Some("refresh_token_expired")
        | Some("refresh_token_invalidated")
        | Some("refresh_token_reused") => {
            "ChatGPT授权已过期、撤销或已被替换，请重新连接账号。".into()
        }
        Some("invalid_client") => {
            "OpenAI拒绝了此客户端注册，请保留错误状态并检查本机注册信息。".into()
        }
        Some("access_denied") => "你取消或拒绝了ChatGPT授权；尚未启用plan使用。".into(),
        _ if status == 429 => "OpenAI授权服务暂时限制了请求频率，请稍后重试。".into(),
        _ if status >= 500 => {
            "OpenAI授权服务暂时不可用，请稍后重试；本地已有连接凭据已保留。".into()
        }
        _ => format!("OpenAI授权请求失败（HTTP {status}）；请重试连接，未保存新凭据。"),
    }
}

fn validate_id_token_with_jwks(
    token: &str,
    jwks_value: &Value,
    audience: &str,
    expected_nonce: Option<&str>,
    now_unix: i64,
) -> Result<String, String> {
    if token.len() > 64 * 1024 {
        return Err("OpenAI返回的ID token超过安全校验限制。".into());
    }
    let mut parts = token.split('.');
    let encoded_header = parts.next().unwrap_or_default();
    let encoded_claims = parts.next().unwrap_or_default();
    let encoded_signature = parts.next().unwrap_or_default();
    if encoded_header.is_empty()
        || encoded_claims.is_empty()
        || encoded_signature.is_empty()
        || parts.next().is_some()
    {
        return Err("OpenAI返回的ID token结构无效。".into());
    }
    let header_bytes = URL_SAFE_NO_PAD
        .decode(encoded_header)
        .map_err(|_| "OpenAI返回的ID token签名头格式无效。".to_string())?;
    let header: JwtHeader = serde_json::from_slice(&header_bytes)
        .map_err(|_| "OpenAI返回的ID token签名头无法解析。".to_string())?;
    if header.kid.is_empty() || header.kid.len() > 256 {
        return Err("OpenAI返回的ID token缺少有效签名密钥编号。".into());
    }
    if header.crit.is_some() || header.b64 == Some(false) {
        return Err("OpenAI返回的ID token包含书隅无法安全处理的扩展签名参数。".into());
    }
    let jwks: JsonWebKeySet = serde_json::from_value(jwks_value.clone())
        .map_err(|_| "OpenAI公布的签名密钥列表格式无效。".to_string())?;
    if jwks.keys.len() > 64 {
        return Err("OpenAI公布的签名密钥列表超出安全校验限制。".into());
    }
    let key = jwks
        .keys
        .iter()
        .find(|key| key.kid == header.kid)
        .ok_or_else(|| "ID token使用的签名密钥不在OpenAI官方密钥列表中。".to_string())?;
    if key
        .public_key_use
        .as_deref()
        .is_some_and(|usage| usage != "sig")
        || (!key.key_ops.is_empty() && !key.key_ops.iter().any(|operation| operation == "verify"))
        || key
            .alg
            .as_deref()
            .is_some_and(|algorithm| algorithm != header.alg)
    {
        return Err("ID token对应的OpenAI公钥用途或签名算法不匹配。".into());
    }
    let signature = URL_SAFE_NO_PAD
        .decode(encoded_signature)
        .map_err(|_| "OpenAI返回的ID token签名格式无效。".to_string())?;
    let signing_input = format!("{encoded_header}.{encoded_claims}");
    verify_jwt_signature(key, &header.alg, signing_input.as_bytes(), &signature)?;

    let claims_bytes = URL_SAFE_NO_PAD
        .decode(encoded_claims)
        .map_err(|_| "OpenAI返回的ID token声明格式无效。".to_string())?;
    let claims: IdentityClaims = serde_json::from_slice(&claims_bytes)
        .map_err(|_| "OpenAI返回的ID token声明无法解析。".to_string())?;
    let latest_accepted_claim_timestamp =
        now_unix.saturating_add(ID_TOKEN_FUTURE_CLOCK_SKEW_SECONDS);
    let audience_matches = match &claims.aud {
        Value::String(value) => value == audience,
        Value::Array(values) => values.iter().any(|value| value.as_str() == Some(audience)),
        _ => false,
    };
    if claims.iss != ISSUER {
        return Err("ID token签发方与OpenAI官方账号服务不匹配。".into());
    }
    if !audience_matches {
        return Err("ID token不是签发给当前ChatGPT客户端注册的。".into());
    }
    if let Value::Array(audiences) = &claims.aud {
        if audiences.len() > 1 && claims.azp.as_deref() != Some(audience) {
            return Err("ID token的授权方与当前ChatGPT客户端注册不匹配。".into());
        }
    }
    if claims.exp <= now_unix.saturating_sub(5) {
        return Err("OpenAI返回的ID token已过期，请重新连接账号。".into());
    }
    if claims.iat > latest_accepted_claim_timestamp || claims.iat > claims.exp {
        return Err("OpenAI返回的ID token签发时间无效。".into());
    }
    if claims
        .nbf
        .is_some_and(|not_before| not_before > latest_accepted_claim_timestamp)
    {
        return Err("OpenAI返回的ID token尚未生效，请检查系统时间后重试。".into());
    }
    if claims.sub.trim().is_empty() || claims.sub.len() > 1024 {
        return Err("OpenAI返回的ID token没有有效的账号主体标识。".into());
    }
    if expected_nonce.is_some_and(|nonce| claims.nonce.as_deref() != Some(nonce)) {
        return Err("ChatGPT授权nonce校验失败，请重新发起连接。".into());
    }
    Ok(claims.sub)
}

fn verify_jwt_signature(
    key: &JsonWebKey,
    algorithm: &str,
    message: &[u8],
    signature: &[u8],
) -> Result<(), String> {
    use ring::signature;
    let invalid = || "OpenAI返回的ID token签名校验失败。".to_string();
    match (algorithm, key.kty.as_str()) {
        ("RS256", "RSA")
        | ("RS384", "RSA")
        | ("RS512", "RSA")
        | ("PS256", "RSA")
        | ("PS384", "RSA")
        | ("PS512", "RSA") => {
            let modulus = key.n.as_deref().ok_or_else(invalid)?;
            let exponent = key.e.as_deref().ok_or_else(invalid)?;
            let modulus = URL_SAFE_NO_PAD.decode(modulus).map_err(|_| invalid())?;
            let exponent = URL_SAFE_NO_PAD.decode(exponent).map_err(|_| invalid())?;
            let verifier = signature::RsaPublicKeyComponents {
                n: &modulus,
                e: &exponent,
            };
            let parameters = match algorithm {
                "RS256" => &signature::RSA_PKCS1_2048_8192_SHA256,
                "RS384" => &signature::RSA_PKCS1_2048_8192_SHA384,
                "RS512" => &signature::RSA_PKCS1_2048_8192_SHA512,
                "PS256" => &signature::RSA_PSS_2048_8192_SHA256,
                "PS384" => &signature::RSA_PSS_2048_8192_SHA384,
                "PS512" => &signature::RSA_PSS_2048_8192_SHA512,
                _ => return Err(invalid()),
            };
            verifier
                .verify(parameters, message, signature)
                .map_err(|_| invalid())
        }
        ("ES256", "EC") if key.crv.as_deref() == Some("P-256") => {
            let x = URL_SAFE_NO_PAD
                .decode(key.x.as_deref().ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            let y = URL_SAFE_NO_PAD
                .decode(key.y.as_deref().ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            if x.len() != 32 || y.len() != 32 {
                return Err(invalid());
            }
            let mut public_key = Vec::with_capacity(65);
            public_key.push(4);
            public_key.extend_from_slice(&x);
            public_key.extend_from_slice(&y);
            signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, public_key)
                .verify(message, signature)
                .map_err(|_| invalid())
        }
        ("ES384", "EC") if key.crv.as_deref() == Some("P-384") => {
            let x = URL_SAFE_NO_PAD
                .decode(key.x.as_deref().ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            let y = URL_SAFE_NO_PAD
                .decode(key.y.as_deref().ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            if x.len() != 48 || y.len() != 48 {
                return Err(invalid());
            }
            let mut public_key = Vec::with_capacity(97);
            public_key.push(4);
            public_key.extend_from_slice(&x);
            public_key.extend_from_slice(&y);
            signature::UnparsedPublicKey::new(&signature::ECDSA_P384_SHA384_FIXED, public_key)
                .verify(message, signature)
                .map_err(|_| invalid())
        }
        ("EdDSA", "OKP") if key.crv.as_deref() == Some("Ed25519") => {
            let public_key = URL_SAFE_NO_PAD
                .decode(key.x.as_deref().ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            if public_key.len() != 32 {
                return Err(invalid());
            }
            signature::UnparsedPublicKey::new(&signature::ED25519, public_key)
                .verify(message, signature)
                .map_err(|_| invalid())
        }
        _ => Err("OpenAI返回的ID token签名算法或公钥类型不受支持。".into()),
    }
}

fn build_authorization_attempt(redirect_uri: String) -> AuthorizationAttempt {
    let state = Uuid::new_v4().to_string() + &Uuid::new_v4().to_string();
    let nonce = Uuid::new_v4().to_string() + &Uuid::new_v4().to_string();
    let mut random = [0u8; 32];
    random[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    random[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    AuthorizationAttempt {
        state,
        nonce,
        verifier: URL_SAFE_NO_PAD.encode(random),
        redirect_uri,
    }
}

fn http_client(timeout: Duration) -> Result<Client, String> {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "无法初始化OpenAI安全网络连接。".to_string())
}

fn encode_form(fields: &[(&str, &str)]) -> Result<String, String> {
    let mut container = Url::parse("https://auth.openai.com/")
        .map_err(|_| "无法初始化OpenAI授权表单编码器。".to_string())?;
    {
        let mut query = container.query_pairs_mut();
        for (key, value) in fields {
            query.append_pair(key, value);
        }
    }
    container
        .query()
        .map(str::to_owned)
        .ok_or_else(|| "无法编码OpenAI授权表单。".into())
}

async fn fetch_openid_configuration(client: &Client) -> Result<OpenIdConfiguration, String> {
    let response = client
        .get(OPENID_CONFIGURATION_URL)
        .send()
        .await
        .map_err(|error| format!("无法连接OpenAI账号服务以读取OIDC配置：{error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "OpenAI账号服务OIDC配置请求失败（HTTP {}）。",
            response.status()
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "读取OpenAI账号服务OIDC配置失败。".to_string())?;
    if bytes.len() > 64 * 1024 {
        return Err("OpenAI账号服务OIDC配置超过安全大小限制。".into());
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|_| "OpenAI账号服务OIDC配置不是有效JSON。".to_string())?;
    validate_discovery_document(&value)
}

async fn fetch_jwks(client: &Client, configuration: &OpenIdConfiguration) -> Result<Value, String> {
    let response = client
        .get(&configuration.jwks_uri)
        .send()
        .await
        .map_err(|error| format!("无法连接OpenAI官方签名密钥服务：{error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "OpenAI官方签名密钥服务请求失败（HTTP {}）。",
            response.status()
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "读取OpenAI官方签名密钥失败。".to_string())?;
    if bytes.len() > 256 * 1024 {
        return Err("OpenAI官方签名密钥列表超过安全大小限制。".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "OpenAI官方签名密钥列表不是有效JSON。".into())
}

async fn exchange_authorization_code(
    client: &Client,
    configuration: &OpenIdConfiguration,
    callback: &AuthorizationCallback,
    attempt: &AuthorizationAttempt,
) -> Result<Value, String> {
    let form = encode_form(&[
        ("grant_type", "authorization_code"),
        ("client_id", callback.client_id.as_str()),
        ("code", callback.code.as_str()),
        ("code_verifier", attempt.verifier.as_str()),
        ("redirect_uri", attempt.redirect_uri.as_str()),
        ("resource", API_RESOURCE),
    ])?;
    let response = client
        .post(&configuration.token_endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .await
        .map_err(|error| format!("连接OpenAI令牌服务失败：{error}"))?;
    let status = response.status();
    let mut body = response
        .bytes()
        .await
        .map_err(|_| "读取OpenAI令牌服务响应失败；未保存新凭据。".to_string())?
        .to_vec();
    if body.len() > 64 * 1024 {
        body.zeroize();
        return Err("OpenAI令牌服务响应超过安全大小限制；未保存新凭据。".into());
    }
    if !status.is_success() {
        let message = classify_oauth_error(
            status.as_u16(),
            std::str::from_utf8(&body).unwrap_or_default(),
        );
        body.zeroize();
        return Err(message);
    }
    let parsed = serde_json::from_slice(&body)
        .map_err(|_| "OpenAI令牌服务返回了无法识别的授权数据；未保存新凭据。".to_string());
    body.zeroize();
    parsed
}

async fn wait_for_authorization_callback(
    listener: TcpListener,
    attempt: &AuthorizationAttempt,
    saved_client_id: Option<&str>,
    cancel: &mut watch::Receiver<bool>,
) -> Result<AuthorizationCallback, String> {
    let deadline = tokio::time::sleep(CALLBACK_TIMEOUT);
    tokio::pin!(deadline);
    let mut rejected_callbacks = 0u8;
    loop {
        let (mut stream, _) = tokio::select! {
            _ = &mut deadline => return Err("等待ChatGPT浏览器授权超过3分钟，请重新点击连接。".into()),
            changed = cancel.changed() => {
                if changed.is_ok() && *cancel.borrow() {
                    return Err("你已取消ChatGPT授权流程。".into());
                }
                continue;
            }
            accepted = listener.accept() => accepted.map_err(|error| format!("无法接收ChatGPT本机授权回调：{error}"))?,
        };
        let mut request =
            match tokio::time::timeout(Duration::from_secs(10), read_callback_request(&mut stream))
                .await
            {
                Ok(Ok(request)) => request,
                _ => {
                    let _ = write_callback_response(&mut stream, false).await;
                    rejected_callbacks = rejected_callbacks.saturating_add(1);
                    if rejected_callbacks >= 8 {
                        return Err("收到过多无效本机授权回调，请重新发起连接。".into());
                    }
                    continue;
                }
            };
        let Some(mut query) = callback_query(&request) else {
            request.zeroize();
            let _ = write_callback_response(&mut stream, false).await;
            rejected_callbacks = rejected_callbacks.saturating_add(1);
            if rejected_callbacks >= 8 {
                return Err("收到过多无效本机授权回调，请重新发起连接。".into());
            }
            continue;
        };
        let parsed = parse_authorization_callback(&query, &attempt.state, saved_client_id);
        query.zeroize();
        request.zeroize();
        match parsed {
            Ok(callback) => {
                let _ = write_callback_response(&mut stream, true).await;
                return Ok(callback);
            }
            Err(error) if error.contains("授权校验失败") || error.contains("重复参数") => {
                let _ = write_callback_response(&mut stream, false).await;
                rejected_callbacks = rejected_callbacks.saturating_add(1);
                if rejected_callbacks >= 8 {
                    return Err("收到过多无效本机授权回调，请重新发起连接。".into());
                }
            }
            Err(error) => {
                let _ = write_callback_response(&mut stream, false).await;
                return Err(error);
            }
        }
    }
}

async fn read_callback_request(stream: &mut TcpStream) -> io::Result<String> {
    let mut request = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while request.len() < MAX_CALLBACK_BYTES {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    if request.len() >= MAX_CALLBACK_BYTES
        || !request.windows(4).any(|window| window == b"\r\n\r\n")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "callback headers too large or incomplete",
        ));
    }
    String::from_utf8(request)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "callback request is not UTF-8"))
}

fn callback_query(request: &str) -> Option<String> {
    let request_line = request.lines().next()?;
    let mut parts = request_line.split_ascii_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    if parts.next()?.starts_with("HTTP/") == false {
        return None;
    }
    let url = Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    if url.path() != CALLBACK_PATH {
        return None;
    }
    Some(url.query()?.to_owned())
}

async fn write_callback_response(stream: &mut TcpStream, success: bool) -> io::Result<()> {
    let (status, body) = if success {
        ("200 OK", "授权已返回书隅，你可以关闭此页面并回到应用。")
    } else {
        (
            "400 Bad Request",
            "授权回调未通过校验，请回到书隅重新发起连接。",
        )
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.as_bytes().len()
    );
    stream.write_all(response.as_bytes()).await
}

#[cfg(windows)]
fn open_system_browser(url: &Url) -> Result<(), String> {
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    let operation: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
    let url: Vec<u16> = url.as_str().encode_utf16().chain(Some(0)).collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if (result as isize) <= 32 {
        Err("无法打开系统浏览器；请检查Windows默认浏览器设置后重试。".into())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn open_system_browser(_url: &Url) -> Result<(), String> {
    Err("ChatGPT本机授权流程目前仅支持Windows桌面客户端。".into())
}

async fn connect_with_chatgpt(
    store: &CredentialStore,
    cancel: &mut watch::Receiver<bool>,
) -> Result<ChatGptConnectionStatus, String> {
    let host_id = store.load_or_create_host_id()?;
    let existing = store.load_registration()?;
    let first_registration = existing.is_none();
    let requested_client_id = existing
        .as_ref()
        .map(|registration| registration.client_id.as_str())
        .unwrap_or("dynamic_agent_client");

    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|error| format!("无法在本机127.0.0.1启动ChatGPT授权回调：{error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("无法读取本机ChatGPT授权回调端口：{error}"))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
    let attempt = build_authorization_attempt(redirect_uri);
    let challenge = pkce_s256_challenge(&attempt.verifier);
    let authorization_url = build_authorization_url(
        requested_client_id,
        first_registration,
        &host_id,
        &attempt.redirect_uri,
        &attempt.state,
        &attempt.nonce,
        &challenge,
    )?;
    open_system_browser(&authorization_url)?;
    let callback = wait_for_authorization_callback(
        listener,
        &attempt,
        existing
            .as_ref()
            .map(|registration| registration.client_id.as_str()),
        cancel,
    )
    .await?;
    let client = http_client(Duration::from_secs(30))?;
    let configuration = fetch_openid_configuration(&client).await?;
    let mut response =
        exchange_authorization_code(&client, &configuration, &callback, &attempt).await?;
    let saved_at = chrono::Utc::now().to_rfc3339();
    let mut tokens =
        parse_token_response(&response, None, chrono::Utc::now().timestamp(), &saved_at)?;
    clear_token_response(&mut response);
    let jwks = fetch_jwks(&client, &configuration).await?;
    let subject = validate_id_token_with_jwks(
        &tokens.id_token,
        &jwks,
        &callback.client_id,
        Some(&attempt.nonce),
        chrono::Utc::now().timestamp(),
    )?;
    if existing
        .as_ref()
        .is_some_and(|registration| registration.subject != subject)
    {
        return Err(
            "ChatGPT返回了不同账号的身份信息；为保护原连接，本次授权未覆盖已保存的账号。".into(),
        );
    }
    let registration = RegistrationMetadata {
        issuer: ISSUER.into(),
        subject,
        client_id: callback.client_id.clone(),
        ext_agent_host_id: host_id,
    };
    store.save_account(&registration, &tokens)?;
    let plan_usage_authorized = tokens.allows_plan_usage();
    tokens.access_token.zeroize();
    Ok(ChatGptConnectionStatus {
        connected: true,
        plan_usage_authorized,
    })
}

fn clear_token_response(response: &mut Value) {
    for name in ["id_token", "access_token", "refresh_token"] {
        if let Some(Value::String(value)) = response.get_mut(name) {
            value.zeroize();
        }
    }
}

pub(crate) fn store_for_app(app: &AppHandle) -> Result<CredentialStore, String> {
    let app_data = app
        .path()
        .app_data_dir()
        .map_err(|error| format!("无法定位书隅本地账号数据目录：{error}"))?;
    Ok(CredentialStore::new(app_data.join("chatgpt")))
}

#[tauri::command]
pub(crate) fn open_chatgpt_usage() -> Result<(), String> {
    let url = Url::parse("https://chatgpt.com/settings/usage")
        .map_err(|_| "ChatGPT用量管理地址无效。".to_string())?;
    open_system_browser(&url)
}

#[tauri::command]
pub(crate) fn open_wikipedia_article(url: String) -> Result<(), String> {
    let parsed = Url::parse(&url).map_err(|_| "维基百科链接格式无效。".to_string())?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("zh.wikipedia.org")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
    {
        return Err("只允许打开维基百科的安全链接。".into());
    }
    open_system_browser(&parsed)
}

#[tauri::command]
pub(crate) async fn connect_chatgpt(
    app: AppHandle,
    manager: State<'_, Arc<ChatGptAuthManager>>,
) -> Result<ChatGptConnectionStatus, String> {
    let manager = Arc::clone(manager.inner());
    let mut cancel = manager.begin()?;
    let result = match store_for_app(&app) {
        Ok(store) => connect_with_chatgpt(&store, &mut cancel).await,
        Err(error) => Err(error),
    };
    manager.finish();
    result
}

#[tauri::command]
pub(crate) fn cancel_chatgpt_sign_in(
    manager: State<'_, Arc<ChatGptAuthManager>>,
) -> Result<bool, String> {
    manager.cancel()
}

#[tauri::command]
pub(crate) fn get_chatgpt_connection_status(
    app: AppHandle,
) -> Result<ChatGptConnectionStatus, String> {
    let store = store_for_app(&app)?;
    let registration = store.load_registration()?;
    let tokens = store.load_tokens()?;
    let connected = registration.is_some() && tokens.is_some();
    let plan_usage_authorized = connected
        && tokens
            .as_ref()
            .is_some_and(TokenCredentials::allows_plan_usage);
    Ok(ChatGptConnectionStatus {
        connected,
        plan_usage_authorized,
    })
}

async fn revoke_tokens(store: &CredentialStore, tokens: &TokenCredentials) -> bool {
    let Ok(client) = http_client(Duration::from_secs(12)) else {
        return false;
    };
    let Ok(configuration) = fetch_openid_configuration(&client).await else {
        return false;
    };
    let Some(revocation_endpoint) = configuration.revocation_endpoint else {
        return false;
    };
    let Ok(Some(registration)) = store.load_registration() else {
        return false;
    };
    let Ok(form) = encode_form(&[
        ("token", tokens.refresh_token.as_str()),
        ("token_type_hint", "refresh_token"),
        ("client_id", registration.client_id.as_str()),
    ]) else {
        return false;
    };
    let response = client
        .post(revocation_endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .await;
    response.is_ok_and(|response| response.status().as_u16() == 200)
}

async fn revoke_and_remove(
    app: &AppHandle,
    forget: bool,
) -> Result<ChatGptDisconnectResult, String> {
    let store = store_for_app(app)?;
    let _refresh_lock = store.acquire_refresh_lock().await?;
    let tokens = store.load_tokens()?;
    let remote_revocation_confirmed = match tokens.as_ref() {
        Some(tokens) => revoke_tokens(&store, tokens).await,
        None => true,
    };
    remove_local_credentials(&store, forget, remote_revocation_confirmed)
}

fn remove_local_credentials(
    store: &CredentialStore,
    forget: bool,
    remote_revocation_confirmed: bool,
) -> Result<ChatGptDisconnectResult, String> {
    if forget {
        store.forget_account()?;
    } else {
        store.disconnect()?;
    }
    let message = match (forget, remote_revocation_confirmed) {
        (true, true) => "已退出登录，本机令牌、注册信息和设备标识已删除。",
        (true, false) => "已退出登录，本机令牌、注册信息和设备标识已删除，但OpenAI未确认远端撤销；可在ChatGPT设置中检查或撤销书隅连接。",
        (false, true) => "已断开ChatGPT账号，本机令牌已删除并保留加密注册信息，之后可复用客户端注册重连。",
        (false, false) => "已断开ChatGPT账号并删除本机令牌，但OpenAI未确认远端撤销；可在ChatGPT设置中检查或撤销书隅连接。",
    };
    Ok(ChatGptDisconnectResult {
        local_credentials_removed: true,
        remote_revocation_confirmed,
        message: message.into(),
    })
}

#[tauri::command]
pub(crate) async fn disconnect_chatgpt(
    app: AppHandle,
    requests: State<'_, Arc<crate::ai_service::AiRequestManager>>,
) -> Result<ChatGptDisconnectResult, String> {
    requests.cancel_all();
    revoke_and_remove(&app, false).await
}

#[tauri::command]
pub(crate) async fn forget_chatgpt_registration(
    app: AppHandle,
    requests: State<'_, Arc<crate::ai_service::AiRequestManager>>,
) -> Result<ChatGptDisconnectResult, String> {
    requests.cancel_all();
    revoke_and_remove(&app, true).await
}

impl ChatGptAuthManager {
    pub(crate) async fn access_token(&self, store: &CredentialStore) -> Result<String, String> {
        let _refresh_guard = self.refresh_lock.lock().await;
        let _cross_process_refresh_guard = store.acquire_refresh_lock().await?;
        let registration = store
            .load_registration()?
            .ok_or_else(|| "尚未连接ChatGPT账号，请先在设置中连接。".to_string())?;
        let mut previous = store
            .load_tokens()?
            .ok_or_else(|| "ChatGPT令牌不存在，请重新连接账号。".to_string())?;
        if !previous.allows_plan_usage() {
            return Err(
                "当前ChatGPT账号没有授予plan直接使用权限，请重新连接并同意相应授权。".into(),
            );
        }
        let now = chrono::Utc::now().timestamp();
        if previous.expires_at_unix > now.saturating_add(60) {
            return Ok(previous.access_token.clone());
        }
        let client = http_client(Duration::from_secs(30))?;
        let configuration = fetch_openid_configuration(&client).await?;
        let form = encode_form(&[
            ("grant_type", "refresh_token"),
            ("client_id", registration.client_id.as_str()),
            ("refresh_token", previous.refresh_token.as_str()),
            ("resource", API_RESOURCE),
        ])?;
        let response = client
            .post(&configuration.token_endpoint)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form)
            .send()
            .await
            .map_err(|error| format!("连接OpenAI令牌服务失败；原有令牌已保留：{error}"))?;
        let status = response.status();
        let mut body = response
            .bytes()
            .await
            .map_err(|_| "读取OpenAI令牌刷新响应失败；原有令牌已保留。".to_string())?
            .to_vec();
        if body.len() > 64 * 1024 {
            body.zeroize();
            return Err("OpenAI令牌刷新响应超过安全大小限制；原有令牌已保留。".into());
        }
        if !status.is_success() {
            let message = classify_oauth_error(
                status.as_u16(),
                std::str::from_utf8(&body).unwrap_or_default(),
            );
            let terminal = message.contains("已过期、撤销或已被替换");
            body.zeroize();
            if terminal {
                store.disconnect()?;
            }
            return Err(message);
        }
        let mut response: Value = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(_) => {
                body.zeroize();
                return Err("OpenAI令牌刷新响应不是有效JSON；原有令牌已保留。".into());
            }
        };
        body.zeroize();
        let refreshed = parse_token_response(
            &response,
            Some(&previous),
            chrono::Utc::now().timestamp(),
            &chrono::Utc::now().to_rfc3339(),
        );
        clear_token_response(&mut response);
        let refreshed = refreshed?;
        if refreshed.allows_plan_usage() {
            if refreshed.id_token != previous.id_token {
                let jwks = fetch_jwks(&client, &configuration).await?;
                let subject = validate_id_token_with_jwks(
                    &refreshed.id_token,
                    &jwks,
                    &registration.client_id,
                    None,
                    chrono::Utc::now().timestamp(),
                )?;
                if subject != registration.subject {
                    return Err("令牌刷新返回了不同账号身份，已拒绝覆盖当前连接。".into());
                }
            }
        } else {
            return Err("令牌刷新后的授权范围不再包含plan直接使用权限，已停止AI请求。".into());
        }
        store.save_tokens(&refreshed)?;
        let access_token = refreshed.access_token.clone();
        previous.refresh_token.zeroize();
        Ok(access_token)
    }

    pub(crate) async fn clear_confirmed_invalid_tokens(
        &self,
        store: &CredentialStore,
    ) -> Result<(), String> {
        let _refresh_guard = self.refresh_lock.lock().await;
        let _cross_process_refresh_guard = store.acquire_refresh_lock().await?;
        store.disconnect()
    }
}

#[cfg(test)]
mod tests {
    use super::ChatGptAuthManager;
    use super::{
        build_authorization_url, callback_query, classify_oauth_error,
        granted_scope_contains_direct_plan_usage, parse_authorization_callback,
        parse_token_response, pkce_s256_challenge, validate_discovery_document,
        validate_id_token_with_jwks,
    };
    use crate::siwc_store::TokenCredentials;
    use crate::siwc_store::{CredentialStore, RegistrationMetadata};
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};

    #[test]
    fn direct_plan_usage_requires_the_exact_granted_scope() {
        assert!(granted_scope_contains_direct_plan_usage(
            "openid profile chatgpt.tokens.use.direct resource.invoke"
        ));
        assert!(!granted_scope_contains_direct_plan_usage(
            "openid profile resource.invoke"
        ));
        assert!(!granted_scope_contains_direct_plan_usage(
            "openid profile prefix-chatgpt.tokens.use.direct-suffix"
        ));
    }

    #[test]
    fn authorization_callback_rejects_a_state_mismatch() {
        let result = parse_authorization_callback(
            "code=auth-code&state=attacker-state&client_id=oaiapp_issued",
            "expected-state",
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn first_registration_requires_the_issued_client_id() {
        let result = parse_authorization_callback(
            "code=auth-code&state=expected-state",
            "expected-state",
            None,
        );
        assert!(result.is_err());
    }

    #[test]
    fn returning_registration_reuses_saved_client_id_when_callback_omits_it() {
        let result = parse_authorization_callback(
            "code=auth-code&state=expected-state",
            "expected-state",
            Some("oaiapp_saved"),
        )
        .unwrap();
        assert_eq!(result.client_id, "oaiapp_saved");
    }

    #[test]
    fn returning_registration_rejects_a_different_client_id() {
        let result = parse_authorization_callback(
            "code=auth-code&state=expected-state&client_id=oaiapp_other",
            "expected-state",
            Some("oaiapp_saved"),
        );
        assert!(result.is_err());
    }

    #[test]
    fn callback_rejects_oauth_denial_without_returning_raw_error_data() {
        let result = parse_authorization_callback(
            "error=access_denied&error_description=private-data&state=expected-state",
            "expected-state",
            None,
        );
        let message = result.err().unwrap();
        assert!(!message.contains("private-data"));
    }

    #[test]
    fn oauth_denial_state_is_validated_before_reporting_declined_consent() {
        let result = parse_authorization_callback(
            "error=access_denied&state=wrong-state",
            "expected-state",
            None,
        );
        assert!(result.err().unwrap().contains("校验失败"));
    }

    #[test]
    fn first_registration_rejects_the_dynamic_entrypoint_as_an_issued_client_id() {
        let result = parse_authorization_callback(
            "code=auth-code&state=expected-state&client_id=dynamic_agent_client",
            "expected-state",
            None,
        );
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn sign_in_can_be_cancelled_and_only_one_flow_runs_at_a_time() {
        let manager = ChatGptAuthManager::default();
        let mut receiver = manager.begin().unwrap();
        assert!(manager.begin().is_err());
        assert!(manager.cancel().unwrap());
        receiver.changed().await.unwrap();
        assert!(*receiver.borrow());
        manager.finish();
        assert!(manager.begin().is_ok());
    }

    #[tokio::test]
    async fn disconnect_removes_local_tokens_when_remote_revocation_is_unconfirmed() {
        let directory = tempfile::tempdir().unwrap();
        let store = CredentialStore::new(directory.path().to_path_buf());
        let host_id = store.load_or_create_host_id().unwrap();
        let registration = RegistrationMetadata {
            issuer: "https://auth.openai.com".into(),
            subject: "verified-subject".into(),
            client_id: "oaiapp_test".into(),
            ext_agent_host_id: host_id,
        };
        let tokens = TokenCredentials {
            id_token: "synthetic-id".into(),
            access_token: "synthetic-access".into(),
            refresh_token: "synthetic-refresh".into(),
            expires_at_unix: 1_900_000_000,
            token_type: "Bearer".into(),
            scopes: vec!["chatgpt.tokens.use.direct".into()],
            saved_at: "test".into(),
        };
        store.save_registration(&registration).unwrap();
        store.save_tokens(&tokens).unwrap();

        let result = super::remove_local_credentials(&store, false, false).unwrap();

        assert!(!result.remote_revocation_confirmed);
        assert!(result.message.contains("OpenAI未确认远端撤销"));
        assert!(store.load_tokens().unwrap().is_none());
        assert_eq!(store.load_registration().unwrap(), Some(registration));
    }

    #[test]
    fn pkce_challenge_uses_the_rfc_7636_s256_encoding() {
        assert_eq!(
            pkce_s256_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn authorization_url_requests_only_the_documented_openai_scopes() {
        let url = build_authorization_url(
            "dynamic_agent_client",
            true,
            "urn:uuid:host-id",
            "http://127.0.0.1:43127/auth/callback",
            "state-value-0123456789",
            "nonce-value-0123456789",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
        )
        .unwrap();
        let query = url
            .query_pairs()
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("auth.openai.com"));
        assert_eq!(
            query.get("client_id").map(String::as_str),
            Some("dynamic_agent_client")
        );
        assert_eq!(
            query.get("resource").map(String::as_str),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(query.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(
            query.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert!(query.get("scope").is_some_and(|scope| {
            [
                "openid",
                "profile",
                "email",
                "offline_access",
                "resource.invoke",
                "chatgpt.tokens.use.direct",
            ]
            .iter()
            .all(|required| scope.split_ascii_whitespace().any(|part| part == *required))
        }));
        assert_eq!(
            query.get("agent_name_hint").map(String::as_str),
            Some("书隅")
        );
    }

    #[test]
    fn returning_authorization_omits_first_registration_metadata() {
        let url = build_authorization_url(
            "oaiapp_saved",
            false,
            "urn:uuid:host-id",
            "http://127.0.0.1:43127/auth/callback",
            "state-value-0123456789",
            "nonce-value-0123456789",
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
        )
        .unwrap();
        let query = url
            .query_pairs()
            .into_owned()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            query.get("client_id").map(String::as_str),
            Some("oaiapp_saved")
        );
        assert!(!query.contains_key("agent_name_hint"));
    }

    #[test]
    fn openid_discovery_rejects_endpoints_outside_the_official_auth_origin() {
        let invalid = serde_json::json!({
            "issuer": "https://auth.openai.com",
            "authorization_endpoint": "https://auth.openai.com/api/accounts/authorize",
            "token_endpoint": "https://attacker.example/token",
            "jwks_uri": "https://auth.openai.com/.well-known/jwks.json",
            "revocation_endpoint": "https://auth.openai.com/api/accounts/oauth/revoke"
        });
        assert!(validate_discovery_document(&invalid).is_err());
    }

    #[test]
    fn token_response_stores_granted_scopes_and_rotated_refresh_token() {
        let response = serde_json::json!({
            "id_token": "new-id",
            "access_token": "new-access",
            "refresh_token": "new-refresh",
            "expires_in": 3600,
            "token_type": "Bearer",
            "scope": "openid chatgpt.tokens.use.direct"
        });
        let parsed = parse_token_response(&response, None, 1_800_000_000, "now").unwrap();

        assert_eq!(parsed.id_token, "new-id");
        assert_eq!(parsed.access_token, "new-access");
        assert_eq!(parsed.refresh_token, "new-refresh");
        assert_eq!(parsed.expires_at_unix, 1_800_003_600);
        assert!(parsed
            .scopes
            .iter()
            .any(|scope| scope == "chatgpt.tokens.use.direct"));
    }

    #[test]
    fn refresh_response_keeps_existing_scope_when_provider_omits_scope() {
        let previous = TokenCredentials {
            id_token: "old-id".into(),
            access_token: "old-access".into(),
            refresh_token: "old-refresh".into(),
            expires_at_unix: 1,
            token_type: "Bearer".into(),
            scopes: vec!["chatgpt.tokens.use.direct".into()],
            saved_at: "before".into(),
        };
        let response = serde_json::json!({
            "access_token": "rotated-access",
            "expires_in": 3600,
            "token_type": "Bearer"
        });
        let parsed =
            parse_token_response(&response, Some(&previous), 1_800_000_000, "now").unwrap();

        assert_eq!(parsed.id_token, "old-id");
        assert_eq!(parsed.refresh_token, "old-refresh");
        assert_eq!(parsed.scopes, previous.scopes);
    }

    #[test]
    fn oauth_error_messages_include_recovery_without_echoing_provider_body() {
        let message = classify_oauth_error(
            400,
            "{\"error\":\"invalid_grant\",\"error_description\":\"private token data\"}",
        );
        assert!(message.contains("重新连接"));
        assert!(!message.contains("private token data"));
        assert!(classify_oauth_error(503, "temporarily unavailable").contains("稍后重试"));
    }

    fn signed_id_token(nonce: &str, issuer: &str, audience: &str) -> (String, serde_json::Value) {
        signed_id_token_from_claims(serde_json::json!({
            "iss": issuer,
            "sub": "synthetic-subject",
            "aud": audience,
            "iat": 1_800_000_000,
            "exp": 1_800_003_600,
            "nonce": nonce
        }))
    }

    fn signed_id_token_from_claims(claims: serde_json::Value) -> (String, serde_json::Value) {
        use ring::{
            rand::SystemRandom,
            signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING},
        };
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
            .unwrap();
        let public = key.public_key().as_ref();
        let header = serde_json::json!({"alg":"ES256", "kid":"test-key", "typ":"JWT"});
        let encoded_header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let encoded_claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let signing_input = format!("{encoded_header}.{encoded_claims}");
        let signature = key.sign(&rng, signing_input.as_bytes()).unwrap();
        let token = format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        );
        let jwks = serde_json::json!({
            "keys": [{
                "kid":"test-key",
                "kty":"EC",
                "use":"sig",
                "alg":"ES256",
                "crv":"P-256",
                "x":URL_SAFE_NO_PAD.encode(&public[1..33]),
                "y":URL_SAFE_NO_PAD.encode(&public[33..65])
            }]
        });
        (token, jwks)
    }

    #[test]
    fn signed_id_token_validates_signature_issuer_audience_expiry_and_nonce() {
        let (token, jwks) =
            signed_id_token("expected-nonce", "https://auth.openai.com", "oaiapp_test");
        assert_eq!(
            validate_id_token_with_jwks(
                &token,
                &jwks,
                "oaiapp_test",
                Some("expected-nonce"),
                1_800_000_010
            )
            .unwrap(),
            "synthetic-subject"
        );
        assert!(validate_id_token_with_jwks(
            &token,
            &jwks,
            "oaiapp_test",
            Some("wrong-nonce"),
            1_800_000_010
        )
        .is_err());
        assert!(validate_id_token_with_jwks(
            &token,
            &jwks,
            "another-client",
            Some("expected-nonce"),
            1_800_000_010
        )
        .is_err());
        assert!(validate_id_token_with_jwks(
            &token,
            &jwks,
            "oaiapp_test",
            Some("expected-nonce"),
            1_900_000_000
        )
        .is_err());
    }

    #[test]
    fn signed_id_token_rejects_wrong_issuer_and_tampered_signature() {
        let (wrong_issuer, wrong_issuer_jwks) =
            signed_id_token("nonce", "https://attacker.example", "oaiapp_test");
        assert!(validate_id_token_with_jwks(
            &wrong_issuer,
            &wrong_issuer_jwks,
            "oaiapp_test",
            Some("nonce"),
            1_800_000_010
        )
        .is_err());

        let (token, jwks) = signed_id_token("nonce", "https://auth.openai.com", "oaiapp_test");
        let mut parts = token.split('.').map(str::to_owned).collect::<Vec<_>>();
        let replacement = if &parts[2][0..1] == "A" { "B" } else { "A" };
        parts[2].replace_range(0..1, replacement);
        let tampered = parts.join(".");
        assert!(validate_id_token_with_jwks(
            &tampered,
            &jwks,
            "oaiapp_test",
            Some("nonce"),
            1_800_000_010
        )
        .is_err());
    }

    #[test]
    fn multiple_audiences_require_matching_authorized_party() {
        let base_claims = serde_json::json!({
            "iss":"https://auth.openai.com",
            "sub":"synthetic-subject",
            "aud":["oaiapp_test", "another-client"],
            "iat":1_800_000_000,
            "exp":1_800_003_600,
            "nonce":"nonce"
        });
        let (wrong_azp, wrong_jwks) = signed_id_token_from_claims({
            let mut claims = base_claims.clone();
            claims["azp"] = serde_json::json!("another-client");
            claims
        });
        assert!(validate_id_token_with_jwks(
            &wrong_azp,
            &wrong_jwks,
            "oaiapp_test",
            Some("nonce"),
            1_800_000_010
        )
        .is_err());
        let (valid_azp, valid_jwks) = signed_id_token_from_claims({
            let mut claims = base_claims;
            claims["azp"] = serde_json::json!("oaiapp_test");
            claims
        });
        assert!(validate_id_token_with_jwks(
            &valid_azp,
            &valid_jwks,
            "oaiapp_test",
            Some("nonce"),
            1_800_000_010
        )
        .is_ok());
    }

    #[test]
    fn callback_requires_loopback_path_and_get_method() {
        assert_eq!(
            callback_query("GET /auth/callback?code=x&state=y HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
            Some("code=x&state=y".into())
        );
        assert!(callback_query("POST /auth/callback?code=x HTTP/1.1\r\n\r\n").is_none());
        assert!(callback_query("GET /other?code=x HTTP/1.1\r\n\r\n").is_none());
    }
}
