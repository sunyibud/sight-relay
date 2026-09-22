use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::{Client as S3Client, primitives::ByteStream};
use axum::{
    Json, Router,
    extract::ws::{Message, WebSocket},
    extract::{DefaultBodyLimit, Multipart, Path, Query, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex, OnceLock, RwLock},
    time::{Duration, Instant},
};
use tokio::fs;
use tokio::sync::{Semaphore, broadcast};
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;
use uuid::Uuid;
mod auth;
use auth::SessionStore;

const CAPTURE_IMAGE_MAX_BYTES: usize = 10 * 1024 * 1024;
// Multipart boundaries and field headers are outside the image payload.
const CAPTURE_REQUEST_BODY_LIMIT: usize = CAPTURE_IMAGE_MAX_BYTES + 64 * 1024;

/// 创建仅供 LLM 调用使用的 HTTP 客户端。
/// 不配置代理时显式禁用环境代理，避免影响或继承服务器其他网络请求。
fn llm_client_with_timeout(
    timeout: Option<Duration>,
    proxy: &str,
) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder = reqwest::Client::builder().connect_timeout(Duration::from_secs(15));
    let proxy = proxy.trim();
    if proxy.is_empty() {
        builder = builder.no_proxy();
    } else {
        let proxy_url = if proxy.contains("://") {
            proxy.to_string()
        } else {
            format!("http://{proxy}")
        };
        builder = builder.proxy(reqwest::Proxy::all(proxy_url)?);
    }
    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }
    builder.build()
}
fn llm_client(proxy: &str) -> Result<reqwest::Client, reqwest::Error> {
    llm_client_with_timeout(None, proxy)
}

fn session(headers: &HeaderMap, s: &AppState) -> Result<auth::Session, StatusCode> {
    let cookie = headers.get(header::COOKIE).and_then(|v| v.to_str().ok());
    auth::cookie_session(cookie, &s.sessions).ok_or(StatusCode::UNAUTHORIZED)
}
fn require_admin(headers: &HeaderMap, s: &AppState) -> Result<auth::Session, StatusCode> {
    let sess = session(headers, s)?;
    if sess.role != auth::Role::Admin {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(sess)
}
fn token_hash(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    format!("{:x}", h.finalize())
}
fn prompt_hash(prompt: &str) -> String {
    let mut h = Sha256::new();
    h.update(prompt.as_bytes());
    format!("{:x}", h.finalize())
}

const LLM_API_PROTOCOL: &str = "chat_completions_v1";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
enum LlmProvider {
    Openai,
    Dashscope,
    Deepseek,
    Kimi,
    Zhipu,
    Minimax,
    Other,
}

impl LlmProvider {
    fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Dashscope => "dashscope",
            Self::Deepseek => "deepseek",
            Self::Kimi => "kimi",
            Self::Zhipu => "zhipu",
            Self::Minimax => "minimax",
            Self::Other => "other",
        }
    }

    fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "dashscope" => Self::Dashscope,
            "deepseek" => Self::Deepseek,
            "kimi" | "moonshot" => Self::Kimi,
            "zhipu" | "glm" => Self::Zhipu,
            "minimax" => Self::Minimax,
            "other" => Self::Other,
            _ => Self::Openai,
        }
    }
}

fn all_llm_providers() -> [LlmProvider; 7] {
    [
        LlmProvider::Openai,
        LlmProvider::Dashscope,
        LlmProvider::Deepseek,
        LlmProvider::Kimi,
        LlmProvider::Zhipu,
        LlmProvider::Minimax,
        LlmProvider::Other,
    ]
}

fn provider_api_key_env(provider: LlmProvider) -> &'static str {
    match provider {
        LlmProvider::Openai => "OPENAI_API_KEY",
        LlmProvider::Dashscope => "DASHSCOPE_API_KEY",
        LlmProvider::Deepseek => "DEEPSEEK_API_KEY",
        LlmProvider::Kimi => "KIMI_API_KEY",
        LlmProvider::Zhipu => "ZHIPU_API_KEY",
        LlmProvider::Minimax => "MINIMAX_API_KEY",
        LlmProvider::Other => "OTHER_API_KEY",
    }
}

fn provider_env_prefix(provider: LlmProvider) -> &'static str {
    match provider {
        LlmProvider::Openai => "OPENAI",
        LlmProvider::Dashscope => "DASHSCOPE",
        LlmProvider::Deepseek => "DEEPSEEK",
        LlmProvider::Kimi => "KIMI",
        LlmProvider::Zhipu => "ZHIPU",
        LlmProvider::Minimax => "MINIMAX",
        LlmProvider::Other => "OTHER",
    }
}

fn provider_profile_env_key(provider: LlmProvider, field: &str) -> String {
    format!("{}_{}", provider_env_prefix(provider), field)
}

fn resolve_provider_api_key(
    keys: &mut HashMap<LlmProvider, String>,
    provider: LlmProvider,
    supplied: &str,
) -> Result<String, String> {
    let supplied = supplied.trim();
    if !supplied.is_empty() {
        keys.insert(provider, supplied.to_string());
        return Ok(supplied.to_string());
    }
    keys.get(&provider)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or_else(|| format!("未配置 {} 的 API Key", provider.as_str()))
}

fn mask_api_key(key: &str) -> String {
    if key.chars().count() <= 8 {
        return String::new();
    }
    let prefix = key.chars().take(4).collect::<String>();
    let suffix = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{prefix}…{suffix}")
}

fn load_provider_api_keys(active_provider: LlmProvider) -> HashMap<LlmProvider, String> {
    let mut keys = HashMap::new();
    for provider in all_llm_providers() {
        if let Ok(value) = std::env::var(provider_api_key_env(provider))
            && !value.is_empty()
        {
            keys.insert(provider, value);
        }
    }
    // 兼容上一版单 Key 配置：AI_API_KEY 属于启动时选中的供应商。
    if let Ok(value) = std::env::var("AI_API_KEY")
        && !value.is_empty()
    {
        keys.insert(active_provider, value);
    }
    keys
}

#[derive(Clone, Debug, Serialize)]
struct ProviderPreset {
    id: String,
    name: String,
    default_base_url: String,
    default_model: String,
    thinking_kind: String,
    default_thinking_mode: String,
    default_reasoning_effort: String,
    reasoning_options: Vec<String>,
    has_api_key: bool,
    api_key_masked: String,
    base_url: String,
    proxy_url: String,
    model: String,
    reasoning_effort: String,
    thinking_mode: String,
    thinking_keep: String,
    clear_thinking: bool,
    extra_body_json: String,
}

fn provider_presets() -> Vec<ProviderPreset> {
    [
        (
            LlmProvider::Openai,
            "OpenAI",
            "https://api.openai.com/v1",
            "gpt-4o-mini",
            "none",
            "auto",
            "",
            &["low", "medium", "high", "xhigh", "max"][..],
        ),
        (
            LlmProvider::Dashscope,
            "DashScope / 通义千问",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "qwen3.8-flash",
            "boolean",
            "disabled",
            "",
            &[][..],
        ),
        (
            LlmProvider::Deepseek,
            "DeepSeek",
            "https://api.deepseek.com",
            "deepseek-v4-flash",
            "enabled_disabled",
            "disabled",
            "low",
            &["low", "high", "max"][..],
        ),
        (
            LlmProvider::Kimi,
            "Kimi / Moonshot",
            "https://api.moonshot.cn/v1",
            "kimi-k3",
            "enabled_disabled_keep",
            "disabled",
            "",
            &[][..],
        ),
        (
            LlmProvider::Zhipu,
            "智谱 GLM",
            "https://open.bigmodel.cn/api/paas/v4",
            "glm-5.2",
            "enabled_disabled_clear",
            "disabled",
            "minimal",
            &["none", "minimal", "low", "medium", "high", "xhigh", "max"][..],
        ),
        (
            LlmProvider::Minimax,
            "MiniMax",
            "https://api.minimaxi.com/v1",
            "MiniMax-M3",
            "adaptive_disabled",
            "disabled",
            "",
            &[][..],
        ),
        (
            LlmProvider::Other,
            "Other / 自定义",
            "",
            "",
            "custom",
            "auto",
            "",
            &[][..],
        ),
    ]
    .into_iter()
    .map(
        |(
            provider,
            name,
            base,
            model,
            thinking_kind,
            default_thinking_mode,
            default_reasoning_effort,
            reasoning,
        )| {
            ProviderPreset {
                id: provider.as_str().into(),
                name: name.into(),
                default_base_url: base.into(),
                default_model: model.into(),
                thinking_kind: thinking_kind.into(),
                default_thinking_mode: default_thinking_mode.into(),
                default_reasoning_effort: default_reasoning_effort.into(),
                reasoning_options: reasoning.iter().map(|value| (*value).to_string()).collect(),
                has_api_key: false,
                api_key_masked: String::new(),
                base_url: base.into(),
                proxy_url: String::new(),
                model: model.into(),
                reasoning_effort: default_reasoning_effort.into(),
                thinking_mode: default_thinking_mode.into(),
                thinking_keep: "null".into(),
                clear_thinking: true,
                extra_body_json: "{}".into(),
            }
        },
    )
    .collect()
}

#[derive(Clone, Debug)]
struct LlmRuntimeConfig {
    provider: LlmProvider,
    base_url: String,
    proxy_url: String,
    api_key: String,
    model: String,
    reasoning_effort: String,
    thinking_mode: String,
    thinking_keep: String,
    clear_thinking: bool,
    extra_body_json: String,
}

impl LlmRuntimeConfig {
    fn for_provider(provider: LlmProvider) -> Self {
        let preset = provider_presets()
            .into_iter()
            .find(|item| item.id == provider.as_str())
            .expect("every provider has a preset");
        Self {
            provider,
            base_url: preset.default_base_url,
            proxy_url: String::new(),
            api_key: String::new(),
            model: preset.default_model,
            reasoning_effort: preset.default_reasoning_effort,
            thinking_mode: preset.default_thinking_mode,
            thinking_keep: "null".into(),
            clear_thinking: true,
            extra_body_json: "{}".into(),
        }
    }

    fn from_env() -> Self {
        let provider =
            LlmProvider::parse(&std::env::var("AI_PROVIDER").unwrap_or_else(|_| "openai".into()));
        let mut config = load_provider_profiles(provider)
            .remove(&provider)
            .unwrap_or_else(|| Self::for_provider(provider));
        config.api_key = load_provider_api_keys(provider)
            .remove(&provider)
            .unwrap_or_default();
        config
    }

    fn cache_identity(&self) -> String {
        format!(
            "provider={};base_url={};model={};reasoning={};thinking={};keep={};clear={};extra={}",
            self.provider.as_str(),
            self.base_url,
            self.model,
            self.reasoning_effort,
            self.thinking_mode,
            self.thinking_keep,
            self.clear_thinking,
            self.extra_body_json
        )
    }

    fn validate(&self) -> Result<(), String> {
        let effort = self.reasoning_effort.as_str();
        let thinking = self.thinking_mode.as_str();
        match self.provider {
            LlmProvider::Openai
                if !effort.is_empty()
                    && !["low", "medium", "high", "xhigh", "max"].contains(&effort) =>
            {
                Err("OpenAI 推理等级无效".into())
            }
            LlmProvider::Dashscope if !["auto", "enabled", "disabled"].contains(&thinking) => {
                Err("DashScope 思考模式无效".into())
            }
            LlmProvider::Deepseek
                if !["enabled", "disabled"].contains(&thinking)
                    || !effort.is_empty() && !["low", "high", "max"].contains(&effort) =>
            {
                Err("DeepSeek 思考模式或推理等级无效".into())
            }
            LlmProvider::Kimi
                if !["enabled", "disabled"].contains(&thinking)
                    || !["null", "all"].contains(&self.thinking_keep.as_str()) =>
            {
                Err("Kimi 思考模式或保留策略无效".into())
            }
            LlmProvider::Zhipu
                if !["enabled", "disabled"].contains(&thinking)
                    || !effort.is_empty()
                        && !["none", "minimal", "low", "medium", "high", "xhigh", "max"]
                            .contains(&effort) =>
            {
                Err("智谱思考模式或推理等级无效".into())
            }
            LlmProvider::Minimax if !["adaptive", "disabled"].contains(&thinking) => {
                Err("MiniMax 思考模式无效".into())
            }
            _ => Ok(()),
        }
    }
}

const PROFILE_FIELDS: [&str; 7] = [
    "BASE_URL",
    "PROXY",
    "MODEL",
    "REASONING_EFFORT",
    "THINKING_MODE",
    "THINKING_KEEP",
    "CLEAR_THINKING",
];

fn apply_profile_env_value(config: &mut LlmRuntimeConfig, field: &str, value: String) {
    match field {
        "BASE_URL" if !value.trim().is_empty() => config.base_url = value,
        "PROXY" => config.proxy_url = value,
        "MODEL" if !value.trim().is_empty() => config.model = value,
        "REASONING_EFFORT" => config.reasoning_effort = value,
        "THINKING_MODE" if !value.is_empty() => config.thinking_mode = value,
        "THINKING_KEEP" if !value.is_empty() => config.thinking_keep = value,
        "CLEAR_THINKING" if !value.is_empty() => {
            config.clear_thinking = value.eq_ignore_ascii_case("true")
        }
        "EXTRA_BODY_JSON" if !value.is_empty() => config.extra_body_json = value,
        _ => {}
    }
}

fn load_provider_profiles(active_provider: LlmProvider) -> HashMap<LlmProvider, LlmRuntimeConfig> {
    let mut profiles = HashMap::new();
    for provider in all_llm_providers() {
        let mut config = LlmRuntimeConfig::for_provider(provider);
        for field in PROFILE_FIELDS.into_iter().chain(["EXTRA_BODY_JSON"]) {
            if let Ok(value) = std::env::var(provider_profile_env_key(provider, field)) {
                apply_profile_env_value(&mut config, field, value);
            }
        }
        profiles.insert(provider, config);
    }
    // 兼容上一版单配置 AI_*：这些值属于启动时选中的供应商。
    if let Some(active) = profiles.get_mut(&active_provider) {
        for field in PROFILE_FIELDS.into_iter().chain(["EXTRA_BODY_JSON"]) {
            if let Ok(value) = std::env::var(format!("AI_{field}"))
                && !value.is_empty()
            {
                apply_profile_env_value(active, field, value);
            }
        }
    }
    profiles
}

static ACTIVE_LLM_CONFIG: OnceLock<RwLock<LlmRuntimeConfig>> = OnceLock::new();
static PROVIDER_API_KEYS: OnceLock<RwLock<HashMap<LlmProvider, String>>> = OnceLock::new();
static PROVIDER_PROFILES: OnceLock<RwLock<HashMap<LlmProvider, LlmRuntimeConfig>>> =
    OnceLock::new();

fn active_llm_config() -> LlmRuntimeConfig {
    ACTIVE_LLM_CONFIG
        .get_or_init(|| RwLock::new(LlmRuntimeConfig::from_env()))
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn replace_active_llm_config(config: LlmRuntimeConfig) {
    let initial = config.clone();
    *ACTIVE_LLM_CONFIG
        .get_or_init(|| RwLock::new(initial))
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = config;
}

fn provider_api_keys_snapshot() -> HashMap<LlmProvider, String> {
    PROVIDER_API_KEYS
        .get_or_init(|| {
            let active = LlmProvider::parse(
                &std::env::var("AI_PROVIDER").unwrap_or_else(|_| "openai".into()),
            );
            RwLock::new(load_provider_api_keys(active))
        })
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn replace_provider_api_keys(keys: HashMap<LlmProvider, String>) {
    let initial = keys.clone();
    *PROVIDER_API_KEYS
        .get_or_init(|| RwLock::new(initial))
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = keys;
}

fn provider_profiles_snapshot() -> HashMap<LlmProvider, LlmRuntimeConfig> {
    PROVIDER_PROFILES
        .get_or_init(|| {
            let active = LlmProvider::parse(
                &std::env::var("AI_PROVIDER").unwrap_or_else(|_| "openai".into()),
            );
            RwLock::new(load_provider_profiles(active))
        })
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn replace_provider_profiles(profiles: HashMap<LlmProvider, LlmRuntimeConfig>) {
    let initial = profiles.clone();
    *PROVIDER_PROFILES
        .get_or_init(|| RwLock::new(initial))
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = profiles;
}

fn analysis_cache_fingerprint(prompt: &str, config: &LlmRuntimeConfig) -> String {
    prompt_hash(&format!(
        "{prompt}\n\0protocol={LLM_API_PROTOCOL}\n{}",
        config.cache_identity()
    ))
}

fn chat_completions_url(base_url: &str) -> String {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.ends_with("/chat/completions") {
        base_url.to_string()
    } else {
        format!("{base_url}/chat/completions")
    }
}

fn build_chat_completion_body(
    config: &LlmRuntimeConfig,
    prompt: &str,
    image_url: Option<&str>,
    stream: bool,
) -> Result<serde_json::Value, String> {
    let images = image_url.into_iter().collect::<Vec<_>>();
    build_chat_completion_body_with_images(config, prompt, &images, stream)
}

fn build_chat_completion_body_with_images(
    config: &LlmRuntimeConfig,
    prompt: &str,
    image_urls: &[&str],
    stream: bool,
) -> Result<serde_json::Value, String> {
    config.validate()?;
    let content = if image_urls.is_empty() {
        serde_json::Value::String(prompt.to_string())
    } else {
        let mut parts = vec![serde_json::json!({"type": "text", "text": prompt})];
        parts.extend(
            image_urls
                .iter()
                .map(|url| serde_json::json!({"type": "image_url", "image_url": {"url": url}})),
        );
        serde_json::Value::Array(parts)
    };
    let mut body = serde_json::json!({
        "model": config.model,
        "messages": [{"role": "user", "content": content}],
        "stream": stream
    });
    match config.provider {
        LlmProvider::Openai => {
            if ["low", "medium", "high", "xhigh", "max"].contains(&config.reasoning_effort.as_str())
            {
                body["reasoning_effort"] = serde_json::json!(config.reasoning_effort);
            }
        }
        LlmProvider::Dashscope => match config.thinking_mode.as_str() {
            "enabled" => body["enable_thinking"] = serde_json::json!(true),
            "disabled" => body["enable_thinking"] = serde_json::json!(false),
            _ => {}
        },
        LlmProvider::Deepseek => {
            if ["enabled", "disabled"].contains(&config.thinking_mode.as_str()) {
                body["thinking"] = serde_json::json!({"type": config.thinking_mode});
            }
            if config.thinking_mode == "enabled"
                && ["low", "high", "max"].contains(&config.reasoning_effort.as_str())
            {
                body["reasoning_effort"] = serde_json::json!(config.reasoning_effort);
            }
        }
        LlmProvider::Kimi => {
            if ["enabled", "disabled"].contains(&config.thinking_mode.as_str()) {
                let mut thinking = serde_json::json!({"type": config.thinking_mode});
                if config.thinking_mode == "enabled" {
                    match config.thinking_keep.as_str() {
                        "all" => thinking["keep"] = serde_json::json!("all"),
                        "null" => thinking["keep"] = serde_json::Value::Null,
                        _ => {}
                    }
                }
                body["thinking"] = thinking;
            }
        }
        LlmProvider::Zhipu => {
            if ["enabled", "disabled"].contains(&config.thinking_mode.as_str()) {
                body["thinking"] = serde_json::json!({
                    "type": config.thinking_mode,
                    "clear_thinking": config.clear_thinking
                });
            }
            if config.thinking_mode == "enabled"
                && ["none", "minimal", "low", "medium", "high", "xhigh", "max"]
                    .contains(&config.reasoning_effort.as_str())
            {
                body["reasoning_effort"] = serde_json::json!(config.reasoning_effort);
            }
        }
        LlmProvider::Minimax => {
            if ["adaptive", "disabled"].contains(&config.thinking_mode.as_str()) {
                body["thinking"] = serde_json::json!({"type": config.thinking_mode});
            }
        }
        LlmProvider::Other => {
            let extra = serde_json::from_str::<serde_json::Value>(&config.extra_body_json)
                .map_err(|error| format!("自定义请求参数不是有效 JSON：{error}"))?;
            let extra = extra
                .as_object()
                .ok_or_else(|| "自定义请求参数必须是 JSON 对象".to_string())?;
            for (key, value) in extra {
                if ["model", "messages", "stream"].contains(&key.as_str()) {
                    return Err(format!("自定义请求参数不能覆盖保留字段：{key}"));
                }
                body[key] = value.clone();
            }
        }
    }
    Ok(body)
}

fn extract_chat_completion_delta(event: &serde_json::Value) -> Option<&str> {
    event
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("delta"))
        .and_then(|delta| delta.get("content"))
        .and_then(|content| content.as_str())
}

fn take_sse_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let (position, delimiter_len) = buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2))
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| (position, 4))
        })?;
    let frame = buffer[..position].to_vec();
    buffer.drain(..position + delimiter_len);
    Some(frame)
}

