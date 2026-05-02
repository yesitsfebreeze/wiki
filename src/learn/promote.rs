//! Cross-reference scoring, conclusion promotion + merge, cross-topic synthesis
//! (bridging) pass.

use super::infra::{
	allowed_kind, find_conclusion_by_hash, fnv_question_id, read_reason_meta, AnswerCandidate,
	PassConfig,
};
use crate::cache;
use crate::{classifier, http, smart, store};
use anyhow::Result;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Deserialize, Debug)]
struct ScoredCand {
	picked_id: String,
	#[serde(default)]
	score: f32,
	#[serde(default)]
	kind: String,
	#[serde(default)]
	body: String,
}

#[derive(Deserialize, Debug)]
struct ScoredResp {
	#[serde(default)]
	scored: Vec<ScoredCand>,
}

pub(crate) async fn cross_reference_question(
	root: &Path,
	question: &str,
	purpose: Option<&str>,
	support_threshold: f32,
) -> Result<Vec<AnswerCandidate>> {
	let res = smart::query(root, question, purpose, 5, 5).await?;
	let results = res.get("results").and_then(|v| v.as_array()).cloned().unwrap_or_default();
	if results.is_empty() {
		return Ok(Vec::new());
	}
	let mut id_to_dt: HashMap<String, String> = HashMap::new();
	for r in &results {
		let Some(id) = r.get("id").and_then(|v| v.as_str()) else { continue };
		for dt in &["entities", "thoughts", "conclusions", "reasons", "questions"] {
			if store::get_document(root, dt, id).is_ok() {
				id_to_dt.insert(id.to_string(), (*dt).to_string());
				break;
			}
		}
	}

	let cand_json = serde_json::to_string(&results)?;
	let sys = "Given a question and these candidate docs, score each 0..1 for how well it \
		answers, and pick a kind from Answers|Supports|Contradicts|Extends|References. \
		Return JSON {\"scored\":[{\"picked_id\":string,\"score\":number,\"kind\":string,\"body\":string}]}. \
		`body` is one short sentence on WHY. Include every candidate id.";
	let user = format!("Question: {}\n\nCandidates:\n{}", question, cand_json);
	let raw = http::chat_json(sys, &user).await?;
	let parsed: ScoredResp = serde_json::from_str(&raw)
		.map_err(|e| anyhow::anyhow!("cross-ref parse: {} body: {}", e, raw))?;

	let out = parsed.scored.into_iter()
		.filter(|c| c.score >= support_threshold)
		.map(|c| AnswerCandidate {
			doc_type: id_to_dt.get(&c.picked_id).cloned().unwrap_or_else(|| "thoughts".to_string()),
			doc_id: c.picked_id,
			score: c.score,
			kind: allowed_kind(&c.kind).to_string(),
			body: c.body,
		})
		.collect();
	Ok(out)
}

const BRIDGING_BONUS: f32 = 1.5;

/// Return the set of distinct purposes from which a candidate doc has inbound
/// `Supports` reason edges.
pub fn candidate_support_purposes(root: &Path, doc_id: &str) -> HashSet<String> {
	let mut set = HashSet::new();
	let adj = cache::reason_index_lookup(root, doc_id);
	for rid in &adj.to {
		let Some((from_id, _to, kind, _rp)) = read_reason_meta(root, rid) else { continue };
		if kind != "Supports" { continue; }
		for dt in &["thoughts", "conclusions", "entities", "questions"] {
			if let Ok(d) = store::get_document(root, dt, &from_id) {
				if let Some(p) = d.purpose {
					if !p.is_empty() { set.insert(p); }
				}
				break;
			}
		}
	}
	set
}

pub fn apply_bridging_bonus(
	root: &Path,
	candidates: Vec<AnswerCandidate>,
	question_purpose: Option<&str>,
) -> Vec<AnswerCandidate> {
	candidates
		.into_iter()
		.map(|mut c| {
			let supports = candidate_support_purposes(root, &c.doc_id);
			let bridges = supports
				.iter()
				.any(|p| Some(p.as_str()) != question_purpose);
			if bridges {
				c.score *= BRIDGING_BONUS;
			}
			c
		})
		.collect()
}

