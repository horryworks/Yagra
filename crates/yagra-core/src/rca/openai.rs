// SPDX-License-Identifier: AGPL-3.0-only
//! An operator's own LLM server, spoken to in the OpenAI chat-completions shape (ADR-247).
//!
//! One adapter covers Ollama, llama.cpp's server (and llama-swap in front of it), vLLM and LM
//! Studio: all of them answer `POST {base_url}/chat/completions` with the same request and reply.
//! Nothing here is specific to any one of them.
//!
//! **This is the one adapter whose endpoint is not a constant.** The operator types the base URL
//! on the settings page, because there is no address Yagra could know in advance. The other three
//! adapters keep their hosts fixed for the reason [`super::provider`] gives, and this file carries
//! what stands in for that rule here:
//!
//! * the client **follows no redirect** — the URL the operator saved is the only place a prompt
//!   goes (`program_guards.rs::the_no_redirect_callers_still_say_so` holds this file to it);
//! * the URL is checked when it is saved ([`validate_base_url`]): `http` or `https`, no user name
//!   or password in it, no query or fragment;
//! * only an error body that parses as an error object is shown back (`error_detail`) — a page
//!   some other internal server answers with is never echoed, only its status;
//! * a stored key is not carried over to a changed URL (`store.rs`'s upsert);
//! * whether the address leaves the operator's boundary is judged from the address
//!   ([`url_leaves_boundary`]), and the page warns when it does.
//!
//! Two reply rules worth knowing:
//!
//! * **Thinking is not the answer.** llama.cpp and vLLM return a model's reasoning in a separate
//!   `reasoning_content` field, which is ignored; a server that does not split it leaves the
//!   reasoning inline in `<think>…</think>` at the start of `content`, which is cut off.
//! * **Tool-call arguments arrive as a JSON string**, not an object. A string that does not parse
//!   is a malformed reply, never an empty argument set the tool would then answer for the wrong
//!   thing.

use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use super::provider::{
    clip, http_client_with, status_error, transport_error, LlmError, LlmProvider, LlmRequest,
    LlmResponse, Part, Turn,
};

/// Names this provider in errors and in `yagra_llm_calls_total{provider}`.
const SERVICE: &str = "The LLM server";

/// Bound on one call. Three times the hosted providers' — an operator's own server may first have
/// to load the model, which measured 30 to 75 seconds on a lab machine before the first token.
/// The whole explanation is still bounded by the orchestrator's task budget.
pub const LOCAL_REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// Longest accepted base URL.
const MAX_URL_CHARS: usize = 500;

/// Placeholder model id for the Settings form. A server names its own models; this is a hint.
pub const SUGGESTED_MODEL: &str = "qwen3";

/// A client for one operator-run server.
pub struct OpenAiProvider {
    http: reqwest::Client,
    /// `{base_url}/chat/completions`, built once from the validated base URL.
    endpoint: String,
    model: String,
    /// Sent as a bearer token when present. llama-swap and vLLM ask for one; Ollama does not.
    /// Never logged, never returned by the API (ADR-018).
    api_key: Option<String>,
}

impl OpenAiProvider {
    /// Build a client for `model` on the server at `base_url`.
    ///
    /// # Errors
    /// Returns operator-facing text when the URL or model is unusable, or the HTTP client cannot be
    /// built.
    pub fn new(base_url: &str, model: &str, api_key: Option<&str>) -> Result<Self, String> {
        let base = validate_base_url(base_url)?;
        let model = model.trim();
        if model.is_empty() {
            return Err("a model id is required".to_owned());
        }
        Ok(Self {
            http: http_client_with(LOCAL_REQUEST_TIMEOUT, yagra_base::http::Redirects::None)?,
            endpoint: format!("{base}/chat/completions"),
            model: model.to_owned(),
            api_key: api_key
                .map(str::trim)
                .filter(|k| !k.is_empty())
                .map(str::to_owned),
        })
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "local"
    }

    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let body = build_body(&self.model, req);
        let mut call = self.http.post(&self.endpoint).json(&body);
        if let Some(key) = &self.api_key {
            call = call.bearer_auth(key);
        }
        let res = call
            .send()
            .await
            .map_err(|e| transport_error(SERVICE, &e))?;

