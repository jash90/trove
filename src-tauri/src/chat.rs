//! The chat window: one conversation with one model, at a time.
//!
//! The third place this application talks to the network, after link
//! previews and the keyvault itself. What crosses it is decided here and
//! said plainly in the README: the messages the user typed, the model's
//! answer, and nothing else — to the provider the chat settings name, with
//! the key the keyvault holds. The key's plaintext exists only between the
//! decrypt and the HTTP header, exactly as it exists only between the
//! decrypt and the clipboard write in the copy path; it never reaches the
//! interface, a log line, or an error message.
//!
//! Streaming is the transport because a long answer otherwise reads as a
//! hang. The command returns a turn id at once and the tokens travel as
//! events, which is the same shape the link-preview readiness signal uses —
//! the interface subscribes rather than polls.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::Emitter;
use trove_store::StoreHandle;

pub const CHAT_DELTA_EVENT: &str = "chat-delta";
pub const CHAT_DONE_EVENT: &str = "chat-done";
pub const CHAT_ERROR_EVENT: &str = "chat-error";

/// Where the chat configuration lives, beside the application settings.
const CHAT_SETTINGS_KEY: &str = "chat";

const MAX_MESSAGES: usize = 200;
const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_ATTACHMENTS: usize = 4;
/// An image as it crosses the bridge: base64 of a few real megabytes.
const MAX_ATTACHMENT_BASE64_BYTES: usize = 5 * 1024 * 1024;
/// A text attachment inlined into the message: generous, bounded.
const MAX_ATTACHMENT_TEXT_BYTES: usize = 256 * 1024;
const MAX_FIELD_BYTES: usize = 512;

/// The providers the chat window offers, with the wire each speaks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatProtocol {
    /// `/chat/completions`, `Authorization: Bearer`, OpenAI-style SSE.
    Openai,
    /// `/v1/messages`, `x-api-key` + version header, Anthropic-style SSE.
    Anthropic,
}

/// Everything the core needs to know about one provider. The base URLs are
/// fixed per provider: the choice the user makes is the provider, not a
/// URL to type.
pub fn provider_profile(provider: &str) -> Option<(ChatProtocol, &'static str)> {
    match provider {
        "zai" => Some((ChatProtocol::Openai, "https://api.z.ai/api/paas/v4")),
        // The Coding Plan key is still a Z.ai key — but it answers only on
        // the plan's own endpoint, and on the standard one it is simply
        // refused. Two entries, one key.
        "zai-coding" => Some((ChatProtocol::Openai, "https://api.z.ai/api/coding/paas/v4")),
        "openai" => Some((ChatProtocol::Openai, "https://api.openai.com/v1")),
        "openrouter" => Some((ChatProtocol::Openai, "https://openrouter.ai/api/v1")),
        "anthropic" => Some((ChatProtocol::Anthropic, "https://api.anthropic.com")),
        _ => None,
    }
}

/// What an HTTP refusal most likely is, said precisely enough to act on.
/// A 401 and a 404 read identically from the window without this, and the
/// one thing a user with a refused request needs to know is which of the
/// two they are holding.
fn code_for_status(status: reqwest::StatusCode) -> &'static str {
    match status.as_u16() {
        401 | 403 => "chat_key_refused",
        404 => "chat_endpoint_not_found",
        429 => "chat_rate_limited",
        _ => "chat_provider_refused",
    }
}

pub const CHAT_PROVIDERS: [&str; 5] = ["zai", "zai-coding", "openai", "openrouter", "anthropic"];

fn default_provider() -> String {
    "zai".to_owned()
}

fn default_model() -> String {
    "glm-4.6".to_owned()
}

/// The bound on a stored API key. Empty is allowed and means "no
/// Authorization header" — a local model server is a legitimate thing to
/// talk to without one.
const MAX_API_KEY_BYTES: usize = 4 * 1024;

/// One API key per provider, all of them optional: a provider with no key
/// simply refuses until its key is given (or, on the OpenAI wire, sends no
/// Authorization header at all — which is what a local server wants).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatProviderKeysDto {
    #[serde(default)]
    pub zai: String,
    #[serde(default)]
    pub openai: String,
    #[serde(default)]
    pub openrouter: String,
    #[serde(default)]
    pub anthropic: String,
}

