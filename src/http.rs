use anyhow::{anyhow, Result};
use serde::Deserialize;
use std::sync::OnceLock;
use std::time::Duration;

const EMBED_MODEL: &str = "text-embedding-3-small";
pub const EMBED_DIM: usize = 1536;
const DEFAULT_RERANK_MODEL: &str = "gpt-4o-mini";

fn client() -> &'static reqwest::Client {
	static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
	CLIENT.get_or_init(|| {
		reqwest::Client::builder()
			.timeout(Duration::from_secs(60))
			.connect_timeout(Duration::from_secs(10))
			.pool_idle_timeout(Duration::from_secs(90))
			.build()
			.expect("build reqwest client")
	})
}

fn openai_key() -> Result<String> {
	std::env::var("OPENAI_API_KEY").map_err(|_| anyhow!("OPENAI_API_KEY not set"))
}

fn rerank_model() -> String {
	std::env::var("WIKI_RERANK_MODEL").unwrap_or_else(|_| DEFAULT_RERANK_MODEL.to_string())
}

#[derive(Deserialize)]
struct EmbedResponse {
	data: Vec<EmbedItem>,
}
#[derive(Deserialize)]
struct EmbedItem {
	embedding: Vec<f32>,
}

/// Embed a batch of texts. Deduplicates inputs to halve API cost on repeat
/// content (common when reindexing entity pools that share titles).
pub async fn embed_batch(texts: &[String]) -> Result<Vec<Vec<f32>>> {
	if texts.is_empty() {
		return Ok(vec![]);
	}

	// Dedupe: build the unique input vector + a backref so we can rehydrate.
	let mut unique: Vec<&String> = Vec::with_capacity(texts.len());
	let mut idx_of: std::collections::HashMap<&String, usize> = std::collections::HashMap::new();
	let mut backref: Vec<usize> = Vec::with_capacity(texts.len());
	for t in texts {
		let pos = match idx_of.get(t) {
			Some(&i) => i,
			None => {
				let i = unique.len();
				unique.push(t);
				idx_of.insert(t, i);
				i
			}
		};
		backref.push(pos);
	}

	let key = openai_key()?;
	let body = serde_json::json!({
		"model": EMBED_MODEL,
		"input": unique,
	});
	let resp = client()
		.post("https://api.openai.com/v1/embeddings")
		.bearer_auth(&key)
		.json(&body)
		.send()
		.await?
		.error_for_status()?
		.json::<EmbedResponse>()
		.await?;

	let unique_embs: Vec<Vec<f32>> = resp.data.into_iter().map(|d| d.embedding).collect();
	if unique_embs.len() != unique.len() {
		return Err(anyhow!(
			"embedding count mismatch: got {}, expected {}",
			unique_embs.len(),
			unique.len()
		));
	}
	Ok(backref.into_iter().map(|i| unique_embs[i].clone()).collect())
}

#[derive(Deserialize)]
struct ChatChoice {
	message: ChatMessage,
}
#[derive(Deserialize)]
struct ChatMessage {
	content: String,
}
#[derive(Deserialize)]
struct ChatResp {
	choices: Vec<ChatChoice>,
}

/// Issue a `response_format=json_object` chat completion. Returns the raw
/// `content` string (caller parses).
pub async fn chat_json(system: &str, user: &str) -> Result<String> {
	let key = openai_key()?;
	let model = rerank_model();
	let body = serde_json::json!({
		"model": model,
		"messages": [
			{"role": "system", "content": system},
			{"role": "user", "content": user},
		],
		"response_format": {"type": "json_object"},
		"temperature": 0,
	});
	let resp: ChatResp = client()
		.post("https://api.openai.com/v1/chat/completions")
		.bearer_auth(&key)
		.json(&body)
		.send()
		.await?
		.error_for_status()?
		.json()
		.await?;
	resp.choices
		.into_iter()
		.next()
		.map(|c| c.message.content)
		.ok_or_else(|| anyhow!("no choices in chat response"))
}