        let status = res.status();
        if !status.is_success() {
            let detail = res.text().await.ok().and_then(|b| error_detail(&b));
            return Err(status_error(SERVICE, status, detail));
        }
        let payload: ChatResponse = res.json().await.map_err(|e| {
            LlmError::Malformed(format!("{SERVICE} sent an unreadable response: {e}"))
        })?;
        read_response(payload)
    }
}

// ── The address ─────────────────────────────────────────────────────────────────────────────

/// Check a base URL as the operator typed it, and return it normalized (trimmed, no trailing `/`).
///
/// Run when the configuration is saved and again when a client is built, so a value that reached
/// the database some other way cannot be used either.
///
/// # Errors
/// Operator-facing text naming the first problem.
pub fn validate_base_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("the server's URL is required".to_owned());
    }
    if trimmed.chars().count() > MAX_URL_CHARS {
        return Err("that URL is implausibly long".to_owned());
    }
    let url = reqwest::Url::parse(trimmed).map_err(|_| "that is not a URL".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("the URL must start with http:// or https://".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        // A credential in the URL would be stored in the clear and shown on the settings page.
        return Err("put the key in the API key field, not in the URL".to_owned());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("the URL may not carry a query or a fragment".to_owned());
    }
    if url.host().is_none() {
        return Err("the URL has no host".to_owned());
    }
    Ok(trimmed.trim_end_matches('/').to_owned())
}

/// Whether a prompt sent to `raw` leaves the operator's own network.
///
/// Inside only when the host is a **literal** address in a private (RFC 1918), loopback,
/// link-local or IPv6 unique-local range. A name answers "leaves" even when it would resolve
/// inside: what a name resolves to can change after it is saved, and the page's job is to make the
/// operator look, not to vouch for their DNS. An unparsable URL answers "leaves" too.
///
/// ⚠️ The settings page asks the same question of the address being typed, in
/// `web/src/pages/aiConfigForm.ts`. Both run the one case table in
/// `web/src/pages/fixtures/llmBaseUrl.cases.json`; a new case goes there, never in one test.
#[must_use]
pub fn url_leaves_boundary(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw.trim()) else {
        return true;
    };
    // The parser has already written any IPv4 spelling (`0x7f.1`, `2130706433`) as dotted
    // decimal, so a literal is recognised however it was typed.
    url.host_str()
        .and_then(yagra_common::host_ip)
        .is_none_or(|ip| !is_inside(ip))
}

fn is_inside(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => a.is_private() || a.is_loopback() || a.is_link_local(),
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_inside(IpAddr::V4(v4));
            }
            let first = a.segments()[0];
            a.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

// ── The request ─────────────────────────────────────────────────────────────────────────────

/// Assemble the request body. Split out from the HTTP call so the shape is unit-testable.
fn build_body(model: &str, req: &LlmRequest) -> serde_json::Value {
    let mut messages = vec![json!({ "role": "system", "content": req.system })];
    messages.extend(req.messages.iter().map(message_of));
    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": req.max_output_tokens,
        // Every server named above defaults to a single reply, but not every proxy in front of
        // one does — and a streamed reply would read as malformed here.
        "stream": false,
    });
    // Omitted entirely when there are none, as every adapter does: a server whose chat template
    // cannot take tools must still answer the single-shot request (ADR-028 WS-G).
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.schema,
                    },
                })
            })
            .collect::<Vec<_>>()
            .into();
    }
    body
}