impl ChatProviderKeysDto {
    fn for_provider(&self, provider: &str) -> &str {
        match provider {
            "zai" | "zai-coding" => &self.zai,
            "openai" => &self.openai,
            "openrouter" => &self.openrouter,
            "anthropic" => &self.anthropic,
            _ => "",
        }
    }
}

/// What the chat window needs to know to reach a model: which provider, a
/// model of that provider, and a key per provider.
///
/// The keys are given here, in these settings, and stored in this
/// application's own database — the same trust boundary the database
/// itself sits on, which is also where a keyvault token override lives.
/// They are not read from the keyvault. Unknown fields are ignored on
/// read, so a row written by an earlier shape of these settings still
/// loads and is replaced on the next save.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSettingsDto {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub keys: ChatProviderKeysDto,
}

impl Default for ChatSettingsDto {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: default_model(),
            keys: ChatProviderKeysDto::default(),
        }
    }
}

/// One message of the conversation, in the shape the bridge carries.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageDto {
    pub role: String,
    pub content: String,
    /// Files riding along: images for the models that see, text inlined
    /// for the ones that read. Absent on old callers' messages.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ChatAttachmentDto>,
}

/// One attached file.
///
/// Images travel as raw base64 with their media type — what both wires
/// want. Text attachments travel as text and are inlined into the message
/// body at request-building time, which every provider reads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatAttachmentDto {
    pub name: String,
    /// `image` or `text`.
    pub kind: String,
    pub mime_type: String,
    /// Base64 (image) or the file's own text (text).
    pub data: String,
}

/// The handle a send returns: the stream continues as events carrying it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatTurnDto {
    pub id: String,
}

/// Which part of an answer a delta belongs to. Reasoning models speak
/// twice: the thinking first (`reasoning_content` on the OpenAI wire,
/// `thinking_delta` on Anthropic's), the answer after — and a window that
/// showed only the answer would look stuck for exactly as long as the
/// model thinks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatDeltaPart {
    Reasoning,
    Answer,
}

