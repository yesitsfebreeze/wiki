//! One-shot template-question cleanup.

use crate::cache;
use crate::store;
use anyhow::Result;
use std::path::Path;

/// Outcome of [`migrate_templated_questions`].
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TemplateMigrationReport {
	pub scanned: usize,
	pub templated: usize,
	pub deleted: usize,
	/// IDs that matched a template but were spared because they had at least
	/// one inbound `Answers` edge.
	pub kept_with_answers: Vec<String>,
}

/// One-shot cleanup: walk all questions, delete those whose title matches a
/// template regex AND have no inbound `Answers` reason edges.
pub fn migrate_templated_questions(root: &Path, dry_run: bool) -> Result<TemplateMigrationReport> {
	let mut rep = TemplateMigrationReport::default();
	let questions = store::list_documents(root, "questions").unwrap_or_default();
	rep.scanned = questions.len();

	for q in questions {
		if !crate::config::is_template_question(&q.title) {
			continue;
		}
		rep.templated += 1;

		let adj = cache::reason_index_lookup(root, &q.id);
		let mut has_answer = false;
		for rid in adj.to.iter().chain(adj.from.iter()) {
			if let Ok(r) = store::get_document(root, "reasons", rid) {
				if r.title.contains("-[Answers]->") {
					has_answer = true;
					break;
				}
			}
		}

		if has_answer {
			rep.kept_with_answers.push(q.id.clone());
			continue;
		}
		if dry_run {
			rep.deleted += 1;
			continue;
		}
		if store::delete_document(root, "questions", &q.id).is_ok() {
			rep.deleted += 1;
		}
	}
	Ok(rep)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::learn::infra::fnv_question_id;
	use tempfile::TempDir;

	#[test]
	fn template_regex_matches_relate_form() {
		assert!(crate::config::is_template_question(
			"How does 'GPU Pipeline (8-Pass)' relate to or differ from similar concepts?"
		));
		assert!(crate::config::is_template_question(
			"What are the key characteristics of 'XPBD'?"
		));
		assert!(crate::config::is_template_question(
			"What are the implications of 'Visibility Buffer'?"
		));
		assert!(crate::config::is_template_question(
			"What is the importance of 'foo'?"
		));
		assert!(!crate::config::is_template_question(
			"Why does the 8-Pass pipeline tile in 32x32 blocks instead of 16x16?"
		));
	}

	#[test]
	fn migration_deletes_unanswered_templates() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();

		let anchor = store::create_document(
			root, "thoughts", "anchor", "b", vec!["thought".into()], Some("phyons"), None,
		).unwrap();

		let mut q_ids = Vec::new();
		for (i, t) in [
			"How does 'A' relate to or differ from similar concepts?",
			"What are the key characteristics of 'B'?",
			"What are the implications of 'C'?",
		].iter().enumerate() {
			let hash = fnv_question_id(t);
			let tags = vec!["question".to_string(), "phyons".to_string(), hash];
			let q = store::create_document(root, "questions", t, "b", tags, Some("phyons"), None).unwrap();
			if i == 0 {
				store::create_reason(root, &anchor.id, &q.id, "Answers", "answers it", Some("phyons")).unwrap();
			}
			q_ids.push(q.id);
		}
		let novel_hash = fnv_question_id("Novel question that survives?");
		let novel_tags = vec!["question".to_string(), "phyons".to_string(), novel_hash];
		let novel = store::create_document(
			root, "questions", "Novel question that survives?", "b", novel_tags, Some("phyons"), None,
		).unwrap();

		let report = migrate_templated_questions(root, false).unwrap();
		assert_eq!(report.scanned, 4);
		assert_eq!(report.templated, 3);
		assert_eq!(report.deleted, 2);
		assert_eq!(report.kept_with_answers.len(), 1);

		let remaining = store::list_documents(root, "questions").unwrap();
		let remaining_ids: std::collections::HashSet<_> =
			remaining.iter().map(|d| d.id.clone()).collect();
		assert!(remaining_ids.contains(&q_ids[0]));
		assert!(remaining_ids.contains(&novel.id));
		assert!(!remaining_ids.contains(&q_ids[1]));
		assert!(!remaining_ids.contains(&q_ids[2]));
	}

	#[test]
	fn migration_dry_run_deletes_nothing() {
		let dir = TempDir::new().unwrap();
		let root = dir.path();
		store::bootstrap(root).unwrap();
		let t = "What are the implications of 'X'?";
		let hash = fnv_question_id(t);
		let tags = vec!["question".to_string(), "p".to_string(), hash];
		store::create_document(root, "questions", t, "b", tags, Some("p"), None).unwrap();
		let r = migrate_templated_questions(root, true).unwrap();
		assert_eq!(r.templated, 1);
		assert_eq!(r.deleted, 1);
		assert_eq!(store::list_documents(root, "questions").unwrap().len(), 1);
	}
}