#[derive(Clone)]
struct ActiveAnalysis {
    task_id: String,
    capture_id: String,
    cancellation: CancellationToken,
}

struct AnalysisRegistration {
    cancellation: CancellationToken,
    superseded: Option<ActiveAnalysis>,
}

#[derive(Clone, Default)]
struct ActiveAnalysisRegistry {
    inner: Arc<Mutex<HashMap<String, ActiveAnalysis>>>,
}

impl ActiveAnalysisRegistry {
    fn replace(
        &self,
        owner_user_id: &str,
        task_id: &str,
        capture_id: &str,
    ) -> AnalysisRegistration {
        let cancellation = CancellationToken::new();
        let active = ActiveAnalysis {
            task_id: task_id.to_string(),
            capture_id: capture_id.to_string(),
            cancellation: cancellation.clone(),
        };
        let superseded = self
            .inner
            .lock()
            .ok()
            .and_then(|mut analyses| analyses.insert(owner_user_id.to_string(), active));
        if let Some(previous) = &superseded {
            previous.cancellation.cancel();
        }
        AnalysisRegistration {
            cancellation,
            superseded,
        }
    }

    fn remove_if_current(&self, owner_user_id: &str, task_id: &str) -> bool {
        let Ok(mut analyses) = self.inner.lock() else {
            return false;
        };
        if analyses
            .get(owner_user_id)
            .is_some_and(|analysis| analysis.task_id == task_id)
        {
            analyses.remove(owner_user_id);
            true
        } else {
            false
        }
    }

    #[cfg(test)]
    fn current_task(&self, owner_user_id: &str) -> Option<String> {
        self.inner
            .lock()
            .ok()
            .and_then(|analyses| analyses.get(owner_user_id).map(|item| item.task_id.clone()))
    }
}

#[cfg(test)]
mod prompt_tests {
    use super::{
        ActiveAnalysisRegistry, LlmProvider, LlmRuntimeConfig, analysis_cache_fingerprint,
        build_chat_completion_body, chat_completions_url, extract_chat_completion_delta,
        mark_task_cancelled, prompt_hash, provider_api_key_env, provider_presets,
        provider_profile_env_key, resolve_provider_api_key, should_reschedule_analysis,
        take_sse_frame,
    };

    #[test]
    fn prompt_hash_changes_when_prompt_changes() {
        assert_ne!(prompt_hash("规则 A"), prompt_hash("规则 B"));
        assert_eq!(prompt_hash("相同规则"), prompt_hash("相同规则"));
    }

    #[test]
    fn cache_fingerprint_changes_with_model_or_reasoning() {
        let mut config = LlmRuntimeConfig::for_provider(LlmProvider::Openai);
        config.model = "model-a".into();
        config.reasoning_effort = "low".into();
        let base = analysis_cache_fingerprint("prompt", &config);
        config.model = "model-b".into();
        assert_ne!(base, analysis_cache_fingerprint("prompt", &config));
        config.model = "model-a".into();
        config.reasoning_effort = "high".into();
        assert_ne!(base, analysis_cache_fingerprint("prompt", &config));
        config.reasoning_effort = "low".into();
        assert_eq!(base, analysis_cache_fingerprint("prompt", &config));
        config.base_url = "https://another-provider.example/v1".into();
        assert_ne!(base, analysis_cache_fingerprint("prompt", &config));
    }

    #[test]
    fn cache_fingerprint_changes_when_prompt_is_hot_switched() {
        let config = LlmRuntimeConfig::for_provider(LlmProvider::Openai);
        let image_key = "capture-sha";
        let first = analysis_cache_fingerprint(
            &format!("{image_key}:{}", prompt_hash("单题规则 A")),
            &config,
        );
        let second = analysis_cache_fingerprint(
            &format!("{image_key}:{}", prompt_hash("单题规则 B")),
            &config,
        );
        assert_ne!(first, second);
    }

    #[test]
    fn ignores_non_chat_top_level_delta() {
        let event = serde_json::json!({"delta":"答案"});
        assert_eq!(extract_chat_completion_delta(&event), None);
    }

    #[test]
    fn extracts_chat_completions_delta() {
        let event = serde_json::json!({"choices":[{"delta":{"content":"解析"}}]});
        assert_eq!(extract_chat_completion_delta(&event), Some("解析"));
    }

    #[test]
    fn builds_standard_chat_completions_urls() {
        assert_eq!(
            chat_completions_url("https://api.example.com/v1"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://api.example.com/v1/"),
            "https://api.example.com/v1/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://api.example.com/v1/chat/completions"),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn builds_standard_multimodal_chat_completions_body() {
        let mut config = LlmRuntimeConfig::for_provider(LlmProvider::Openai);
        config.model = "gpt-compatible-model".into();
        config.reasoning_effort = "high".into();
        let body = build_chat_completion_body(
            &config,
            "识别并解答题目",
            Some("data:image/jpeg;base64,abc"),
            true,
        )
        .unwrap();
        assert_eq!(body["model"], "gpt-compatible-model");
        assert_eq!(body["stream"], true);
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["messages"][0]["content"][0]["text"], "识别并解答题目");
        assert_eq!(
            body["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/jpeg;base64,abc"
        );
        assert!(body.get("input").is_none());
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn builds_text_only_body_without_optional_reasoning() {
        let config = LlmRuntimeConfig::for_provider(LlmProvider::Openai);
        let body = build_chat_completion_body(&config, "Reply OK", None, true).unwrap();
        assert_eq!(body["messages"][0]["content"], "Reply OK");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn exposes_all_supported_provider_presets() {
        let providers = provider_presets();
        let ids = providers
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec![
                "openai",
                "dashscope",
                "deepseek",
                "kimi",
                "zhipu",
                "minimax",
                "other"
            ]
        );
        assert_eq!(
            providers[1].default_base_url,
            "https://dashscope.aliyuncs.com/compatible-mode/v1"
        );
        assert_eq!(providers[2].default_base_url, "https://api.deepseek.com");
        assert_eq!(providers[3].default_base_url, "https://api.moonshot.cn/v1");
    }

    #[test]
    fn assigns_a_distinct_api_key_environment_variable_to_each_provider() {
        assert_eq!(provider_api_key_env(LlmProvider::Openai), "OPENAI_API_KEY");
        assert_eq!(
            provider_api_key_env(LlmProvider::Dashscope),
            "DASHSCOPE_API_KEY"
        );
        assert_eq!(
            provider_api_key_env(LlmProvider::Deepseek),
            "DEEPSEEK_API_KEY"
        );
        assert_eq!(provider_api_key_env(LlmProvider::Kimi), "KIMI_API_KEY");
        assert_eq!(provider_api_key_env(LlmProvider::Zhipu), "ZHIPU_API_KEY");
        assert_eq!(
            provider_api_key_env(LlmProvider::Minimax),
            "MINIMAX_API_KEY"
        );
        assert_eq!(provider_api_key_env(LlmProvider::Other), "OTHER_API_KEY");
    }

    #[test]
    fn switching_provider_keeps_each_saved_api_key() {
        let mut keys = std::collections::HashMap::new();
        let openai =
            resolve_provider_api_key(&mut keys, LlmProvider::Openai, "  openai-secret  ").unwrap();
        let deepseek =
            resolve_provider_api_key(&mut keys, LlmProvider::Deepseek, "deepseek-secret").unwrap();
        let restored_openai = resolve_provider_api_key(&mut keys, LlmProvider::Openai, "").unwrap();

        assert_eq!(openai, "openai-secret");
        assert_eq!(deepseek, "deepseek-secret");
        assert_eq!(restored_openai, "openai-secret");
        assert_eq!(keys.get(&LlmProvider::Deepseek).unwrap(), "deepseek-secret");
    }

    #[test]
    fn switching_to_provider_without_saved_key_requires_a_new_key() {
        let mut keys = std::collections::HashMap::new();
        assert!(resolve_provider_api_key(&mut keys, LlmProvider::Kimi, "").is_err());
    }

    #[test]
    fn assigns_distinct_profile_environment_variables_to_each_provider() {
        assert_eq!(
            provider_profile_env_key(LlmProvider::Openai, "BASE_URL"),
            "OPENAI_BASE_URL"
        );
        assert_eq!(
            provider_profile_env_key(LlmProvider::Dashscope, "MODEL"),
            "DASHSCOPE_MODEL"
        );
        assert_eq!(
            provider_profile_env_key(LlmProvider::Deepseek, "THINKING_MODE"),
            "DEEPSEEK_THINKING_MODE"
        );
        assert_eq!(
            provider_profile_env_key(LlmProvider::Kimi, "THINKING_KEEP"),
            "KIMI_THINKING_KEEP"
        );
        assert_eq!(
            provider_profile_env_key(LlmProvider::Zhipu, "CLEAR_THINKING"),
            "ZHIPU_CLEAR_THINKING"
        );
        assert_eq!(
            provider_profile_env_key(LlmProvider::Minimax, "MODEL"),
            "MINIMAX_MODEL"
        );
        assert_eq!(
            provider_profile_env_key(LlmProvider::Other, "EXTRA_BODY_JSON"),
            "OTHER_EXTRA_BODY_JSON"
        );
    }

    #[test]
    fn provider_recommended_defaults_are_used_only_as_initial_values() {
        let dashscope = LlmRuntimeConfig::for_provider(LlmProvider::Dashscope);
        assert_eq!(dashscope.thinking_mode, "disabled");

        let deepseek = LlmRuntimeConfig::for_provider(LlmProvider::Deepseek);
        assert_eq!(deepseek.thinking_mode, "disabled");
        assert_eq!(deepseek.reasoning_effort, "low");

        let kimi = LlmRuntimeConfig::for_provider(LlmProvider::Kimi);
        assert_eq!(kimi.thinking_mode, "disabled");
        assert_eq!(kimi.thinking_keep, "null");

        let zhipu = LlmRuntimeConfig::for_provider(LlmProvider::Zhipu);
        assert_eq!(zhipu.thinking_mode, "disabled");
        assert_eq!(zhipu.reasoning_effort, "minimal");
        assert!(zhipu.clear_thinking);

        let minimax = LlmRuntimeConfig::for_provider(LlmProvider::Minimax);
        assert_eq!(minimax.thinking_mode, "disabled");
    }

    #[test]
    fn builds_provider_specific_thinking_parameters() {
        let mut dashscope = LlmRuntimeConfig::for_provider(LlmProvider::Dashscope);
        dashscope.thinking_mode = "disabled".into();
        let body = build_chat_completion_body(&dashscope, "test", None, true).unwrap();
        assert_eq!(body["enable_thinking"], false);

        let mut deepseek = LlmRuntimeConfig::for_provider(LlmProvider::Deepseek);
        deepseek.thinking_mode = "enabled".into();
        deepseek.reasoning_effort = "high".into();
        let body = build_chat_completion_body(&deepseek, "test", None, true).unwrap();
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["reasoning_effort"], "high");

        deepseek.thinking_mode = "disabled".into();
        let body = build_chat_completion_body(&deepseek, "test", None, true).unwrap();
        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("reasoning_effort").is_none());

        let mut kimi = LlmRuntimeConfig::for_provider(LlmProvider::Kimi);
        kimi.thinking_mode = "enabled".into();
        kimi.thinking_keep = "null".into();
        let body = build_chat_completion_body(&kimi, "test", None, true).unwrap();
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body["thinking"]["keep"].is_null());

        let mut zhipu = LlmRuntimeConfig::for_provider(LlmProvider::Zhipu);
        zhipu.thinking_mode = "enabled".into();
        zhipu.clear_thinking = true;
        zhipu.reasoning_effort = "minimal".into();
        let body = build_chat_completion_body(&zhipu, "test", None, true).unwrap();
        assert_eq!(body["thinking"]["clear_thinking"], true);
        assert_eq!(body["reasoning_effort"], "minimal");

        let mut minimax = LlmRuntimeConfig::for_provider(LlmProvider::Minimax);
        minimax.thinking_mode = "adaptive".into();
        let body = build_chat_completion_body(&minimax, "test", None, true).unwrap();
        assert_eq!(body["thinking"]["type"], "adaptive");
    }

    #[test]
    fn merges_safe_other_provider_parameters_and_rejects_reserved_keys() {
        let mut other = LlmRuntimeConfig::for_provider(LlmProvider::Other);
        other.extra_body_json = r#"{"temperature":0.2,"top_p":0.8}"#.into();
        let body = build_chat_completion_body(&other, "test", None, false).unwrap();
        assert_eq!(body["temperature"], 0.2);
        assert_eq!(body["top_p"], 0.8);

        other.extra_body_json = r#"{"messages":[]}"#.into();
        assert!(build_chat_completion_body(&other, "test", None, false).is_err());
    }

    #[test]
    fn splits_lf_and_crlf_sse_frames() {
        let mut lf = b"data: one\n\ndata: two\n\n".to_vec();
        assert_eq!(take_sse_frame(&mut lf).as_deref(), Some(&b"data: one"[..]));
        assert_eq!(take_sse_frame(&mut lf).as_deref(), Some(&b"data: two"[..]));
        let mut crlf = b"data: three\r\n\r\nrest".to_vec();
        assert_eq!(
            take_sse_frame(&mut crlf).as_deref(),
            Some(&b"data: three"[..])
        );
        assert_eq!(crlf, b"rest");
    }

    #[test]
    fn newer_analysis_cancels_previous_task_for_same_user() {
        let registry = ActiveAnalysisRegistry::default();
        let first = registry.replace("user-a", "task-1", "capture-1");
        assert!(first.superseded.is_none());
        assert!(!first.cancellation.is_cancelled());

        let second = registry.replace("user-a", "task-2", "capture-2");
        let superseded = second.superseded.expect("previous task must be returned");
        assert_eq!(superseded.task_id, "task-1");
        assert_eq!(superseded.capture_id, "capture-1");
        assert!(first.cancellation.is_cancelled());
        assert!(!second.cancellation.is_cancelled());
    }

    #[test]
    fn replacing_one_users_analysis_does_not_cancel_another_user() {
        let registry = ActiveAnalysisRegistry::default();
        let user_a = registry.replace("user-a", "task-a", "capture-a");
        let user_b = registry.replace("user-b", "task-b", "capture-b");

        assert!(user_b.superseded.is_none());
        assert!(!user_a.cancellation.is_cancelled());
        assert!(!user_b.cancellation.is_cancelled());
    }

    #[test]
    fn stale_task_cleanup_cannot_remove_newer_active_task() {
        let registry = ActiveAnalysisRegistry::default();
        registry.replace("user-a", "task-1", "capture-1");
        registry.replace("user-a", "task-2", "capture-2");

        assert!(!registry.remove_if_current("user-a", "task-1"));
        assert_eq!(registry.current_task("user-a").as_deref(), Some("task-2"));
        assert!(registry.remove_if_current("user-a", "task-2"));
        assert!(registry.current_task("user-a").is_none());
    }