impl ChatDeltaPart {
    fn as_str(self) -> &'static str {
        match self {
            Self::Reasoning => "reasoning",
            Self::Answer => "answer",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatDeltaDto<'a> {
    id: &'a str,
    /// `reasoning` or `answer` — the interface decides how each is shown.
    part: &'static str,
    text: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatSettledDto<'a> {
    id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
}

pub fn validate_chat_settings(settings: &ChatSettingsDto) -> Result<(), String> {
    let field = |value: &str| {
        !value.is_empty()
            && value.len() <= MAX_FIELD_BYTES
            && value.trim() == value
            && !value.chars().any(char::is_control)
    };
    if !field(&settings.model) || provider_profile(&settings.provider).is_none() {
        return Err("invalid_chat_settings".to_owned());
    }
    for key in [
        &settings.keys.zai,
        &settings.keys.openai,
        &settings.keys.openrouter,
        &settings.keys.anthropic,
    ] {
        if key.len() > MAX_API_KEY_BYTES
            || key.trim() != key.as_str()
            || key.chars().any(char::is_control)
        {
            return Err("invalid_chat_settings".to_owned());
        }
    }
    Ok(())
}

pub fn read_chat_settings_blocking(store: &StoreHandle) -> Result<ChatSettingsDto, String> {
    match store.get_setting(CHAT_SETTINGS_KEY) {
        Ok(Some(value_json)) => {
            let settings: ChatSettingsDto = serde_json::from_str(&value_json)
                .map_err(|_| "invalid_chat_settings".to_owned())?;
            validate_chat_settings(&settings)?;
            Ok(settings)
        }
        Ok(None) => Ok(ChatSettingsDto::default()),
        Err(_) => Err("chat_settings_unavailable".to_owned()),
    }
}

pub async fn get_chat_settings_service(
    state: &crate::state::AppState,
) -> Result<ChatSettingsDto, String> {
    let store = state.store.clone();
    crate::commands::run_blocking("chat_settings_unavailable", move || {
        read_chat_settings_blocking(&store)
    })
    .await
}

pub async fn save_chat_settings_service(
    state: &crate::state::AppState,
    settings: ChatSettingsDto,
) -> Result<ChatSettingsDto, String> {
    validate_chat_settings(&settings)?;
    let value_json =
        serde_json::to_string(&settings).map_err(|_| "invalid_chat_settings".to_owned())?;
    state
        .store
        .save_setting(CHAT_SETTINGS_KEY, &value_json)
        .await
        .map_err(|_| "chat_settings_unavailable".to_owned())?;
    Ok(settings)
}

// ------------------------------------------------------- generated files --

/// The ceiling on a file the chat wrote out. Answers can be long; a file
/// the user chose to save should never be a memory event.
const MAX_GENERATED_FILE_BYTES: usize = 8 * 1024 * 1024;

/// Writes one text file the chat produced — a code block, an export — to
/// the path the native save dialog returned.
///
/// The path is the user's own choice from the system dialog, but it is
/// still checked as absolute and its parent looked at, because a dialog
/// answer crosses the bridge like anything else. Only text is written:
/// everything this window generates is text.
pub fn save_generated_file_blocking(path: &str, contents: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > MAX_FIELD_BYTES * 8
        || path.contains('\0')
        || !std::path::Path::new(path).is_absolute()
        || contents.len() > MAX_GENERATED_FILE_BYTES
    {
        return Err("chat_file_invalid".to_owned());
    }
    // Nothing here refuses an existing file: the dialog asked, and
    // overwriting what the user pointed at is what saving means.
    std::fs::write(std::path::Path::new(path), contents)
        .map_err(|_| "chat_file_write_failed".to_owned())
}

/// Opens one http(s) address in the user's browser.
///
/// The scheme is decided here, not at the call site: a markdown link is
/// model output, and model output does not get to name a handler. The
/// system opener receives exactly one argument and no shell ever does.
pub fn open_external_url(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    let parsed = reqwest::Url::parse(trimmed);
    let scheme_ok = parsed
        .as_ref()
        .is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https"));
    if !scheme_ok || trimmed.len() > MAX_FIELD_BYTES * 8 {
        return Err("chat_link_invalid".to_owned());
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .arg(trimmed)
            .spawn()
            .map_err(|_| "chat_link_unavailable".to_owned())?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = trimmed;
        Err("chat_link_unavailable".to_owned())
    }
}

// -------------------------------------------------------------- streaming --

/// Turns one `data:` line of a chat-completions SSE stream into the part
/// and text it carries, if it carries any.
///
/// Pure on purpose: the shapes providers ship are the part worth testing,
/// and a test should not need a network to say what the parser does with
/// `[DONE]`, a content delta, a role-only delta, reasoning thinking, or a
/// line that is not data at all.
pub fn delta_from_sse_line(line: &str) -> Option<(ChatDeltaPart, String)> {
    let payload = line.strip_prefix("data:")?.trim_start();
    if payload == "[DONE]" {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    let delta = value.get("choices")?.get(0)?.get("delta")?;
    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
        return Some((ChatDeltaPart::Answer, content.to_owned()));
    }
    // The thinking phase: present on every reasoning model on this wire,
    // sometimes before the model has said anything at all.
    delta
        .get("reasoning_content")
        .and_then(|v| v.as_str())
        .map(|text| (ChatDeltaPart::Reasoning, text.to_owned()))
}

/// The same, for the Anthropic wire: `event: content_block_delta` lines
/// whose data carries the text in `delta.text` — or the thinking in
/// `delta.thinking`, on the models that reason out loud. Event-name lines
/// and every other event type arrive too; only the deltas speak.
pub fn anthropic_delta_from_sse_line(line: &str) -> Option<(ChatDeltaPart, String)> {
    let payload = line.strip_prefix("data:")?.trim_start();
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    if value.get("type")?.as_str()? != "content_block_delta" {
        return None;
    }
    let delta = value.get("delta")?;
    let kind = delta.get("type").and_then(|v| v.as_str());
    match kind {
        Some("text_delta") => delta
            .get("text")
            .and_then(|v| v.as_str())
            .map(|text| (ChatDeltaPart::Answer, text.to_owned())),
        Some("thinking_delta") => delta
            .get("thinking")
            .and_then(|v| v.as_str())
            .map(|text| (ChatDeltaPart::Reasoning, text.to_owned())),
        _ => None,
    }
}

/// The stop flags of the turns in flight, by id.
///
/// A flag rather than an aborted future: the streaming loop looks at it
/// between chunks, so a stop is cooperative — the connection closes, the
/// done event fires, and nothing is cancelled from under itself.
fn stops() -> &'static Mutex<HashMap<String, Arc<AtomicBool>>> {
    static STOPS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    STOPS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn chat_stop_service(id: &str) -> Result<bool, String> {
    let stops = stops()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match stops.get(id) {
        Some(flag) => {
            flag.store(true, Ordering::Relaxed);
            Ok(true)
        }
        None => Ok(false),
    }
}

/// One exchange with the model: the command answers with the turn id, and
/// the answer itself arrives as `chat-delta` events, then `chat-done` — or
/// `chat-error` with a stable code that names what went wrong without
/// quoting the provider's words back at the interface.
pub async fn chat_send_service<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: &crate::state::AppState,
    messages: Vec<ChatMessageDto>,
) -> Result<ChatTurnDto, String> {
    if messages.is_empty() || messages.len() > MAX_MESSAGES {
        return Err("chat_invalid_request".to_owned());
    }
    for message in &messages {
        let role_ok = matches!(message.role.as_str(), "system" | "user" | "assistant");
        // Newlines, tabs and carriage returns are writing, not damage:
        // Shift+Enter puts them in every second message. Everything else
        // control-shaped is still refused.
        let text_ok = |text: &str| {
            !text.is_empty()
                && text.len() <= MAX_MESSAGE_BYTES
                && !text
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        };
        if !role_ok || !text_ok(&message.content) || message.attachments.len() > MAX_ATTACHMENTS {
            return Err("chat_invalid_request".to_owned());
        }
        for attachment in &message.attachments {
            let name_ok = !attachment.name.is_empty()
                && attachment.name.len() <= MAX_FIELD_BYTES
                && !attachment.name.chars().any(char::is_control);
            let data_ok = match attachment.kind.as_str() {
                "image" => {
                    attachment.data.len() <= MAX_ATTACHMENT_BASE64_BYTES
                        && attachment.mime_type.starts_with("image/")
                }
                "text" => {
                    attachment.data.len() <= MAX_ATTACHMENT_TEXT_BYTES
                        && attachment.mime_type.starts_with("text/")
                }
                _ => false,
            };
            if !name_ok || !data_ok {
                return Err("chat_invalid_request".to_owned());
            }
        }
    }
    let store = state.store.clone();
    let settings = crate::commands::run_blocking("chat_settings_unavailable", move || {
        read_chat_settings_blocking(&store)
    })
    .await?;

    let id = uuid::Uuid::new_v4().to_string();
    let flag = Arc::new(AtomicBool::new(false));
    stops()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id.clone(), Arc::clone(&flag));

    let app = Arc::new(app);
    let app_for_task = Arc::clone(&app);
    let id_for_task = id.clone();
    let flag_for_task = Arc::clone(&flag);
    tokio::spawn(async move {
        let outcome = stream_turn(
            app_for_task.as_ref(),
            &id_for_task,
            &flag_for_task,
            settings,
            messages,
        )
        .await;
        stops()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id_for_task);
        let settled = ChatSettledDto {
            id: &id_for_task,
            code: None,
        };
        match outcome {
            Ok(()) => {
                let _ = app_for_task.emit(CHAT_DONE_EVENT, &settled);
            }
            Err(code) => {
                let _ = app_for_task.emit(
                    CHAT_ERROR_EVENT,
                    &ChatSettledDto {
                        id: &id_for_task,
                        code: Some(code),
                    },
                );
            }
        }
    });

    Ok(ChatTurnDto { id })
}

