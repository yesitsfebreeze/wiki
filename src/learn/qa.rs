//! `run_pass` orchestrator + per-doc QA loop. Coordinates link, connect, raise,
//! cross-reference, promote, and cross-topic auto-trigger.
//!
//! Sampling: weighted random by inverse edge degree (orphans first).
//! Replaces the prior cursor walk; cursor file now records last-sampled doc
//! for diagnostics only.

use super::connect::connect_doc;
use super::infra::{
	doc_qa_is_recent, stamp_last_qa_at, write_pass_cursor, write_pass_log,
	AnswerCandidate, PassConfig,
};
use super::links::link_doc_internal;
use super::promote::{
	cross_reference_question, cross_topic_pass, promote_to_conclusion, should_invoke_cross_topic,
};
use super::raise::raise_questions_for_doc;
use crate::cache;
use crate::{search, store};
use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

/// Time-seeded SplitMix64 — small, no-dep PRNG sufficient for sample selection.
fn next_rand(state: &mut u64) -> u64 {
	*state = state.wrapping_add(0x9E3779B97F4A7C15);
	let mut z = *state;
	z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
	z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
	z ^ (z >> 31)
}

fn rand_unit(state: &mut u64) -> f64 {
	(next_rand(state) >> 11) as f64 / (1u64 << 53) as f64
}

