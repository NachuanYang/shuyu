use crate::ai::{
    build_book_recommendation_messages, build_contextual_explanation_messages,
    build_daily_quote_selection_messages, build_explanation_messages, build_responses_request,
    parse_book_recommendations, parse_daily_quote_candidate_id, AiCitation, AiMessage, AiProvider,
    AiSettings, AiStreamItem, BookRecommendation, ChatGptAiSettings, RecommendationShelfBook,
    ResponsesSearchMode, StreamDecoder, StreamProtocol, AI_PRIVACY_NOTICE_VERSION,
};
use crate::ai_context::{self, AiQuestionContextPreview};
use crate::{
    db::Database,
    models::DailyQuoteCandidate,
    siwc::{self, ChatGptAuthManager},
};
use futures_util::StreamExt;
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::watch;
use zeroize::Zeroize;

const AI_SETTINGS_KEY: &str = "ai_client";
const KEY_SERVICE: &str = "com.caohua.shuyu";
const KEY_USERNAME: &str = "openrouter-api-key";
const STREAM_EVENT: &str = "shuyu://ai-stream";
const CHATGPT_MODELS_URL: &str = "https://api.openai.com/v1/models";
const CHATGPT_RESPONSES_URL: &str = "https://api.openai.com/v1/responses";
const MAX_RESPONSES_SSE_PROBE_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BookRecommendationResult {
    pub items: Vec<BookRecommendation>,
    pub message: String,
}

#[derive(Debug)]
struct ChatGptResponseStream {
    response: reqwest::Response,
    initial_chunk: Option<Vec<u8>>,
    client_request_id: String,
}

impl ChatGptResponseStream {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, reqwest::Error> {
        if let Some(chunk) = self.initial_chunk.take() {
            return Ok(Some(chunk));
        }
        self.response
            .chunk()
            .await
            .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptModel {
    pub slug: String,
    pub display_name: String,
}

#[derive(Deserialize)]
struct ChatGptModelCatalog {
    models: Vec<ChatGptModelRecord>,
}

#[derive(Deserialize)]
struct ChatGptModelRecord {
    slug: String,
    display_name: String,
    visibility: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSettingsSnapshot {
    #[serde(flatten)]
    pub settings: AiSettings,
    pub open_router_key_configured: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConnectionResult {
    pub provider: AiProvider,
    pub model: String,
    pub service_available: bool,
    pub model_available: bool,
    pub message: String,
    pub available_models: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiStreamEvent {
    pub request_id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl AiStreamEvent {
    fn token(request_id: &str, text: String) -> Self {
        Self {
            request_id: request_id.into(),
            kind: "token".into(),
            text: Some(text),
            title: None,
            url: None,
            start_index: None,
            end_index: None,
            message: None,
        }
    }

    fn citation(request_id: &str, citation: AiCitation) -> Self {
        Self {
            request_id: request_id.into(),
            kind: "citation".into(),
            text: None,
            title: Some(citation.title),
            url: Some(citation.url),
            start_index: Some(citation.start_index),
            end_index: Some(citation.end_index),
            message: None,
        }
    }

    fn terminal(request_id: &str, kind: &str, message: Option<String>) -> Self {
        Self {
            request_id: request_id.into(),
            kind: kind.into(),
            text: None,
            title: None,
            url: None,
            start_index: None,
            end_index: None,
            message,
        }
    }
}

#[derive(Default)]
pub struct AiRequestManager {
    active: Mutex<HashMap<String, (Arc<AtomicBool>, watch::Sender<bool>)>>,
}

impl AiRequestManager {
    fn register(
        &self,
        request_id: &str,
    ) -> Result<(Arc<AtomicBool>, watch::Receiver<bool>), String> {
        if request_id.is_empty()
            || request_id.len() > 64
            || !request_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err("AI请求编号无效。".into());
        }
        let mut active = self
            .active
            .lock()
            .map_err(|_| "AI请求状态不可用。".to_string())?;
        if !active.is_empty() {
            return Err("已有AI请求正在处理，请先等待或取消当前请求。".into());
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = watch::channel(false);
        active.insert(request_id.into(), (cancelled.clone(), sender));
        Ok((cancelled, receiver))
    }

    fn cancel(&self, request_id: &str) -> bool {
        let Ok(active) = self.active.lock() else {
            return false;
        };
        let Some((cancelled, sender)) = active.get(request_id) else {
            return false;
        };
        cancelled.store(true, Ordering::Release);
        let _ = sender.send(true);
        true
    }

    fn finish(&self, request_id: &str) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(request_id);
        }
    }

    pub(crate) fn cancel_all(&self) {
        let Ok(active) = self.active.lock() else {
            return;
        };
        for (cancelled, sender) in active.values() {
            cancelled.store(true, Ordering::Release);
            let _ = sender.send(true);
        }
    }
}

fn credential_entry() -> Result<Entry, String> {
    Entry::new(KEY_SERVICE, KEY_USERNAME).map_err(|_| "Windows凭据库不可用。".to_string())
}

fn read_openrouter_key() -> Result<Option<String>, String> {
    match credential_entry()?.get_password() {
        Ok(value) if !value.trim().is_empty() => Ok(Some(value)),
        _ => Ok(None),
    }
}

#[tauri::command]
pub fn get_ai_settings(database: State<'_, Arc<Database>>) -> Result<AiSettingsSnapshot, String> {
    let settings = load_settings(&database)?;
    Ok(AiSettingsSnapshot {
        settings,
        open_router_key_configured: read_openrouter_key()?.is_some(),
    })
}

#[tauri::command]
pub fn get_chatgpt_ai_settings(
    database: State<'_, Arc<Database>>,
) -> Result<ChatGptAiSettings, String> {
    load_chatgpt_ai_settings(&database)
}

#[tauri::command]
pub fn save_chatgpt_ai_settings(
    database: State<'_, Arc<Database>>,
    manager: State<'_, Arc<AiRequestManager>>,
    settings: ChatGptAiSettings,
    accept_privacy_notice: bool,
) -> Result<ChatGptAiSettings, String> {
    let previous = load_chatgpt_ai_settings(&database)?;
    let settings = apply_privacy_acceptance(
        settings,
        &previous,
        accept_privacy_notice,
        chrono::Utc::now().to_rfc3339(),
    )?;
    let json = serde_json::to_string(&settings).map_err(|_| "无法保存AI隐私设置。".to_string())?;
    database
        .save_preference(AI_SETTINGS_KEY, &json)
        .map_err(|_| "无法保存AI隐私设置；数据库未确认写入。".to_string())?;
    if (previous.enabled && !settings.enabled)
        || (previous.allow_ai_qa && !settings.allow_ai_qa)
        || (previous.allow_ai_daily_quote && !settings.allow_ai_daily_quote)
        || !settings.enabled
    {
        manager.cancel_all();
    }
    Ok(settings)
}

fn apply_privacy_acceptance(
    mut next: ChatGptAiSettings,
    previous: &ChatGptAiSettings,
    accept_privacy_notice: bool,
    accepted_at: String,
) -> Result<ChatGptAiSettings, String> {
    if accept_privacy_notice {
        next.privacy_notice_version = AI_PRIVACY_NOTICE_VERSION;
        next.privacy_notice_accepted_at = Some(accepted_at);
    } else {
        next.privacy_notice_version = previous.privacy_notice_version;
        next.privacy_notice_accepted_at = previous.privacy_notice_accepted_at.clone();
    }
    next.validate_for_save()?;
    next.normalized()
}

#[tauri::command]
pub fn save_ai_settings(
    database: State<'_, Arc<Database>>,
    settings: AiSettings,
) -> Result<AiSettingsSnapshot, String> {
    let normalized = settings.normalized()?;
    let json = serde_json::to_string(&normalized).map_err(|_| "无法保存AI设置。".to_string())?;
    database
        .save_preference(AI_SETTINGS_KEY, &json)
        .map_err(|_| "无法保存AI设置。".to_string())?;
    Ok(AiSettingsSnapshot {
        settings: normalized,
        open_router_key_configured: read_openrouter_key()?.is_some(),
    })
}

#[tauri::command]
pub fn save_openrouter_api_key(api_key: String) -> Result<bool, String> {
    let key = api_key.trim();
    if key.is_empty() || key.len() > 4096 {
        return Err("API Key不能为空且不能超过4,096个字符。".into());
    }
    credential_entry()?
        .set_password(key)
        .map_err(|_| "无法将API Key保存到Windows凭据库。".to_string())?;
    Ok(true)
}

#[tauri::command]
pub fn clear_openrouter_api_key() -> Result<bool, String> {
    let entry = credential_entry()?;
    if entry.get_password().is_err() {
        return Ok(false);
    }
    entry
        .delete_credential()
        .map_err(|_| "无法从Windows凭据库删除API Key。".to_string())?;
    Ok(true)
}

#[tauri::command]
pub async fn test_ai_connection(
    database: State<'_, Arc<Database>>,
) -> Result<AiConnectionResult, String> {
    let settings = load_settings(&database)?;
    let client = http_client(Duration::from_secs(15))?;
    match settings.provider {
        AiProvider::Ollama => test_ollama(&client, &settings).await,
        AiProvider::OpenRouter => test_openrouter(&client, &settings).await,
    }
}

#[tauri::command]
pub async fn get_chatgpt_models(
    app: AppHandle,
    manager: State<'_, Arc<ChatGptAuthManager>>,
) -> Result<Vec<ChatGptModel>, String> {
    // Model discovery can cross the user's Windows system proxy, so allow a
    // slower upstream response than interactive AI calls before reporting a timeout.
    const MODEL_CATALOG_TIMEOUT: Duration = Duration::from_secs(60);
    let client = http_client(MODEL_CATALOG_TIMEOUT)?;
    let store = siwc::store_for_app(&app)?;
    let mut access_token = manager.access_token(&store).await?;
    let response = fetch_chatgpt_models(&client, CHATGPT_MODELS_URL, &access_token).await;
    access_token.zeroize();
    if let Err(error) = &response {
        if is_confirmed_auth_failure(error) {
            manager.clear_confirmed_invalid_tokens(&store).await?;
        }
    }
    response
}

async fn fetch_chatgpt_models(
    client: &reqwest::Client,
    endpoint: &str,
    access_token: &str,
) -> Result<Vec<ChatGptModel>, String> {
    let response = client
        .get(endpoint)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|error| network_failure("OpenAI官方模型目录", &error))?;
    if !response.status().is_success() {
        return Err(read_openai_error(response, "模型目录").await);
    }
    const MAX_CATALOG_BYTES: usize = 2 * 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_CATALOG_BYTES as u64)
    {
        return Err("OpenAI模型目录超过2MiB安全大小限制，已拒绝读取。".into());
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                body.zeroize();
                return Err(network_failure("读取OpenAI模型目录", &error));
            }
        };
        if body.len().saturating_add(chunk.len()) > MAX_CATALOG_BYTES {
            body.zeroize();
            return Err("OpenAI模型目录超过2MiB安全大小限制，已拒绝读取。".into());
        }
        body.extend_from_slice(&chunk);
    }
    let parsed = parse_chatgpt_models(&body);
    body.zeroize();
    parsed
}