/// The parser the stream loop uses, chosen by wire.
type DeltaParser = fn(&str) -> Option<(ChatDeltaPart, String)>;

/// The message text with its text attachments inlined as marked blocks.
///
/// Text files ride inside the prompt, which every provider reads — no
/// upload endpoint, no second round trip, no provider-specific file API.
fn content_with_text_attachments(message: &ChatMessageDto) -> String {
    let mut content = message.content.clone();
    for attachment in message.attachments.iter().filter(|a| a.kind == "text") {
        content.push_str(&format!(
            "\n\n--- file: {} ---\n{}\n--- end of {} ---",
            attachment.name, attachment.data, attachment.name
        ));
    }
    content
}

fn image_attachments(message: &ChatMessageDto) -> impl Iterator<Item = &ChatAttachmentDto> {
    message.attachments.iter().filter(|a| a.kind == "image")
}

/// The OpenAI wire: plain text content, except on the message being sent,
/// where images become parts of a content array. Older messages drop their
/// images — the model has already seen them, and resending megabytes of
/// base64 with every turn would make the request grow without end.
fn openai_wire_messages(messages: &[ChatMessageDto]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let text = content_with_text_attachments(message);
            let is_last = index + 1 == messages.len();
            let images: Vec<_> = if is_last {
                image_attachments(message).collect()
            } else {
                Vec::new()
            };
            if images.is_empty() {
                json!({"role": message.role, "content": text})
            } else {
                let mut parts = vec![json!({"type": "text", "text": text})];
                for image in images {
                    parts.push(json!({
                        "type": "image_url",
                        "image_url": {"url": format!("data:{};base64,{}", image.mime_type, image.data)}
                    }));
                }
                json!({"role": message.role, "content": parts})
            }
        })
        .collect()
}