pub fn question_support_purposes(root: &Path, question_id: &str) -> HashSet<String> {
	let mut set = HashSet::new();
	let adj = cache::reason_index_lookup(root, question_id);
	for rid in &adj.to {
		let Some((from_id, _to, kind, _rp)) = read_reason_meta(root, rid) else { continue };
		if kind != "Supports" { continue; }
		for dt in &["thoughts", "conclusions", "entities", "questions"] {
			if let Ok(d) = store::get_document(root, dt, &from_id) {
				if let Some(p) = d.purpose {
					if !p.is_empty() { set.insert(p); }
				}
				break;
			}
		}
	}
	set
}

pub fn should_invoke_cross_topic(root: &Path, question_id: &str) -> bool {
	question_support_purposes(root, question_id).len() >= 2
}

fn tag_conclusion_with_bridges(
	root: &Path,
	conclusion_id: &str,
	bridge_purposes: &HashSet<String>,
) -> Result<()> {
	let doc = store::get_document(root, "conclusions", conclusion_id)?;
	let mut tags = doc.tags.clone();
	let crosstopic = "crosstopic".to_string();
	if !tags.iter().any(|t| t == &crosstopic) {
		tags.push(crosstopic);
	}
	for p in bridge_purposes {
		let bt = format!("bridges-{}", p);
		if !tags.iter().any(|t| t == &bt) {
			tags.push(bt);
		}
	}
	if tags != doc.tags {
		store::update_document(root, "conclusions", conclusion_id, None, Some(tags))?;
	}
	Ok(())
}

async fn cross_topic_emit_and_promote(
	root: &Path,
	question_id: &str,
	question_purpose: Option<&str>,
	candidates: &[AnswerCandidate],
	cfg: &PassConfig,
) -> Result<usize> {
	let strong = cfg.answer_threshold;
	let mut strong_edges: Vec<AnswerCandidate> = Vec::new();
	let mut got_answer = false;
	for c in candidates {
		let kind = if c.score >= strong { "Answers" } else { "Supports" };
		if c.score >= strong { got_answer = true; }
		let _ = store::create_reason(
			root, question_id, &c.doc_id, kind, &c.body, question_purpose,
		);
		if c.score >= strong { strong_edges.push(c.clone()); }
	}
	if !got_answer {
		return Ok(0);
	}

	if let Ok(mut q) = store::get_document(root, "questions", question_id) {
		if !q.tags.iter().any(|t| t == "resolved") {
			q.tags.push("resolved".to_string());
			let _ = store::update_document(root, "questions", question_id, None, Some(q.tags));
		}
	}

	let mut bridges: HashSet<String> = HashSet::new();
	for c in &strong_edges {
		for p in candidate_support_purposes(root, &c.doc_id) {
			if Some(p.as_str()) != question_purpose {
				bridges.insert(p);
			}
		}
		for dt in &["thoughts", "conclusions", "entities"] {
			if let Ok(d) = store::get_document(root, dt, &c.doc_id) {
				if let Some(p) = d.purpose {
					if !p.is_empty() && Some(p.as_str()) != question_purpose {
						bridges.insert(p);
					}
				}
				break;
			}
		}
	}

	let cid = promote_to_conclusion(root, question_id, &strong_edges, cfg).await?;
	if let Some(cid) = cid.as_ref() {
		let _ = tag_conclusion_with_bridges(root, cid, &bridges);
	}
	Ok(strong_edges.len())
}

pub async fn cross_topic_pass(root: &Path, question_id: &str, cfg: &PassConfig) -> Result<usize> {
	let q = store::get_document(root, "questions", question_id)?;
	let cands = cross_reference_question(root, &q.title, None, cfg.support_threshold).await?;
	if cands.is_empty() {
		return Ok(0);
	}
	let scored = apply_bridging_bonus(root, cands, q.purpose.as_deref());
	cross_topic_emit_and_promote(root, question_id, q.purpose.as_deref(), &scored, cfg).await
}

