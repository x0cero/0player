//! Ollama chat client with streaming. Sends the current screen as an image
//! and streams the model's reply token by token through a callback.

use serde::Serialize;
use std::io::{BufRead, BufReader};

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    options: Options,
}

#[derive(Serialize, Clone)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>, // base64 PNGs
}

#[derive(Serialize)]
struct Options {
    temperature: f32,
    num_predict: i32,
    repeat_penalty: f32,
}

pub struct Ollama {
    pub host: String,
    pub model: String,
}

impl Ollama {
    pub fn new(host: &str, model: &str) -> Self {
        Self {
            host: host.trim_end_matches('/').to_string(),
            model: model.to_string(),
        }
    }

    /// Stream a chat completion; `on_token` fires per token. Returns the full
    /// reply.
    pub fn chat(
        &self,
        messages: &[Message],
        on_token: &mut dyn FnMut(&str),
    ) -> Result<String, String> {
        let req = ChatRequest {
            model: &self.model,
            messages,
            stream: true,
            options: Options {
                temperature: 0.4,
                // Thinking models spend tokens reasoning before the ACTION
                // line; the early-cutoff below keeps turns short anyway.
                num_predict: 900,
                repeat_penalty: 1.15,
            },
        };
        let resp = ureq::post(&format!("{}/api/chat", self.host))
            .send_json(serde_json::to_value(&req).map_err(|e| e.to_string())?)
            .map_err(|e| format!("ollama request failed: {e}"))?;

        let reader = BufReader::new(resp.into_reader());
        let mut full = String::new();
        for line in reader.lines() {
            let line = line.map_err(|e| e.to_string())?;
            if line.is_empty() {
                continue;
            }
            let v: serde_json::Value =
                serde_json::from_str(&line).map_err(|e| format!("bad ollama chunk: {e}"))?;
            if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
                return Err(err.to_string());
            }
            // Thinking models stream their reasoning separately; show it too.
            if let Some(think) = v
                .get("message")
                .and_then(|m| m.get("thinking"))
                .and_then(|c| c.as_str())
            {
                if !think.is_empty() {
                    // Counts toward the reply too: if the model only writes
                    // its ACTION inside the thinking stream, we still see it.
                    full.push_str(think);
                    on_token(think);
                    if let Some(pos) = full.to_ascii_uppercase().find("ACTION:") {
                        if full[pos..].contains('\n') {
                            break;
                        }
                    }
                }
            }
            if let Some(tok) = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                if !tok.is_empty() {
                    full.push_str(tok);
                    on_token(tok);
                    // Cut generation once a complete ACTION line exists;
                    // anything after it is discarded anyway.
                    if let Some(pos) = full.to_ascii_uppercase().find("ACTION:") {
                        if full[pos..].contains('\n') {
                            break;
                        }
                    }
                }
            }
            if v.get("done").and_then(|d| d.as_bool()) == Some(true) {
                break;
            }
        }
        Ok(full)
    }
}