fn parse_chatgpt_models(body: &[u8]) -> Result<Vec<ChatGptModel>, String> {
    if body.len() > 2 * 1024 * 1024 {
        return Err("OpenAI模型目录超过2MiB安全大小限制，已拒绝读取。".into());
    }
    let catalog: ChatGptModelCatalog = serde_json::from_slice(body)
        .map_err(|_| "OpenAI模型目录格式无效或缺少models字段。".to_string())?;
    if catalog.models.len() > 2_000 {
        return Err("OpenAI模型目录包含过多项目，已拒绝读取。".into());
    }
    let mut models = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for model in catalog.models {
        let slug = model.slug.trim();
        let display_name = model.display_name.trim();
        if model.visibility != "list"
            || slug.is_empty()
            || slug.len() > 256
            || !slug.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/')
            })
            || display_name.is_empty()
            || display_name.len() > 256
            || !seen.insert(slug.to_owned())
        {
            continue;
        }
        models.push(ChatGptModel {
            slug: slug.into(),
            display_name: display_name.into(),
        });
    }
    Ok(models)
}

#[cfg(test)]
fn require_listed_chatgpt_model(
    models: &[ChatGptModel],
    selected_slug: &str,
) -> Result<(), String> {
    if models.iter().any(|model| model.slug == selected_slug) {
        Ok(())
    } else {
        Err("当前选择的模型已不在此ChatGPT账号的可用目录中；请刷新模型列表并重新选择。".into())
    }
}

async fn read_openai_error(response: reqwest::Response, operation: &str) -> String {
    let status = response.status();
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.len() <= 256)
        .map(|value| safe_openai_detail(value, 128));
    if response
        .content_length()
        .is_some_and(|length| length > 64 * 1024)
    {
        return format!(
            "OpenAI{operation}请求失败（HTTP {}），错误响应超过64KiB限制。",
            status.as_u16()
        );
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            body.zeroize();
            return format!(
                "OpenAI{operation}请求失败（HTTP {}），读取错误详情时连接中断。",
                status.as_u16()
            );
        };
        if body.len().saturating_add(chunk.len()) > 64 * 1024 {
            body.zeroize();
            return format!(
                "OpenAI{operation}请求失败（HTTP {}），错误响应超过64KiB限制。",
                status.as_u16()
            );
        }
        body.extend_from_slice(&chunk);
    }
    let parsed = serde_json::from_slice::<serde_json::Value>(&body).ok();
    let code = parsed
        .as_ref()
        .and_then(|value| {
            value
                .pointer("/error/code")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            parsed
                .as_ref()
                .and_then(|value| value.pointer("/code").and_then(serde_json::Value::as_str))
        })
        .map(str::to_owned);
    let param = parsed
        .as_ref()
        .and_then(|value| {
            value
                .pointer("/error/param")
                .and_then(serde_json::Value::as_str)
        })
        .map(str::to_owned);
    body.zeroize();
    openai_error_message(
        status,
        code.as_deref(),
        param.as_deref(),
        request_id.as_deref(),
    )
}

fn openai_error_message(
    status: reqwest::StatusCode,
    code: Option<&str>,
    param: Option<&str>,
    request_id: Option<&str>,
) -> String {
    let message = match code {
        Some("subscription_sharing_user_not_eligible") => {
            "当前ChatGPT账号、工作区或策略不符合plan usage使用条件。"
        }
        Some("subscription_sharing_usage_limit_exceeded") => {
            "ChatGPT plan usage已达到当前应用用量上限；请前往ChatGPT设置的Usage检查书隅额度。"
        }
        Some("subscription_sharing_usage_unavailable") => {
            "OpenAI暂时无法查询ChatGPT plan usage，请稍后重试；当前请求没有切换到其他计费方式。"
        }
        Some("subscription_sharing_unsupported_capability") => {
            "OpenAI拒绝了当前Responses请求能力或参数；对于问答或书籍推荐，可能是当前模型或账号策略未开放内置web_search。"
        }
        Some("subscription_sharing_route_not_supported") => {
            "OpenAI未允许此请求路径用于ChatGPT plan usage；书隅只请求公开Responses API。"
        }
        Some("subscription_sharing_invalid_user") => {
            "OpenAI无法验证当前ChatGPT账号授权，请重新连接账号。"
        }
        Some("subscription_sharing_user_unavailable") => {
            "OpenAI暂时无法读取当前ChatGPT账号或工作区信息，请稍后重试。"
        }
        Some("chatpass_v2_scope_not_authorized") => {
            "当前ChatGPT授权没有允许plan usage，请重新连接并确认授权范围。"
        }
        Some("chatpass_v2_invalid_authorization_context") => {
            "当前ChatGPT授权上下文无效，请检查连接账号与客户端注册。"
        }
        _ if status.as_u16() == 401 => {
            "OpenAI拒绝了当前账号凭据或plan使用权限，请在设置中重新连接ChatGPT。"
        }
        _ if status.as_u16() == 403 => {
            "OpenAI因账号权限、区域或服务策略拒绝了请求；请核对账号与当前服务可用性。"
        }
        _ if status.as_u16() == 429 => {
            "OpenAI限制了当前请求频率或plan用量；请稍后重试并检查ChatGPT设置中的Usage。"
        }
        _ if status.as_u16() == 503 => {
            "OpenAI的ChatGPT plan直接服务暂时不可用，请稍后重试；本地连接凭据已保留。"
        }
        _ => "OpenAI未能完成请求，请根据HTTP状态与请求编号检查连接或授权。",
    };
    let mut detail = format!("{message}（HTTP {}）", status.as_u16());
    if let Some(code) = code
        .map(|value| safe_openai_detail(value, 128))
        .filter(|code| !code.is_empty())
    {
        detail.push_str(&format!("；错误代码：{code}"));
    }
    if let Some(param) = param
        .map(|value| safe_openai_detail(value, 128))
        .filter(|param| !param.is_empty())
    {
        detail.push_str(&format!("；参数：{param}"));
    }
    if let Some(request_id) = request_id
        .map(|value| safe_openai_detail(value, 128))
        .filter(|id| !id.is_empty())
    {
        detail.push_str(&format!("；请求编号：{request_id}"));
    }
    detail
}

#[tauri::command]
pub fn cancel_ai_request(request_id: String, manager: State<'_, Arc<AiRequestManager>>) -> bool {
    manager.cancel(&request_id)
}

