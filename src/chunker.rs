use crate::classifier;
use anyhow::Result;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Chunk {
	pub purpose: String,
	pub content: String,
}

pub fn split_paragraphs(content: &str) -> Vec<String> {
	content
		.split("\n\n")
		.map(|s| s.trim().to_string())
		.filter(|s| !s.is_empty())
		.collect()
}

/// Split content into per-purpose chunks. Each chunk = consecutive paragraphs
/// classified to the same purpose.
pub async fn chunk_by_purpose(root: &Path, content: &str) -> Result<Vec<Chunk>> {
	let paragraphs = split_paragraphs(content);
	if paragraphs.is_empty() {
		return Ok(vec![]);
	}
	let purposes = classifier::classify(root, &paragraphs).await?;

	let mut out: Vec<Chunk> = Vec::new();
	for (para, purpose) in paragraphs.into_iter().zip(purposes.into_iter()) {
		match out.last_mut() {
			Some(last) if last.purpose == purpose => {
				last.content.push_str("\n\n");
				last.content.push_str(&para);
			}
			_ => out.push(Chunk { purpose, content: para }),
		}
	}
	Ok(out)
}