    #[test]
    fn cancelling_an_active_task_persists_cancelled_state_and_event() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE tasks(id TEXT PRIMARY KEY,status TEXT NOT NULL,answer TEXT NOT NULL);
             CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT,task_id TEXT NOT NULL,sequence INTEGER NOT NULL,event_type TEXT NOT NULL,payload TEXT NOT NULL,created_at TEXT NOT NULL);",
        )
        .unwrap();
        db.execute(
            "INSERT INTO tasks VALUES('task-1','parsing','partial answer')",
            [],
        )
        .unwrap();

        assert!(mark_task_cancelled(&mut db, "task-1").unwrap());
        let state: (String, String) = db
            .query_row(
                "SELECT status,answer FROM tasks WHERE id='task-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, ("cancelled".into(), String::new()));
        let event: String = db
            .query_row(
                "SELECT event_type FROM events WHERE task_id='task-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(event, "analysis.cancelled");
    }

    #[test]
    fn cancelling_does_not_overwrite_an_already_completed_task() {
        let mut db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE tasks(id TEXT PRIMARY KEY,status TEXT NOT NULL,answer TEXT NOT NULL);
             CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT,task_id TEXT NOT NULL,sequence INTEGER NOT NULL,event_type TEXT NOT NULL,payload TEXT NOT NULL,created_at TEXT NOT NULL);
             INSERT INTO tasks VALUES('task-1','completed','final answer');",
        )
        .unwrap();

        assert!(!mark_task_cancelled(&mut db, "task-1").unwrap());
        let state: (String, String) = db
            .query_row(
                "SELECT status,answer FROM tasks WHERE id='task-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state, ("completed".into(), "final answer".into()));
    }

    #[test]
    fn cancelled_task_is_never_rescheduled_even_if_status_is_still_queued() {
        assert!(!should_reschedule_analysis(true, "queued"));
        assert!(should_reschedule_analysis(false, "queued"));
        assert!(!should_reschedule_analysis(false, "parsing"));
        assert!(!should_reschedule_analysis(false, "cancelled"));
    }
}

#[derive(Clone)]
struct AppState {
    db: Arc<Mutex<Connection>>,
    data_dir: PathBuf,
    object_store: Option<S3Client>,
    object_bucket: String,
    capture_events: broadcast::Sender<String>,
    devices: Arc<Mutex<HashMap<String, DeviceStatus>>>,
    sessions: SessionStore,
    analysis_slots: Arc<Semaphore>,
    analysis_inflight: Arc<Mutex<HashSet<String>>>,
    analysis_active_keys: Arc<Mutex<HashSet<String>>>,
    active_analyses: ActiveAnalysisRegistry,
}
#[derive(Clone, Serialize)]
struct DeviceStatus {
    device_id: String,
    connected: bool,
    last_heartbeat: String,
    heartbeat_count: u64,
    reconnect_count: u64,
}
#[derive(Serialize)]
struct ManagedDevice {
    id: String,
    device_id: String,
    name: String,
    created_at: String,
    connected: bool,
    last_heartbeat: String,
    heartbeat_count: u64,
    reconnect_count: u64,
}
#[derive(Serialize)]
struct CaptureReceipt {
    capture_id: String,
    sha256: String,
    bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_order: Option<i64>,
}
#[derive(Clone, Serialize)]
struct Capture {
    id: String,
    device_id: String,
    sha256: String,
    width: u32,
    height: u32,
    received_at: String,
}
#[derive(Clone, Serialize)]
struct Task {
    id: String,
    capture_id: String,
    status: String,
    answer: String,
}

#[derive(Serialize)]
struct AnalysisSnapshot {
    capture: Capture,
    task: Option<Task>,
    group: Option<CaptureGroupSnapshot>,
}