/// One turn in the chat-completions shape. A tool result is its own role, `tool`, answering the
/// call by `tool_call_id`; a call's arguments go back as the JSON **string** they arrived as.
fn message_of(turn: &Turn) -> serde_json::Value {
    match turn {
        Turn::User(text) => json!({ "role": "user", "content": text }),
        Turn::Assistant(parts) => {
            let text: String = parts
                .iter()
                .filter_map(|p| match p {
                    Part::Text(t) => Some(t.as_str()),
                    Part::ToolCall { .. } => None,
                })
                .collect();
            let calls: Vec<serde_json::Value> = parts
                .iter()
                .filter_map(|p| match p {
                    Part::ToolCall { id, name, args } => Some(json!({
                        "id": id,
                        "type": "function",
                        "function": { "name": name, "arguments": args.to_string() },
                    })),
                    Part::Text(_) => None,
                })
                .collect();
            let mut m = json!({ "role": "assistant", "content": text });
            if !calls.is_empty() {
                m["tool_calls"] = calls.into();
            }
            m
        }
        Turn::ToolResult { id, content } => {
            json!({ "role": "tool", "tool_call_id": id, "content": content })
        }
    }
}

// ── The reply ───────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    #[serde(default)]
    message: Option<Message>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Option<String>,
    // `reasoning_content` is deliberately not read: it is the model thinking, not the answer.
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
}

#[derive(Deserialize)]
struct ToolCall {
    #[serde(default)]
    id: Option<String>,
    function: FunctionCall,
}

#[derive(Deserialize)]
struct FunctionCall {
    name: String,
    /// A JSON string by the specification; some servers send the object itself.
    #[serde(default)]
    arguments: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
}

/// Turn a successful reply into parts, or into the typed reason there are none.
fn read_response(payload: ChatResponse) -> Result<LlmResponse, LlmError> {
    let Some(choice) = payload.choices.into_iter().next() else {
        return Err(LlmError::Malformed(format!(
            "{SERVICE} returned no choices"
        )));
    };
    if choice.finish_reason.as_deref() == Some("content_filter") {
        return Err(LlmError::Refused(None));
    }
    let message = choice.message.unwrap_or(Message {
        content: None,
        tool_calls: Vec::new(),
    });

    let mut parts = Vec::new();
    let text = strip_thinking(message.content.as_deref().unwrap_or(""));
    if !text.is_empty() {
        parts.push(Part::Text(text.to_owned()));
    }
    for (i, call) in message.tool_calls.into_iter().enumerate() {
        let args = match call.function.arguments {
            None => json!({}),
            Some(serde_json::Value::String(s)) if s.trim().is_empty() => json!({}),
            Some(serde_json::Value::String(s)) => serde_json::from_str(&s).map_err(|_| {
                LlmError::Malformed(format!(
                    "{SERVICE} asked for {} with arguments that are not JSON",
                    call.function.name
                ))
            })?,
            Some(other) => other,
        };
        parts.push(Part::ToolCall {
            // Some servers leave the id out; the loop only needs it to pair a call with its answer.
            id: call
                .id
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| format!("call_{i}")),
            name: call.function.name,
            args,
        });
    }

    if parts.is_empty() {
        return Err(LlmError::Malformed(format!(
            "{SERVICE} returned no text{}",
            match choice.finish_reason.as_deref() {
                // Thinking is drawn from the same budget, so this is what an under-sized ceiling
                // produces on a model that thinks.
                Some("length") => " (the output budget ran out — raise it and retry)",
                _ => "",
            }
        )));
    }
    let (in_tokens, out_tokens) = payload
        .usage
        .map_or((None, None), |u| (u.prompt_tokens, u.completion_tokens));
    Ok(LlmResponse {
        parts,
        in_tokens,
        out_tokens,
    })
}

/// The answer without the model's inline reasoning.
///
/// Everything up to the last `</think>` is reasoning. An opening `<think>` with no close means the
/// budget ran out mid-thought, so there is no answer at all.
fn strip_thinking(content: &str) -> &str {
    if let Some(end) = content.rfind("</think>") {
        return content[end + "</think>".len()..].trim();
    }
    let trimmed = content.trim();
    if trimmed.starts_with("<think>") {
        return "";
    }
    trimmed
}