pub fn find_similar_conclusion(
	root: &Path,
	body_emb: &[f32],
	purpose_tag: &str,
	threshold: f32,
) -> Option<(String, f32)> {
	let candidates = cache::tag_index_lookup(root, purpose_tag);
	let mut best: Option<(String, f32)> = None;
	for dref in candidates {
		if dref.doc_type != "conclusions" {
			continue;
		}
		let Some(entry) = cache::pool_get(root, &dref.id) else { continue };
		let s = classifier::cosine(body_emb, &entry.vec);
		if s >= threshold && best.as_ref().is_none_or(|(_, bs)| s > *bs) {
			best = Some((dref.id.clone(), s));
		}
	}
	best
}

pub async fn promote_to_conclusion(
	root: &Path,
	question_id: &str,
	edges: &[AnswerCandidate],
	cfg: &PassConfig,
) -> Result<Option<String>> {
	let question = store::get_document(root, "questions", question_id)?;
	let hash = fnv_question_id(&question.title);
	if let Some(existing) = find_conclusion_by_hash(root, &hash) {
		return Ok(Some(existing));
	}

	#[derive(serde::Serialize)]
	struct EdgeView<'a> {
		doc_id: &'a str,
		doc_type: &'a str,
		score: f32,
		kind: &'a str,
		body: &'a str,
	}
	let edges_json: Vec<EdgeView> = edges.iter().map(|e| EdgeView {
		doc_id: &e.doc_id, doc_type: &e.doc_type, score: e.score, kind: &e.kind, body: &e.body,
	}).collect();

	let sys = "Synthesize a 1-paragraph conclusion answering this question, citing the \
		supplied edges. Be concise. Return JSON {\"body\": string}.";
	let user = format!(
		"Question: {}\n\nEdges JSON:\n{}",
		question.title,
		serde_json::to_string(&edges_json)?,
	);
	let raw = http::chat_json(sys, &user).await?;
	#[derive(Deserialize)]
	struct Body { body: String }
	let parsed: Body = serde_json::from_str(&raw)
		.map_err(|e| anyhow::anyhow!("promote parse: {} body: {}", e, raw))?;

	let purpose = question.purpose.clone();
	let purpose_tag = purpose.clone().unwrap_or_else(|| "general".to_string());

	let body_text = format!("{}\n\n{}", question.title, parsed.body);
	let threshold = cfg.conclusion_merge_threshold;
	let body_embs = http::embed_batch(&[body_text]).await?;
	if let Some(body_emb) = body_embs.into_iter().next() {
		if let Some((existing_id, _)) =
			find_similar_conclusion(root, &body_emb, &purpose_tag, threshold)
		{
			let _ = store::create_reason(
				root,
				question_id,
				&existing_id,
				"Consolidates",
				"merged into existing conclusion via embedding similarity",
				purpose.as_deref(),
			);
			eprintln!("merged into existing conclusion {}", existing_id);
			return Ok(Some(existing_id));
		}
	}

	let tags = vec!["conclusion".to_string(), purpose_tag.clone(), hash];
	let cdoc = store::create_document(
		root, "conclusions", &question.title, &parsed.body, tags, Some(&purpose_tag), None,
	)?;

	let _ = store::create_reason(root, question_id, &cdoc.id, "Derives", "promoted from resolved question", purpose.as_deref());
	let strong = cfg.answer_threshold;
	for e in edges {
		if e.score >= strong {
			let _ = store::create_reason(root, &cdoc.id, &e.doc_id, "References", &e.body, purpose.as_deref());
		}
	}
	Ok(Some(cdoc.id))
}

#[cfg(test)]
mod tests {
	use super::*;
	use tempfile::TempDir;

	fn seed_pool_entry(root: &Path, id: &str, doc_type: &str, title: &str, content: &str, vec: Vec<f32>) {
		let doc = store::Document {
			id: id.to_string(),
			title: title.to_string(),
			tags: vec![],
			purpose: None,
			source_doc_id: None,
			created_at: String::new(),
			updated_at: String::new(),
			content: content.to_string(),
		};
		cache::pool_insert(root, cache::PoolEntry {
			doc_type: doc_type.to_string(),
			doc,
			content_hash: "x".to_string(),
			vec,
		});
	}