fn build_ai_question_context(
    database: &Database,
    book_id: &str,
    chapter_id: &str,
    start_paragraph_id: &str,
    start_utf16: usize,
    end_paragraph_id: &str,
    end_utf16: usize,
    selected_text: &str,
    question: &str,
) -> Result<AiQuestionContextPreview, String> {
    database
        .reconstruct_ai_question_selection(
            book_id,
            chapter_id,
            start_paragraph_id,
            start_utf16,
            end_paragraph_id,
            end_utf16,
            selected_text,
        )
        .map_err(|error| format!("无法校验选中的本地原文：{error}"))?
        .ok_or_else(|| {
            "选段与本书当前章节的原文不一致；为避免发送错误内容，请重新选择原文。".to_string()
        })?;
    let context = database
        .read_ai_question_context(book_id, chapter_id)
        .map_err(|error| format!("无法读取本机阅读位置和书籍上下文：{error}"))?
        .ok_or_else(|| {
            "无法确认当前章节的阅读位置；请回到本章并保存阅读位置后重试。".to_string()
        })?;
    ai_context::prepare_context(&context, selected_text, question)
}

async fn build_ai_question_context_async(
    database: Arc<Database>,
    book_id: String,
    chapter_id: String,
    start_paragraph_id: String,
    start_utf16: usize,
    end_paragraph_id: String,
    end_utf16: usize,
    selected_text: String,
    question: String,
) -> Result<AiQuestionContextPreview, String> {
    tokio::task::spawn_blocking(move || {
        build_ai_question_context(
            &database,
            &book_id,
            &chapter_id,
            &start_paragraph_id,
            start_utf16,
            &end_paragraph_id,
            end_utf16,
            &selected_text,
            &question,
        )
    })
    .await
    .map_err(|error| format!("本机阅读上下文准备任务失败：{error}"))?
}

#[tauri::command]
pub async fn start_chatgpt_ai_explanation(
    app: AppHandle,
    database: State<'_, Arc<Database>>,
    auth_manager: State<'_, Arc<ChatGptAuthManager>>,
    requests: State<'_, Arc<AiRequestManager>>,
    request_id: String,
    selected_text: String,
    question: String,
    book_id: String,
    chapter_id: String,
    start_paragraph_id: String,
    start_utf16: usize,
    end_paragraph_id: String,
    end_utf16: usize,
) -> Result<(), String> {
    let settings = load_chatgpt_ai_settings(&database)?;
    if !settings.can_use_ai_qa() {
        return Err(
            "阅读AI问答当前未获许可；请在设置中启用AI、重新确认数据说明并允许阅读问答。".into(),
        );
    }
    let model_slug = settings
        .selected_model_slug
        .clone()
        .ok_or_else(|| "尚未选择ChatGPT模型，请先刷新当前账号的模型目录。".to_string())?;
    let request_manager = Arc::clone(requests.inner());
    let auth_manager = Arc::clone(auth_manager.inner());
    let database = Arc::clone(database.inner());
    let (cancelled, mut cancel_rx) = request_manager.register(&request_id)?;
    let app = app.clone();
    let id_for_task = request_id.clone();
    tauri::async_runtime::spawn(async move {
        let result = async {
            let latest = load_chatgpt_ai_settings(&database)?;
            if !latest.can_use_ai_qa() || latest.selected_model_slug.as_deref() != Some(&model_slug)
            {
                return Err("问答许可或模型选择在发送前已更改；请确认设置后重试。".into());
            }
            let context = build_ai_question_context_async(
                Arc::clone(&database),
                book_id.clone(),
                chapter_id.clone(),
                start_paragraph_id.clone(),
                start_utf16,
                end_paragraph_id.clone(),
                end_utf16,
                selected_text.clone(),
                question.clone(),
            )
            .await?;
            let messages = build_contextual_explanation_messages(
                &selected_text,
                &question,
                &context.current_chapter,
                &context.related_passages,
            )?;
            let store = siwc::store_for_app(&app)?;
            let mut access_token = auth_manager.access_token(&store).await?;
            let response = stream_chatgpt_response(
                &app,
                &id_for_task,
                &model_slug,
                &messages,
                &access_token,
                &auth_manager,
                &store,
                Arc::clone(&cancelled),
                &mut cancel_rx,
            )
            .await;
            access_token.zeroize();
            response
        }
        .await;
        match result {
            Ok(true) => emit(
                &app,
                AiStreamEvent::terminal(&id_for_task, "cancelled", None),
            ),
            Ok(false) => emit(
                &app,
                AiStreamEvent::terminal(&id_for_task, "complete", None),
            ),
            Err(message) => emit(
                &app,
                AiStreamEvent::terminal(&id_for_task, "error", Some(message)),
            ),
        }
        request_manager.finish(&id_for_task);
    });
    Ok(())
}

#[tauri::command]
pub fn start_ai_explanation(
    app: AppHandle,
    database: State<'_, Arc<Database>>,
    manager: State<'_, Arc<AiRequestManager>>,
    request_id: String,
    selected_text: String,
    question: String,
) -> Result<(), String> {
    let settings = load_settings(&database)?;
    if !settings.enabled {
        return Err("请先在设置中启用实验性AI。".into());
    }
    settings.validate_ready()?;
    let messages = build_explanation_messages(&selected_text, &question)?;
    let api_key = match settings.provider {
        AiProvider::Ollama => None,
        AiProvider::OpenRouter => Some(
            read_openrouter_key()?
                .ok_or_else(|| "请先在设置中保存OpenRouter API Key。".to_string())?,
        ),
    };
    let manager = manager.inner().clone();
    let (cancelled, mut cancel_rx) = manager.register(&request_id)?;
    let app = app.clone();
    let id_for_task = request_id.clone();
    tauri::async_runtime::spawn(async move {
        let result = stream_explanation(
            &app,
            &id_for_task,
            &settings,
            &messages,
            api_key.as_deref(),
            cancelled,
            &mut cancel_rx,
        )
        .await;
        match result {
            Ok(true) => emit(
                &app,
                AiStreamEvent::terminal(&id_for_task, "cancelled", None),
            ),
            Ok(false) => emit(
                &app,
                AiStreamEvent::terminal(&id_for_task, "complete", None),
            ),
            Err(message) => emit(
                &app,
                AiStreamEvent::terminal(&id_for_task, "error", Some(message)),
            ),
        }
        manager.finish(&id_for_task);
    });
    Ok(())
}

fn load_settings(database: &Database) -> Result<AiSettings, String> {
    let stored = database
        .preference(AI_SETTINGS_KEY)
        .map_err(|_| "无法读取AI设置。".to_string())?;
    let settings = stored
        .and_then(|json| serde_json::from_str::<AiSettings>(&json).ok())
        .unwrap_or_default();
    settings.normalized()
}

fn load_chatgpt_ai_settings(database: &Database) -> Result<ChatGptAiSettings, String> {
    let stored = database
        .preference(AI_SETTINGS_KEY)
        .map_err(|_| "无法读取AI隐私设置。".to_string())?;
    stored
        .and_then(|json| serde_json::from_str::<ChatGptAiSettings>(&json).ok())
        .unwrap_or_default()
        .normalized()
}

fn http_client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "无法初始化AI网络客户端。".to_string())
}

#[derive(Deserialize)]
struct OllamaTags {
    #[serde(default)]
    models: Vec<OllamaModel>,
}

#[derive(Deserialize)]
struct OllamaModel {
    name: String,
}

async fn test_ollama(
    client: &reqwest::Client,
    settings: &AiSettings,
) -> Result<AiConnectionResult, String> {
    let url = format!(
        "{}/api/tags",
        settings.ollama_endpoint.trim_end_matches('/')
    );
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| "连接不到本机Ollama，请确认服务正在运行。".to_string())?;
    if !response.status().is_success() {
        return Err(provider_status_message(
            AiProvider::Ollama,
            response.status(),
        ));
    }
    let tags: OllamaTags = response
        .json()
        .await
        .map_err(|_| "Ollama返回了无法识别的模型列表。".to_string())?;
    let available_models = tags
        .models
        .into_iter()
        .map(|model| model.name)
        .take(100)
        .collect::<Vec<_>>();
    let model_available = !settings.model.is_empty()
        && available_models
            .iter()
            .any(|model| model == &settings.model);
    let message = if settings.model.is_empty() {
        if available_models.is_empty() {
            "Ollama已连接，但没有发现已安装模型。".into()
        } else {
            format!(
                "Ollama已连接，发现{}个模型；请填写一个模型名称。",
                available_models.len()
            )
        }
    } else if model_available {
        format!("Ollama已连接，模型“{}”可用。", settings.model)
    } else {
        format!("Ollama已连接，但没有找到模型“{}”。", settings.model)
    };
    Ok(AiConnectionResult {
        provider: AiProvider::Ollama,
        model: settings.model.clone(),
        service_available: true,
        model_available,
        message,
        available_models,
    })
}