/// The message out of an error body, when the body is an error object.
///
/// OpenAI, llama.cpp and vLLM send `{"error": {"message": …}}`; Ollama sends `{"error": "…"}`.
/// Anything else — an HTML page, another service's JSON — is dropped, so a URL pointed at some
/// other internal server reports its status and nothing it said.
fn error_detail(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    match v.get("error")? {
        serde_json::Value::String(s) => clip(s),
        serde_json::Value::Object(err) => {
            let message = err.get("message").and_then(serde_json::Value::as_str);
            let kind = err.get("type").and_then(serde_json::Value::as_str);
            match (kind, message) {
                (Some(k), Some(m)) => clip(&format!("{k}: {m}")),
                (None, Some(m)) => clip(m),
                (Some(k), None) => clip(k),
                (None, None) => None,
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rca::provider::ToolDef;
    use serde_json::Value;

    fn req() -> LlmRequest {
        LlmRequest::single(
            "you are an NMS assistant".to_owned(),
            "explain this incident".to_owned(),
            4096,
        )
    }

    fn parse(body: Value) -> Result<LlmResponse, LlmError> {
        read_response(serde_json::from_value(body).unwrap())
    }

    #[test]
    fn a_single_shot_request_carries_the_system_turn_first_and_no_tools() {
        let body = build_body("qwen3", &req());
        assert_eq!(body["model"], json!("qwen3"));
        assert_eq!(body["max_tokens"], json!(4096));
        assert_eq!(body["stream"], json!(false));
        assert_eq!(body["messages"][0]["role"], json!("system"));
        assert_eq!(
            body["messages"][0]["content"],
            json!("you are an NMS assistant")
        );
        assert_eq!(body["messages"][1]["role"], json!("user"));
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert!(
            body.get("tools").is_none(),
            "tools must be omitted, not empty"
        );
    }

    #[test]
    fn tools_calls_and_results_take_the_chat_completions_spelling() {
        let mut r = req();
        r.tools = vec![ToolDef {
            name: "query_metrics".to_owned(),
            description: "read a metric".to_owned(),
            schema: json!({"type": "object", "properties": {"node_id": {"type": "string"}}}),
        }];
        r.messages.push(Turn::Assistant(vec![Part::ToolCall {
            id: "call_1".to_owned(),
            name: "query_metrics".to_owned(),
            args: json!({"node_id": "n1"}),
        }]));
        r.messages.push(Turn::ToolResult {
            id: "call_1".to_owned(),
            content: "{\"points\":[]}".to_owned(),
        });
        let body = build_body("qwen3", &r);
        assert_eq!(body["tools"][0]["type"], json!("function"));
        assert_eq!(body["tools"][0]["function"]["name"], json!("query_metrics"));
        assert_eq!(
            body["tools"][0]["function"]["parameters"]["type"],
            json!("object")
        );

        let call = &body["messages"][2];
        assert_eq!(call["role"], json!("assistant"));
        assert_eq!(call["tool_calls"][0]["id"], json!("call_1"));
        // Arguments go back as a string, which is what the specification asks for.
        let args = call["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(args).unwrap(),
            json!({"node_id": "n1"})
        );

        let result = &body["messages"][3];
        assert_eq!(result["role"], json!("tool"));
        assert_eq!(result["tool_call_id"], json!("call_1"));
    }

    #[test]
    fn a_tool_call_is_read_with_its_arguments_parsed() {
        let out = parse(json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [{
                        "id": "call_9",
                        "type": "function",
                        "function": { "name": "get_neighbors", "arguments": "{\"node_id\":\"n1\"}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }))
        .expect("a tool-call turn is a valid answer");
        let calls = out.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!((calls[0].0, calls[0].1), ("call_9", "get_neighbors"));
        assert_eq!(calls[0].2, &json!({"node_id": "n1"}));
    }

    #[test]
    fn a_call_without_an_id_or_with_object_arguments_is_still_read() {
        let out = parse(json!({
            "choices": [{
                "message": {
                    "tool_calls": [
                        { "function": { "name": "list_nodes", "arguments": { "limit": 5 } } },
                        { "function": { "name": "get_fleet_summary", "arguments": "" } }
                    ]
                },
                "finish_reason": "tool_calls"
            }]
        }))
        .unwrap();
        let calls = out.tool_calls();
        assert_eq!(calls[0].0, "call_0");
        assert_eq!(calls[0].2, &json!({"limit": 5}));
        assert_eq!(calls[1].0, "call_1");
        assert_eq!(calls[1].2, &json!({}));
    }

    #[test]
    fn arguments_that_are_not_json_are_malformed_rather_than_empty() {
        let err = parse(json!({
            "choices": [{
                "message": { "tool_calls": [
                    { "id": "c", "function": { "name": "list_nodes", "arguments": "{limit: 5" } }
                ] },
                "finish_reason": "tool_calls"
            }]
        }))
        .unwrap_err();
        assert!(matches!(err, LlmError::Malformed(_)), "{err:?}");
        assert!(err.to_string().contains("list_nodes"), "{err}");
    }

    #[test]
    fn reasoning_is_never_part_of_the_answer() {
        // The separate field (llama.cpp, vLLM) is not read at all.
        let out = parse(json!({
            "choices": [{
                "message": { "content": "root cause: uplink", "reasoning_content": "hmm, let me see" },
                "finish_reason": "stop"
            }]
        }))
        .unwrap();
        assert_eq!(out.text(), "root cause: uplink");

        // Inline (Qwen through a server that does not split it).
        let out = parse(json!({
            "choices": [{
                "message": { "content": "<think>\nfirst the alert...\n</think>\n\nroot cause: uplink" },
                "finish_reason": "stop"
            }]
        }))
        .unwrap();
        assert_eq!(out.text(), "root cause: uplink");
    }

    #[test]
    fn a_reply_cut_off_mid_thought_says_the_budget_ran_out() {
        let err = parse(json!({
            "choices": [{
                "message": { "content": "<think>\nfirst the alert, then the" },
                "finish_reason": "length"
            }]
        }))
        .unwrap_err();
        assert!(matches!(err, LlmError::Malformed(_)));
        assert!(err.to_string().contains("output budget"), "{err}");
    }

    #[test]
    fn a_filtered_reply_is_a_refusal_and_no_choices_is_malformed() {
        assert!(matches!(
            parse(
                json!({ "choices": [{ "message": { "content": "" }, "finish_reason": "content_filter" }] })
            ),
            Err(LlmError::Refused(None))
        ));
        assert!(matches!(
            parse(json!({ "choices": [] })),
            Err(LlmError::Malformed(_))
        ));
    }

    #[test]
    fn usage_is_read_when_present() {
        let out = parse(json!({
            "choices": [{ "message": { "content": "ok" }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 1200, "completion_tokens": 300 }
        }))
        .unwrap();
        assert_eq!((out.in_tokens, out.out_tokens), (Some(1200), Some(300)));
    }

    #[test]
    fn only_an_error_object_is_shown_back() {
        assert_eq!(
            error_detail(&json!({ "error": { "type": "invalid_request_error", "message": "model not found" } }).to_string()).unwrap(),
            "invalid_request_error: model not found"
        );
        assert_eq!(
            error_detail(&json!({ "error": "model \"x\" not found" }).to_string()).unwrap(),
            "model \"x\" not found"
        );
        // Another service's page or JSON says nothing back.
        assert!(error_detail("<html><body>internal wiki</body></html>").is_none());
        assert!(error_detail(&json!({ "users": ["a", "b"] }).to_string()).is_none());
    }

    #[test]
    fn a_base_url_is_checked_and_normalized() {
        assert_eq!(
            validate_base_url("  http://10.0.0.1:8080/v1/  ").unwrap(),
            "http://10.0.0.1:8080/v1"
        );
        assert_eq!(
            validate_base_url("https://llm.example.com/v1").unwrap(),
            "https://llm.example.com/v1"
        );
        for bad in [
            "",
            "   ",
            "llm.example.com/v1",
            "ftp://llm.example.com/v1",
            "file:///etc/passwd",
            "http://user:secret@llm.example.com/v1",
            "http://user@llm.example.com/v1",
            "http://llm.example.com/v1?key=x",
            "http://llm.example.com/v1#x",
        ] {
            assert!(validate_base_url(bad).is_err(), "{bad} must be refused");
        }
        assert!(validate_base_url(&format!("http://h/{}", "a".repeat(600))).is_err());
    }

    /// One case table, run by this test and by `aiConfigForm.test.ts` (ADR-247).
    #[test]
    fn the_boundary_rule_answers_the_case_table_the_webui_form_shares() {
        #[derive(Deserialize)]
        struct Case {
            url: String,
            leaves: bool,
        }
        let cases: Vec<Case> = serde_json::from_str(include_str!(
            "../../../../web/src/pages/fixtures/llmBaseUrl.cases.json"
        ))
        .unwrap();
        assert!(cases.len() >= 20, "only {} cases", cases.len());
        assert!(cases.iter().any(|c| c.leaves) && cases.iter().any(|c| !c.leaves));
        for c in &cases {
            assert_eq!(url_leaves_boundary(&c.url), c.leaves, "{}", c.url);
        }
    }

    #[test]
    fn a_client_needs_an_address_and_a_model_but_not_a_key() {
        assert!(OpenAiProvider::new("", "qwen3", None).is_err());
        assert!(OpenAiProvider::new("http://10.0.0.1/v1", "  ", None).is_err());
        let p = OpenAiProvider::new(" http://10.0.0.1:8080/v1/ ", " qwen3 ", Some("  ")).unwrap();
        assert_eq!(p.endpoint, "http://10.0.0.1:8080/v1/chat/completions");
        assert_eq!(p.model, "qwen3");
        assert!(p.api_key.is_none(), "a blank key is no key");
        assert_eq!(p.name(), "local");
    }

    fn ok_reply() -> String {
        json!({
            "choices": [{ "message": { "content": "hello" }, "finish_reason": "stop" }]
        })
        .to_string()
    }

    #[tokio::test]
    async fn a_completion_posts_to_the_typed_address_with_the_key_as_a_bearer() {
        let (addr, seen) = yagra_base::httpfake::serve(vec![(200, ok_reply())]).await;
        let out = OpenAiProvider::new(&format!("http://{addr}/v1"), "qwen3", Some("sk-local"))
            .unwrap()
            .complete(&req())
            .await
            .unwrap();
        assert_eq!(out.text(), "hello");
        let reqs = seen.lock().unwrap();
        let head = &reqs[0].head;
        assert!(head.starts_with("POST /v1/chat/completions "), "{head}");
        assert!(
            head.to_ascii_lowercase()
                .contains("authorization: bearer sk-local"),
            "{head}"
        );
    }

    #[tokio::test]
    async fn without_a_key_no_authorization_is_sent() {
        let (addr, seen) = yagra_base::httpfake::serve(vec![(200, ok_reply())]).await;
        OpenAiProvider::new(&format!("http://{addr}/v1"), "qwen3", None)
            .unwrap()
            .complete(&req())
            .await
            .unwrap();
        let head = &seen.lock().unwrap()[0].head;
        assert!(
            !head.to_ascii_lowercase().contains("authorization"),
            "{head}"
        );
    }

    #[tokio::test]
    async fn a_401_is_an_auth_error_and_a_429_is_rate_limiting() {
        let (addr, _) = yagra_base::httpfake::serve(vec![
            (
                401,
                json!({ "error": { "message": "Invalid API key" } }).to_string(),
            ),
            (429, "{}".to_owned()),
        ])
        .await;
        let p = OpenAiProvider::new(&format!("http://{addr}/v1"), "qwen3", Some("bad")).unwrap();
        let err = p.complete(&req()).await.unwrap_err();
        assert!(matches!(err, LlmError::Auth(_)), "{err:?}");
        assert!(err.to_string().contains("Invalid API key"), "{err}");
        assert!(matches!(
            p.complete(&req()).await.unwrap_err(),
            LlmError::RateLimited
        ));
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed() {
        let (addr, seen) = yagra_base::httpfake::serve(vec![(302, String::new())]).await;
        let err = OpenAiProvider::new(&format!("http://{addr}/v1"), "qwen3", None)
            .unwrap()
            .complete(&req())
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Upstream(_)), "{err:?}");
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "exactly one request was made"
        );
    }
}