#[derive(Serialize)]
struct CaptureGroupSnapshot {
    id: String,
    page_count: i64,
    captures: Vec<Capture>,
}
#[derive(Deserialize)]
struct TaskRequest {
    #[serde(rename = "profile")]
    _profile: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();
    replace_active_llm_config(LlmRuntimeConfig::from_env());
    tracing_subscriber::fmt::init();
    let data_dir =
        PathBuf::from(std::env::var("SIGHT_DATA_DIR").unwrap_or_else(|_| "./data".into()));
    fs::create_dir_all(&data_dir).await?;
    let db = Connection::open(data_dir.join("app.db"))?;
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS users(id TEXT PRIMARY KEY,username TEXT NOT NULL UNIQUE,role TEXT NOT NULL,password_hash TEXT NOT NULL,disabled INTEGER NOT NULL DEFAULT 0,created_at TEXT NOT NULL); CREATE TABLE IF NOT EXISTS devices(id TEXT PRIMARY KEY,owner_user_id TEXT NOT NULL,device_id TEXT NOT NULL UNIQUE,name TEXT NOT NULL,token_hash TEXT NOT NULL,created_at TEXT NOT NULL); CREATE TABLE IF NOT EXISTS captures(id TEXT PRIMARY KEY,owner_user_id TEXT NOT NULL,device_id TEXT NOT NULL,sha256 TEXT NOT NULL,width INTEGER NOT NULL,height INTEGER NOT NULL,path TEXT NOT NULL,received_at TEXT NOT NULL); CREATE TABLE IF NOT EXISTS llm_answers(owner_user_id TEXT NOT NULL,sha256 TEXT NOT NULL,answer TEXT NOT NULL,created_at TEXT NOT NULL,PRIMARY KEY(owner_user_id,sha256)); CREATE TABLE IF NOT EXISTS llm_prompt_configs(id INTEGER PRIMARY KEY CHECK(id=1),prompt TEXT NOT NULL,prompt_hash TEXT NOT NULL,version INTEGER NOT NULL,updated_by TEXT,updated_at TEXT NOT NULL); CREATE TABLE IF NOT EXISTS llm_multi_page_prompt_configs(id INTEGER PRIMARY KEY CHECK(id=1),prompt TEXT NOT NULL,prompt_hash TEXT NOT NULL,version INTEGER NOT NULL,updated_by TEXT,updated_at TEXT NOT NULL); CREATE TABLE IF NOT EXISTS analysis_groups(id TEXT PRIMARY KEY,owner_user_id TEXT NOT NULL,device_id TEXT NOT NULL,status TEXT NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL); CREATE TABLE IF NOT EXISTS analysis_group_captures(group_id TEXT NOT NULL,capture_id TEXT NOT NULL,page_order INTEGER NOT NULL,PRIMARY KEY(group_id,capture_id),UNIQUE(group_id,page_order)); CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY,owner_user_id TEXT NOT NULL,capture_id TEXT NOT NULL,status TEXT NOT NULL,profile TEXT NOT NULL,answer TEXT NOT NULL,created_at TEXT NOT NULL,group_id TEXT); CREATE TABLE IF NOT EXISTS events(id INTEGER PRIMARY KEY AUTOINCREMENT,task_id TEXT NOT NULL,sequence INTEGER NOT NULL,event_type TEXT NOT NULL,payload TEXT NOT NULL,created_at TEXT NOT NULL,UNIQUE(task_id,sequence));")?;
    let _ = db.execute(
        "ALTER TABLE llm_answers ADD COLUMN prompt_hash TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = db.execute("ALTER TABLE tasks ADD COLUMN group_id TEXT", []);
    db.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_captures_owner_received ON captures(owner_user_id,received_at DESC);
         CREATE INDEX IF NOT EXISTS idx_tasks_owner_capture_created ON tasks(owner_user_id,capture_id,created_at DESC);
         CREATE INDEX IF NOT EXISTS idx_analysis_groups_owner_status ON analysis_groups(owner_user_id,status,updated_at DESC);
         CREATE INDEX IF NOT EXISTS idx_analysis_group_captures_group_order ON analysis_group_captures(group_id,page_order);
         CREATE INDEX IF NOT EXISTS idx_tasks_profile_status_created ON tasks(profile,status,created_at);",
    )?;
    if db
        .query_row("SELECT COUNT(*) FROM llm_prompt_configs", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap_or(0)
        == 0
    {
        let prompt = current_prompt();
        db.execute("INSERT INTO llm_prompt_configs(id,prompt,prompt_hash,version,updated_at) VALUES(1,?,?,1,?)", rusqlite::params![prompt, prompt_hash(&prompt), Utc::now().to_rfc3339()])?;
    }
    if db
        .query_row(
            "SELECT COUNT(*) FROM llm_multi_page_prompt_configs",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        == 0
    {
        let prompt = current_multi_page_prompt();
        db.execute("INSERT INTO llm_multi_page_prompt_configs(id,prompt,prompt_hash,version,updated_at) VALUES(1,?,?,1,?)", rusqlite::params![prompt, prompt_hash(&prompt), Utc::now().to_rfc3339()])?;
    }
    if db.query_row("SELECT COUNT(*) FROM users", [], |r| r.get::<_, i64>(0))? == 0 {
        let username = std::env::var("ADMIN_USERNAME").unwrap_or_else(|_| "admin".into());
        let password = std::env::var("ADMIN_PASSWORD").unwrap_or_else(|_| "admin-change-me".into());
        let hash = auth::hash_password(&password).map_err(std::io::Error::other)?;
        db.execute(
            "INSERT INTO users VALUES(?,?,?,?,0,?)",
            rusqlite::params![
                Uuid::new_v4().to_string(),
                username,
                "admin",
                hash,
                Utc::now().to_rfc3339()
            ],
        )?;
        println!(
            "initialized admin account: {} (set ADMIN_PASSWORD in production)",
            username
        );
    }
    let (capture_events, _) = broadcast::channel(64);
    let state = AppState {
        db: Arc::new(Mutex::new(db)),
        data_dir,
        object_store: {
            let endpoint = std::env::var("MINIO_ENDPOINT").ok();
            let key = std::env::var("MINIO_ACCESS_KEY").ok();
            let secret = std::env::var("MINIO_SECRET_KEY").ok();
            match (endpoint, key, secret) {
                (Some(endpoint), Some(key), Some(secret)) => {
                    let region =
                        std::env::var("MINIO_REGION").unwrap_or_else(|_| "us-east-1".into());
                    let conf = aws_config::defaults(BehaviorVersion::latest())
                        .region(aws_sdk_s3::config::Region::new(region))
                        .endpoint_url(endpoint)
                        .credentials_provider(Credentials::new(key, secret, None, None, "minio"))
                        .load()
                        .await;
                    Some(S3Client::new(&conf))
                }
                _ => None,
            }
        },
        object_bucket: std::env::var("MINIO_BUCKET").unwrap_or_else(|_| "sight-relay".into()),
        capture_events,
        devices: Arc::new(Mutex::new(HashMap::new())),
        sessions: SessionStore::default(),
        analysis_slots: Arc::new(Semaphore::new(
            std::env::var("SIGHT_ANALYSIS_CONCURRENCY")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(2)
                .clamp(1, 16),
        )),
        analysis_inflight: Arc::new(Mutex::new(HashSet::new())),
        analysis_active_keys: Arc::new(Mutex::new(HashSet::new())),
        active_analyses: ActiveAnalysisRegistry::default(),
    };
    resume_analysis_jobs(&state);
    let app = Router::new()
        .route("/", get(index))
        .route("/settings", get(settings_original))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/me", get(me))
        .route("/api/v1/admin/users", get(list_users).post(create_user))
        .route(
            "/api/v1/admin/users/{id}",
            axum::routing::delete(delete_user),
        )
        .route("/api/v1/admin/usage", get(usage))
        .route("/api/v1/devices", get(devices).post(bind_device))
        .route("/api/v1/capture/status", get(capture_status))
        .route("/api/v1/devices/{id}", axum::routing::delete(delete_device))
        .route(
            "/api/v1/devices/{id}/rotate-token",
            post(rotate_device_token),
        )
        .route("/api/v1/captures", post(upload))
        .route("/api/v1/capture-groups/submit", post(submit_capture_group))
        .route("/api/v1/capture-groups/cancel", post(cancel_capture_group))
        .route("/api/v1/captures/{id}", get(capture))
        .route("/api/v1/captures/{id}/image", get(image))
        .route("/api/v1/captures/{id}/tasks", post(create_task))
        .route("/api/v1/latest/solve", post(latest_solve))
        .route("/api/v1/latest/solve/stream", post(analysis_stream_compat))
        .route("/api/v1/latest/analysis", get(latest_analysis))
        .route("/api/v1/captures/{id}/analysis", get(capture_analysis))
        .route("/api/v1/captures/{id}/analysis/retry", post(retry_analysis))
        .route("/api/v1/tasks/{id}", get(task))
        .route("/api/v1/tasks/{id}/events", get(events))
        .route("/healthz", get(health))
        .route("/api/v1/ws", get(ws))
        .route("/api/v1/history", get(history))
        .route("/api/v1/profiles", get(profiles))
        .route("/api/v1/config/test-llm", post(test_llm))
        .route("/api/v1/config/llm", get(get_llm).post(save_llm))
        .route("/api/v1/config/llm/", get(get_llm).post(save_llm))
        .layer(DefaultBodyLimit::max(CAPTURE_REQUEST_BODY_LIMIT))
        .layer(CorsLayer::permissive())
        .with_state(state);
    let addr: SocketAddr = std::env::var("SIGHT_BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".into())
        .parse()?;
    println!("sight-relay listening on http://{addr}");
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}
async fn index(headers: HeaderMap, State(s): State<AppState>) -> impl IntoResponse {
    if session(&headers, &s).is_err() {
        return (
            [(header::CACHE_CONTROL, "no-store")],
            Html(include_str!("../assets/login.html").to_string()),
        );
    }
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mobile = ua.contains("mobile")
        || ua.contains("iphone")
        || ua.contains("android")
        || ua.contains("ipad");
    let source = if mobile {
        include_str!("../assets/mobile-main.html")
    } else {
        include_str!("../assets/pc-main.html")
    };
    (
        [(header::CACHE_CONTROL, "no-store")],
        Html(source.to_string()),
    )
}

#[derive(Deserialize)]
struct LoginInput {
    username: String,
    password: String,
}
#[derive(Serialize)]
struct UserInfo {
    id: String,
    username: String,
    role: String,
}
async fn login(
    State(s): State<AppState>,
    Json(input): Json<LoginInput>,
) -> Result<impl IntoResponse, StatusCode> {
    let row =
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT id,role,password_hash FROM users WHERE username=? AND disabled=0",
                [&input.username],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|_| StatusCode::UNAUTHORIZED)?;
    if !auth::verify_password(&input.password, &row.2) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let sid = auth::random_token();
    let role = if row.1 == "admin" {
        auth::Role::Admin
    } else {
        auth::Role::User
    };
    s.sessions.0.lock().unwrap().insert(
        sid.clone(),
        auth::Session {
            user_id: row.0.clone(),
            role,
        },
    );
    let cookie = format!("sight_session={sid}; Path=/; HttpOnly; SameSite=Lax");
    Ok((
        [(header::SET_COOKIE, cookie)],
        Json(UserInfo {
            id: row.0,
            username: input.username,
            role: row.1,
        }),
    ))
}
async fn me(State(s): State<AppState>, headers: HeaderMap) -> Result<Json<UserInfo>, StatusCode> {
    let cookie = headers.get(header::COOKIE).and_then(|v| v.to_str().ok());
    let session = auth::cookie_session(cookie, &s.sessions).ok_or(StatusCode::UNAUTHORIZED)?;
    let row =
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT id,username,role FROM users WHERE id=?",
                [&session.user_id],
                |r| {
                    Ok(UserInfo {
                        id: r.get(0)?,
                        username: r.get(1)?,
                        role: r.get(2)?,
                    })
                },
            )
            .map_err(|_| StatusCode::UNAUTHORIZED)?;
    Ok(Json(row))
}
#[derive(Deserialize)]
struct CreateUserInput {
    username: String,
    password: String,
}
#[derive(Serialize)]
struct UserPage {
    items: Vec<UserInfo>,
    page: u32,
    page_size: u32,
    total: i64,
}
async fn list_users(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<UserPage>, StatusCode> {
    require_admin(&headers, &s)?;
    let db = s.db.lock().unwrap();
    let page = q.page.unwrap_or(1).max(1);
    let size = q.limit.unwrap_or(5).clamp(1, 5);
    let offset = (page - 1) * size;
    let total: i64 = db
        .query_row("SELECT COUNT(*) FROM users WHERE disabled=0", [], |r| {
            r.get(0)
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut st = db
        .prepare("SELECT id,username,role FROM users WHERE disabled=0 ORDER BY created_at LIMIT ? OFFSET ?")
        .map_err(|error| {
            eprintln!("history query prepare failed: {error}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let rows = st
        .query_map(params![size as i64, offset as i64], |r| {
            Ok(UserInfo {
                id: r.get(0)?,
                username: r.get(1)?,
                role: r.get(2)?,
            })
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(UserPage {
        items: rows.filter_map(Result::ok).collect(),
        page,
        page_size: size,
        total,
    }))
}
async fn delete_user(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let sess = require_admin(&headers, &s)?;
    if sess.user_id == id {
        return Err(StatusCode::BAD_REQUEST);
    }
    let role: String =
        s.db.lock()
            .unwrap()
            .query_row("SELECT role FROM users WHERE id=?", [&id], |r| r.get(0))
            .map_err(|_| StatusCode::NOT_FOUND)?;
    if role == "admin" {
        return Err(StatusCode::FORBIDDEN);
    }
    s.db.lock()
        .unwrap()
        .execute("UPDATE users SET disabled=1 WHERE id=?", [&id])
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct UsageUser {
    user_id: String,
    username: String,
    total_calls: i64,
    completed_calls: i64,
    failed_calls: i64,
    active_calls: i64,
    last_called_at: Option<String>,
}

#[derive(Serialize)]
struct UsageDay {
    date: String,
    calls: i64,
}

#[derive(Serialize)]
struct UsageReport {
    total_calls: i64,
    completed_calls: i64,
    failed_calls: i64,
    active_calls: i64,
    users: Vec<UsageUser>,
    daily: Vec<UsageDay>,
}

async fn usage(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<UsageReport>, StatusCode> {
    require_admin(&headers, &s)?;
    let db = s.db.lock().unwrap();
    let mut users_stmt = db
        .prepare(
            "SELECT u.id,u.username,COUNT(t.id),\
             COALESCE(SUM(CASE WHEN t.status='completed' THEN 1 ELSE 0 END),0),\
             COALESCE(SUM(CASE WHEN t.status IN ('failed','timed_out') THEN 1 ELSE 0 END),0),\
             COALESCE(SUM(CASE WHEN t.status IN ('queued','parsing') THEN 1 ELSE 0 END),0),\
             MAX(t.created_at) \
             FROM users u LEFT JOIN tasks t ON t.owner_user_id=u.id \
             WHERE u.disabled=0 GROUP BY u.id ORDER BY COUNT(t.id) DESC,u.username",
        )
        .map_err(|error| {
            eprintln!("usage query prepare failed: {error}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let users = users_stmt
        .query_map([], |row| {
            Ok(UsageUser {
                user_id: row.get(0)?,
                username: row.get(1)?,
                total_calls: row.get(2)?,
                completed_calls: row.get(3)?,
                failed_calls: row.get(4)?,
                active_calls: row.get(5)?,
                last_called_at: row.get(6)?,
            })
        })
        .map_err(|error| {
            eprintln!("usage query execution failed: {error}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            eprintln!("usage row conversion failed: {error}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut daily_stmt = db
        .prepare(
            "SELECT substr(created_at,1,10),COUNT(*) FROM tasks \
             WHERE datetime(replace(created_at,'T',' ')) >= datetime('now','-13 days') \
             GROUP BY substr(created_at,1,10) ORDER BY substr(created_at,1,10)",
        )
        .map_err(|error| {
            eprintln!("usage daily query prepare failed: {error}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let daily = daily_stmt
        .query_map([], |row| {
            Ok(UsageDay {
                date: row.get(0)?,
                calls: row.get(1)?,
            })
        })
        .map_err(|error| {
            eprintln!("usage daily query execution failed: {error}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            eprintln!("usage daily row conversion failed: {error}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(UsageReport {
        total_calls: users.iter().map(|user| user.total_calls).sum(),
        completed_calls: users.iter().map(|user| user.completed_calls).sum(),
        failed_calls: users.iter().map(|user| user.failed_calls).sum(),
        active_calls: users.iter().map(|user| user.active_calls).sum(),
        users,
        daily,
    }))
}

#[derive(Deserialize)]
struct BindDeviceInput {
    device_id: String,
    name: Option<String>,
}
#[derive(Serialize)]
struct DeviceBinding {
    id: String,
    device_id: String,
    name: String,
    token: String,
}
async fn bind_device(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(i): Json<BindDeviceInput>,
) -> Result<Json<DeviceBinding>, StatusCode> {
    let sess = session(&headers, &s)?;
    if i.device_id.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let id = Uuid::new_v4().to_string();
    let token = auth::random_token();
    let name = i.name.unwrap_or_else(|| i.device_id.clone());
    s.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO devices VALUES(?,?,?,?,?,?)",
            params![
                id,
                sess.user_id,
                i.device_id,
                name,
                token_hash(&token),
                Utc::now().to_rfc3339()
            ],
        )
        .map_err(|_| StatusCode::CONFLICT)?;
    Ok(Json(DeviceBinding {
        id,
        device_id: i.device_id,
        name,
        token,
    }))
}
async fn rotate_device_token(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<DeviceBinding>, StatusCode> {
    let sess = session(&headers, &s)?;
    let token = auth::random_token();
    let (device_id, name) =
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT device_id,name FROM devices WHERE id=? AND owner_user_id=?",
                params![id, sess.user_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(|_| StatusCode::NOT_FOUND)?;
    s.db.lock()
        .unwrap()
        .execute(
            "UPDATE devices SET token_hash=? WHERE id=?",
            params![token_hash(&token), id],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(DeviceBinding {
        id,
        device_id,
        name,
        token,
    }))
}
async fn delete_device(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let sess = session(&headers, &s)?;
    let device_id: String =
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT device_id FROM devices WHERE id=? AND owner_user_id=?",
                params![id, sess.user_id],
                |r| r.get(0),
            )
            .map_err(|_| StatusCode::NOT_FOUND)?;
    s.db.lock()
        .unwrap()
        .execute(
            "DELETE FROM devices WHERE id=? AND owner_user_id=?",
            params![id, sess.user_id],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    s.devices.lock().unwrap().remove(&device_id);
    Ok(StatusCode::NO_CONTENT)
}
fn device_owner(s: &AppState, device_id: &str, token: &str) -> Result<String, StatusCode> {
    s.db.lock()
        .unwrap()
        .query_row(
            "SELECT owner_user_id FROM devices WHERE device_id=? AND token_hash=?",
            params![device_id, token_hash(token)],
            |r| r.get(0),
        )
        .map_err(|_| StatusCode::UNAUTHORIZED)
}

#[derive(Deserialize)]
struct CaptureStatusInput {
    device_id: String,
    token: String,
}

#[derive(Serialize)]
struct CaptureStatusResponse {
    authenticated: bool,
    connected: bool,
    last_heartbeat: Option<String>,
    heartbeat_count: u64,
    reconnect_count: u64,
}

async fn capture_status(
    State(s): State<AppState>,
    Query(input): Query<CaptureStatusInput>,
) -> Result<Json<CaptureStatusResponse>, StatusCode> {
    device_owner(&s, input.device_id.trim(), input.token.trim())?;
    let cutoff = Utc::now() - chrono::Duration::seconds(30);
    let device = s
        .devices
        .lock()
        .unwrap()
        .get(input.device_id.trim())
        .cloned();
    let connected = device.as_ref().is_some_and(|d| {
        d.connected
            && DateTime::parse_from_rfc3339(&d.last_heartbeat)
                .map(|last| last.with_timezone(&Utc) >= cutoff)
                .unwrap_or(false)
    });
    Ok(Json(CaptureStatusResponse {
        authenticated: true,
        connected,
        last_heartbeat: device.as_ref().map(|d| d.last_heartbeat.clone()),
        heartbeat_count: device.as_ref().map_or(0, |d| d.heartbeat_count),
        reconnect_count: device.as_ref().map_or(0, |d| d.reconnect_count),
    }))
}
async fn create_user(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<CreateUserInput>,
) -> Result<Json<UserInfo>, StatusCode> {
    let cookie = headers.get(header::COOKIE).and_then(|v| v.to_str().ok());
    let session = auth::cookie_session(cookie, &s.sessions).ok_or(StatusCode::UNAUTHORIZED)?;
    if session.role != auth::Role::Admin
        || input.username.trim().is_empty()
        || input.password.len() < 8
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let id = Uuid::new_v4().to_string();
    let hash =
        auth::hash_password(&input.password).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    s.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO users VALUES(?,?,?,?,0,?)",
            rusqlite::params![id, input.username, "user", hash, Utc::now().to_rfc3339()],
        )
        .map_err(|_| StatusCode::CONFLICT)?;
    Ok(Json(UserInfo {
        id,
        username: input.username,
        role: "user".into(),
    }))
}

fn settings_navigation(base: &str, is_admin: bool) -> String {
    let admin_navigation = "<button data-p=\"devices\">Capture设备</button><button data-p=\"users\">用户管理</button><button data-p=\"llm\">LLM配置</button><button data-p=\"usage\">使用记录</button>";
    let navigation = if is_admin {
        admin_navigation
    } else {
        "<button data-p=\"devices\">Capture设备</button>"
    };
    base.replace("<button data-p=\"llm\">LLM配置</button>", navigation)
}

async fn settings_original(headers: HeaderMap, State(s): State<AppState>) -> impl IntoResponse {
    let current_session = match session(&headers, &s) {
        Ok(session) => session,
        Err(_) => {
            return (
                [(header::CACHE_CONTROL, "no-store")],
                Html(include_str!("../assets/login.html").to_string()),
            )
                .into_response();
        }
    };
    let base = include_str!("../assets/settings.html")
        .replace("LLM 配置与测试", "LLM配置")
        .replace(
            "value=\"${esc(x.proxy_url||'http://127.0.0.1:7890')}\"",
            "value=\"${esc(x.proxy_url||'')}\"",
        );
    let base = settings_navigation(&base, current_session.role == auth::Role::Admin);
    let capture_script = include_str!("../assets/capture-panel.js");
    let user_script = include_str!("../assets/user-panel.js");
    let panel_styles = include_str!("../assets/panel-buttons.css");
    let llm_styles = include_str!("../assets/llm-panel.css");
    let llm_script = include_str!("../assets/llm-panel.js");
    let history_script = include_str!("../assets/history-panel.js");
    let usage_script = include_str!("../assets/usage-panel.js");
    let injected_assets = if current_session.role == auth::Role::Admin {
        format!(
            "<style>{panel_styles}</style><style>{llm_styles}</style><script>{capture_script}</script><script>{user_script}</script><script>{llm_script}</script><script>{usage_script}</script>"
        )
    } else {
        format!("<style>{panel_styles}</style><script>{capture_script}</script>")
    };
    let html = base.replace(
        "</body>",
        &format!("{injected_assets}<script>{history_script}</script></body>"),
    );
    ([(header::CACHE_CONTROL, "no-store")], Html(html)).into_response()
}
async fn upload(
    State(s): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<CaptureReceipt>, (StatusCode, String)> {
    let mut device = "unknown".to_string();
    let mut token = String::new();
    let mut capture_action = "single".to_string();
    let mut bytes = Vec::new();
    while let Some(field) = multipart.next_field().await.map_err(bad)? {
        let name = field.name().unwrap_or("").to_string();
        if name == "token" {
            token = field.text().await.map_err(bad)?;
        } else if name == "device_id" {
            device = field.text().await.map_err(bad)?
        } else if name == "capture_action" {
            capture_action = field.text().await.map_err(bad)?;
        } else if name == "image" {
            let mut field = field;
            while let Some(chunk) = field.chunk().await.map_err(bad)? {
                if bytes.len().saturating_add(chunk.len()) > CAPTURE_IMAGE_MAX_BYTES {
                    return Err((StatusCode::BAD_REQUEST, "invalid image size".into()));
                }
                bytes.extend_from_slice(&chunk);
            }
        }
    }
    let owner = device_owner(&s, &device, &token)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "invalid device token".into()))?;
    if bytes.is_empty() || bytes.len() > CAPTURE_IMAGE_MAX_BYTES {
        return Err((StatusCode::BAD_REQUEST, "invalid image size".into()));
    }
    let img = image::load_from_memory(&bytes)
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid image".into()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let hash = format!("{:x}", h.finalize());
    let now = Utc::now().to_rfc3339();
    let id = format!("cap_{}", Uuid::new_v4());
    let safe_device: String = device
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let object_key = format!(
        "{}_{}_{}.jpg",
        Utc::now().format("%Y-%m-%d_%H-%M-%S"),
        safe_device,
        &id[4..12]
    );
    if let Some(store) = &s.object_store {
        store
            .put_object()
            .bucket(&s.object_bucket)
            .key(&object_key)
            .content_type("image/jpeg")
            .body(ByteStream::from(bytes.clone()))
            .send()
            .await
            .map_err(internal)?;
    } else {
        let dir = s.data_dir.join("captures");
        fs::create_dir_all(&dir).await.map_err(internal)?;
        fs::write(dir.join(&object_key), &bytes)
            .await
            .map_err(internal)?;
    }
    s.db.lock()
        .unwrap()
        .execute(
            "INSERT INTO captures VALUES(?,?,?,?,?,?,?,?)",
            params![
                id,
                owner.clone(),
                device,
                hash,
                img.width(),
                img.height(),
                object_key,
                now
            ],
        )
        .map_err(internal)?;
    let (task_id, group_id, page_order) = if capture_action == "multi_append" {
        let (group_id, page_order) =
            append_capture_to_group(&s, &owner, &device, &id).map_err(internal)?;
        (None, Some(group_id), Some(page_order))
    } else {
        let task_id = create_analysis_task(&s, &owner, &id).map_err(internal)?;
        (Some(task_id), None, None)
    };
    let event = serde_json::json!({"type":"capture_created","owner_user_id":owner,"capture_id":id,"task_id":task_id,"group_id":group_id,"page_order":page_order,"device_id":device,"received_at":now,"stage":if capture_action == "multi_append" { "group_appended" } else { "upload" }}).to_string();
    let listeners = s.capture_events.send(event).unwrap_or(0);
    println!("capture stored: id={id}, device={device}, websocket_listeners={listeners}");
    if let Some(task_id) = task_id {
        schedule_analysis(s.clone(), task_id);
    }
    Ok(Json(CaptureReceipt {
        capture_id: id,
        sha256: hash,
        bytes: bytes.len(),
        group_id,
        page_order,
    }))
}

fn append_capture_to_group(
    state: &AppState,
    owner_user_id: &str,
    device_id: &str,
    capture_id: &str,
) -> Result<(String, i64), String> {
    let mut db = state
        .db
        .lock()
        .map_err(|_| "database lock poisoned".to_string())?;
    let tx = db.transaction().map_err(|error| error.to_string())?;
    let now = Utc::now().to_rfc3339();
    let group_id = tx
        .query_row(
            "SELECT id FROM analysis_groups WHERE owner_user_id=? AND device_id=? AND status='draft' ORDER BY updated_at DESC LIMIT 1",
            params![owner_user_id, device_id],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| format!("group_{}", Uuid::new_v4()));
    tx.execute(
        "INSERT OR IGNORE INTO analysis_groups(id,owner_user_id,device_id,status,created_at,updated_at) VALUES(?,?,?,?,?,?)",
        params![group_id, owner_user_id, device_id, "draft", now, now],
    ).map_err(|error| error.to_string())?;
    let page_order: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(page_order),0)+1 FROM analysis_group_captures WHERE group_id=?",
            [&group_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    tx.execute(
        "INSERT INTO analysis_group_captures(group_id,capture_id,page_order) VALUES(?,?,?)",
        params![group_id, capture_id, page_order],
    )
    .map_err(|error| error.to_string())?;
    tx.execute(
        "UPDATE analysis_groups SET updated_at=? WHERE id=?",
        params![now, group_id],
    )
    .map_err(|error| error.to_string())?;
    tx.commit().map_err(|error| error.to_string())?;
    Ok((group_id, page_order))
}

#[derive(Serialize)]
struct CaptureGroupActionResponse {
    group_id: Option<String>,
    page_count: i64,
    task_id: Option<String>,
    status: String,
}

async fn submit_capture_group(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<CaptureGroupActionResponse>, (StatusCode, String)> {
    let (device_id, token) = capture_action_credentials(&mut multipart).await?;
    let owner = device_owner(&state, &device_id, &token)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "invalid device token".into()))?;
    let (group_id, task_id, page_count, first_capture) = {
        let mut db = state
            .db
            .lock()
            .map_err(|_| internal("database lock poisoned"))?;
        let tx = db.transaction().map_err(internal)?;
        let group = tx.query_row(
            "SELECT id FROM analysis_groups WHERE owner_user_id=? AND device_id=? AND status='draft' ORDER BY updated_at DESC LIMIT 1",
            params![owner, device_id], |row| row.get::<_, String>(0)).optional().map_err(internal)?;
        let Some(group_id) = group else {
            return Ok(Json(CaptureGroupActionResponse {
                group_id: None,
                page_count: 0,
                task_id: None,
                status: "empty".into(),
            }));
        };
        let (page_count, first_capture): (i64, String) = tx
            .query_row(
                "SELECT COUNT(*),(SELECT capture_id FROM analysis_group_captures WHERE group_id=? ORDER BY page_order DESC LIMIT 1) FROM analysis_group_captures WHERE group_id=?",
                params![&group_id, &group_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(internal)?;
        if page_count == 0 {
            return Ok(Json(CaptureGroupActionResponse {
                group_id: Some(group_id),
                page_count: 0,
                task_id: None,
                status: "empty".into(),
            }));
        }
        let task_id =
            create_group_task_tx(&tx, &owner, &first_capture, &group_id).map_err(internal)?;
        tx.execute(
            "UPDATE analysis_groups SET status='submitted',updated_at=? WHERE id=?",
            params![Utc::now().to_rfc3339(), group_id],
        )
        .map_err(internal)?;
        tx.commit().map_err(internal)?;
        (group_id, task_id, page_count, first_capture)
    };
    let _ = state.capture_events.send(
        serde_json::json!({
            "type": "capture_group_submitted",
            "owner_user_id": owner.clone(),
            "capture_id": first_capture.clone(),
            "task_id": task_id.clone(),
            "group_id": group_id.clone(),
            "page_count": page_count,
            "stage": "group_submitted"
        })
        .to_string(),
    );
    schedule_analysis(state, task_id.clone());
    Ok(Json(CaptureGroupActionResponse {
        group_id: Some(group_id),
        page_count,
        task_id: Some(task_id),
        status: "queued".into(),
    }))
}

async fn cancel_capture_group(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<CaptureGroupActionResponse>, (StatusCode, String)> {
    let (device_id, token) = capture_action_credentials(&mut multipart).await?;
    let owner = device_owner(&state, &device_id, &token)
        .map_err(|_| (StatusCode::UNAUTHORIZED, "invalid device token".into()))?;
    let (group_id, page_count, paths) = {
        let mut db = state
            .db
            .lock()
            .map_err(|_| internal("database lock poisoned"))?;
        let tx = db.transaction().map_err(internal)?;
        let group = tx.query_row(
        "SELECT id FROM analysis_groups WHERE owner_user_id=? AND device_id=? AND status='draft' ORDER BY updated_at DESC LIMIT 1",
        params![owner, device_id], |row| row.get::<_, String>(0)).optional().map_err(internal)?;
        let Some(group_id) = group else {
            return Ok(Json(CaptureGroupActionResponse {
                group_id: None,
                page_count: 0,
                task_id: None,
                status: "empty".into(),
            }));
        };
        let page_count: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM analysis_group_captures WHERE group_id=?",
                [&group_id],
                |row| row.get(0),
            )
            .map_err(internal)?;
        let paths = {
            let mut statement = tx
            .prepare("SELECT c.path FROM analysis_group_captures g JOIN captures c ON c.id=g.capture_id WHERE g.group_id=?")
            .map_err(internal)?;
            statement
                .query_map([&group_id], |row| row.get::<_, String>(0))
                .map_err(internal)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(internal)?
        };
        tx.execute("DELETE FROM captures WHERE id IN (SELECT capture_id FROM analysis_group_captures WHERE group_id=?)", [&group_id]).map_err(internal)?;
        tx.execute(
            "DELETE FROM analysis_group_captures WHERE group_id=?",
            [&group_id],
        )
        .map_err(internal)?;
        tx.execute(
            "UPDATE analysis_groups SET status='cancelled',updated_at=? WHERE id=?",
            params![Utc::now().to_rfc3339(), group_id],
        )
        .map_err(internal)?;
        tx.commit().map_err(internal)?;
        (group_id, page_count, paths)
    };
    if let Some(store) = &state.object_store {
        for path in paths {
            if let Err(error) = store
                .delete_object()
                .bucket(&state.object_bucket)
                .key(&path)
                .send()
                .await
            {
                eprintln!("failed to delete cancelled group object {path}: {error}");
            }
        }
    } else {
        for path in paths {
            if !FsPath::new(&path).is_absolute() && !path.starts_with("./") {
                let _ = std::fs::remove_file(state.data_dir.join("captures").join(path));
            }
        }
    }
    let _ = state.capture_events.send(
        serde_json::json!({
            "type": "capture_group_cancelled",
            "owner_user_id": owner.clone(),
            "group_id": group_id.clone(),
            "page_count": page_count,
            "stage": "group_cancelled"
        })
        .to_string(),
    );
    Ok(Json(CaptureGroupActionResponse {
        group_id: Some(group_id),
        page_count,
        task_id: None,
        status: "cancelled".into(),
    }))
}

async fn capture_action_credentials(
    multipart: &mut Multipart,
) -> Result<(String, String), (StatusCode, String)> {
    let mut device_id = String::new();
    let mut token = String::new();
    while let Some(field) = multipart.next_field().await.map_err(bad)? {
        match field.name().unwrap_or("") {
            "device_id" => device_id = field.text().await.map_err(bad)?,
            "token" => token = field.text().await.map_err(bad)?,
            _ => {}
        }
    }
    Ok((device_id, token))
}

fn create_group_task_tx(
    tx: &rusqlite::Transaction<'_>,
    owner_user_id: &str,
    capture_id: &str,
    group_id: &str,
) -> Result<String, rusqlite::Error> {
    let id = format!("task_{}", Uuid::new_v4());
    tx.execute(
        "INSERT INTO tasks(id,owner_user_id,capture_id,status,profile,answer,created_at,group_id) VALUES(?,?,?,?,?,?,?,?)",
        params![id, owner_user_id, capture_id, "queued", "multi_page", "", Utc::now().to_rfc3339(), group_id],
    )?;
    Ok(id)
}
async fn capture(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Capture>, StatusCode> {
    let c = s.db.lock().unwrap();
    let sess = session(&headers, &s)?;
    c.query_row(
        "SELECT id,device_id,sha256,width,height,received_at FROM captures WHERE id=? AND owner_user_id=?",
        params![id,sess.user_id],
        |r| {
            Ok(Capture {
                id: r.get(0)?,
                device_id: r.get(1)?,
                sha256: r.get(2)?,
                width: r.get(3)?,
                height: r.get(4)?,
                received_at: r.get(5)?,
            })
        },
    )
    .map(Json)
    .map_err(|_| StatusCode::NOT_FOUND)
}
async fn image(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, StatusCode> {
    let sess = session(&headers, &s)?;
    let p: String =
        s.db.lock()
            .unwrap()
            .query_row(
                "SELECT path FROM captures WHERE id=? AND owner_user_id=?",
                params![id, sess.user_id],
                |r| r.get(0),
            )
            .map_err(|_| StatusCode::NOT_FOUND)?;
    let b = if let Some(store) = &s.object_store {
        store
            .get_object()
            .bucket(&s.object_bucket)
            .key(&p)
            .send()
            .await
            .map_err(|_| StatusCode::NOT_FOUND)?
            .body
            .collect()
            .await
            .map_err(|_| StatusCode::NOT_FOUND)?
            .into_bytes()
            .to_vec()
    } else {
        let local_path = if std::path::Path::new(&p).is_absolute() || p.starts_with("./") {
            PathBuf::from(&p)
        } else {
            s.data_dir.join("captures").join(&p)
        };
        fs::read(local_path)
            .await
            .map_err(|_| StatusCode::NOT_FOUND)?
    };
    Ok(([(header::CONTENT_TYPE, "image/jpeg")], b).into_response())
}
async fn create_task(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(cid): Path<String>,
    Json(_req): Json<TaskRequest>,
) -> Result<Json<Task>, StatusCode> {
    let sess = session(&headers, &s)?;
    let snapshot = analysis_snapshot_for_capture(&s, &sess.user_id, &cid)?;
    if let Some(task) = snapshot.task {
        if matches!(task.status.as_str(), "queued" | "parsing" | "completed") {
            return Ok(Json(task));
        }
        s.db.lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .execute(
                "UPDATE tasks SET status='queued',answer='' WHERE id=?",
                [&task.id],
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        schedule_analysis(s, task.id.clone());
        return Ok(Json(Task {
            status: "queued".into(),
            answer: String::new(),
            ..task
        }));
    }
    let task_id = create_analysis_task(&s, &sess.user_id, &cid)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    schedule_analysis(s, task_id.clone());
    Ok(Json(Task {
        id: task_id,
        capture_id: cid,
        status: "queued".into(),
        answer: String::new(),
    }))
}
#[derive(Serialize)]
struct LatestSolve {
    capture: Capture,
    result: serde_json::Value,
}

async fn latest_solve(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<LatestSolve>, StatusCode> {
    let snapshot = latest_analysis(State(s), headers).await?.0;
    let result = snapshot
        .task
        .map(|task| serde_json::json!({"raw":task.answer,"status":task.status,"task_id":task.id}))
        .unwrap_or_else(|| serde_json::json!({"raw":"","status":"queued"}));
    Ok(Json(LatestSolve {
        capture: snapshot.capture,
        result,
    }))
}

const DEFAULT_LLM_PROMPT: &str = r#"
你是一个严谨的图像题目识别与解答助手。请严格按照下面的决策流程处理这张截图。

一、先判断是否存在“可作答的完整题目”
1. 只关注截图中真实的题干、选项、填空线、公式、代码和题目编号；忽略浏览器/应用标题栏、按钮、状态栏、设备信息、时间、网页导航和其他界面文案。
2. “完整题目”必须至少包含清晰可读的题干，并且题干没有被截图边缘截断。选择题还应尽量能读清选项；关键条件、数字、单位、代码或图表看不清时，不得猜测。
3. 如果没有任何可可靠识别的完整题目，禁止推断或编造答案，按“未识别到完整题目，暂不作答”处理。

二、多个题目时的选择规则
1. 按从上到下、从左到右的阅读顺序，选择遇到的第一个“完整题目”；后面的题目全部忽略。
2. 如果最先出现的题目不完整，但后面有一个完整题目，则处理后面第一个完整题目。
3. 不要把同一题的题干、选项或分页内容拆成多个题目，也不要同时回答多个题目。

三、识别与解答规则
1. 尽可能逐字保留所选题目的题干和全部可见选项原文，包括编号、标点、大小写、公式和代码；看不清的局部写“[看不清]”，不要用常识补全。
2. 判断题型后再作答，题型可能是：单选题、多选题、判断题、填空题、简答题、计算/证明题、阅读理解题、算法/编程题或其他明确类型。
3. 单选/多选只给出能由题干确定的选项；填空题给出每个空的答案；计算/证明题展示必要的关键步骤；简答题给出直接、简洁的要点；算法题给出思路、关键边界条件和复杂度，只有截图明确指定语言时才提供该语言代码。
4. 如果题干或关键选项存在无法消除的识别歧义，明确说明“关键信息无法辨认，无法可靠作答”，不要猜答案。
5. 只基于截图中可见内容和通用知识作答，不要声称执行了代码、访问了网页或验证了外部资料。

四、输出格式（必须严格遵守）
只用中文 Markdown，并且只能有以下两个二级标题，标题文字必须完全一致，不要添加括号、解释或占位文案：
## 题目
在这里写题型（如能判断）、题干和选项原文。若未识别到完整题目，写“未识别到完整题目，暂不作答”。

## 答案和解析
正常识别时按“答案：…”和“解析：…”给出简洁结果；无法可靠识别时写“答案：无法确定。”并说明原因。不要再创建任何其他二级标题，不要输出与本格式无关的前言或结语。
"#;

fn prompt_file_path() -> std::path::PathBuf {
    std::env::var("SIGHT_PROMPT_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(".sight-prompt.md"))
}

fn current_prompt() -> String {
    std::fs::read_to_string(prompt_file_path())
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_LLM_PROMPT.to_string())
}

#[derive(Clone, Copy)]
enum PromptKind {
    Single,
    MultiPage,
}

fn configured_prompt(db: &Connection, kind: PromptKind) -> String {
    let (table, fallback) = match kind {
        PromptKind::Single => ("llm_prompt_configs", current_prompt()),
        PromptKind::MultiPage => ("llm_multi_page_prompt_configs", current_multi_page_prompt()),
    };
    db.query_row(
        &format!("SELECT prompt FROM {table} WHERE id=1"),
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .filter(|prompt| !prompt.trim().is_empty())
    .unwrap_or(fallback)
}

const DEFAULT_MULTI_PAGE_PROMPT: &str = r#"
你是一个严谨的题目解析助手。以下图片全部属于同一道题，是互相补充的证据，不要求它们是连续页，也可能存在重复、重叠、顺序不完整或同一内容被多次截取的情况。请综合所有图片后统一作答。

请遵守以下规则：
1. 将图片中的题干、条件、选项、公式、图表、代码和补充说明合并重建，不要把每张图片当成独立题目。
2. 对重复或重叠内容只保留一份；不要因为同一文字出现在多张图片中而重复输出。
3. 对非连续页面，利用跨图片的上下文补全题目；不要因为缺少页码或页面跳跃就忽略一张图片。
4. 如果不同图片之间出现文字、数字、选项或条件冲突，明确指出冲突和无法确定的部分，不要擅自选择其中一份。
5. 只基于图片中能可靠辨认的内容作答；关键信息无法辨认时，说明原因，不要猜测。
6. 最终只给出一个统一答案和解析，不要按图片逐张回答。

只用中文 Markdown，并且只能有以下两个二级标题：
## 题目
合并写出题型、去重后的题干、选项和必要的补充内容。

## 答案和解析
给出一个统一答案和解析；如果存在图片间冲突或关键信息缺失，要明确说明。
"#;

fn current_multi_page_prompt() -> String {
    std::fs::read_to_string(multi_page_prompt_file_path())
        .ok()
        .filter(|prompt| !prompt.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_MULTI_PAGE_PROMPT.to_string())
}

fn multi_page_prompt_file_path() -> std::path::PathBuf {
    std::env::var("SIGHT_MULTI_PAGE_PROMPT_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(".sight-multi-page-prompt.md"))
}

fn create_analysis_task(
    state: &AppState,
    owner_user_id: &str,
    capture_id: &str,
) -> Result<String, String> {
    let id = format!("task_{}", Uuid::new_v4());
    state
        .db
        .lock()
        .map_err(|_| "database lock poisoned".to_string())?
        .execute(
            "INSERT INTO tasks(id,owner_user_id,capture_id,status,profile,answer,created_at) VALUES(?,?,?,?,?,?,?)",
            params![
                id,
                owner_user_id,
                capture_id,
                "queued",
                "automatic",
                "",
                Utc::now().to_rfc3339()
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(id)
}

fn set_analysis_state(
    state: &AppState,
    task_id: &str,
    status: &str,
    answer: Option<&str>,
) -> Result<(), String> {
    let mut db = state
        .db
        .lock()
        .map_err(|_| "database lock poisoned".to_string())?;
    let transaction = db.transaction().map_err(|error| error.to_string())?;
    if let Some(answer) = answer {
        transaction
            .execute(
                "UPDATE tasks SET status=?,answer=? WHERE id=?",
                params![status, answer, task_id],
            )
            .map_err(|error| error.to_string())?;
    } else {
        transaction
            .execute(
                "UPDATE tasks SET status=? WHERE id=?",
                params![status, task_id],
            )
            .map_err(|error| error.to_string())?;
    }
    let sequence: i64 = transaction
        .query_row(
            "SELECT COALESCE(MAX(sequence),0)+1 FROM events WHERE task_id=?",
            [task_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "INSERT INTO events(task_id,sequence,event_type,payload,created_at) VALUES(?,?,?,?,?)",
            params![
                task_id,
                sequence,
                format!("analysis.{status}"),
                "{}",
                Utc::now().to_rfc3339()
            ],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())
}

fn mark_task_cancelled(db: &mut Connection, task_id: &str) -> Result<bool, String> {
    let transaction = db.transaction().map_err(|error| error.to_string())?;
    let changed = transaction
        .execute(
            "UPDATE tasks SET status='cancelled',answer='' WHERE id=? AND status IN ('queued','parsing')",
            [task_id],
        )
        .map_err(|error| error.to_string())?;
    if changed == 0 {
        return Ok(false);
    }
    let sequence: i64 = transaction
        .query_row(
            "SELECT COALESCE(MAX(sequence),0)+1 FROM events WHERE task_id=?",
            [task_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "INSERT INTO events(task_id,sequence,event_type,payload,created_at) VALUES(?,?,?,?,?)",
            params![
                task_id,
                sequence,
                "analysis.cancelled",
                r#"{"reason":"superseded_by_new_capture"}"#,
                Utc::now().to_rfc3339()
            ],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(true)
}

fn cancel_superseded_analysis(state: &AppState, owner_user_id: &str, analysis: &ActiveAnalysis) {
    let cancelled = state
        .db
        .lock()
        .map_err(|_| "database lock poisoned".to_string())
        .and_then(|mut db| mark_task_cancelled(&mut db, &analysis.task_id));
    match cancelled {
        Ok(true) => broadcast_analysis_state(
            state,
            owner_user_id,
            &analysis.capture_id,
            &analysis.task_id,
            "cancelled",
            Some("已停止：有更新的截图需要解析"),
        ),
        Ok(false) => {}
        Err(error) => eprintln!(
            "failed to persist cancelled analysis: task={}, error={error}",
            analysis.task_id
        ),
    }
}

fn update_analysis_answer(state: &AppState, task_id: &str, answer: &str) -> Result<(), String> {
    state
        .db
        .lock()
        .map_err(|_| "database lock poisoned".to_string())?
        .execute(
            "UPDATE tasks SET answer=? WHERE id=? AND status='parsing'",
            params![answer, task_id],
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn broadcast_analysis_state(
    state: &AppState,
    owner_user_id: &str,
    capture_id: &str,
    task_id: &str,
    stage: &str,
    error: Option<&str>,
) {
    let _ = state.capture_events.send(
        serde_json::json!({
            "type":"capture_stage",
            "owner_user_id":owner_user_id,
            "capture_id":capture_id,
            "task_id":task_id,
            "stage":stage,
            "error":error
        })
        .to_string(),
    );
}

fn should_reschedule_analysis(was_cancelled: bool, persisted_status: &str) -> bool {
    !was_cancelled && persisted_status == "queued"
}

fn schedule_analysis(state: AppState, task_id: String) {
    if !state
        .analysis_inflight
        .lock()
        .map(|mut jobs| jobs.insert(task_id.clone()))
        .unwrap_or(false)
    {
        return;
    }
    let Ok((owner_user_id, capture_id)) = analysis_job_identity(&state, &task_id) else {
        if let Ok(mut jobs) = state.analysis_inflight.lock() {
            jobs.remove(&task_id);
        }
        return;
    };
    let registration = state
        .active_analyses
        .replace(&owner_user_id, &task_id, &capture_id);
    if let Some(superseded) = &registration.superseded {
        cancel_superseded_analysis(&state, &owner_user_id, superseded);
    }
    let cancellation = registration.cancellation;
    tokio::spawn(async move {
        let execution = async {
            let Ok(_permit) = state.analysis_slots.clone().acquire_owned().await else {
                return Ok(());
            };
            run_analysis_job(&state, &task_id).await
        };
        let result = tokio::select! {
            _ = cancellation.cancelled() => None,
            result = execution => Some(result),
        };
        let was_cancelled = result.is_none();
        if let Some(Err((status, message))) = result {
            let job = analysis_job_identity(&state, &task_id).ok();
            let _ = set_analysis_state(&state, &task_id, status, Some(&message));
            if let Some((owner_user_id, capture_id)) = job {
                broadcast_analysis_state(
                    &state,
                    &owner_user_id,
                    &capture_id,
                    &task_id,
                    status,
                    Some(&message),
                );
            }
            eprintln!("analysis failed: task={task_id}, status={status}, error={message}");
        }
        state
            .active_analyses
            .remove_if_current(&owner_user_id, &task_id);
        if let Ok(mut jobs) = state.analysis_inflight.lock() {
            jobs.remove(&task_id);
        }
        let persisted_status = state
            .db
            .lock()
            .ok()
            .and_then(|db| {
                db.query_row("SELECT status FROM tasks WHERE id=?", [&task_id], |row| {
                    row.get::<_, String>(0)
                })
                .ok()
            })
            .unwrap_or_default();
        if should_reschedule_analysis(was_cancelled, &persisted_status) {
            schedule_analysis(state.clone(), task_id);
        }
    });
}

fn resume_analysis_jobs(state: &AppState) {
    let task_ids = state
        .db
        .lock()
        .ok()
        .and_then(|db| {
            let mut statement = db
                .prepare("SELECT id FROM tasks WHERE profile='automatic' AND status IN ('queued','parsing') ORDER BY created_at")
                .ok()?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0)).ok()?;
            Some(rows.filter_map(Result::ok).collect::<Vec<_>>())
        })
        .unwrap_or_default();
    for task_id in task_ids {
        let _ = set_analysis_state(state, &task_id, "queued", None);
        schedule_analysis(state.clone(), task_id);
    }
}

fn analysis_job_identity(state: &AppState, task_id: &str) -> Result<(String, String), String> {
    state
        .db
        .lock()
        .map_err(|_| "database lock poisoned".to_string())?
        .query_row(
            "SELECT owner_user_id,capture_id FROM tasks WHERE id=?",
            [task_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| error.to_string())
}

struct AnalysisKeyGuard {
    active_keys: Arc<Mutex<HashSet<String>>>,
    key: String,
}

impl Drop for AnalysisKeyGuard {
    fn drop(&mut self) {
        if let Ok(mut active_keys) = self.active_keys.lock() {
            active_keys.remove(&self.key);
        }
    }
}

async fn acquire_analysis_key(state: &AppState, key: String) -> AnalysisKeyGuard {
    loop {
        let acquired = state
            .analysis_active_keys
            .lock()
            .map(|mut active_keys| active_keys.insert(key.clone()))
            .unwrap_or(false);
        if acquired {
            return AnalysisKeyGuard {
                active_keys: state.analysis_active_keys.clone(),
                key,
            };
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn capture_bytes(state: &AppState, path: &str) -> Result<Vec<u8>, String> {
    if let Some(store) = &state.object_store {
        return store
            .get_object()
            .bucket(&state.object_bucket)
            .key(path)
            .send()
            .await
            .map_err(|error| error.to_string())?
            .body
            .collect()
            .await
            .map(|body| body.into_bytes().to_vec())
            .map_err(|error| error.to_string());
    }
    let path = if FsPath::new(path).is_absolute() || path.starts_with("./") {
        PathBuf::from(path)
    } else {
        state.data_dir.join("captures").join(path)
    };
    fs::read(path).await.map_err(|error| error.to_string())
}

async fn run_analysis_job(state: &AppState, task_id: &str) -> Result<(), (&'static str, String)> {
    let (owner_user_id, capture_id, group_id, sha256, path) = state
        .db
        .lock()
        .map_err(|_| ("failed", "database lock poisoned".to_string()))?
        .query_row(
            "SELECT t.owner_user_id,t.capture_id,t.group_id,c.sha256,c.path FROM tasks t JOIN captures c ON c.id=t.capture_id AND c.owner_user_id=t.owner_user_id WHERE t.id=?",
            [task_id],
            |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,String>(3)?,row.get::<_,String>(4)?)),
        )
        .map_err(|error| ("failed", error.to_string()))?;

    set_analysis_state(state, task_id, "parsing", Some("")).map_err(|error| ("failed", error))?;
    broadcast_analysis_state(state, &owner_user_id, &capture_id, task_id, "parsing", None);

    let llm = active_llm_config();
    let (image_data, cache_sha, prompt) = if let Some(group_id) = group_id.as_deref() {
        let captures = state
            .db
            .lock()
            .map_err(|_| ("failed", "database lock poisoned".to_string()))?
            .prepare("SELECT c.sha256,c.path FROM analysis_group_captures g JOIN captures c ON c.id=g.capture_id WHERE g.group_id=? ORDER BY g.page_order")
            .map_err(|error| ("failed", error.to_string()))?
            .query_map([group_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map_err(|error| ("failed", error.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| ("failed", error.to_string()))?;
        let mut bytes = Vec::with_capacity(captures.len());
        for (sha, path) in captures {
            bytes.push((
                sha,
                capture_bytes(state, &path)
                    .await
                    .map_err(|error| ("failed", error))?,
            ));
        }
        let joined = bytes
            .iter()
            .map(|(sha, _)| sha.as_str())
            .collect::<Vec<_>>()
            .join("|");
        let multi_prompt = configured_prompt(
            &*state
                .db
                .lock()
                .map_err(|_| ("failed", "database lock poisoned".to_string()))?,
            PromptKind::MultiPage,
        );
        (bytes, joined, multi_prompt)
    } else {
        (
            vec![(
                sha256.clone(),
                capture_bytes(state, &path)
                    .await
                    .map_err(|error| ("failed", format!("读取截图失败：{error}")))?,
            )],
            sha256,
            configured_prompt(
                &*state
                    .db
                    .lock()
                    .map_err(|_| ("failed", "database lock poisoned".to_string()))?,
                PromptKind::Single,
            ),
        )
    };
    // Prompt 也是解析结果的一部分：热切换 Prompt 后必须绕过旧答案缓存。
    let cache_fingerprint =
        analysis_cache_fingerprint(&format!("{cache_sha}:{}", prompt_hash(&prompt)), &llm);
    let analysis_key = format!("{owner_user_id}:{cache_sha}:{cache_fingerprint}");
    let _analysis_key_guard = acquire_analysis_key(state, analysis_key).await;
    let cached = state
        .db
        .lock()
        .map_err(|_| ("failed", "database lock poisoned".to_string()))?
        .query_row(
            "SELECT answer FROM llm_answers WHERE owner_user_id=? AND sha256=? AND prompt_hash=?",
            params![owner_user_id, cache_sha, cache_fingerprint],
            |row| row.get::<_, String>(0),
        )
        .ok();
    if let Some(answer) = cached {
        set_analysis_state(state, task_id, "completed", Some(&answer))
            .map_err(|error| ("failed", error))?;
        let _ = state.capture_events.send(
            serde_json::json!({"type":"analysis_completed","owner_user_id":owner_user_id,"capture_id":capture_id,"task_id":task_id,"answer":answer,"cached":true}).to_string(),
        );
        broadcast_analysis_state(
            state,
            &owner_user_id,
            &capture_id,
            task_id,
            "complete",
            None,
        );
        return Ok(());
    }

    if llm.api_key.trim().is_empty() {
        return Err(("failed", "未配置当前供应商的 API Key".to_string()));
    }
    let image_urls = image_data
        .iter()
        .map(|(_, bytes)| {
            format!(
                "data:image/jpeg;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )
        })
        .collect::<Vec<_>>();
    let image_refs = image_urls.iter().map(String::as_str).collect::<Vec<_>>();
    let body = build_chat_completion_body_with_images(&llm, &prompt, &image_refs, true)
        .map_err(|error| ("failed", error))?;
    let client = llm_client(&llm.proxy_url).map_err(|error| ("failed", error.to_string()))?;
    let request = client
        .post(chat_completions_url(&llm.base_url))
        .bearer_auth(&llm.api_key)
        .json(&body)
        .send();
    let response = tokio::time::timeout(Duration::from_secs(30), request)
        .await
        .map_err(|_| ("timed_out", "等待 LLM 首次响应超过 30 秒".to_string()))?
        .map_err(|error| ("failed", format!("LLM 请求失败：{error}")))?;
    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        return Err((
            "failed",
            format!(
                "LLM 返回 HTTP {status}：{}",
                detail.chars().take(300).collect::<String>()
            ),
        ));
    }

    let started = Instant::now();
    let mut upstream = response.bytes_stream();
    let mut buffer = Vec::new();
    let mut answer = String::new();
    let mut sequence = 0u64;
    let mut done = false;
    while !done {
        if started.elapsed() >= Duration::from_secs(180) {
            return Err(("timed_out", "LLM 解析总耗时超过 180 秒".to_string()));
        }
        let next = tokio::time::timeout(Duration::from_secs(30), upstream.next())
            .await
            .map_err(|_| ("timed_out", "LLM 流连续 30 秒没有返回数据".to_string()))?;
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|error| ("failed", format!("LLM 流中断：{error}")))?;
        buffer.extend_from_slice(&chunk);
        while let Some(frame) = take_sse_frame(&mut buffer) {
            let frame = String::from_utf8_lossy(&frame);
            for line in frame.lines() {
                let Some(data) = line.strip_prefix("data:").map(str::trim_start) else {
                    continue;
                };
                if data == "[DONE]" {
                    done = true;
                    continue;
                }
                let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                    continue;
                };
                if let Some(error) = value.get("error") {
                    let message = error
                        .get("message")
                        .and_then(|message| message.as_str())
                        .unwrap_or("LLM 返回流式错误");
                    return Err(("failed", message.to_string()));
                }
                if let Some(delta) = extract_chat_completion_delta(&value) {
                    answer.push_str(delta);
                    sequence += 1;
                    let _ = update_analysis_answer(state, task_id, &answer);
                    let _ = state.capture_events.send(
                        serde_json::json!({"type":"analysis_delta","owner_user_id":owner_user_id,"capture_id":capture_id,"task_id":task_id,"sequence":sequence,"delta":delta}).to_string(),
                    );
                }
            }
        }
    }
    if answer.trim().is_empty() {
        return Err(("failed", "LLM 未返回可显示的文本".to_string()));
    }
    state
        .db
        .lock()
        .map_err(|_| ("failed", "database lock poisoned".to_string()))?
        .execute(
            "INSERT INTO llm_answers(owner_user_id,sha256,answer,created_at,prompt_hash) VALUES(?,?,?,?,?) ON CONFLICT(owner_user_id,sha256) DO UPDATE SET answer=excluded.answer,created_at=excluded.created_at,prompt_hash=excluded.prompt_hash",
            params![owner_user_id, cache_sha, answer, Utc::now().to_rfc3339(), cache_fingerprint],
        )
        .map_err(|error| ("failed", error.to_string()))?;
    set_analysis_state(state, task_id, "completed", Some(&answer))
        .map_err(|error| ("failed", error))?;
    let _ = state.capture_events.send(
        serde_json::json!({"type":"analysis_completed","owner_user_id":owner_user_id,"capture_id":capture_id,"task_id":task_id,"answer":answer,"cached":false}).to_string(),
    );
    broadcast_analysis_state(
        state,
        &owner_user_id,
        &capture_id,
        task_id,
        "complete",
        None,
    );
    Ok(())
}

fn merge_env_content(existing: &str, updates: &[(&str, &str)]) -> String {
    let updates: HashMap<&str, &str> = updates.iter().copied().collect();
    let mut written = std::collections::HashSet::new();
    let mut output = String::new();

    for line in existing.lines() {
        let candidate = line
            .trim_start()
            .strip_prefix("export ")
            .unwrap_or_else(|| line.trim_start());
        let key = candidate.split_once('=').map(|(key, _)| key.trim());
        if let Some((&key, &value)) = key.and_then(|key| updates.get_key_value(key)) {
            if written.insert(key) {
                output.push_str(key);
                output.push('=');
                output.push_str(value);
                output.push('\n');
            }
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }

    for (&key, &value) in &updates {
        if written.insert(key) {
            output.push_str(key);
            output.push('=');
            output.push_str(value);
            output.push('\n');
        }
    }
    output
}

fn is_safe_env_value(value: &str) -> bool {
    !value.contains(['\n', '\r', '\0'])
}

static LLM_ENV_WRITE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static LLM_CONFIG_SAVE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn persist_env_updates(path: &FsPath, updates: &[(&str, &str)]) -> std::io::Result<()> {
    let _guard = LLM_ENV_WRITE_LOCK.lock().await;
    let existing = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let merged = merge_env_content(&existing, updates);
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| FsPath::new("."));
    tokio::fs::create_dir_all(parent).await?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid env path"))?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4().simple()));

    let result = async {
        tokio::fs::write(&temporary, merged).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600)).await?;
        }
        tokio::fs::File::open(&temporary).await?.sync_all().await?;
        tokio::fs::rename(&temporary, path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

async fn write_file_atomically(path: &FsPath, contents: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension(format!(
        "{}{}.tmp",
        path.extension()
            .and_then(|value| value.to_str())
            .map(|value| format!("{value}."))
            .unwrap_or_default(),
        Uuid::new_v4().simple()
    ));
    tokio::fs::write(&temporary, contents).await?;
    if let Err(error) = tokio::fs::rename(&temporary, path).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error);
    }
    Ok(())
}

async fn restore_file(path: &FsPath, previous: Option<&[u8]>) {
    if let Some(previous) = previous {
        let _ = write_file_atomically(path, previous).await;
    } else {
        let _ = tokio::fs::remove_file(path).await;
    }
}

fn analysis_snapshot_for_capture(
    state: &AppState,
    owner_user_id: &str,
    capture_id: &str,
) -> Result<AnalysisSnapshot, StatusCode> {
    let db = state
        .db
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let capture = db
        .query_row(
            "SELECT id,device_id,sha256,width,height,received_at FROM captures WHERE id=? AND owner_user_id=?",
            params![capture_id, owner_user_id],
            |row| {
                Ok(Capture {
                    id: row.get(0)?,
                    device_id: row.get(1)?,
                    sha256: row.get(2)?,
                    width: row.get(3)?,
                    height: row.get(4)?,
                    received_at: row.get(5)?,
                })
            },
        )
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let task = db
        .query_row(
            "SELECT id,capture_id,status,answer FROM tasks WHERE owner_user_id=? AND capture_id=? AND profile IN ('automatic','multi_page') ORDER BY created_at DESC LIMIT 1",
            params![owner_user_id, capture_id],
            |row| {
                Ok(Task {
                    id: row.get(0)?,
                    capture_id: row.get(1)?,
                    status: row.get(2)?,
                    answer: row.get(3)?,
                })
            },
        )
        .ok()
        .or_else(|| {
            db.query_row(
                "SELECT t.id,t.capture_id,t.status,t.answer FROM tasks t JOIN analysis_group_captures g ON g.group_id=t.group_id WHERE t.owner_user_id=? AND g.capture_id=? ORDER BY t.created_at DESC LIMIT 1",
                params![owner_user_id, capture_id],
                |row| Ok(Task { id: row.get(0)?, capture_id: row.get(1)?, status: row.get(2)?, answer: row.get(3)? }),
            ).ok()
        });
    let group_id = task
        .as_ref()
        .and_then(|task| {
            db.query_row("SELECT group_id FROM tasks WHERE id=?", [&task.id], |row| {
                row.get::<_, Option<String>>(0)
            })
            .ok()
            .flatten()
        })
        .or_else(|| {
            db.query_row(
                "SELECT group_id FROM analysis_group_captures WHERE capture_id=?",
                [capture_id],
                |row| row.get::<_, String>(0),
            )
            .ok()
        });
    let group = group_id.and_then(|group_id| {
        let mut statement = db
            .prepare(
                "SELECT c.id,c.device_id,c.sha256,c.width,c.height,c.received_at FROM analysis_group_captures g JOIN captures c ON c.id=g.capture_id WHERE g.group_id=? AND c.owner_user_id=? ORDER BY g.page_order",
            )
            .ok()?;
        let captures = statement
            .query_map(params![group_id, owner_user_id], |row| {
                Ok(Capture {
                    id: row.get(0)?,
                    device_id: row.get(1)?,
                    sha256: row.get(2)?,
                    width: row.get(3)?,
                    height: row.get(4)?,
                    received_at: row.get(5)?,
                })
            })
            .ok()?
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        Some(CaptureGroupSnapshot {
            id: group_id,
            page_count: captures.len() as i64,
            captures,
        })
    });
    Ok(AnalysisSnapshot {
        capture,
        task,
        group,
    })
}

async fn latest_analysis(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<AnalysisSnapshot>, StatusCode> {
    let session = session(&headers, &state)?;
    let capture_id = state
        .db
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .query_row(
            "SELECT id FROM captures WHERE owner_user_id=? ORDER BY received_at DESC LIMIT 1",
            [session.user_id.clone()],
            |row| row.get::<_, String>(0),
        )
        .map_err(|_| StatusCode::NOT_FOUND)?;
    analysis_snapshot_for_capture(&state, &session.user_id, &capture_id).map(Json)
}

async fn capture_analysis(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(capture_id): Path<String>,
) -> Result<Json<AnalysisSnapshot>, StatusCode> {
    let session = session(&headers, &state)?;
    analysis_snapshot_for_capture(&state, &session.user_id, &capture_id).map(Json)
}

async fn retry_analysis(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(capture_id): Path<String>,
) -> Result<Json<Task>, StatusCode> {
    let session = session(&headers, &state)?;
    let snapshot = analysis_snapshot_for_capture(&state, &session.user_id, &capture_id)?;
    if let Some(task) = snapshot.task {
        if matches!(task.status.as_str(), "queued" | "parsing") {
            return Ok(Json(task));
        }
        state
            .db
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .execute(
                "UPDATE tasks SET status='queued',answer='' WHERE id=?",
                [&task.id],
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        schedule_analysis(state, task.id.clone());
        return Ok(Json(Task {
            status: "queued".into(),
            answer: String::new(),
            ..task
        }));
    }
    let task_id = create_analysis_task(&state, &session.user_id, &capture_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    schedule_analysis(state, task_id.clone());
    Ok(Json(Task {
        id: task_id,
        capture_id,
        status: "queued".into(),
        answer: String::new(),
    }))
}

async fn analysis_stream_compat(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let snapshot = latest_analysis(State(state), headers).await?.0;
    let mut payload = format!(
        "event: meta\ndata: {}\n\n",
        serde_json::json!({"capture":snapshot.capture})
    );
    if let Some(task) = snapshot.task
        && !task.answer.is_empty()
    {
        payload.push_str(&format!(
            "data: {}\n\n",
            serde_json::to_string(&task.answer).unwrap_or_else(|_| "\"\"".into())
        ));
    }
    payload.push_str("data: [DONE]\n\n");
    Ok((
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        payload,
    )
        .into_response())
}

async fn task(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Task>, StatusCode> {
    let session = session(&headers, &s)?;
    s.db.lock()
        .unwrap()
        .query_row(
            "SELECT id,capture_id,status,answer FROM tasks WHERE id=? AND owner_user_id=?",
            params![id, session.user_id],
            |r| {
                Ok(Task {
                    id: r.get(0)?,
                    capture_id: r.get(1)?,
                    status: r.get(2)?,
                    answer: r.get(3)?,
                })
            },
        )
        .map(Json)
        .map_err(|_| StatusCode::NOT_FOUND)
}
fn bad<E: ToString>(e: E) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, e.to_string())
}
fn internal<E: ToString>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

async fn health() -> impl IntoResponse {
    ([(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")], "ok")
}
#[derive(Serialize)]
struct Event {
    sequence: i64,
    event_type: String,
    payload: String,
    created_at: String,
}
async fn events(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Vec<Event>>, StatusCode> {
    let session = session(&headers, &s)?;
    let db = s.db.lock().unwrap();
    let mut st=db.prepare("SELECT e.sequence,e.event_type,e.payload,e.created_at FROM events e JOIN tasks t ON t.id=e.task_id WHERE e.task_id=? AND t.owner_user_id=? ORDER BY e.sequence").map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?;
    let rows = st
        .query_map(params![id, session.user_id], |r| {
            Ok(Event {
                sequence: r.get(0)?,
                event_type: r.get(1)?,
                payload: r.get(2)?,
                created_at: r.get(3)?,
            })
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(rows.filter_map(Result::ok).collect()))
}

async fn ws(
    ws: WebSocketUpgrade,
    State(s): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    println!("websocket client connected");
    let viewer = session(&headers, &s).ok().map(|x| x.user_id);
    ws.on_upgrade(move |socket| ws_loop(socket, s, viewer))
}
async fn ws_loop(mut socket: WebSocket, s: AppState, viewer: Option<String>) {
    let mut events = s.capture_events.subscribe();
    loop {
        let msg = tokio::select! { incoming = socket.recv() => incoming, event = events.recv() => {
            if let Ok(payload) = event {
                let allowed = viewer.as_ref().and_then(|uid| serde_json::from_str::<serde_json::Value>(&payload).ok().map(|v| v.get("owner_user_id").and_then(|x|x.as_str())==Some(uid))).unwrap_or(false);
                if allowed { let _ = socket.send(Message::Text(payload.into())).await; }
            }
            continue;
        }};
        let Some(Ok(msg)) = msg else {
            break;
        };
        match msg {
            Message::Ping(data) => {
                let _ = socket.send(Message::Pong(data)).await;
            }
            Message::Text(t) => {
                let value: serde_json::Value = serde_json::from_str(&t).unwrap_or_default();
                let kind = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if kind == "viewer_ping" {
                    println!("websocket viewer_ping received");
                }
                if kind == "heartbeat"
                    && device_owner(
                        &s,
                        value
                            .get("device_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                        value.get("token").and_then(|v| v.as_str()).unwrap_or(""),
                    )
                    .is_err()
                {
                    println!(
                        "websocket heartbeat rejected: device={:?}, invalid token",
                        value.get("device_id")
                    );
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
                if kind == "heartbeat" {
                    println!(
                        "websocket heartbeat received: device={:?}",
                        value.get("device_id")
                    );
                }
                let response = if kind == "heartbeat" {
                    if let Some(id) = value.get("device_id").and_then(|v| v.as_str()) {
                        let owner_id = device_owner(
                            &s,
                            id,
                            value.get("token").and_then(|v| v.as_str()).unwrap_or(""),
                        )
                        .ok();
                        let mut devices = s.devices.lock().unwrap();
                        let entry = devices.entry(id.to_string()).or_insert(DeviceStatus {
                            device_id: id.to_string(),
                            connected: true,
                            last_heartbeat: String::new(),
                            heartbeat_count: 0,
                            reconnect_count: 0,
                        });
                        if !entry.connected {
                            entry.reconnect_count += 1;
                        }
                        entry.connected = true;
                        entry.last_heartbeat = Utc::now().to_rfc3339();
                        entry.heartbeat_count += 1;
                        let _ = s.capture_events.send(serde_json::json!({"type":"device_status","owner_user_id":owner_id,"device":entry}).to_string());
                    }
                    serde_json::json!({"type":"heartbeat_ack","device_id":value.get("device_id"),"server_time":Utc::now().to_rfc3339()})
                } else {
                    serde_json::json!({"type":"ack"})
                };
                let _ = socket
                    .send(Message::Text(response.to_string().into()))
                    .await;
            }
            Message::Close(_) => return,
            _ => {}
        }
    }
    // Do not mark a device offline immediately when one socket closes. During
    // reconnect, the old socket can close just after the replacement socket
    // has already delivered a heartbeat. The stale-heartbeat watchdog below
    // is the single source of truth for offline state.
    println!("websocket client disconnected");
}

async fn devices(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<ManagedDevice>>, StatusCode> {
    let sess = session(&headers, &s)?;
    // A TCP/WebSocket connection can remain half-open without delivering a
    // close frame. Treat a device as offline when no authenticated heartbeat
    // has arrived for 30 seconds (three expected 10s intervals).
    let cutoff = Utc::now() - chrono::Duration::seconds(30);
    let mut devices = s.devices.lock().unwrap();
    for device in devices.values_mut() {
        if let Ok(last) = DateTime::parse_from_rfc3339(&device.last_heartbeat)
            && last.with_timezone(&Utc) < cutoff
        {
            device.connected = false;
        }
    }
    let bound: Vec<(String, String, String, String)> = {
        let db = s.db.lock().unwrap();
        let mut st = db
            .prepare("SELECT id,device_id,name,created_at FROM devices WHERE owner_user_id=? ORDER BY created_at DESC")
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        st.query_map([sess.user_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .filter_map(Result::ok)
        .collect()
    };
    Ok(Json(
        bound
            .into_iter()
            .map(|(id, device_id, name, created_at)| {
                let live = devices.get(&device_id);
                ManagedDevice {
                    id,
                    device_id,
                    name,
                    created_at,
                    connected: live.map(|x| x.connected).unwrap_or(false),
                    last_heartbeat: live.map(|x| x.last_heartbeat.clone()).unwrap_or_default(),
                    heartbeat_count: live.map(|x| x.heartbeat_count).unwrap_or(0),
                    reconnect_count: live.map(|x| x.reconnect_count).unwrap_or(0),
                }
            })
            .collect(),
    ))
}

#[derive(Serialize)]
struct HistoryItem {
    capture_id: String,
    device_id: String,
    received_at: String,
    task_count: i64,
    answer_preview: String,
    answer_status: String,
    group_id: Option<String>,
    group_page_count: i64,
}
#[derive(Deserialize)]
struct HistoryQuery {
    page: Option<u32>,
    limit: Option<u32>,
}
#[derive(Serialize)]
struct HistoryPage {
    items: Vec<HistoryItem>,
    page: u32,
    page_size: u32,
    total: i64,
}

fn answer_preview(answer: &str) -> String {
    let mut chars = answer.chars();
    let preview: String = chars.by_ref().take(120).collect();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

async fn history(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<HistoryPage>, StatusCode> {
    let sess = session(&headers, &s)?;
    let db = s.db.lock().unwrap();
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.limit.unwrap_or(5).clamp(1, 50);
    let offset = (page - 1) * page_size;
    let total: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM captures WHERE owner_user_id=?",
            [&sess.user_id],
            |r| r.get(0),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut st = db
        .prepare(
            "SELECT c.id,c.device_id,c.received_at,COUNT(t.id),\
             COALESCE((SELECT answer FROM tasks t2 WHERE t2.capture_id=c.id ORDER BY t2.created_at DESC LIMIT 1),''),\
             COALESCE((SELECT status FROM tasks t3 WHERE t3.capture_id=c.id ORDER BY t3.created_at DESC LIMIT 1),'') \
             ,(SELECT group_id FROM analysis_group_captures g2 WHERE g2.capture_id=c.id LIMIT 1)\
             ,(SELECT COUNT(*) FROM analysis_group_captures g3 WHERE g3.group_id=(SELECT group_id FROM analysis_group_captures g4 WHERE g4.capture_id=c.id LIMIT 1))\
             FROM captures c LEFT JOIN tasks t ON t.capture_id=c.id \
             WHERE c.owner_user_id=? GROUP BY c.id ORDER BY c.received_at DESC LIMIT ? OFFSET ?",
        )
        .map_err(|error| {
            eprintln!("history query prepare failed: {error}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let rows = st
        .query_map(
            rusqlite::params![sess.user_id, page_size as i64, offset as i64],
            |r| {
                Ok(HistoryItem {
                    capture_id: r.get(0)?,
                    device_id: r.get(1)?,
                    received_at: r.get(2)?,
                    task_count: r.get(3)?,
                    answer_preview: answer_preview(&r.get::<_, String>(4)?),
                    answer_status: r.get(5)?,
                    group_id: r.get(6)?,
                    group_page_count: r.get(7)?,
                })
            },
        )
        .map_err(|error| {
            eprintln!("history query execution failed: {error}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let items = rows.collect::<Result<Vec<_>, _>>().map_err(|error| {
        eprintln!("history row conversion failed: {error}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    Ok(Json(HistoryPage {
        items,
        page,
        page_size,
        total,
    }))
}
#[derive(Serialize)]
struct Profile {
    id: &'static str,
    name: &'static str,
    description: &'static str,
}
async fn profiles() -> Json<Vec<Profile>> {
    Json(vec![
        Profile {
            id: "default",
            name: "Default",
            description: "General screenshot analysis",
        },
        Profile {
            id: "choice_question",
            name: "Choice question",
            description: "Extract question and options",
        },
        Profile {
            id: "code",
            name: "Code",
            description: "Explain code and errors",
        },
    ])
}

#[derive(Serialize)]
struct LlmTest {
    ok: bool,
    model: String,
    message: String,
}
async fn test_llm(
    headers: HeaderMap,
    State(s): State<AppState>,
) -> Result<Json<LlmTest>, StatusCode> {
    require_admin(&headers, &s)?;
    let llm = active_llm_config();
    let model = llm.model.clone();
    if llm.api_key.trim().is_empty() {
        return Ok(Json(LlmTest {
            ok: false,
            model,
            message: "未配置当前供应商的 API Key".into(),
        }));
    }
    // 64×64 白色 PNG，满足已知视觉供应商对最小图片尺寸的限制。
    const TEST_IMAGE: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAS0lEQVR42u3PMQ0AAAwDoPo33UrYvQQckD4XAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAYHLAMpT0sIcNbcEAAAAAElFTkSuQmCC";
    let body = match build_chat_completion_body(
        &llm,
        "This is a connectivity and vision-input test. Reply with exactly: OK",
        Some(TEST_IMAGE),
        true,
    ) {
        Ok(body) => body,
        Err(message) => {
            return Ok(Json(LlmTest {
                ok: false,
                model,
                message,
            }));
        }
    };
    let client = match llm_client_with_timeout(Some(Duration::from_secs(30)), &llm.proxy_url) {
        Ok(client) => client,
        Err(e) => {
            return Ok(Json(LlmTest {
                ok: false,
                model,
                message: format!("客户端初始化失败：{e}"),
            }));
        }
    };
    let started = Instant::now();
    let response = match client
        .post(chat_completions_url(&llm.base_url))
        .bearer_auth(&llm.api_key)
        .json(&body)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response,
        Ok(response) => {
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            return Ok(Json(LlmTest {
                ok: false,
                model,
                message: format!(
                    "Chat Completions 返回 HTTP {status}：{}",
                    detail.chars().take(300).collect::<String>()
                ),
            }));
        }
        Err(e) => {
            let proxy = llm.proxy_url.clone();
            let message = if e.is_connect() && !proxy.trim().is_empty() {
                format!(
                    "代理连接失败：{}。当前代理为 {}，请清空代理后直连，或确认该代理端口正在运行。",
                    e, proxy
                )
            } else if e.is_timeout() {
                format!("请求超时：{}。请检查网络、代理或服务端地址。", e)
            } else {
                format!("请求失败：{}", e)
            };
            return Ok(Json(LlmTest {
                ok: false,
                model,
                message,
            }));
        }
    };
    let first_delta = tokio::time::timeout(Duration::from_secs(30), async move {
        let mut upstream = response.bytes_stream();
        let mut buffer = Vec::new();
        while let Some(chunk) = upstream.next().await {
            let chunk = chunk.map_err(|error| format!("LLM 流中断：{error}"))?;
            buffer.extend_from_slice(&chunk);
            while let Some(frame) = take_sse_frame(&mut buffer) {
                let frame = String::from_utf8_lossy(&frame);
                for line in frame.lines() {
                    let Some(data) = line.strip_prefix("data:").map(str::trim_start) else {
                        continue;
                    };
                    if data == "[DONE]" {
                        return Err("LLM 流结束但没有返回文本".to_string());
                    }
                    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                        continue;
                    };
                    if let Some(error) = value.get("error") {
                        return Err(error
                            .get("message")
                            .and_then(|message| message.as_str())
                            .unwrap_or("LLM 返回流式错误")
                            .to_string());
                    }
                    if let Some(delta) = extract_chat_completion_delta(&value)
                        && !delta.is_empty()
                    {
                        return Ok(());
                    }
                }
            }
        }
        Err("LLM 流结束但没有返回文本".to_string())
    })
    .await;
    match first_delta {
        Ok(Ok(())) => Ok(Json(LlmTest {
            ok: true,
            model,
            message: format!(
                "Chat Completions 配置正常，首字响应延迟 {} ms",
                started.elapsed().as_millis()
            ),
        })),
        Ok(Err(message)) => Ok(Json(LlmTest {
            ok: false,
            model,
            message,
        })),
        Err(_) => Ok(Json(LlmTest {
            ok: false,
            model,
            message: "等待 LLM 首字响应超过 30 秒".into(),
        })),
    }
}
#[derive(Serialize)]
struct LlmConfig {
    provider: String,
    providers: Vec<ProviderPreset>,
    base_url: String,
    proxy_url: String,
    api_key_masked: String,
    model: String,
    reasoning_effort: String,
    thinking_mode: String,
    thinking_keep: String,
    clear_thinking: bool,
    extra_body_json: String,
    prompt: String,
    multi_page_prompt: String,
}
#[derive(Deserialize)]
struct LlmConfigInput {
    provider: String,
    base_url: String,
    #[serde(default)]
    proxy_url: String,
    api_key: String,
    model: String,
    reasoning_effort: String,
    #[serde(default)]
    thinking_mode: String,
    #[serde(default = "default_thinking_keep")]
    thinking_keep: String,
    #[serde(default = "default_true")]
    clear_thinking: bool,
    #[serde(default = "default_extra_body_json")]
    extra_body_json: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    multi_page_prompt: String,
}
fn default_thinking_keep() -> String {
    "null".into()
}
fn default_true() -> bool {
    true
}
fn default_extra_body_json() -> String {
    "{}".into()
}

fn current_llm(db: Option<&Connection>) -> LlmConfig {
    let llm = active_llm_config();
    let provider_keys = provider_api_keys_snapshot();
    let profiles = provider_profiles_snapshot();
    let mut providers = provider_presets();
    for preset in &mut providers {
        let provider = LlmProvider::parse(&preset.id);
        preset.has_api_key = provider_keys
            .get(&provider)
            .is_some_and(|key| !key.is_empty());
        preset.api_key_masked = provider_keys
            .get(&provider)
            .map(|key| mask_api_key(key))
            .unwrap_or_default();
        if let Some(profile) = profiles.get(&provider) {
            preset.base_url = profile.base_url.clone();
            preset.proxy_url = profile.proxy_url.clone();
            preset.model = profile.model.clone();
            preset.reasoning_effort = profile.reasoning_effort.clone();
            preset.thinking_mode = profile.thinking_mode.clone();
            preset.thinking_keep = profile.thinking_keep.clone();
            preset.clear_thinking = profile.clear_thinking;
            preset.extra_body_json = profile.extra_body_json.clone();
        }
    }
    LlmConfig {
        provider: llm.provider.as_str().into(),
        providers,
        base_url: llm.base_url,
        proxy_url: llm.proxy_url,
        api_key_masked: mask_api_key(&llm.api_key),
        model: llm.model,
        reasoning_effort: llm.reasoning_effort,
        thinking_mode: llm.thinking_mode,
        thinking_keep: llm.thinking_keep,
        clear_thinking: llm.clear_thinking,
        extra_body_json: llm.extra_body_json,
        prompt: db
            .map(|conn| configured_prompt(conn, PromptKind::Single))
            .unwrap_or_else(current_prompt),
        multi_page_prompt: db
            .map(|conn| configured_prompt(conn, PromptKind::MultiPage))
            .unwrap_or_else(current_multi_page_prompt),
    }
}
async fn get_llm(
    headers: HeaderMap,
    State(s): State<AppState>,
) -> Result<Json<LlmConfig>, StatusCode> {
    require_admin(&headers, &s)?;
    let db = s.db.lock().unwrap();
    Ok(Json(current_llm(Some(&db))))
}
async fn save_llm(
    headers: HeaderMap,
    State(s): State<AppState>,
    Json(i): Json<LlmConfigInput>,
) -> Result<Json<LlmConfig>, StatusCode> {
    let admin = require_admin(&headers, &s)?;
    let provider = LlmProvider::parse(&i.provider);
    let extra_body_json = serde_json::from_str::<serde_json::Value>(i.extra_body_json.trim())
        .ok()
        .filter(|value| value.is_object())
        .and_then(|value| serde_json::to_string(&value).ok())
        .ok_or(StatusCode::BAD_REQUEST)?;
    if provider.as_str() != i.provider.trim().to_ascii_lowercase()
        || i.base_url.trim().is_empty()
        || i.model.is_empty()
        || i.prompt.chars().count() > 50_000
        || i.multi_page_prompt.chars().count() > 50_000
        || i.extra_body_json.len() > 20_000
        || [
            i.provider.as_str(),
            i.base_url.as_str(),
            i.proxy_url.as_str(),
            i.api_key.as_str(),
            i.model.as_str(),
            i.reasoning_effort.as_str(),
            i.thinking_mode.as_str(),
            i.thinking_keep.as_str(),
        ]
        .iter()
        .any(|value| !is_safe_env_value(value))
        || !i.base_url.trim().starts_with("http://") && !i.base_url.trim().starts_with("https://")
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut candidate = LlmRuntimeConfig {
        provider,
        base_url: i.base_url.trim().into(),
        proxy_url: i.proxy_url.trim().into(),
        api_key: String::new(),
        model: i.model.trim().into(),
        reasoning_effort: i.reasoning_effort.clone(),
        thinking_mode: i.thinking_mode.clone(),
        thinking_keep: i.thinking_keep.clone(),
        clear_thinking: i.clear_thinking,
        extra_body_json: extra_body_json.clone(),
    };
    if build_chat_completion_body(&candidate, "validation", None, false).is_err() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let _save_guard = LLM_CONFIG_SAVE_LOCK.lock().await;
    let mut provider_keys = provider_api_keys_snapshot();
    let api_key = resolve_provider_api_key(&mut provider_keys, provider, &i.api_key)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    candidate.api_key = api_key.clone();
    let mut provider_profiles = provider_profiles_snapshot();
    let mut saved_profile = candidate.clone();
    saved_profile.api_key.clear();
    provider_profiles.insert(provider, saved_profile);
    let prompt_path = prompt_file_path();
    let multi_prompt_path = multi_page_prompt_file_path();
    let previous_prompt_file = tokio::fs::read(&prompt_path).await.ok();
    let previous_multi_prompt_file = tokio::fs::read(&multi_prompt_path).await.ok();
    let previous_prompt_row = {
        let db = s.db.lock().unwrap();
        db.query_row(
            "SELECT prompt,prompt_hash,version,updated_by,updated_at FROM llm_prompt_configs WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .ok()
    };
    let previous_multi_prompt_row = {
        let db = s.db.lock().unwrap();
        db.query_row(
            "SELECT prompt,prompt_hash,version,updated_by,updated_at FROM llm_multi_page_prompt_configs WHERE id=1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .ok()
    };
    let prompt_text = if i.prompt.trim().is_empty() {
        let db = s.db.lock().unwrap();
        db.query_row(
            "SELECT prompt FROM llm_prompt_configs WHERE id=1",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap_or_else(|_| current_prompt())
    } else {
        i.prompt.trim().to_string()
    };
    let multi_prompt_text = if i.multi_page_prompt.trim().is_empty() {
        let db = s.db.lock().unwrap();
        configured_prompt(&db, PromptKind::MultiPage)
    } else {
        i.multi_page_prompt.trim().to_string()
    };
    write_file_atomically(&prompt_path, prompt_text.as_bytes())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if write_file_atomically(&multi_prompt_path, multi_prompt_text.as_bytes())
        .await
        .is_err()
    {
        restore_file(&prompt_path, previous_prompt_file.as_deref()).await;
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let digest = prompt_hash(&prompt_text);
    let multi_digest = prompt_hash(&multi_prompt_text);
    let prompt_db_result: Result<(), rusqlite::Error> = (|| {
        let mut db = s.db.lock().unwrap();
        let tx = db.transaction()?;
        let next_version: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(version),0)+1 FROM llm_prompt_configs",
                [],
                |r| r.get(0),
            )
            .unwrap_or(1);
        tx.execute(
            "INSERT INTO llm_prompt_configs(id,prompt,prompt_hash,version,updated_by,updated_at) VALUES(1,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET prompt=excluded.prompt,prompt_hash=excluded.prompt_hash,version=excluded.version,updated_by=excluded.updated_by,updated_at=excluded.updated_at",
            rusqlite::params![prompt_text, digest, next_version, admin.user_id, Utc::now().to_rfc3339()],
        )?;
        let multi_version: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(version),0)+1 FROM llm_multi_page_prompt_configs",
                [],
                |r| r.get(0),
            )
            .unwrap_or(1);
        tx.execute(
            "INSERT INTO llm_multi_page_prompt_configs(id,prompt,prompt_hash,version,updated_by,updated_at) VALUES(1,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET prompt=excluded.prompt,prompt_hash=excluded.prompt_hash,version=excluded.version,updated_by=excluded.updated_by,updated_at=excluded.updated_at",
            rusqlite::params![multi_prompt_text, multi_digest, multi_version, admin.user_id, Utc::now().to_rfc3339()],
        )?;
        tx.commit()
    })();
    if prompt_db_result.is_err() {
        restore_file(&prompt_path, previous_prompt_file.as_deref()).await;
        restore_file(&multi_prompt_path, previous_multi_prompt_file.as_deref()).await;
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    // 配置文件最后落盘：Prompt 文件或数据库更新失败时，避免出现接口返回失败，
    // 但重启后却意外加载了新供应商配置的半保存状态。
    let env_path = PathBuf::from(std::env::var("SIGHT_ENV_FILE").unwrap_or_else(|_| ".env".into()));
    let mut owned_env_updates = vec![
        ("AI_PROVIDER".to_string(), provider.as_str().to_string()),
        ("AI_BASE_URL".to_string(), String::new()),
        ("AI_PROXY".to_string(), String::new()),
        ("AI_API_KEY".to_string(), String::new()),
        ("AI_MODEL".to_string(), String::new()),
        ("AI_REASONING_EFFORT".to_string(), String::new()),
        ("AI_THINKING_MODE".to_string(), String::new()),
        ("AI_THINKING_KEEP".to_string(), String::new()),
        ("AI_CLEAR_THINKING".to_string(), String::new()),
        ("AI_EXTRA_BODY_JSON".to_string(), String::new()),
    ];
    for saved_provider in all_llm_providers() {
        if let Some(saved_key) = provider_keys.get(&saved_provider) {
            owned_env_updates.push((
                provider_api_key_env(saved_provider).to_string(),
                saved_key.clone(),
            ));
        }
        if let Some(profile) = provider_profiles.get(&saved_provider) {
            for (field, value) in [
                ("BASE_URL", profile.base_url.clone()),
                ("PROXY", profile.proxy_url.clone()),
                ("MODEL", profile.model.clone()),
                ("REASONING_EFFORT", profile.reasoning_effort.clone()),
                ("THINKING_MODE", profile.thinking_mode.clone()),
                ("THINKING_KEEP", profile.thinking_keep.clone()),
                ("CLEAR_THINKING", profile.clear_thinking.to_string()),
                ("EXTRA_BODY_JSON", profile.extra_body_json.clone()),
            ] {
                owned_env_updates.push((provider_profile_env_key(saved_provider, field), value));
            }
        }
    }
    let env_updates = owned_env_updates
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let env_result = persist_env_updates(&env_path, &env_updates).await;
    if env_result.is_err() {
        restore_file(&prompt_path, previous_prompt_file.as_deref()).await;
        restore_file(&multi_prompt_path, previous_multi_prompt_file.as_deref()).await;
        let db = s.db.lock().unwrap();
        if let Some((prompt, prompt_hash, version, updated_by, updated_at)) = previous_prompt_row {
            let _ = db.execute(
                "INSERT INTO llm_prompt_configs(id,prompt,prompt_hash,version,updated_by,updated_at) VALUES(1,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET prompt=excluded.prompt,prompt_hash=excluded.prompt_hash,version=excluded.version,updated_by=excluded.updated_by,updated_at=excluded.updated_at",
                rusqlite::params![prompt, prompt_hash, version, updated_by, updated_at],
            );
        } else {
            let _ = db.execute("DELETE FROM llm_prompt_configs WHERE id=1", []);
        }
        if let Some((prompt, prompt_hash, version, updated_by, updated_at)) =
            previous_multi_prompt_row
        {
            let _ = db.execute(
                "INSERT INTO llm_multi_page_prompt_configs(id,prompt,prompt_hash,version,updated_by,updated_at) VALUES(1,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET prompt=excluded.prompt,prompt_hash=excluded.prompt_hash,version=excluded.version,updated_by=excluded.updated_by,updated_at=excluded.updated_at",
                rusqlite::params![prompt, prompt_hash, version, updated_by, updated_at],
            );
        } else {
            let _ = db.execute("DELETE FROM llm_multi_page_prompt_configs WHERE id=1", []);
        }
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    replace_provider_api_keys(provider_keys);
    replace_provider_profiles(provider_profiles);
    replace_active_llm_config(candidate);
    let db = s.db.lock().unwrap();
    Ok(Json(current_llm(Some(&db))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updating_llm_env_preserves_unrelated_configuration_and_comments() {
        let existing = "# Server\nADMIN_USERNAME=admin\nMINIO_BUCKET=sight-relay\nOPENAI_MODEL=old-model\n\n# Agent\nSIGHT_DEVICE_TOKEN=keep-me\n";
        let updated = merge_env_content(
            existing,
            &[
                ("OPENAI_BASE_URL", "https://api.openai.com/v1"),
                ("OPENAI_PROXY", "http://127.0.0.1:7890"),
                ("OPENAI_API_KEY", "secret"),
                ("OPENAI_MODEL", "new-model"),
                ("OPENAI_REASONING_EFFORT", "high"),
            ],
        );

        assert!(updated.contains("# Server\nADMIN_USERNAME=admin\nMINIO_BUCKET=sight-relay\n"));
        assert!(updated.contains("OPENAI_MODEL=new-model\n"));
        assert!(!updated.contains("OPENAI_MODEL=old-model"));
        assert!(updated.contains("# Agent\nSIGHT_DEVICE_TOKEN=keep-me\n"));
        assert!(updated.contains("OPENAI_PROXY=http://127.0.0.1:7890\n"));
    }

    #[test]
    fn updating_llm_env_removes_stale_duplicate_keys() {
        let existing = "OPENAI_MODEL=old\nexport OPENAI_MODEL=older\nKEEP=value\n";
        let updated = merge_env_content(existing, &[("OPENAI_MODEL", "new")]);

        assert_eq!(updated.matches("OPENAI_MODEL=").count(), 1);
        assert!(updated.contains("OPENAI_MODEL=new\n"));
        assert!(updated.contains("KEEP=value\n"));
    }

    #[test]
    fn env_values_reject_line_break_injection() {
        assert!(is_safe_env_value("http://127.0.0.1:7890"));
        assert!(!is_safe_env_value("value\nADMIN_PASSWORD=overwritten"));
        assert!(!is_safe_env_value("value\rOTHER=overwritten"));
        assert!(!is_safe_env_value("value\0tail"));
    }

    #[tokio::test]
    async fn env_updates_are_persisted_without_replacing_other_settings() {
        let dir = std::env::temp_dir().join(format!("sight-relay-env-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".env");
        std::fs::write(
            &path,
            "ADMIN_USERNAME=admin\nOPENAI_MODEL=old\nMINIO_BUCKET=shots\n",
        )
        .unwrap();

        persist_env_updates(&path, &[("OPENAI_MODEL", "new")])
            .await
            .unwrap();

        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("ADMIN_USERNAME=admin\n"));
        assert!(saved.contains("OPENAI_MODEL=new\n"));
        assert!(saved.contains("MINIO_BUCKET=shots\n"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn server_assets_do_not_depend_on_repository_level_design_files() {
        let source = include_str!("main.rs");
        let repository_doc = ["..", "..", "..", "doc", ""].join("/");
        let repository_web = ["..", "..", "..", "web", ""].join("/");
        assert!(!source.contains(&repository_doc));
        assert!(!source.contains(&repository_web));
    }

    #[test]
    fn main_pages_only_observe_server_managed_analysis_jobs() {
        for page in [
            include_str!("../assets/pc-main.html"),
            include_str!("../assets/mobile-main.html"),
        ] {
            assert!(page.contains("/api/v1/latest/analysis"));
            assert!(!page.contains("/api/v1/latest/solve"));
            assert!(page.contains("analysis_delta"));
            assert!(page.contains("analysis_completed"));
        }
    }

    #[test]
    fn main_pages_understand_superseded_analysis_state() {
        for page in [
            include_str!("../assets/pc-main.html"),
            include_str!("../assets/mobile-main.html"),
        ] {
            assert!(page.contains("cancelled"));
            assert!(page.contains("已被新截图取代"));
        }
    }

    #[test]
    fn pc_main_page_contains_no_mock_screenshot() {
        let page = include_str!("../assets/pc-main.html");
        assert!(!page.contains("data:image/"));
        assert!(!page.contains("数学试题.jpg"));
        assert!(page.contains("pc-image-lightbox") && page.contains("pc-image-zoom"));
    }

    #[test]
    fn pc_main_group_pager_is_below_the_image_frame() {
        let page = include_str!("../assets/pc-main.html");
        assert!(!page.contains("group-switcher-top"));
        assert!(page.contains("group-switcher-bottom"));
        let frame = page.find("class=\"image-frame\"").unwrap();
        let pager = page.find("group-switcher-bottom").unwrap();
        assert!(pager > frame);
    }

    #[test]
    fn group_submit_and_cancel_broadcast_state_events_to_main_pages() {
        let source = include_str!("main.rs");
        assert!(source.contains("capture_group_submitted"));
        assert!(source.contains("capture_group_cancelled"));
        let pc = include_str!("../assets/pc-main.html");
        let mobile = include_str!("../assets/mobile-main.html");
        assert!(pc.contains("group_submitted") && pc.contains("group_cancelled"));
        assert!(mobile.contains("group_submitted") && mobile.contains("group_cancelled"));
    }

    #[test]
    fn main_pages_distinguish_draft_group_from_uploaded_analysis() {
        let pc = include_str!("../assets/pc-main.html");
        let mobile = include_str!("../assets/mobile-main.html");
        assert!(pc.contains("题目组已收集") && pc.contains("等待 Option+Z"));
        assert!(mobile.contains("题目组已收集") && mobile.contains("等待 Option+Z"));
    }

    #[test]
    fn cancelling_group_clears_the_current_page_state_without_restoring_old_pages() {
        let pc = include_str!("../assets/pc-main.html");
        let mobile = include_str!("../assets/mobile-main.html");
        assert!(pc.contains("clearGroupView") && pc.contains("题目组已取消"));
        assert!(mobile.contains("clearGroupView") && mobile.contains("题目组已取消"));
    }

    #[test]
    fn pc_group_pager_hidden_attribute_overrides_its_flex_layout() {
        let pc = include_str!("../assets/pc-main.html");
        assert!(pc.contains(".group-switcher[hidden]{display:none}"));
    }

    #[test]
    fn single_capture_event_clears_stale_group_pager_state() {
        let pc = include_str!("../assets/pc-main.html");
        let mobile = include_str!("../assets/mobile-main.html");
        assert!(pc.contains("snapshotEpoch++;currentGroup=null;currentGroupIndex=0;updateGroupControls();currentCaptureId="));
        assert!(
            mobile.contains(
                "snapshotEpoch++;currentGroup=null;currentGroupIndex=0;currentCaptureId="
            )
        );
    }

    #[test]
    fn capture_upload_limits_the_request_and_streams_the_image_field() {
        let source = include_str!("main.rs");
        let (source, _) = source.rsplit_once("#[cfg(test)]").unwrap();

        assert!(source.contains(".layer(DefaultBodyLimit::max(CAPTURE_REQUEST_BODY_LIMIT))"));
        assert!(source.contains("while let Some(chunk) = field.chunk().await"));
    }

    #[test]
    fn settings_page_includes_history_answer_panel() {
        let source = include_str!("main.rs");
        assert!(source.contains("history-panel.js"));
        assert!(source.contains("usage-panel.js"));
        let panel = include_str!("../assets/history-panel.js");
        assert!(panel.contains("/api/v1/captures/") && panel.contains("/analysis"));
        assert!(panel.contains("marked") || panel.contains("renderMarkdown"));
        assert!(panel.contains("history-dialog") && panel.contains("data-history-close"));
        assert!(panel.contains("history-lightbox") && panel.contains("data-history-zoom"));
    }

    #[test]
    fn multi_page_prompt_is_loaded_from_the_hot_config_table() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE llm_multi_page_prompt_configs(\
             id INTEGER PRIMARY KEY CHECK(id=1),prompt TEXT NOT NULL,prompt_hash TEXT NOT NULL,\
             version INTEGER NOT NULL,updated_by TEXT,updated_at TEXT NOT NULL);\
             INSERT INTO llm_multi_page_prompt_configs VALUES(1,'数据库题目组提示词','hash',2,NULL,'now');",
        )
        .unwrap();

        assert_eq!(
            configured_prompt(&db, PromptKind::MultiPage),
            "数据库题目组提示词"
        );
    }

    #[test]
    fn llm_panel_separates_connection_and_both_prompt_editors() {
        let panel = include_str!("../assets/llm-panel.js");
        assert!(panel.contains("data-llm-tab=\"connection\""));
        assert!(panel.contains("data-llm-tab=\"single-prompt\""));
        assert!(panel.contains("data-llm-tab=\"multi-prompt\""));
        assert!(panel.contains("id=\"multiPrompt\""));
    }

    #[test]
    fn settings_history_group_detail_includes_image_navigation() {
        let panel = include_str!("../assets/history-panel.js");
        assert!(panel.contains("data-history-group-prev"));
        assert!(panel.contains("data-history-group-next"));
        assert!(panel.contains("updateGroupDetail"));
    }

    #[test]
    fn settings_navigation_hides_admin_pages_for_regular_users() {
        let regular = settings_navigation("<button data-p=\"llm\">LLM配置</button>", false);
        assert!(regular.contains("data-p=\"devices\""));
        assert!(!regular.contains("data-p=\"llm\""));
        assert!(!regular.contains("data-p=\"users\""));

        let admin = settings_navigation("<button data-p=\"llm\">LLM配置</button>", true);
        assert!(admin.contains("data-p=\"devices\""));
        assert!(admin.contains("data-p=\"users\""));
        assert!(admin.contains("data-p=\"llm\""));
        assert!(admin.contains("data-p=\"usage\""));
    }

    #[test]
    fn capture_panel_separates_device_pagination_from_settings_page_state() {
        let panel = include_str!("../assets/capture-panel.js");
        assert!(panel.contains("let devicePage = 1"));
        assert!(panel.contains("page = 'devices'"));
        assert!(panel.contains("page !== 'devices' || generation !== loadGeneration"));
        assert!(panel.contains("let loadGeneration = 0"));
        assert!(panel.contains("generation !== loadGeneration"));
        assert!(!panel.contains("let page = 1"));
    }

    #[test]
    fn history_answer_preview_is_shortened_with_ellipsis() {
        let long = "字".repeat(121);
        assert_eq!(answer_preview(&long).chars().count(), 123);
        assert!(answer_preview(&long).ends_with("..."));
        assert_eq!(answer_preview("short"), "short");
    }

    #[test]
    fn multi_page_prompt_handles_overlapping_supplemental_images() {
        assert!(DEFAULT_MULTI_PAGE_PROMPT.contains("重复"));
        assert!(DEFAULT_MULTI_PAGE_PROMPT.contains("补充"));
        assert!(DEFAULT_MULTI_PAGE_PROMPT.contains("冲突"));
        assert!(DEFAULT_MULTI_PAGE_PROMPT.contains("合并"));
    }
}