#[derive(Deserialize)]
struct OpenRouterModelList {
    #[serde(default)]
    data: Vec<OpenRouterModel>,
}

#[derive(Deserialize)]
struct OpenRouterModel {
    id: String,
}

async fn test_openrouter(
    client: &reqwest::Client,
    settings: &AiSettings,
) -> Result<AiConnectionResult, String> {
    let key =
        read_openrouter_key()?.ok_or_else(|| "请先在设置中保存OpenRouter API Key。".to_string())?;
    if settings.model.is_empty() {
        let response = client
            .get("https://openrouter.ai/api/v1/key")
            .bearer_auth(key)
            .send()
            .await
            .map_err(|_| "连接OpenRouter失败，请检查网络。".to_string())?;
        if !response.status().is_success() {
            return Err(provider_status_message(
                AiProvider::OpenRouter,
                response.status(),
            ));
        }
        return Ok(AiConnectionResult {
            provider: AiProvider::OpenRouter,
            model: String::new(),
            service_available: true,
            model_available: false,
            message: "OpenRouter API Key有效；请填写模型ID。".into(),
            available_models: Vec::new(),
        });
    }
    let mut models_url = reqwest::Url::parse("https://openrouter.ai/api/v1/models")
        .map_err(|_| "无法初始化OpenRouter模型目录地址。".to_string())?;
    models_url
        .query_pairs_mut()
        .append_pair("q", &settings.model)
        .append_pair("output_modalities", "text");
    let response = client
        .get(models_url)
        .bearer_auth(key)
        .send()
        .await
        .map_err(|_| "连接OpenRouter失败，请检查网络。".to_string())?;
    if !response.status().is_success() {
        return Err(provider_status_message(
            AiProvider::OpenRouter,
            response.status(),
        ));
    }
    let models: OpenRouterModelList = response
        .json()
        .await
        .map_err(|_| "OpenRouter返回了无法识别的模型列表。".to_string())?;
    let available_models = models
        .data
        .iter()
        .map(|model| model.id.clone())
        .take(20)
        .collect::<Vec<_>>();
    let model_available = models.data.iter().any(|model| model.id == settings.model);
    let message = if model_available {
        format!("OpenRouter已连接，模型“{}”可用。", settings.model)
    } else {
        format!(
            "OpenRouter已连接，但目录中没有找到模型“{}”。",
            settings.model
        )
    };
    Ok(AiConnectionResult {
        provider: AiProvider::OpenRouter,
        model: settings.model.clone(),
        service_available: true,
        model_available,
        message,
        available_models,
    })
}

#[tauri::command]
pub async fn recommend_books(
    app: AppHandle,
    database: State<'_, Arc<Database>>,
    auth_manager: State<'_, Arc<ChatGptAuthManager>>,
    requests: State<'_, Arc<AiRequestManager>>,
    query: String,
) -> Result<BookRecommendationResult, String> {
    let settings = load_chatgpt_ai_settings(&database)?;
    if !settings.enabled {
        return Err("请先在偏好设置中启用AI，再使用ChatGPT书籍推荐。".into());
    }
    if !settings.has_current_privacy_acceptance() {
        return Err("AI数据使用说明尚未确认；请在偏好设置中阅读并保存隐私许可后重试。".into());
    }
    let model_slug = settings
        .selected_model_slug
        .clone()
        .ok_or_else(|| "尚未选择ChatGPT模型，请先刷新当前账号的模型目录。".to_string())?;

    let shelf = database
        .list_books("recent")
        .map_err(|_| "无法从本机数据库读取书架元数据；没有发送任何书籍信息。".to_string())?
        .into_iter()
        .map(|book| RecommendationShelfBook {
            title: book.title,
            author: book.author,
            chapter_count: book.chapter_count,
            progress_ratio: book.progress_ratio,
        })
        .collect::<Vec<_>>();
    let messages = build_book_recommendation_messages(&query, &shelf)?;
    let request_id = format!("recommend-{}", uuid::Uuid::new_v4().simple());
    let request_manager = Arc::clone(requests.inner());
    let auth_manager = Arc::clone(auth_manager.inner());
    let database = Arc::clone(database.inner());
    let (cancelled, mut cancel_rx) = request_manager.register(&request_id)?;

    let result = async {
        let latest = load_chatgpt_ai_settings(&database)?;
        if !latest.enabled
            || !latest.has_current_privacy_acceptance()
            || latest.selected_model_slug.as_deref() != Some(&model_slug)
        {
            return Err("AI许可或模型选择在发送前已更改；本次推荐请求未发送。".into());
        }
        let store = siwc::store_for_app(&app)?;
        let mut access_token = auth_manager.access_token(&store).await?;
        let request_result = async {
            let client = http_client(Duration::from_secs(120))?;
            let response_result = chatgpt_response_with_current_model(
                &client,
                &access_token,
                &model_slug,
                &messages,
                ResponsesSearchMode::WebSearch,
                &cancelled,
                &mut cancel_rx,
            )
            .await;
            if let Err(error) = &response_result {
                if is_confirmed_auth_failure(error) {
                    auth_manager.clear_confirmed_invalid_tokens(&store).await?;
                }
            }
            let response = response_result?
                .ok_or_else(|| "书籍推荐请求已取消；没有生成推荐结果。".to_string())?;
            let mut answer = String::new();
            let completion_result = consume_chatgpt_response_stream(
                response,
                &request_id,
                Arc::clone(&cancelled),
                &mut cancel_rx,
                |text| answer.push_str(&text),
            )
            .await;
            if let Err(error) = &completion_result {
                if is_confirmed_auth_failure(error) {
                    auth_manager.clear_confirmed_invalid_tokens(&store).await?;
                }
            }
            if completion_result? || cancelled.load(Ordering::Acquire) {
                return Err("书籍推荐请求已取消；没有生成推荐结果。".into());
            }
            let items = parse_book_recommendations(&answer, &shelf)?;
            let message = if items.is_empty() {
                "没有找到合适的书籍；可以补充作者、主题或内容后重新查找。".into()
            } else {
                format!("已结合你的兴趣和书架信息联网查找到{}本书。", items.len())
            };
            Ok(BookRecommendationResult { items, message })
        }
        .await;
        access_token.zeroize();
        request_result
    }
    .await;
    request_manager.finish(&request_id);
    result
}

pub fn daily_quote_ai_enabled(database: &Database) -> bool {
    load_chatgpt_ai_settings(database)
        .map(|settings| settings.can_use_ai_daily_quote() && settings.selected_model_slug.is_some())
        .unwrap_or(false)
}

pub async fn select_chatgpt_daily_quote_candidate(
    app: &AppHandle,
    database: &Database,
    auth_manager: &ChatGptAuthManager,
    requests: &AiRequestManager,
    candidates: &[DailyQuoteCandidate],
) -> Result<Option<String>, String> {
    let settings = load_chatgpt_ai_settings(database)?;
    if !settings.can_use_ai_daily_quote() {
        return Ok(None);
    }
    let model_slug = settings
        .selected_model_slug
        .clone()
        .ok_or_else(|| "尚未选择ChatGPT模型，请先刷新当前账号的模型目录。".to_string())?;
    let messages = build_daily_quote_selection_messages(candidates)?;
    let request_id = format!("quote-{}", uuid::Uuid::new_v4().simple());
    let (cancelled, mut cancel_rx) = requests.register(&request_id)?;
    let result = async {
        let latest = load_chatgpt_ai_settings(database)?;
        if !latest.can_use_ai_daily_quote()
            || latest.selected_model_slug.as_deref() != Some(&model_slug)
        {
            return Ok(None);
        }
        let store = siwc::store_for_app(app)?;
        let mut access_token = auth_manager.access_token(&store).await?;
        let request_result = async {
            let client = http_client(Duration::from_secs(120))?;
            let response_result = chatgpt_response_with_current_model(
                &client,
                &access_token,
                &model_slug,
                &messages,
                ResponsesSearchMode::Disabled,
                &cancelled,
                &mut cancel_rx,
            )
            .await;
            if let Err(error) = &response_result {
                if is_confirmed_auth_failure(error) {
                    auth_manager.clear_confirmed_invalid_tokens(&store).await?;
                }
            }
            let Some(response) = response_result? else {
                return Ok(None);
            };
            let mut completion = String::new();
            let completion_result = consume_chatgpt_response_stream(
                response,
                &request_id,
                Arc::clone(&cancelled),
                &mut cancel_rx,
                |text| completion.push_str(&text),
            )
            .await;
            if let Err(error) = &completion_result {
                if is_confirmed_auth_failure(error) {
                    auth_manager.clear_confirmed_invalid_tokens(&store).await?;
                }
            }
            let completed = completion_result?;
            if completed || cancelled.load(Ordering::Acquire) {
                return Ok(None);
            }
            parse_daily_quote_candidate_id(&completion, candidates)
                .map(Some)
                .ok_or_else(|| {
                    "模型已完成，但返回的今日摘录候选编号无效；本机原文摘录已保留。".into()
                })
        }
        .await;
        access_token.zeroize();
        request_result
    }
    .await;
    requests.finish(&request_id);
    result
}

