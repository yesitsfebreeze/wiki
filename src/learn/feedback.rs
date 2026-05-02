//! Feedback-driven learning pass: walks `feedback.jsonl`, asks an LLM what to
//! do per entry, and applies the decision (create question, link picks, etc.).

use super::infra::{
	allowed_kind, build_entity_index, find_question_by_hash, fnv_question_id, read_cursor,
	write_cursor, write_pass_log, EntityRef, FeedbackEntry, LlmDecision,
};
use super::links::link_doc_internal;
use crate::{http, store};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

async fn decide_via_llm(entry: &FeedbackEntry, picks_ctx: &str) -> Result<LlmDecision> {
	let sys = "You curate a knowledge wiki from search feedback. Given a user question and the \
		documents picked as relevant (with reasons), decide: \
		(a) is the question worth saving as a 'questions' doc? \
		(b) for each picked doc, what reason kind connects question→doc and what is a 1-sentence \
		body summarizing WHY it answers/supports/etc the question? \
		Return JSON: {\"keep_question\":bool, \"question_title\":string|null, \
		\"question_body\":string|null, \"purpose\":string|null, \"resolved\":bool, \
		\"edges\":[{\"picked_id\":string, \"score\":0..1, \"kind\":\"Answers|Supports|Contradicts|Extends|Requires|References|Derives|Instances\", \"body\":string}]}. \
		Set resolved=true only if at least one edge is a strong direct Answers (score>=0.8). \
		Skip edges below score 0.3.";
	let user = format!(
		"Question: {}\nPurpose hint (tag_filter): {:?}\n\nPicks (id, search_reason, snippet):\n{}",
		entry.question, entry.tag_filter, picks_ctx
	);
	let content = http::chat_json(sys, &user).await?;
	let parsed: LlmDecision = serde_json::from_str(&content)
		.map_err(|e| anyhow::anyhow!("decision parse: {} body: {}", e, content))?;
	Ok(parsed)
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

	let mut picks_ctx = String::new();
	let reason_map: HashMap<&str, &str> = entry
		.reasons
		.iter()
		.map(|(a, b)| (a.as_str(), b.as_str()))
		.collect();
	let mut found_any = false;
	let mut linked_ids: Vec<String> = Vec::new();
	for pid in &entry.picked {
		let mut doc_lookup: Option<(String, store::Document)> = None;
		for dt in &["entities", "thoughts", "conclusions", "reasons", "questions"] {
			if let Ok(d) = store::get_document(root, dt, pid) {
				doc_lookup = Some(((*dt).to_string(), d));
				break;
			}
		}
		let Some((dt, doc)) = doc_lookup else { continue };
		found_any = true;
		let snippet = if doc.content.len() > 400 {
			let mut end = 400;
			while !doc.content.is_char_boundary(end) && end > 0 { end -= 1; }
			format!("{}…", &doc.content[..end])
		} else {
			doc.content.clone()
		};
		let r = reason_map.get(pid.as_str()).copied().unwrap_or("");
		picks_ctx.push_str(&format!(
			"- id={} type={} title={:?} reason={:?}\n  snippet={}\n",
			pid, dt, doc.title, r, snippet
		));
		if !dry_run {
			let _ = link_doc_internal(root, &dt, pid, entities, false).await;
			linked_ids.push(pid.clone());
		}
	}
	if !found_any {
		return Ok(serde_json::json!({"skipped": "no picks resolved"}));
	}

	let decision = match decide_via_llm(entry, &picks_ctx).await {
		Ok(d) => d,
		Err(e) => return Ok(serde_json::json!({"error": e.to_string()})),
	};

	let hash_id = fnv_question_id(&entry.question);
	let mut question_id: Option<String> = find_question_by_hash(root, &hash_id);
	let mut created_question = false;

	if decision.keep_question && question_id.is_none() && !dry_run {
		let purpose = decision.purpose.clone().unwrap_or_else(|| {
			entry.tag_filter.clone().unwrap_or_else(|| "general".to_string())
		});
		let body = decision.question_body.clone().unwrap_or_else(|| entry.question.clone());
		let mut tags = vec!["question".to_string(), purpose.clone(), hash_id.clone()];
		if decision.resolved {
			tags.push("resolved".to_string());
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
	} else if decision.resolved && !dry_run {
		if let Some(qid) = &question_id {
			if let Ok(mut q) = store::get_document(root, "questions", qid) {
				if !q.tags.iter().any(|t| t == "resolved") {
					q.tags.push("resolved".to_string());
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
				).is_ok() {
					edges_created += 1;
				}
			}
		}
	}

	Ok(serde_json::json!({
		"question": entry.question,
		"question_id": question_id,
		"created_question": created_question,
		"resolved": decision.resolved,
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
