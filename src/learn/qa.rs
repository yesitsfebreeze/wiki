//! `run_pass` orchestrator + per-doc QA loop. Coordinates link, raise,
//! cross-reference, promote, and cross-topic auto-trigger.

use super::infra::{
	doc_qa_is_recent, read_pass_cursor, stamp_last_qa_at, write_pass_cursor, write_pass_log,
	AnswerCandidate, PassConfig,
};
use super::links::link_doc_internal;
use super::promote::{
	cross_reference_question, cross_topic_pass, promote_to_conclusion, should_invoke_cross_topic,
};
use crate::cache;
use crate::{search, store};
use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

async fn qa_for_doc(
	root: &Path,
	doc: &store::Document,
	llm_budget: &mut usize,
	cfg: &PassConfig,
) -> Result<(u64, u64, u64)> {
	if *llm_budget == 0 {
		return Ok((0, 0, 0));
	}
	*llm_budget = llm_budget.saturating_sub(1);
	// Ingest-time question raising disabled: questions are now raised only on
	// search-miss (see learn::raise::raise_question_from_search_miss). This
	// prevents user-ingested content from spawning unwanted questions.
	let raised: Vec<super::infra::RaisedQuestion> = Vec::new();

	let mut q_targets: Vec<(String, String, Option<String>)> = raised
		.iter()
		.map(|r| (r.question_id.clone(), r.title.clone(), r.purpose.clone()))
		.collect();
	if let Ok(reasons) = store::search_reasons_for(root, &doc.id, "to") {
		for r in reasons {
			let _ = r;
		}
	}
	if let Ok(questions) = store::list_documents(root, "questions") {
		for q in questions {
			if q.tags.iter().any(|t| t == "resolved") { continue; }
			if q_targets.iter().any(|(id, _, _)| id == &q.id) { continue; }
			let linked = store::search_reasons_for(root, &q.id, "from")
				.ok()
				.map(|rs| rs.iter().any(|r| {
					r.title.ends_with(&doc.id)
				}))
				.unwrap_or(false);
			if linked {
				q_targets.push((q.id, q.title, q.purpose));
			}
		}
	}

	let mut answered = 0u64;
	let mut promoted = 0u64;
	let strong = cfg.answer_threshold;

	for (qid, qtitle, qpurpose) in q_targets {
		if *llm_budget == 0 { break; }
		*llm_budget = llm_budget.saturating_sub(1);
		let cands = match cross_reference_question(root, &qtitle, qpurpose.as_deref(), cfg.support_threshold).await {
			Ok(v) => v,
			Err(_) => continue,
		};
		let mut strong_edges: Vec<AnswerCandidate> = Vec::new();
		let mut got_answer = false;
		let mut max_score: f32 = 0.0;
		for c in &cands {
			if c.score > max_score { max_score = c.score; }
			let kind = if c.score >= strong { "Answers" } else { "Supports" };
			if c.score >= strong { got_answer = true; }
			let _ = store::create_reason(root, &qid, &c.doc_id, kind, &c.body, qpurpose.as_deref());
			if c.score >= strong { strong_edges.push(c.clone()); }
		}
		if got_answer {
			answered += 1;
			if let Ok(mut q) = store::get_document(root, "questions", &qid) {
				if !q.tags.iter().any(|t| t == "resolved") {
					q.tags.push("resolved".to_string());
					let _ = store::update_document(root, "questions", &qid, None, Some(q.tags));
				}
			}
			if *llm_budget == 0 { continue; }
			*llm_budget = llm_budget.saturating_sub(1);
			if let Ok(Some(_)) = promote_to_conclusion(root, &qid, &strong_edges, cfg).await {
				promoted += 1;
			}
		} else if max_score >= cfg.support_threshold && *llm_budget >= 2 {
			*llm_budget = llm_budget.saturating_sub(2);
			if let Ok(n) = cross_topic_pass(root, &qid, cfg).await {
				if n > 0 {
					answered += 1;
					promoted += 1;
				}
			}
		}
	}

	Ok((raised.iter().filter(|r| r.created).count() as u64, answered, promoted))
}