pub async fn select_daily_quote_candidate(
    database: &Database,
    candidates: &[DailyQuoteCandidate],
) -> Result<Option<String>, String> {
    let settings = load_settings(database)?;
    if !settings.enabled {
        return Ok(None);
    }
    settings.validate_ready()?;
    let messages = build_daily_quote_selection_messages(candidates)?;
    let api_key = match settings.provider {
        AiProvider::Ollama => None,
        AiProvider::OpenRouter => Some(
            read_openrouter_key()?
                .ok_or_else(|| "请先在设置中保存OpenRouter API Key。".to_string())?,
        ),
    };
    let client = http_client(Duration::from_secs(45))?;
    let (url, body) = match settings.provider {
        AiProvider::Ollama => {
            let candidate_ids = candidates
                .iter()
                .map(|candidate| candidate.candidate_id.clone())
                .collect::<Vec<_>>();
            (
                format!(
                    "{}/api/chat",
                    settings.ollama_endpoint.trim_end_matches('/')
                ),
                build_ollama_quote_request(&settings.model, &messages, &candidate_ids),
            )
        }
        AiProvider::OpenRouter => (
            "https://openrouter.ai/api/v1/chat/completions".to_owned(),
            serde_json::json!({ "model": settings.model, "messages": messages, "stream": false, "temperature": 0, "max_tokens": 80, "response_format": { "type": "json_object" } }),
        ),
    };
    let mut request = client.post(url).json(&body);
    if let Some(key) = api_key.as_deref() {
        request = request.bearer_auth(key);
    }
    let response = request
        .send()
        .await
        .map_err(|_| provider_network_message(settings.provider))?;
    if !response.status().is_success() {
        return Err(provider_status_message(
            settings.provider,
            response.status(),
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > 16_384)
    {
        return Err("模型返回内容过长，已忽略本次AI选择。".into());
    }
    let body = response
        .bytes()
        .await
        .map_err(|_| "读取模型响应失败。".to_string())?;
    if body.len() > 16_384 {
        return Err("模型返回内容过长，已忽略本次AI选择。".into());
    }
    let value: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| "无法解析模型返回的今日摘录选择。".to_string())?;
    let completion = completion_text(settings.provider, &value)
        .ok_or_else(|| "模型没有返回有效的摘录候选编号。".to_string())?;
    Ok(parse_daily_quote_candidate_id(completion, candidates))
}

fn build_ollama_quote_request(
    model: &str,
    messages: &[AiMessage],
    candidate_ids: &[String],
) -> serde_json::Value {
    let is_gpt_oss = model.trim().to_ascii_lowercase().starts_with("gpt-oss");
    let think = if is_gpt_oss {
        // gpt-oss may return an empty content field when thinking is disabled.
        serde_json::json!("low")
    } else {
        serde_json::json!(false)
    };
    let num_predict = if is_gpt_oss { 128 } else { 80 };
    serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
        "think": think,
        "format": {
            "type": "object",
            "properties": {
                "candidate_id": {
                    "type": "string",
                    "enum": candidate_ids,
                }
            },
            "required": ["candidate_id"],
            "additionalProperties": false,
        },
        "options": { "temperature": 0, "num_predict": num_predict },
    })
}

fn completion_text(provider: AiProvider, value: &serde_json::Value) -> Option<&str> {
    match provider {
        AiProvider::Ollama => value.pointer("/message/content").and_then(|v| v.as_str()),
        AiProvider::OpenRouter => value
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str()),
    }
}

fn provider_status_message(provider: AiProvider, status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 | 403 if provider == AiProvider::OpenRouter => {
            "OpenRouter API Key无效或无权访问。".into()
        }
        404 => "服务地址或模型不存在，请核对配置。".into(),
        429 => "请求频率或账户额度受限，请稍后重试。".into(),
        code => format!("模型服务返回HTTP状态码{code}。"),
    }
}

fn emit(app: &AppHandle, event: AiStreamEvent) {
    let _ = app.emit(STREAM_EVENT, event);
}

async fn stream_explanation(
    app: &AppHandle,
    request_id: &str,
    settings: &AiSettings,
    messages: &[crate::ai::AiMessage],
    api_key: Option<&str>,
    cancelled: Arc<AtomicBool>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<bool, String> {
    let client = http_client(Duration::from_secs(120))?;
    let (url, protocol) = match settings.provider {
        AiProvider::Ollama => (
            format!(
                "{}/api/chat",
                settings.ollama_endpoint.trim_end_matches('/')
            ),
            StreamProtocol::Ollama,
        ),
        AiProvider::OpenRouter => (
            "https://openrouter.ai/api/v1/chat/completions".into(),
            StreamProtocol::OpenRouter,
        ),
    };
    let body = match settings.provider {
        AiProvider::Ollama => {
            serde_json::json!({ "model": settings.model, "messages": messages, "stream": true, "options": { "temperature": 0.2 } })
        }
        AiProvider::OpenRouter => {
            serde_json::json!({ "model": settings.model, "messages": messages, "stream": true, "temperature": 0.2, "max_tokens": 1200 })
        }
    };
    let mut request = client.post(url).json(&body);
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }
    let response = tokio::select! {
        result = request.send() => result.map_err(|_| provider_network_message(settings.provider))?,
        _ = cancel_rx.changed() => return Ok(cancelled.load(Ordering::Acquire)),
    };
    if !response.status().is_success() {
        return Err(provider_status_message(
            settings.provider,
            response.status(),
        ));
    }
    let mut stream = response.bytes_stream();
    let mut decoder = StreamDecoder::new(protocol);
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Ok(true);
        }
        let next = tokio::select! {
            result = stream.next() => result,
            _ = cancel_rx.changed() => {
                if cancelled.load(Ordering::Acquire) { return Ok(true); }
                continue;
            },
        };
        match next {
            Some(Ok(chunk)) => {
                for item in decoder.push_chunk(&chunk)? {
                    match item {
                        AiStreamItem::Token(text) => {
                            emit(app, AiStreamEvent::token(request_id, text))
                        }
                        AiStreamItem::Citation(_) => {}
                        AiStreamItem::Done => return Ok(false),
                    }
                }
            }
            Some(Err(_)) => return Err("模型连接中断，请重试。".into()),
            None => break,
        }
    }
    for item in decoder.finish()? {
        if let AiStreamItem::Token(text) = item {
            emit(app, AiStreamEvent::token(request_id, text));
        }
    }
    Ok(false)
}