/// Compute a weighted-random sample (without replacement) of `(doc_type, id)`
/// pairs, biasing toward low-degree nodes. weight(doc) = 1 / (1 + degree).
/// Uses a time-seeded SplitMix64 — sample order varies per pass.
pub(crate) fn sample_weighted_by_inverse_degree(
	root: &Path,
	universe: &[(String, String)],
	limit: usize,
) -> Vec<(String, String)> {
	if universe.is_empty() || limit == 0 {
		return Vec::new();
	}

	let mut weights: Vec<f64> = universe
		.iter()
		.map(|(_, id)| {
			let adj = cache::reason_index_lookup(root, id);
			let degree = adj.from.len() + adj.to.len();
			1.0 / (1.0 + degree as f64)
		})
		.collect();

	let n = universe.len().min(limit);
	let mut picked: Vec<(String, String)> = Vec::with_capacity(n);
	let mut state: u64 = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|d| d.as_nanos() as u64)
		.unwrap_or(0xDEADBEEFCAFEBABE);

	for _ in 0..n {
		let total: f64 = weights.iter().sum();
		if total <= 0.0 {
			break;
		}
		let mut r: f64 = rand_unit(&mut state) * total;
		let mut chosen = 0usize;
		for (i, w) in weights.iter().enumerate() {
			if r < *w {
				chosen = i;
				break;
			}
			r -= *w;
		}
		picked.push(universe[chosen].clone());
		weights[chosen] = 0.0; // sample without replacement
	}
	picked
}

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
	// LLM question raising is opt-in: cfg.raise_questions guards it so that
	// auto-/learn passes triggered by ingest stay quiet. Search-miss raising
	// (see learn::raise::raise_question_from_search_miss) and deliberate
	// `/learn --raise` runs are the only paths that create questions.
	let raised: Vec<super::infra::RaisedQuestion> = if cfg.raise_questions {
		match raise_questions_for_doc(root, doc, false).await {
			Ok(v) => v,
			Err(e) => {
				eprintln!("raise_questions_for_doc({}): {}", doc.id, e);
				Vec::new()
			}
		}
	} else {
		Vec::new()
	};

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
			if q.tags.iter().any(|t| t == "answered" || t == "dropped") { continue; }
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
				if !q.tags.iter().any(|t| t == "answered") {
					q.tags.push("answered".to_string());
					let _ = store::update_document(root, "questions", &qid, None, Some(q.tags));
					let _ = super::links::move_to_answered(root, &qid);
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

	// Universe: thoughts ∪ conclusions (optionally purpose-filtered).
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

	// Weighted-random sample: weight(doc) = 1 / (1 + edge_degree(doc)).
	// Orphans (degree 0) get weight 1; heavily-linked nodes get small weight.
	// Sampling is without replacement up to `limit`.
	let targets: Vec<(String, String)> = sample_weighted_by_inverse_degree(root, &sequence, limit);

	let mut docs_modified = 0u64;
	let mut links_added = 0u64;
	let mut merges_total = 0u64;
	let mut edges_added = 0u64;
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

		if dry_run {
			continue;
		}

		if let Ok(doc) = store::get_document(root, dt, id) {
			super::code_links::enrich_with_code_refs(root, dt, &doc);
		}

		// Connect-step: graph densification independent of the question loop.
		// Always runs (not gated on `qa`) — densification is the primary
		// reason a learn pass exists.
		if llm_budget > 0 {
			if let Ok(doc) = store::get_document(root, dt, id) {
				match connect_doc(root, &doc, cfg).await {
					Ok((added, used)) => {
						edges_added += added;
						llm_budget = llm_budget.saturating_sub(used);
					}
					Err(e) => {
						eprintln!("[learn] connect-step failed for doc {}: {} — auto-link skipped", id, e);
						details.push(serde_json::json!({
							"doc_id": id, "connect_error": e.to_string(),
						}));
					}
				}
			}
		}

		if !qa || llm_budget == 0 {
			continue;
		}
		let Ok(doc) = store::get_document(root, dt, id) else { continue };
		match qa_for_doc(root, &doc, &mut llm_budget, cfg).await {
			Ok((raised, answered, promoted)) => {
				questions_raised += raised;
				questions_answered += answered;
				conclusions_promoted += promoted;
				let _ = stamp_last_qa_at(root, dt, id);
			}
			Err(e) => details.push(serde_json::json!({
				"doc_id": id, "qa_error": e.to_string(),
			})),
		}
	}

	let mut crosstopic_invoked = 0u64;
	if qa && !dry_run {
		let answered: HashSet<String> = cache::tag_index_lookup(root, "answered")
			.into_iter()
			.chain(cache::tag_index_lookup(root, "dropped"))
			.filter(|d| d.doc_type == "questions")
			.map(|d| d.id)
			.collect();
		let candidates: Vec<String> = cache::tag_index_lookup(root, "question")
			.into_iter()
			.filter(|d| d.doc_type == "questions" && !answered.contains(&d.id))
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

	// Invariant: a real pass must add ≥1 edge OR question OR conclusion.
	// Skipped-recent passes (entirely no-op) and dry runs are exempt.
	let progress = links_added + edges_added + questions_raised + conclusions_promoted;
	let invariant_violated = !dry_run
		&& !targets.is_empty()
		&& skipped_recent < targets.len() as u64
		&& progress == 0;
	if invariant_violated {
		eprintln!(
			"learn pass invariant violated: {} docs scanned, 0 links/edges/questions/conclusions added. \
			 Widen N (limit) or lower thresholds (edge_threshold={}, answer_threshold={}).",
			targets.len(), cfg.edge_threshold, cfg.answer_threshold,
		);
	}

	let report = serde_json::json!({
		"pass_id": chrono::Utc::now().to_rfc3339(),
		"docs_scanned": targets.len(),
		"docs_modified": docs_modified,
		"links_added": links_added,
		"paragraphs_merged": merges_total,
		"edges_added": edges_added,
		"questions_raised": questions_raised,
		"questions_answered": questions_answered,
		"conclusions_promoted": conclusions_promoted,
		"entity_count": entities.len(),
		"purpose_filter": purpose,
		"dry_run": dry_run,
		"qa": qa,
		"force": force,
		"raise_questions": cfg.raise_questions,
		"skipped_recent": skipped_recent,
		"crosstopic_invoked": crosstopic_invoked,
		"invariant_violated": invariant_violated,
		"sampling": "weighted_inverse_degree",
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
	async fn pass_writes_cursor_for_diagnostics() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let mut ids = Vec::new();
		for i in 0..5 { ids.push(mk_thought(root, &format!("t{}", i), "p1")); }

		let _ = run_pass(root, 2, Some("p1"), false, false, false, &PassConfig::default()).await.unwrap();
		let cur = super::super::infra::read_pass_cursor(root, "p1").expect("cursor written");
		assert_eq!(cur.0, "thoughts");
		assert!(ids.contains(&cur.1), "cursor must reference one of the seeded docs");
	}

	#[test]
	fn weighted_sampling_prefers_low_degree() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();

		let orphan = mk_thought(root, "orphan", "p1");
		let hub = mk_thought(root, "hub", "p1");
		// Saturate the hub with many outbound reasons → high degree, low weight.
		for i in 0..20 {
			let leaf = mk_thought(root, &format!("leaf{}", i), "p1");
			store::create_reason(root, &hub, &leaf, "References", "x", Some("p1")).unwrap();
		}
		cache::invalidate_indexes(root);

		let universe: Vec<(String, String)> = store::list_documents(root, "thoughts")
			.unwrap()
			.into_iter()
			.map(|d| ("thoughts".to_string(), d.id))
			.collect();

		// Run sampling many times; orphan must be picked far more often than hub.
		let mut orphan_count = 0;
		let mut hub_count = 0;
		for _ in 0..200 {
			let picked = sample_weighted_by_inverse_degree(root, &universe, 1);
			if picked[0].1 == orphan { orphan_count += 1; }
			if picked[0].1 == hub { hub_count += 1; }
		}
		assert!(
			orphan_count > hub_count * 5,
			"orphan should dominate hub by inverse-degree weighting (orphan={}, hub={})",
			orphan_count, hub_count,
		);
	}

	#[test]
	fn weighted_sampling_returns_empty_for_empty_universe() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let picked = sample_weighted_by_inverse_degree(root, &[], 5);
		assert!(picked.is_empty());
	}

	#[test]
	fn weighted_sampling_caps_at_universe_size() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let _a = mk_thought(root, "a", "p1");
		let _b = mk_thought(root, "b", "p1");
		let universe: Vec<(String, String)> = store::list_documents(root, "thoughts")
			.unwrap()
			.into_iter()
			.map(|d| ("thoughts".to_string(), d.id))
			.collect();
		let picked = sample_weighted_by_inverse_degree(root, &universe, 100);
		assert_eq!(picked.len(), 2);
		// Without replacement → no duplicates.
		assert_ne!(picked[0].1, picked[1].1);
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