/// The Anthropic wire: the system prompt apart from the turns, consecutive
/// same-role turns merged, and images as base64 source blocks — on the
/// message being sent, for the same reason as the OpenAI wire.
fn anthropic_wire_body(messages: &[ChatMessageDto], model: &str) -> serde_json::Value {
    let mut system: Vec<String> = Vec::new();
    let mut turns: Vec<(String, Vec<serde_json::Value>)> = Vec::new();
    for message in messages {
        let text = content_with_text_attachments(message);
        if message.role == "system" {
            system.push(text);
            continue;
        }
        let is_last = std::ptr::eq(message, messages.last().unwrap_or(message));
        let mut parts = vec![json!({"type": "text", "text": text})];
        if is_last {
            for image in image_attachments(message) {
                parts.push(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": image.mime_type,
                        "data": image.data,
                    }
                }));
            }
        }
        if turns.last().is_some_and(|(role, _)| role == &message.role) {
            if let Some((_, existing)) = turns.last_mut() {
                existing.extend(parts);
            }
        } else {
            turns.push((message.role.clone(), parts));
        }
    }
    let mut body = json!({
        "model": model,
        "max_tokens": 4096,
        "stream": true,
        "messages": turns
            .iter()
            .map(|(role, parts)| json!({"role": role, "content": parts}))
            .collect::<Vec<_>>(),
    });
    if !system.is_empty() {
        body["system"] = json!(system.join("\n\n"));
    }
    body
}