async fn stream_chatgpt_response(
    app: &AppHandle,
    request_id: &str,
    model_slug: &str,
    messages: &[AiMessage],
    access_token: &str,
    auth_manager: &ChatGptAuthManager,
    store: &crate::siwc_store::CredentialStore,
    cancelled: Arc<AtomicBool>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<bool, String> {
    let client = http_client(Duration::from_secs(120))?;
    let response_result = chatgpt_response_with_current_model(
        &client,
        access_token,
        model_slug,
        messages,
        ResponsesSearchMode::WebSearch,
        &cancelled,
        cancel_rx,
    )
    .await;
    if let Err(error) = &response_result {
        if is_confirmed_auth_failure(error) {
            auth_manager.clear_confirmed_invalid_tokens(store).await?;
        }
    }
    let Some(response) = response_result? else {
        return Ok(true);
    };
    let stream_result = consume_chatgpt_response_stream_with_citations(
        response,
        request_id,
        cancelled,
        cancel_rx,
        |text| emit(app, AiStreamEvent::token(request_id, text)),
        |citation| emit(app, AiStreamEvent::citation(request_id, citation)),
    )
    .await;
    if let Err(error) = &stream_result {
        if is_confirmed_auth_failure(error) {
            auth_manager.clear_confirmed_invalid_tokens(store).await?;
        }
    }
    stream_result
}

async fn chatgpt_response_with_current_model(
    client: &reqwest::Client,
    access_token: &str,
    model_slug: &str,
    messages: &[AiMessage],
    search_mode: ResponsesSearchMode,
    cancelled: &AtomicBool,
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<Option<ChatGptResponseStream>, String> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    tokio::select! {
        _ = cancel_rx.changed() => Ok(None),
        result = send_chatgpt_responses_with_mode(client, CHATGPT_RESPONSES_URL, access_token, model_slug, messages, search_mode) => result.map(Some),
    }
}

async fn consume_chatgpt_response_stream<F>(
    response: ChatGptResponseStream,
    request_id: &str,
    cancelled: Arc<AtomicBool>,
    cancel_rx: &mut watch::Receiver<bool>,
    on_token: F,
) -> Result<bool, String>
where
    F: FnMut(String),
{
    consume_chatgpt_response_stream_with_citations(
        response,
        request_id,
        cancelled,
        cancel_rx,
        on_token,
        |_| {},
    )
    .await
}

async fn consume_chatgpt_response_stream_with_citations<F>(
    mut response: ChatGptResponseStream,
    request_id: &str,
    cancelled: Arc<AtomicBool>,
    cancel_rx: &mut watch::Receiver<bool>,
    mut on_token: F,
    mut on_citation: impl FnMut(AiCitation),
) -> Result<bool, String>
where
    F: FnMut(String),
{
    let request_id_header = response
        .response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .map(|value| safe_openai_detail(value, 128))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if response.client_request_id.is_empty() {
                request_id.to_owned()
            } else {
                response.client_request_id.clone()
            }
        });
    let mut decoder =
        StreamDecoder::with_request_id(StreamProtocol::Responses, request_id_header.clone());
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Ok(true);
        }
        let next = tokio::select! {
            result = response.next_chunk() => result,
            _ = cancel_rx.changed() => {
                if cancelled.load(Ordering::Acquire) { return Ok(true); }
                continue;
            },
        };
        match next {
            Ok(Some(chunk)) => {
                for item in decoder.push_chunk(&chunk)? {
                    match item {
                        AiStreamItem::Token(text) => on_token(text),
                        AiStreamItem::Citation(citation) => on_citation(citation),
                        AiStreamItem::Done => return Ok(false),
                    }
                }
            }
            Err(error) => {
                let cause = network_failure("读取OpenAI Responses数据流", &error);
                let cause = cause.strip_suffix('。').unwrap_or(&cause);
                return Err(format!("{cause}（请求编号：{request_id_header}）。"));
            }
            Ok(None) => break,
        }
    }
    for item in decoder.finish()? {
        match item {
            AiStreamItem::Token(text) => on_token(text),
            AiStreamItem::Citation(citation) => on_citation(citation),
            AiStreamItem::Done => {}
        }
    }
    Ok(false)
}

async fn send_chatgpt_responses(
    client: &reqwest::Client,
    endpoint: &str,
    access_token: &str,
    model_slug: &str,
    messages: &[AiMessage],
) -> Result<ChatGptResponseStream, String> {
    send_chatgpt_responses_with_mode(
        client,
        endpoint,
        access_token,
        model_slug,
        messages,
        ResponsesSearchMode::Disabled,
    )
    .await
}

async fn send_chatgpt_responses_with_mode(
    client: &reqwest::Client,
    endpoint: &str,
    access_token: &str,
    model_slug: &str,
    messages: &[AiMessage],
    search_mode: ResponsesSearchMode,
) -> Result<ChatGptResponseStream, String> {
    let body = build_responses_request(model_slug, messages, search_mode)?;
    let client_request_id = uuid::Uuid::new_v4().to_string();
    let response = client
        .post(endpoint)
        .bearer_auth(access_token)
        .header("x-client-request-id", &client_request_id)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .header(reqwest::header::ACCEPT_ENCODING, "identity")
        .json(&body)
        .send()
        .await
        .map_err(|error| network_failure("OpenAI Responses API", &error))?;
    if !response.status().is_success() {
        return Err(read_openai_error(response, "Responses").await);
    }
    if !response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"))
    {
        let status = response.status();
        let content_type_header_present = response
            .headers()
            .contains_key(reqwest::header::CONTENT_TYPE);
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(safe_media_type)
            .unwrap_or_else(|| "未提供".into());
        let content_encoding = response
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .and_then(|value| value.to_str().ok())
            .map(safe_content_encoding)
            .unwrap_or_else(|| "未提供".into());
        let request_id = response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .map(|value| safe_openai_detail(value, 128))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "未提供".into());
        let body_kind = if content_type_header_present {
            None
        } else {
            let mut response = response;
            let mut prefix = Vec::new();
            loop {
                match responses_sse_probe_state(&prefix) {
                    SseProbeState::ValidResponsesEvent => {
                        return Ok(ChatGptResponseStream {
                            response,
                            initial_chunk: Some(prefix),
                            client_request_id,
                        });
                    }
                    SseProbeState::NotSse(kind) => break Some(kind),
                    SseProbeState::NeedMore if prefix.len() >= MAX_RESPONSES_SSE_PROBE_BYTES => {
                        break Some("未知格式");
                    }
                    SseProbeState::NeedMore => match response.chunk().await {
                        Ok(Some(chunk)) => {
                            if prefix.len().saturating_add(chunk.len())
                                > MAX_RESPONSES_SSE_PROBE_BYTES
                            {
                                break Some("未知格式");
                            }
                            prefix.extend_from_slice(&chunk);
                        }
                        Ok(None) => {
                            break Some(if prefix.is_empty() {
                                "空响应"
                            } else {
                                "未知格式"
                            })
                        }
                        Err(error) => {
                            return Err(network_failure("检查OpenAI Responses响应格式", &error));
                        }
                    },
                }
            }
        };
        return Err(responses_stream_format_error(
            status,
            &content_type,
            &content_encoding,
            body_kind,
            &request_id,
            &client_request_id,
        ));
    }
    Ok(ChatGptResponseStream {
        response,
        initial_chunk: None,
        client_request_id,
    })
}

enum SseProbeState {
    ValidResponsesEvent,
    NeedMore,
    NotSse(&'static str),
}

fn responses_sse_probe_state(prefix: &[u8]) -> SseProbeState {
    const GZIP_SIGNATURE: [u8; 2] = [0x1f, 0x8b];
    const ZSTD_SIGNATURE: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
    if (prefix.len() < GZIP_SIGNATURE.len() && GZIP_SIGNATURE.starts_with(prefix))
        || (prefix.len() < ZSTD_SIGNATURE.len() && ZSTD_SIGNATURE.starts_with(prefix))
    {
        return SseProbeState::NeedMore;
    }
    if prefix.starts_with(&[0x1f, 0x8b]) {
        return SseProbeState::NotSse("gzip压缩数据");
    }
    if prefix.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        return SseProbeState::NotSse("Zstandard压缩数据");
    }
    let text = match std::str::from_utf8(prefix) {
        Ok(text) => text,
        Err(error) if error.error_len().is_none() => return SseProbeState::NeedMore,
        Err(_) => return SseProbeState::NotSse("非UTF-8数据"),
    };
    let text = text.trim_start_matches('\u{feff}').trim_start();
    if text.is_empty() {
        return SseProbeState::NeedMore;
    }
    if text.starts_with('{') || text.starts_with('[') {
        return SseProbeState::NotSse("JSON");
    }
    if text.starts_with('<') {
        return SseProbeState::NotSse("HTML或网关页面");
    }
    if !(text.starts_with("data:")
        || text.starts_with("event:")
        || text.starts_with(':')
        || text.starts_with("retry:"))
    {
        return SseProbeState::NotSse("未知格式");
    }
    for line in text.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(data.trim()) else {
            continue;
        };
        let Some(event_type) = value.get("type").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if event_type.starts_with("response.") || event_type == "error" {
            return SseProbeState::ValidResponsesEvent;
        }
    }
    SseProbeState::NeedMore
}

fn responses_stream_format_error(
    status: reqwest::StatusCode,
    content_type: &str,
    content_encoding: &str,
    body_kind: Option<&str>,
    request_id: &str,
    client_request_id: &str,
) -> String {
    let body_detail = body_kind
        .map(|kind| format!("；响应体探测：{kind}"))
        .unwrap_or_default();
    format!(
        "Responses请求收到HTTP成功状态，但未获得可处理的SSE事件流：HTTP {}，Content-Type：{}，Content-Encoding：{}{body_detail}；书隅已请求stream=true和Accept-Encoding: identity。请检查服务响应及Windows系统代理、VPN或安全网关是否压缩或改写SSE流。OpenAI请求编号：{request_id}；X-Client-Request-Id：{client_request_id}。",
        status.as_u16(),
        content_type,
        content_encoding,
    )
}

fn safe_media_type(value: &str) -> String {
    let media_type = value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !media_type.is_empty()
        && media_type.len() <= 128
        && media_type
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'+' | b'-' | b'.'))
    {
        media_type
    } else {
        "无法识别".into()
    }
}