	#[test]
	fn promote_creates_new_when_no_similar() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let probe = vec![1.0, 0.0, 0.0];
		let hit = find_similar_conclusion(root, &probe, "general", 0.92);
		assert!(hit.is_none(), "expected no merge candidate in empty vault");
	}

	#[test]
	fn promote_merges_when_similar() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();

		let existing_emb = vec![0.6, 0.8, 0.0];
		let cdoc = store::create_document(
			root,
			"conclusions",
			"X means Y",
			"X means Y because reasons.",
			vec!["conclusion".to_string(), "general".to_string()],
			Some("general"),
			None,
		).unwrap();
		seed_pool_entry(root, &cdoc.id, "conclusions", &cdoc.title, &cdoc.content, existing_emb.clone());

		let probe = vec![0.6, 0.8, 0.0];
		let hit = find_similar_conclusion(root, &probe, "general", 0.92);
		assert_eq!(hit.as_ref().map(|(id, _)| id.clone()), Some(cdoc.id.clone()));

		let ortho = vec![0.0, 0.0, 1.0];
		let no_hit = find_similar_conclusion(root, &ortho, "general", 0.92);
		assert!(no_hit.is_none());

		cache::pool_remove(root, &cdoc.id);
	}

	#[test]
	fn merge_threshold_param_controls_similarity_match() {
		assert!((PassConfig::default().conclusion_merge_threshold - 0.92).abs() < 1e-6);

		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let cdoc = store::create_document(
			root,
			"conclusions",
			"Z",
			"z body",
			vec!["conclusion".to_string(), "general".to_string()],
			Some("general"),
			None,
		).unwrap();
		let existing = vec![1.0, 0.0, 0.0];
		seed_pool_entry(root, &cdoc.id, "conclusions", &cdoc.title, &cdoc.content, existing);
		let probe = vec![0.94, 0.34, 0.0];
		let cos = classifier::cosine(&probe, &[1.0, 0.0, 0.0]);
		assert!(cos > 0.92 && cos < 0.99, "cos={}", cos);

		assert!(find_similar_conclusion(root, &probe, "general", 0.99).is_none());
		assert!(find_similar_conclusion(root, &probe, "general", 0.92).is_some());

		cache::pool_remove(root, &cdoc.id);
	}

	#[tokio::test]
	async fn promote_to_conclusion_idempotent() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let qtitle = "What is ownership?";
		let qhash = fnv_question_id(qtitle);
		let qtags = vec!["question".to_string(), "general".to_string(), qhash.clone()];
		let qdoc = store::create_document(root, "questions", qtitle, "q body", qtags, Some("general"), None).unwrap();

		let ctags = vec!["conclusion".to_string(), "general".to_string(), qhash.clone()];
		let cdoc = store::create_document(root, "conclusions", qtitle, "existing", ctags, Some("general"), None).unwrap();

		let result = promote_to_conclusion(root, &qdoc.id, &[], &PassConfig::default()).await.unwrap();
		assert_eq!(result, Some(cdoc.id));
		let concs = store::list_documents(root, "conclusions").unwrap();
		assert_eq!(concs.len(), 1);
	}

	#[tokio::test]
	async fn cross_topic_finds_bridging_answer() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();

		let qtitle = "How does Clustered Forward+ relate to deferred shading?";
		let qhash = fnv_question_id(qtitle);
		let qtags = vec!["question".to_string(), "phyons".to_string(), qhash.clone()];
		let qdoc = store::create_document(root, "questions", qtitle, "qbody", qtags, Some("phyons"), None).unwrap();

		let cand = store::create_document(
			root,
			"thoughts",
			"Forward+ Clustering",
			"Clustered Forward+ partitions the view frustum into 3D clusters.",
			vec!["thought".to_string(), "forward-plus".to_string()],
			Some("forward-plus"),
			None,
		).unwrap();

		let cdoc = store::create_document(
			root,
			"conclusions",
			qtitle,
			"existing",
			vec!["conclusion".to_string(), "phyons".to_string(), qhash.clone()],
			Some("phyons"),
			None,
		).unwrap();

		let cands = vec![AnswerCandidate {
			doc_id: cand.id.clone(),
			doc_type: "thoughts".to_string(),
			score: 0.95,
			kind: "Answers".to_string(),
			body: "directly explains the relation".to_string(),
		}];

		let n = cross_topic_emit_and_promote(root, &qdoc.id, Some("phyons"), &cands, &PassConfig::default()).await.unwrap();
		assert_eq!(n, 1, "one strong edge expected");

		let q = store::get_document(root, "questions", &qdoc.id).unwrap();
		assert!(q.tags.iter().any(|t| t == "resolved"));

		let from_q = store::search_reasons_for(root, &qdoc.id, "from").unwrap();
		assert!(
			from_q.iter().any(|r| r.title.contains("-[Answers]->") && r.title.ends_with(&cand.id)),
			"expected Answers edge q→cand"
		);

		let cf = store::get_document(root, "conclusions", &cdoc.id).unwrap();
		assert!(cf.tags.iter().any(|t| t == "crosstopic"));
		assert!(cf.tags.iter().any(|t| t == "bridges-forward-plus"));
	}

	#[test]
	fn bridging_bonus_applied() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();

		let cand_a = store::create_document(
			root, "thoughts", "A", "a body",
			vec!["thought".to_string(), "phyons".to_string()],
			Some("phyons"), None,
		).unwrap();
		let cand_b = store::create_document(
			root, "thoughts", "B", "b body",
			vec!["thought".to_string(), "phyons".to_string()],
			Some("phyons"), None,
		).unwrap();

		let same_purpose_src = store::create_document(
			root, "thoughts", "src-same", "x",
			vec!["thought".to_string(), "phyons".to_string()],
			Some("phyons"), None,
		).unwrap();
		let other_purpose_src = store::create_document(
			root, "thoughts", "src-other", "y",
			vec!["thought".to_string(), "forward-plus".to_string()],
			Some("forward-plus"), None,
		).unwrap();

		store::create_reason(root, &same_purpose_src.id, &cand_a.id, "Supports", "z", Some("phyons")).unwrap();
		store::create_reason(root, &other_purpose_src.id, &cand_a.id, "Supports", "z", Some("forward-plus")).unwrap();
		store::create_reason(root, &same_purpose_src.id, &cand_b.id, "Supports", "z", Some("phyons")).unwrap();

		cache::invalidate_indexes(root);

		let cands = vec![
			AnswerCandidate { doc_id: cand_a.id.clone(), doc_type: "thoughts".into(), score: 0.5, kind: "Supports".into(), body: "".into() },
			AnswerCandidate { doc_id: cand_b.id.clone(), doc_type: "thoughts".into(), score: 0.5, kind: "Supports".into(), body: "".into() },
		];
		let scored = apply_bridging_bonus(root, cands, Some("phyons"));
		let a = scored.iter().find(|c| c.doc_id == cand_a.id).unwrap();
		let b = scored.iter().find(|c| c.doc_id == cand_b.id).unwrap();
		assert!(a.score > b.score, "bridging candidate should outrank single-purpose");
		assert!((a.score - 0.75).abs() < 1e-5, "expected 1.5× boost, got {}", a.score);
		assert!((b.score - 0.5).abs() < 1e-5, "expected no boost, got {}", b.score);
	}

	#[test]
	fn auto_trigger_on_multi_support_question() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();

		let qtitle = "Open question?";
		let qhash = fnv_question_id(qtitle);
		let qdoc = store::create_document(
			root, "questions", qtitle, "body",
			vec!["question".to_string(), "phyons".to_string(), qhash],
			Some("phyons"), None,
		).unwrap();

		let src_a = store::create_document(
			root, "thoughts", "src-A", "a",
			vec!["thought".to_string(), "phyons".to_string()],
			Some("phyons"), None,
		).unwrap();
		let src_b = store::create_document(
			root, "thoughts", "src-B", "b",
			vec!["thought".to_string(), "forward-plus".to_string()],
			Some("forward-plus"), None,
		).unwrap();

		store::create_reason(root, &src_a.id, &qdoc.id, "Supports", "z", Some("phyons")).unwrap();
		cache::invalidate_indexes(root);
		assert!(!should_invoke_cross_topic(root, &qdoc.id));

		store::create_reason(root, &src_b.id, &qdoc.id, "Supports", "z", Some("forward-plus")).unwrap();
		cache::invalidate_indexes(root);
		assert!(should_invoke_cross_topic(root, &qdoc.id));
		let purposes = question_support_purposes(root, &qdoc.id);
		assert!(purposes.contains("phyons"));
		assert!(purposes.contains("forward-plus"));
	}
}