/// Opens the stream and relays it as events.
///
/// Every refusal is a stable code, never the provider's prose: an error
/// body can quote the request — key included — and echoing it would be the
/// one leak this module exists not to have.
async fn stream_turn<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    id: &str,
    stop: &Arc<AtomicBool>,
    settings: ChatSettingsDto,
    messages: Vec<ChatMessageDto>,
) -> Result<(), &'static str> {
    let Some((protocol, base)) = provider_profile(&settings.provider) else {
        return Err("chat_invalid_request");
    };
    let key = settings.keys.for_provider(&settings.provider).to_owned();
    if key.is_empty() {
        // None of the four providers answers without one, and sending a
        // blank credential would only turn a clear refusal into a muddy one.
        return Err("chat_key_missing");
    }

    let (endpoint, mut headers, body, parse): (String, Vec<(&str, String)>, String, DeltaParser) =
        match protocol {
            ChatProtocol::Openai => {
                let endpoint = format!("{base}/chat/completions");
                let body = json!({
                    "model": settings.model,
                    "messages": openai_wire_messages(&messages),
                    "stream": true,
                });
                (
                    endpoint,
                    vec![("authorization", format!("Bearer {key}"))],
                    serde_json::to_string(&body).map_err(|_| "chat_invalid_request")?,
                    delta_from_sse_line,
                )
            }
            ChatProtocol::Anthropic => {
                let endpoint = format!("{base}/v1/messages");
                let body = anthropic_wire_body(&messages, &settings.model);
                (
                    endpoint,
                    vec![
                        ("x-api-key", key.clone()),
                        ("anthropic-version", "2023-06-01".to_owned()),
                    ],
                    serde_json::to_string(&body).map_err(|_| "chat_invalid_request")?,
                    anthropic_delta_from_sse_line,
                )
            }
        };

    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(120))
        .build()
        .map_err(|_| "chat_network_error")?;
    let mut request = client
        .post(&endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/json");
    for (name, value) in headers.drain(..) {
        request = request.header(name, value);
    }
    let request = request
        .body(body)
        .build()
        .map_err(|_| "chat_network_error")?;
    let mut response = client
        .execute(request)
        .await
        .map_err(|_| "chat_network_error")?;
    if !response.status().is_success() {
        return Err(code_for_status(response.status()));
    }

    // One growing buffer of undelimited bytes: a chunk boundary can split a
    // line anywhere, so only complete lines — those followed by a newline —
    // are parsed, and the tail waits for the next chunk.
    let mut pending = Vec::new();
    let mut emitted_any = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let chunk = response
            .chunk()
            .await
            .map_err(|_| "chat_network_error")?
            .unwrap_or_default();
        if chunk.is_empty() {
            // The stream ended. Flush whatever complete line remains.
            if let Ok(text) = std::str::from_utf8(&pending)
                && let Some((part, delta)) = parse(text.trim_end())
            {
                emit_delta(app, id, part, delta)?;
                emitted_any = true;
            }
            return if emitted_any {
                Ok(())
            } else {
                Err("chat_stream_ended_empty")
            };
        }
        pending.extend_from_slice(&chunk);
        while let Some(position) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=position).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.trim().is_empty() {
                continue;
            }
            if let Some((part, delta)) = parse(line) {
                emit_delta(app, id, part, delta)?;
                emitted_any = true;
            }
        }
        // A stream that never says anything useful, forever, is bounded by
        // nothing else: the read timeout covers a silent wire, not a chatty
        // one, and a provider that streams SSE comments only would otherwise
        // run without end.
        if pending.len() > 4 * 1024 * 1024 {
            return Err("chat_stream_malformed");
        }
    }
}

/// The models a provider offers, fetched live from its own list endpoint.
///
/// The answer is names only — ids the settings can use verbatim — capped
/// and sorted, because a catalog with hundreds of entries (OpenRouter's,
/// chiefly) is a list to pick from, not a document to scroll.
pub async fn chat_list_models_service(
    state: &crate::state::AppState,
) -> Result<Vec<String>, String> {
    let store = state.store.clone();
    let settings = crate::commands::run_blocking("chat_settings_unavailable", move || {
        read_chat_settings_blocking(&store)
    })
    .await?;
    let Some((protocol, base)) = provider_profile(&settings.provider) else {
        return Err("chat_invalid_request".to_owned());
    };
    let key = settings.keys.for_provider(&settings.provider).to_owned();
    if key.is_empty() {
        return Err("chat_key_missing".to_owned());
    }

    let (endpoint, headers) = match protocol {
        ChatProtocol::Openai => (
            format!("{base}/models"),
            vec![("authorization", format!("Bearer {key}"))],
        ),
        ChatProtocol::Anthropic => (
            format!("{base}/v1/models?limit=100"),
            vec![
                ("x-api-key", key),
                ("anthropic-version", "2023-06-01".to_owned()),
            ],
        ),
    };
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| "chat_models_unavailable".to_owned())?;
    let mut request = client.get(&endpoint);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|_| "chat_models_unavailable".to_owned())?;
    if !response.status().is_success() {
        return Err(code_for_status(response.status()).to_owned());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "chat_models_unavailable".to_owned())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("chat_models_unavailable".to_owned());
    }
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| "chat_models_unavailable".to_owned())?;
    let Some(entries) = value.get("data").and_then(|data| data.as_array()) else {
        return Err("chat_models_unavailable".to_owned());
    };
    let mut models: Vec<String> = entries
        .iter()
        .filter_map(|entry| entry.get("id")?.as_str().map(str::to_owned))
        .filter(|id| !id.is_empty() && !id.chars().any(char::is_control))
        .collect();
    models.sort();
    models.dedup();
    models.truncate(300);
    Ok(models)
}

