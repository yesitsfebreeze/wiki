//! Feedback-driven learning pass: walks `feedback.jsonl`, derives a decision
//! deterministically from the feedback entry, and applies it (create question,
//! link picks, etc.).  No LLM call; resolution is left to the QA pass which
//! can measure cosine similarity against actual answer candidates.

use super::infra::{
	allowed_kind, build_entity_index, find_question_by_hash, fnv_question_id, read_cursor,
	write_cursor, write_pass_log, EntityRef, FeedbackEntry, LlmDecision, LlmEdge,
};
use super::links::link_doc_internal;
use crate::store;
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

/// Derive a decision from the feedback entry without an LLM call.
///
/// - `keep_question`: always true (caller already skips empty-picks entries)
/// - `purpose`: tag_filter from the original search, or "general"
/// - `resolved`: false — the QA pass resolves questions via cosine threshold
/// - `edges`: one "References" edge per picked doc; body is the search reason
fn decide(entry: &FeedbackEntry, reason_map: &HashMap<&str, &str>) -> LlmDecision {
	let purpose = entry
		.tag_filter
		.clone()
		.unwrap_or_else(|| "general".to_string());

	let edges = entry
		.picked
		.iter()
		.map(|pid| {
			let body = reason_map
				.get(pid.as_str())
				.copied()
				.filter(|r| !r.is_empty())
				.map(|r| r.to_string())
				.unwrap_or_else(|| format!("Picked as relevant to: {}", entry.question));
			LlmEdge {
				picked_id: pid.clone(),
				// 0.7: above the 0.3 edge floor, below the 0.8 "Answers" threshold
				// so we never auto-mark resolved here.
				score: 0.7,
				kind: "References".to_string(),
				body,
			}
		})
		.collect();

	LlmDecision {
		keep_question: true,
		question_title: Some(entry.question.clone()),
		question_body: Some(entry.question.clone()),
		purpose: Some(purpose),
		answered: false,
		edges,
	}
}

async fn process_entry(
	root: &Path,
	entry: &FeedbackEntry,
	entities: &[EntityRef],
	dry_run: bool,
) -> Result<serde_json::Value> {
	if entry.picked.is_empty() {
		return Ok(serde_json::json!({"skipped": "no picks"}));
	}

	let reason_map: HashMap<&str, &str> = entry
		.reasons
		.iter()
		.map(|(a, b)| (a.as_str(), b.as_str()))
		.collect();

	let mut found_any = false;
	let mut linked_ids: Vec<String> = Vec::new();
	for pid in &entry.picked {
		let mut resolved_dt: Option<String> = None;
		for dt in &["entities", "thoughts", "conclusions", "reasons", "questions"] {
			if store::get_document(root, dt, pid).is_ok() {
				resolved_dt = Some((*dt).to_string());
				break;
			}
		}
		let Some(dt) = resolved_dt else { continue };
		found_any = true;
		if !dry_run {
			let _ = link_doc_internal(root, &dt, pid, entities, false).await;
			linked_ids.push(pid.clone());
		}
	}
	if !found_any {
		return Ok(serde_json::json!({"skipped": "no picks resolved"}));
	}

	let decision = decide(entry, &reason_map);

	let hash_id = fnv_question_id(&entry.question);
	let mut question_id: Option<String> = find_question_by_hash(root, &hash_id);
	let mut created_question = false;

	if decision.keep_question && question_id.is_none() && !dry_run {
		let purpose = decision.purpose.clone().unwrap_or_else(|| {
			entry.tag_filter.clone().unwrap_or_else(|| "general".to_string())
		});
		let body = decision.question_body.clone().unwrap_or_else(|| entry.question.clone());
		let mut tags = vec!["question".to_string(), purpose.clone(), hash_id.clone()];
		if decision.answered {
			tags.push("answered".to_string());
		}
		if let Ok(qdoc) = store::create_document(
			root,
			"questions",
			decision.question_title.as_deref().unwrap_or(entry.question.as_str()),
			&body,
			tags,
			Some(&purpose),
			None,
		) {
			question_id = Some(qdoc.id);
			created_question = true;
		}
	} else if decision.answered && !dry_run {
		if let Some(qid) = &question_id {
			if let Ok(mut q) = store::get_document(root, "questions", qid) {
				if !q.tags.iter().any(|t| t == "answered") {
					q.tags.push("answered".to_string());
					let _ = store::update_document(root, "questions", qid, None, Some(q.tags.clone()));
				}
			}
		}
	}

	let mut edges_created = 0u64;
	if let Some(qid) = &question_id {
		if !dry_run {
			for edge in &decision.edges {
				if edge.score < 0.3 {
					continue;
				}
				let kind = allowed_kind(&edge.kind);
				if store::create_reason(
					root,
					qid,
					&edge.picked_id,
					kind,
					&edge.body,
					decision.purpose.as_deref(),
				)
				.is_ok()
				{
					edges_created += 1;
				}
			}
		}
	}

	Ok(serde_json::json!({
		"question": entry.question,
		"question_id": question_id,
		"created_question": created_question,
		"answered": decision.answered,
		"edges_created": edges_created,
		"docs_relinked": linked_ids.len(),
		"keep_question": decision.keep_question,
	}))
}

pub async fn run_feedback_pass(root: &Path, limit: usize, dry_run: bool) -> Result<serde_json::Value> {
	let path = root.join("feedback.jsonl");
	if !path.exists() {
		return Ok(serde_json::json!({"processed": 0, "note": "no feedback.jsonl"}));
	}
	let raw = std::fs::read(&path)?;
	let cursor = read_cursor(root) as usize;
	let cursor = cursor.min(raw.len());
	let slice = &raw[cursor..];
	let text = std::str::from_utf8(slice).map_err(|e| anyhow::anyhow!("utf8: {}", e))?;

	let entities = build_entity_index(root).await?;
	let mut details = Vec::new();
	let mut consumed_bytes = cursor as u64;
	let mut processed = 0usize;

	for line in text.split_inclusive('\n') {
		let stripped = line.trim_end_matches('\n').trim();
		consumed_bytes += line.len() as u64;
		if stripped.is_empty() {
			continue;
		}
		if processed >= limit {
			consumed_bytes -= line.len() as u64;
			break;
		}
		let entry: FeedbackEntry = match serde_json::from_str(stripped) {
			Ok(e) => e,
			Err(e) => {
				details.push(serde_json::json!({"parse_error": e.to_string(), "line": stripped}));
				processed += 1;
				continue;
			}
		};
		match process_entry(root, &entry, entities.as_slice(), dry_run).await {
			Ok(v) => details.push(v),
			Err(e) => details.push(serde_json::json!({"error": e.to_string()})),
		}
		processed += 1;
	}

	if !dry_run {
		write_cursor(root, consumed_bytes)?;
	}

	let report = serde_json::json!({
		"pass_id": chrono::Utc::now().to_rfc3339(),
		"processed": processed,
		"cursor": consumed_bytes,
		"total_bytes": raw.len(),
		"dry_run": dry_run,
		"details": details,
	});

	write_pass_log(root, "learn-feedback", &report)?;

	Ok(report)
}