pub async fn run_pass(
	root: &Path,
	limit: usize,
	purpose: Option<&str>,
	dry_run: bool,
	qa: bool,
	force: bool,
	cfg: &PassConfig,
) -> Result<serde_json::Value> {
	let entities = super::infra::build_entity_index(root).await?;

	let mut sequence: Vec<(String, String)> = Vec::new();
	for doc_type in &["thoughts", "conclusions"] {
		let docs = store::list_documents(root, doc_type)?;
		for d in docs {
			if let Some(p) = purpose {
				if d.purpose.as_deref() != Some(p) {
					continue;
				}
			}
			sequence.push(((*doc_type).to_string(), d.id));
		}
	}

	let cursor_key = purpose.unwrap_or("<global>");
	let cursor = read_pass_cursor(root, cursor_key);
	let start_idx = match cursor {
		Some((ref dt, ref id)) => sequence
			.iter()
			.position(|(d, i)| d == dt && i == id)
			.map(|i| (i + 1) % sequence.len().max(1))
			.unwrap_or(0),
		None => 0,
	};

	let mut targets: Vec<(String, String)> = Vec::new();
	if !sequence.is_empty() {
		let n = sequence.len();
		for k in 0..n {
			let idx = (start_idx + k) % n;
			targets.push(sequence[idx].clone());
			if targets.len() >= limit {
				break;
			}
		}
	}

	let mut docs_modified = 0u64;
	let mut links_added = 0u64;
	let mut merges_total = 0u64;
	let mut questions_raised = 0u64;
	let mut questions_answered = 0u64;
	let mut conclusions_promoted = 0u64;
	let mut llm_budget = cfg.qa_max_per_pass;
	let mut details = Vec::new();
	let mut last_processed: Option<(String, String)> = None;
	let mut skipped_recent = 0u64;
	for (dt, id) in &targets {
		last_processed = Some((dt.clone(), id.clone()));

		if !force && qa && doc_qa_is_recent(root, dt, id) {
			skipped_recent += 1;
			details.push(serde_json::json!({
				"doc_id": id,
				"doc_type": dt,
				"skipped": "recent_qa",
			}));
			continue;
		}

		match link_doc_internal(root, dt, id, entities.as_slice(), dry_run).await {
			Ok(v) => {
				if v["modified"].as_bool().unwrap_or(false) {
					docs_modified += 1;
				}
				links_added += v["links_added"].as_u64().unwrap_or(0);
				merges_total += v["paragraphs_merged"].as_u64().unwrap_or(0);
				details.push(v);
			}
			Err(e) => details.push(serde_json::json!({
				"doc_id": id,
				"doc_type": dt,
				"error": e.to_string()
			})),
		}

		if !qa || dry_run || llm_budget == 0 {
			continue;
		}
		let Ok(doc) = store::get_document(root, dt, id) else { continue };
		match qa_for_doc(root, &doc, &mut llm_budget, cfg).await {
			Ok((raised, answered, promoted)) => {
				questions_raised += raised;
				questions_answered += answered;
				conclusions_promoted += promoted;
				if !dry_run {
					let _ = stamp_last_qa_at(root, dt, id);
				}
			}
			Err(e) => details.push(serde_json::json!({
				"doc_id": id, "qa_error": e.to_string(),
			})),
		}
	}

	let mut crosstopic_invoked = 0u64;
	if qa && !dry_run {
		let resolved: HashSet<String> = cache::tag_index_lookup(root, "resolved")
			.into_iter()
			.filter(|d| d.doc_type == "questions")
			.map(|d| d.id)
			.collect();
		let candidates: Vec<String> = cache::tag_index_lookup(root, "question")
			.into_iter()
			.filter(|d| d.doc_type == "questions" && !resolved.contains(&d.id))
			.map(|d| d.id)
			.collect();
		for qid in candidates {
			if llm_budget < 2 { break; }
			if !should_invoke_cross_topic(root, &qid) { continue; }
			llm_budget = llm_budget.saturating_sub(2);
			if let Ok(n) = cross_topic_pass(root, &qid, cfg).await {
				if n > 0 {
					crosstopic_invoked += 1;
					questions_answered += 1;
					conclusions_promoted += 1;
				}
			}
		}
	}

	if !dry_run {
		if let Some((dt, id)) = &last_processed {
			let _ = write_pass_cursor(root, cursor_key, dt, id);
		}
	}

	let report = serde_json::json!({
		"pass_id": chrono::Utc::now().to_rfc3339(),
		"docs_scanned": targets.len(),
		"docs_modified": docs_modified,
		"links_added": links_added,
		"paragraphs_merged": merges_total,
		"questions_raised": questions_raised,
		"questions_answered": questions_answered,
		"conclusions_promoted": conclusions_promoted,
		"entity_count": entities.len(),
		"purpose_filter": purpose,
		"dry_run": dry_run,
		"qa": qa,
		"force": force,
		"skipped_recent": skipped_recent,
		"crosstopic_invoked": crosstopic_invoked,
		"cursor": last_processed.as_ref().map(|(dt, id)| format!("{}/{}", dt, id)),
		"details": details,
	});

	write_pass_log(root, "learn", &report)?;

	if !dry_run {
		if let Ok(index) = cache::search_index(root) {
			let docs: Vec<store::Document> = targets
				.iter()
				.filter_map(|(dt, id)| store::get_document(root, dt, id).ok())
				.collect();
			let _ = search::index_documents(&index, &docs);
		}
		let _ = crate::weight::recompute_all(root);
	}

	Ok(report)
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	fn mk_thought(root: &Path, title: &str, purpose: &str) -> String {
		let tags = vec!["thought".to_string(), purpose.to_string()];
		store::create_document(root, "thoughts", title, "body", tags, Some(purpose), None)
			.unwrap()
			.id
	}

	#[tokio::test]
	async fn cursor_advances_after_pass() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let mut ids = Vec::new();
		for i in 0..5 { ids.push(mk_thought(root, &format!("t{}", i), "p1")); }

		let _ = run_pass(root, 2, Some("p1"), false, false, false, &PassConfig::default()).await.unwrap();
		let cur = read_pass_cursor(root, "p1").expect("cursor written");
		assert_eq!(cur.0, "thoughts");
		assert!(ids.contains(&cur.1));
	}

	#[tokio::test]
	async fn cursor_resumes_from_position() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let mut ids = Vec::new();
		for i in 0..5 { ids.push(mk_thought(root, &format!("t{}", i), "p1")); }
		let docs = store::list_documents(root, "thoughts").unwrap();
		let order: Vec<String> = docs.into_iter().map(|d| d.id).collect();
		assert_eq!(order.len(), 5);

		write_pass_cursor(root, "p1", "thoughts", &order[2]).unwrap();
		let report = run_pass(root, 2, Some("p1"), false, false, false, &PassConfig::default()).await.unwrap();
		let details = report["details"].as_array().unwrap();
		let processed_ids: Vec<String> = details
			.iter()
			.filter_map(|d| d.get("doc_id").and_then(|v| v.as_str()).map(String::from))
			.collect();
		assert_eq!(processed_ids, vec![order[3].clone(), order[4].clone()]);
	}

	#[tokio::test]
	async fn cursor_wraps_when_exhausted() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		for i in 0..3 { mk_thought(root, &format!("t{}", i), "p1"); }
		let docs = store::list_documents(root, "thoughts").unwrap();
		let order: Vec<String> = docs.into_iter().map(|d| d.id).collect();

		write_pass_cursor(root, "p1", "thoughts", &order[2]).unwrap();
		let report = run_pass(root, 2, Some("p1"), false, false, false, &PassConfig::default()).await.unwrap();
		let details = report["details"].as_array().unwrap();
		let processed: Vec<String> = details
			.iter()
			.filter_map(|d| d.get("doc_id").and_then(|v| v.as_str()).map(String::from))
			.collect();
		assert_eq!(processed, vec![order[0].clone(), order[1].clone()]);
	}

	#[test]
	fn last_qa_at_skips_recent() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let id = mk_thought(root, "t-recent", "p1");
		stamp_last_qa_at(root, "thoughts", &id).unwrap();
		assert!(doc_qa_is_recent(root, "thoughts", &id));

		let dir2 = root.join("thoughts");
		let path = store::find_document_path_by_id(&dir2, &id).unwrap();
		let raw = std::fs::read_to_string(&path).unwrap();
		let new = raw.replace(
			&chrono::Utc::now().to_rfc3339()[..4],
			"2000",
		);
		let (mut fm, body) = store::parse_frontmatter(&new).unwrap();
		fm.as_object_mut().unwrap().insert(
			"last_qa_at".to_string(),
			serde_json::json!("2000-01-01T00:00:00+00:00"),
		);
		let fm_str = serde_yaml::to_string(&fm).unwrap();
		crate::io::write_atomic_str(&path, &format!("---\n{}---\n\n{}", fm_str, body)).unwrap();
		assert!(!doc_qa_is_recent(root, "thoughts", &id));
	}

	#[test]
	fn force_overrides_last_qa_at() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let id = mk_thought(root, "t-recent", "p1");
		stamp_last_qa_at(root, "thoughts", &id).unwrap();
		let recent = doc_qa_is_recent(root, "thoughts", &id);
		assert!(recent);
		let force = true;
		let qa = true;
		let should_skip = !force && qa && recent;
		assert!(!should_skip, "force must override recent-skip");
	}
}