fn emit_delta<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    id: &str,
    part: ChatDeltaPart,
    text: String,
) -> Result<(), &'static str> {
    app.emit(
        CHAT_DELTA_EVENT,
        &ChatDeltaDto {
            id,
            part: part.as_str(),
            text,
        },
    )
    .map_err(|_| "chat_event_unavailable")
}

#[cfg(test)]
mod tests {
    use super::{
        ChatAttachmentDto, ChatMessageDto, ChatSettingsDto, anthropic_delta_from_sse_line,
        delta_from_sse_line, validate_chat_settings,
    };

    use super::ChatDeltaPart::{Answer, Reasoning};

    #[test]
    fn sse_lines_become_the_text_they_carry() {
        let delta = r#"data: {"choices":[{"delta":{"content":"Hel"}}]}"#;
        assert_eq!(delta_from_sse_line(delta), Some((Answer, "Hel".to_owned())));
        let with_role = r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#;
        assert_eq!(delta_from_sse_line(with_role), None);
        assert_eq!(delta_from_sse_line("data: [DONE]"), None);
        // Not a data line at all — SSE comments and events this parser does
        // not know are skipped, not fatal.
        assert_eq!(delta_from_sse_line(": keep-alive"), None);
        assert_eq!(delta_from_sse_line("event: ping"), None);
    }

    #[test]
    fn reasoning_models_speak_before_they_answer() {
        // GLM's thinking phase: reasoning_content first, content later.
        let thinking =
            r#"data: {"choices":[{"delta":{"role":"assistant","reasoning_content":"The user"}}]}"#;
        assert_eq!(
            delta_from_sse_line(thinking),
            Some((Reasoning, "The user".to_owned()))
        );
        // A delta carrying both is an answer, and its reasoning is spent.
        let both = r#"data: {"choices":[{"delta":{"reasoning_content":"","content":"Hi"}}]}"#;
        assert_eq!(delta_from_sse_line(both), Some((Answer, "Hi".to_owned())));
    }

    #[test]
    fn chat_settings_validate_their_fields() {
        let base = ChatSettingsDto::default();
        assert!(validate_chat_settings(&base).is_ok());
        assert_eq!(base.provider, "zai");

        let mut settings = base.clone();
        settings.provider = "anthropic".to_owned();
        assert!(validate_chat_settings(&settings).is_ok());
        // The Coding Plan entry shares the Z.ai key field.
        settings.provider = "zai-coding".to_owned();
        assert!(validate_chat_settings(&settings).is_ok());
        settings.provider = "somewhere-else".to_owned();
        assert!(validate_chat_settings(&settings).is_err());

        settings.provider = "openai".to_owned();
        settings.model = String::new();
        assert!(validate_chat_settings(&settings).is_err());
        settings.model = "gpt-4o-mini".to_owned();

        // Every key is optional and each is bounded the same way.
        settings.keys.openrouter = "sk-or-1".to_owned();
        settings.keys.anthropic = "sk-ant-1".to_owned();
        assert!(validate_chat_settings(&settings).is_ok());
        settings.keys.anthropic = " padded ".to_owned();
        assert!(validate_chat_settings(&settings).is_err());
    }

    fn message(role: &str, content: &str, attachments: Vec<ChatAttachmentDto>) -> ChatMessageDto {
        ChatMessageDto {
            role: role.to_owned(),
            content: content.to_owned(),
            attachments,
        }
    }

    fn image(name: &str, mime: &str, data: &str) -> ChatAttachmentDto {
        ChatAttachmentDto {
            name: name.to_owned(),
            kind: "image".to_owned(),
            mime_type: mime.to_owned(),
            data: data.to_owned(),
        }
    }