fn safe_content_encoding(value: &str) -> String {
    let encodings: Vec<&str> = value.split(',').map(str::trim).collect();
    if encodings.is_empty()
        || encodings.len() > 4
        || encodings.iter().any(|encoding| {
            encoding.is_empty()
                || encoding.len() > 32
                || !encoding
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        return "无法识别".into();
    }
    encodings.join(", ").to_ascii_lowercase()
}

fn network_failure(operation: &str, error: &reqwest::Error) -> String {
    let cause = if error.is_timeout() {
        "连接或响应等待超时，请检查网络和系统代理后重试"
    } else if error.is_connect() {
        "无法建立TLS连接，请检查网络、防火墙、系统代理及证书状态"
    } else if error.is_body() {
        "读取网络响应时连接中断"
    } else {
        "请求传输失败，请检查网络、系统代理和TLS证书状态"
    };
    format!("{operation}{cause}。")
}

fn is_confirmed_auth_failure(message: &str) -> bool {
    message.contains("HTTP 401") || message.contains("ChatGPT账号授权已失效")
}

fn safe_openai_detail(value: &str, max_bytes: usize) -> String {
    value
        .bytes()
        .take(max_bytes)
        .filter(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'[' | b']')
        })
        .map(char::from)
        .collect()
}

fn provider_network_message(provider: AiProvider) -> String {
    match provider {
        AiProvider::Ollama => "连接本机Ollama失败，请确认服务正在运行。".into(),
        AiProvider::OpenRouter => "连接OpenRouter失败，请检查网络和API Key。".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_privacy_acceptance, build_ollama_quote_request, completion_text,
        consume_chatgpt_response_stream, fetch_chatgpt_models, openai_error_message,
        parse_chatgpt_models, require_listed_chatgpt_model, responses_sse_probe_state,
        responses_stream_format_error, safe_content_encoding, send_chatgpt_responses,
        send_chatgpt_responses_with_mode, AiRequestManager, ResponsesSearchMode, SseProbeState,
    };
    use crate::ai::{AiProvider, AiStreamItem, ChatGptAiSettings, StreamDecoder, StreamProtocol};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::watch,
    };

    #[test]
    fn chatgpt_model_catalog_returns_only_visible_list_models_and_deduplicates_slugs() {
        let body = br#"{"models":[
            {"slug":"gpt-one","display_name":"GPT One","visibility":"list"},
            {"slug":"gpt-hidden","display_name":"Hidden","visibility":"hide"},
            {"slug":"gpt-one","display_name":"Duplicate","visibility":"list"},
            {"slug":"","display_name":"Invalid","visibility":"list"}
        ]}"#;

        let models = parse_chatgpt_models(body).unwrap();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].slug, "gpt-one");
        assert_eq!(models[0].display_name, "GPT One");
    }

    #[test]
    fn chatgpt_catalog_does_not_invent_fallback_models_when_empty_or_invalid() {
        assert!(parse_chatgpt_models(br#"{"models":[]}"#)
            .unwrap()
            .is_empty());
        assert!(parse_chatgpt_models(br#"{"data":[{"id":"static-model"}]}"#).is_err());
    }

    #[test]
    fn chatgpt_request_refuses_a_model_removed_from_the_live_catalog() {
        let models = parse_chatgpt_models(
            br#"{"models":[{"slug":"gpt-current","display_name":"Current","visibility":"list"}]}"#,
        )
        .unwrap();
        assert!(require_listed_chatgpt_model(&models, "gpt-current").is_ok());
        assert!(require_listed_chatgpt_model(&models, "gpt-stale")
            .unwrap_err()
            .contains("重新选择"));
    }

    #[tokio::test]
    async fn local_http_mock_checks_live_model_catalog_route_and_bearer_header() {
        let (endpoint, captured) = mock_http_once(
            "200 OK",
            "application/json",
            r#"{"models":[{"slug":"gpt-local-mock","display_name":"Mock model","visibility":"list"},{"slug":"hidden","display_name":"Hidden","visibility":"hide"}]}"#,
        )
        .await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();

        let models = fetch_chatgpt_models(&client, &endpoint, "synthetic-test-token")
            .await
            .unwrap();
        let request = captured.await.unwrap();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].slug, "gpt-local-mock");
        assert!(request.starts_with("GET /v1/models HTTP/1.1"));
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer synthetic-test-token"));
    }

    #[tokio::test]
    async fn local_http_mock_checks_responses_body_sse_completion_and_usage_limit_error() {
        let (endpoint, captured) = mock_http_once(
            "200 OK",
            "text/event-stream",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"SYNTHETIC_OK\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        )
        .await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let messages =
            crate::ai::build_explanation_messages("synthetic selected passage", "test").unwrap();
        let response = send_chatgpt_responses(
            &client,
            &endpoint,
            "synthetic-test-token",
            "gpt-local-mock",
            &messages,
        )
        .await
        .unwrap();
        let request = captured.await.unwrap();
        let body_start = request.find("\r\n\r\n").unwrap() + 4;
        let body: serde_json::Value = serde_json::from_str(&request[body_start..]).unwrap();
        assert!(request.starts_with("POST /v1/responses HTTP/1.1"));
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer synthetic-test-token"));
        assert!(request
            .to_ascii_lowercase()
            .contains("x-client-request-id: "));
        assert!(request
            .to_ascii_lowercase()
            .contains("accept: text/event-stream"));
        assert!(request
            .to_ascii_lowercase()
            .contains("accept-encoding: identity"));
        assert_eq!(body["model"], "gpt-local-mock");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert!(body.to_string().contains("synthetic selected passage"));
        for forbidden in ["previous_response_id", "tools", "metadata"] {
            assert!(body.get(forbidden).is_none());
        }

        let mut decoder = StreamDecoder::new(StreamProtocol::Responses);
        let mut events = Vec::new();
        let mut response = response;
        while let Some(chunk) = response.next_chunk().await.unwrap() {
            events.extend(decoder.push_chunk(&chunk).unwrap());
        }
        events.extend(decoder.finish().unwrap());
        assert!(events.contains(&AiStreamItem::Token("SYNTHETIC_OK".into())));
        assert_eq!(events.last(), Some(&AiStreamItem::Done));

        let (endpoint, _) = mock_http_once(
            "429 Too Many Requests",
            "application/json",
            r#"{"error":{"code":"subscription_sharing_usage_limit_exceeded","param":"model"}}"#,
        )
        .await;
        let error = send_chatgpt_responses(
            &client,
            &endpoint,
            "synthetic-test-token",
            "gpt-local-mock",
            &messages,
        )
        .await
        .unwrap_err();
        assert!(error.contains("用量上限"));
        assert!(error.contains("HTTP 429"));
    }

    #[tokio::test]
    async fn web_search_request_streams_recommendation_json_without_citation_payloads() {
        let (endpoint, captured) = mock_http_once(
            "200 OK",
            "text/event-stream",
            r#"data: {"type":"response.output_text.delta","delta":"{\"recommendations\":[{\"title\":\"New Book\",\"author\":\"New Author\",\"reason\":\"Similar theme\"}]}"}
data: {"type":"response.completed","response":{}}

"#,
        )
        .await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let messages =
            crate::ai::build_book_recommendation_messages("similar themes", &[]).unwrap();
        let response = send_chatgpt_responses_with_mode(
            &client,
            &endpoint,
            "synthetic-test-token",
            "gpt-local-mock",
            &messages,
            ResponsesSearchMode::WebSearch,
        )
        .await
        .unwrap();
        let request = captured.await.unwrap();
        let body_start = request.find("\r\n\r\n").unwrap() + 4;
        let body: serde_json::Value = serde_json::from_str(&request[body_start..]).unwrap();
        assert_eq!(body["tools"], serde_json::json!([{ "type": "web_search" }]));
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);

        let (_cancel_sender, mut cancel_rx) = watch::channel(false);
        let mut answer = String::new();
        let completed = consume_chatgpt_response_stream(
            response,
            "synthetic-recommendation-request",
            Arc::new(AtomicBool::new(false)),
            &mut cancel_rx,
            |text| answer.push_str(&text),
        )
        .await
        .unwrap();
        assert!(!completed);
        let recommendations = crate::ai::parse_book_recommendations(&answer, &[]).unwrap();
        assert_eq!(recommendations.len(), 1);
        assert_eq!(recommendations[0].title, "New Book");
    }

    #[tokio::test]
    async fn non_streaming_success_response_reports_status_and_content_type_without_echoing_body() {
        let (endpoint, _) = mock_http_once(
            "200 OK",
            "application/json",
            r#"{"id":"resp-sensitive-fixture","object":"response","status":"completed"}"#,
        )
        .await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let messages = crate::ai::build_explanation_messages("synthetic passage", "test").unwrap();

        let error = send_chatgpt_responses(
            &client,
            &endpoint,
            "synthetic-test-token",
            "gpt-local-mock",
            &messages,
        )
        .await
        .unwrap_err();

        assert!(error.contains("HTTP 200"));
        assert!(error.contains("application/json"));
        assert!(error.contains("stream=true"));
        assert!(!error.contains("resp-sensitive-fixture"));
    }

    #[test]
    fn responses_format_error_reports_safe_encoding_and_compression_probe() {
        let error = responses_stream_format_error(
            reqwest::StatusCode::OK,
            "未提供",
            &safe_content_encoding("gzip"),
            Some("gzip压缩数据"),
            "req-safe",
            "client-safe",
        );

        assert!(error.contains("Content-Encoding：gzip"));
        assert!(error.contains("响应体探测：gzip压缩数据"));
        assert!(error.contains("Accept-Encoding: identity"));
        assert!(!error.contains("authorization"));
    }

    #[test]
    fn responses_sse_probe_recognizes_compressed_binary_without_exposing_payload() {
        assert!(matches!(
            responses_sse_probe_state(&[0x1f, 0x8b, 0x00, 0xff]),
            SseProbeState::NotSse("gzip压缩数据")
        ));
        assert!(matches!(
            responses_sse_probe_state(&[0x28, 0xb5, 0x2f, 0xfd, 0xff]),
            SseProbeState::NotSse("Zstandard压缩数据")
        ));
    }

    #[test]
    fn responses_sse_probe_waits_for_a_split_utf8_character() {
        let stream = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"读\"}\n\n".as_bytes();
        let character = stream
            .windows(3)
            .position(|bytes| bytes == "读".as_bytes())
            .unwrap();
        let split = character + 2;

        assert!(matches!(
            responses_sse_probe_state(&stream[..split]),
            SseProbeState::NeedMore
        ));
        assert!(matches!(
            responses_sse_probe_state(stream),
            SseProbeState::ValidResponsesEvent
        ));
    }

    #[tokio::test]
    async fn responses_event_stream_without_content_type_is_consumed_to_completion() {
        let (endpoint, captured) = mock_http_once(
            "200 OK",
            "",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"SSE_OK\"}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{}}\n\n",
        )
        .await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let messages = crate::ai::build_explanation_messages("synthetic passage", "test").unwrap();
        let response = send_chatgpt_responses(
            &client,
            &endpoint,
            "synthetic-test-token",
            "gpt-local-mock",
            &messages,
        )
        .await
        .unwrap();
        let (_cancel_sender, mut cancel_rx) = watch::channel(false);
        let mut answer = String::new();
        let completed = consume_chatgpt_response_stream(
            response,
            "synthetic-client-request-id",
            Arc::new(AtomicBool::new(false)),
            &mut cancel_rx,
            |token| answer.push_str(&token),
        )
        .await
        .unwrap();
        let request = captured.await.unwrap();

        assert!(!completed);
        assert_eq!(answer, "SSE_OK");
        assert!(request
            .to_ascii_lowercase()
            .contains("x-client-request-id: "));
    }

    async fn mock_http_once(
        status: &'static str,
        content_type: &'static str,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = socket.read(&mut buffer).await.unwrap();
                assert_ne!(read, 0, "client closed before sending a full request");
                request.extend_from_slice(&buffer[..read]);
                let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let content_type_header = if content_type.is_empty() {
                String::new()
            } else {
                format!("Content-Type: {content_type}\r\n")
            };
            let response = format!(
                "HTTP/1.1 {status}\r\n{content_type_header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (
            format!(
                "http://{address}/v1/{}",
                if status == "200 OK" && content_type == "application/json" {
                    "models"
                } else {
                    "responses"
                }
            ),
            server,
        )
    }

    #[test]
    fn openai_errors_report_specific_codes_status_and_safe_request_ids() {
        let usage_limit = openai_error_message(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            Some("subscription_sharing_usage_limit_exceeded"),
            None,
            Some("req-safe"),
        );
        assert!(usage_limit.contains("用量上限"));
        assert!(usage_limit.contains("req-safe"));

        let region = openai_error_message(
            reqwest::StatusCode::FORBIDDEN,
            None,
            None,
            Some("req-forbidden"),
        );
        assert!(region.contains("区域或服务策略"));
        assert!(region.contains("HTTP 403"));

        let unavailable = openai_error_message(
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            Some("subscription_sharing_usage_unavailable"),
            None,
            None,
        );
        assert!(unavailable.contains("暂时无法查询"));
        assert!(unavailable.contains("没有切换到其他计费方式"));
    }

    #[tokio::test]
    async fn cancellation_reaches_only_the_active_request_and_allows_a_new_request() {
        let manager = AiRequestManager::default();
        let (cancelled, mut receiver) = manager.register("request-one").unwrap();
        assert!(manager.register("request-two").is_err());
        assert!(!manager.cancel("stale-request"));
        assert!(manager.cancel("request-one"));
        receiver.changed().await.unwrap();
        assert!(cancelled.load(Ordering::Acquire));

        manager.finish("request-one");
        assert!(!manager.cancel("request-one"));
        assert!(manager.register("request-two").is_ok());
    }

    #[tokio::test]
    async fn revoking_ai_permission_cancels_the_active_request_immediately() {
        let manager = AiRequestManager::default();
        let (cancelled, mut receiver) = manager.register("request-one").unwrap();
        manager.cancel_all();
        receiver.changed().await.unwrap();
        assert!(cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn renderer_cannot_forge_privacy_acceptance_and_each_permission_stays_separate() {
        let default = ChatGptAiSettings::default();
        let forged = ChatGptAiSettings {
            enabled: true,
            allow_ai_qa: true,
            privacy_notice_version: 1,
            privacy_notice_accepted_at: Some("renderer-forged".into()),
            ..ChatGptAiSettings::default()
        };
        assert!(apply_privacy_acceptance(forged, &default, false, "unused".into()).is_err());

        let opted_in = ChatGptAiSettings {
            enabled: true,
            allow_ai_qa: true,
            ..ChatGptAiSettings::default()
        };
        let accepted =
            apply_privacy_acceptance(opted_in, &default, true, "2026-10-09T12:00:00Z".into())
                .unwrap();
        assert!(accepted.can_use_ai_qa());
        assert!(!accepted.can_use_ai_daily_quote());
        assert_eq!(
            accepted.privacy_notice_accepted_at.as_deref(),
            Some("2026-10-09T12:00:00Z")
        );
    }

    #[test]
    fn extracts_non_streaming_quote_selection_from_both_provider_shapes() {
        let ollama = serde_json::json!({ "message": { "content": "{\"candidate_id\":\"Q001\"}" } });
        let openrouter = serde_json::json!({ "choices": [{ "message": { "content": "{\"candidate_id\":\"Q002\"}" } }] });
        let malformed = serde_json::json!({ "choices": [] });

        assert_eq!(
            completion_text(AiProvider::Ollama, &ollama),
            Some(r#"{"candidate_id":"Q001"}"#)
        );
        assert_eq!(
            completion_text(AiProvider::OpenRouter, &openrouter),
            Some(r#"{"candidate_id":"Q002"}"#)
        );
        assert_eq!(completion_text(AiProvider::OpenRouter, &malformed), None);
    }

    #[test]
    fn ollama_quote_request_disables_thinking_and_constrains_candidate_id_schema() {
        let messages = vec![
            crate::ai::AiMessage {
                role: "system".into(),
                content: "select".into(),
            },
            crate::ai::AiMessage {
                role: "user".into(),
                content: "candidates".into(),
            },
        ];
        let candidate_ids = vec!["Q001".to_owned(), "Q002".to_owned()];

        let body = build_ollama_quote_request("qwen3:4b", &messages, &candidate_ids);

        assert_eq!(body["think"], false);
        assert_eq!(body["format"]["type"], "object");
        assert_eq!(
            body["format"]["properties"]["candidate_id"]["enum"],
            serde_json::json!(["Q001", "Q002"])
        );
        assert_eq!(
            body["format"]["required"],
            serde_json::json!(["candidate_id"])
        );
        assert_eq!(body["format"]["additionalProperties"], false);
    }

    #[test]
    fn gpt_oss_quote_request_uses_low_thinking_with_room_for_final_content() {
        let messages = vec![
            crate::ai::AiMessage {
                role: "system".into(),
                content: "select".into(),
            },
            crate::ai::AiMessage {
                role: "user".into(),
                content: "candidates".into(),
            },
        ];
        let candidate_ids = vec!["Q001".to_owned(), "Q002".to_owned()];

        let body = build_ollama_quote_request("gpt-oss:20b", &messages, &candidate_ids);

        assert_eq!(body["think"], "low");
        assert_eq!(body["options"]["num_predict"], 128);
    }
}