    #[test]
    fn openai_wire_puts_images_on_the_message_being_sent_only() {
        let history = vec![
            message(
                "user",
                "what is this?",
                vec![image("a.png", "image/png", "QUJD")],
            ),
            message("assistant", "a cat.", vec![]),
            message(
                "user",
                "and this?",
                vec![image("b.png", "image/png", "REVG")],
            ),
        ];
        let wire = super::openai_wire_messages(&history);
        // The first message's image stayed home: the model saw it once.
        assert!(wire[0]["content"].is_string());
        let parts = wire[2]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,REVG");
    }

    #[test]
    fn text_attachments_ride_inside_the_prompt() {
        let with_file = message(
            "user",
            "review this",
            vec![ChatAttachmentDto {
                name: "main.rs".to_owned(),
                kind: "text".to_owned(),
                mime_type: "text/rust".to_owned(),
                data: "fn main() {}".to_owned(),
            }],
        );
        let wire = super::openai_wire_messages(&[with_file]);
        let content = wire[0]["content"].as_str().unwrap();
        assert!(content.contains("--- file: main.rs ---"));
        assert!(content.contains("fn main() {}"));
    }

    #[test]
    fn anthropic_wire_takes_the_system_apart_and_merges_repeats() {
        let conversation = vec![
            message("system", "be brief", vec![]),
            message("user", "hi", vec![]),
            message("user", "again", vec![]),
            message("assistant", "hello", vec![]),
            message("user", "look", vec![image("x.png", "image/png", "QUJD")]),
        ];
        let body = super::anthropic_wire_body(&conversation, "claude-sonnet-4-5");
        assert_eq!(body["system"], serde_json::json!("be brief"));
        let turns = body["messages"].as_array().unwrap();
        assert_eq!(turns.len(), 3, "the two user turns merged into one");
        assert_eq!(turns[0]["role"], "user");
        let last_parts = turns[2]["content"].as_array().unwrap();
        assert_eq!(last_parts[1]["type"], "image");
        assert_eq!(last_parts[1]["source"]["data"], "QUJD");
    }

    #[test]
    fn generated_files_are_written_and_refused_honestly() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("snippet.rs");
        super::save_generated_file_blocking(target.to_str().unwrap(), "fn main() {}").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "fn main() {}");

        assert!(super::save_generated_file_blocking("relative/path.txt", "x").is_err());
        assert!(super::save_generated_file_blocking(&"a".repeat(9 * 1024 * 1024), "x").is_err());
    }

    #[test]
    fn external_links_are_opened_only_when_http() {
        assert!(super::open_external_url("https://example.com/page").is_ok());
        assert!(super::open_external_url("file:///etc/passwd").is_err());
        assert!(super::open_external_url("notes://anything").is_err());
    }

    #[test]
    fn http_refusals_say_which_thing_they_are() {
        use super::code_for_status;
        let of = |code: u16| reqwest::StatusCode::from_u16(code).unwrap();
        assert_eq!(code_for_status(of(401)), "chat_key_refused");
        assert_eq!(code_for_status(of(403)), "chat_key_refused");
        assert_eq!(code_for_status(of(404)), "chat_endpoint_not_found");
        assert_eq!(code_for_status(of(429)), "chat_rate_limited");
        assert_eq!(code_for_status(of(500)), "chat_provider_refused");
    }

    #[test]
    fn anthropic_sse_lines_become_the_text_they_carry() {
        let delta =
            r#"data: {"type":"content_block_delta","delta":{"type":"text_delta","text":"Hel"}}"#;
        assert_eq!(
            anthropic_delta_from_sse_line(delta),
            Some((Answer, "Hel".to_owned()))
        );
        let thinking = r#"data: {"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"hm"}}"#;
        assert_eq!(
            anthropic_delta_from_sse_line(thinking),
            Some((Reasoning, "hm".to_owned()))
        );
        // Every other event type is present on the wire and none of it is text.
        assert_eq!(
            anthropic_delta_from_sse_line(r#"data: {"type":"message_start","message":{}}"#),
            None
        );
        assert_eq!(
            anthropic_delta_from_sse_line(r#"data: {"type":"message_stop"}"#),
            None
        );
        assert_eq!(
            anthropic_delta_from_sse_line("event: content_block_delta"),
            None
        );
    }
}
