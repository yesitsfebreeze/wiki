use crate::store::Document;
use std::path::Path;
use std::sync::Mutex;
use tantivy::{schema::*, Index};

pub struct SearchIndex {
	index: Index,
	reader: tantivy::IndexReader,
	writer: Mutex<tantivy::IndexWriter>,
}

pub fn create_index(path: &Path) -> anyhow::Result<SearchIndex> {
	let mut schema_builder = Schema::builder();
	schema_builder.add_text_field("title", TEXT | STORED);
	schema_builder.add_text_field("content", TEXT | STORED);
	schema_builder.add_text_field("tags", TEXT | STORED);
	schema_builder.add_text_field("id", STRING | STORED);
	let schema = schema_builder.build();

	let index = if path.exists() {
		match Index::open_in_dir(path) {
			Ok(idx) => idx,
			Err(_) => {
				let _ = std::fs::remove_dir_all(path);
				std::fs::create_dir_all(path)?;
				Index::create_in_dir(path, schema.clone())?
			}
		}
	} else {
		std::fs::create_dir_all(path)?;
		Index::create_in_dir(path, schema.clone())?
	};

	let reader = index.reader()?;
	let writer = index.writer(50_000_000)?;

	Ok(SearchIndex {
		index,
		reader,
		writer: Mutex::new(writer),
	})
}

/// Index a single document and commit. Cheap for occasional writes; for bulk
/// loads call `index_documents` instead to amortize commit cost.
pub fn index_document(index: &SearchIndex, doc: &Document) -> anyhow::Result<()> {
	index_documents(index, std::slice::from_ref(doc))
}

/// Bulk-index a batch with a single commit + reader reload. Replaces any
/// prior copy keyed by `id` so re-ingest is idempotent.
pub fn index_documents(index: &SearchIndex, docs: &[Document]) -> anyhow::Result<()> {
	if docs.is_empty() {
		return Ok(());
	}
	let mut writer = index.writer.lock().unwrap();
	let schema = index.index.schema();
	let title_field = schema.get_field("title").unwrap();
	let content_field = schema.get_field("content").unwrap();
	let tags_field = schema.get_field("tags").unwrap();
	let id_field = schema.get_field("id").unwrap();

	use tantivy::Term;
	for doc in docs {
		writer.delete_term(Term::from_field_text(id_field, &doc.id));
		let mut td = tantivy::TantivyDocument::default();
		td.add_text(title_field, &doc.title);
		td.add_text(content_field, &doc.content);
		td.add_text(tags_field, doc.tags.join(" "));
		td.add_text(id_field, &doc.id);
		writer.add_document(td)?;
	}
	writer.commit()?;
	index.reader.reload()?;
	Ok(())
}

pub fn delete_by_id(index: &SearchIndex, id: &str) -> anyhow::Result<()> {
	let mut writer = index.writer.lock().unwrap();
	let id_field = index.index.schema().get_field("id").unwrap();
	writer.delete_term(tantivy::Term::from_field_text(id_field, id));
	writer.commit()?;
	index.reader.reload()?;
	Ok(())
}

pub fn search_topk(
	index: &SearchIndex,
	query_str: &str,
	tag_filter: Option<&str>,
	k: usize,
) -> anyhow::Result<Vec<(Document, f32)>> {
	use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
	use tantivy::schema::IndexRecordOption;
	use tantivy::Term;

	let searcher = index.reader.searcher();
	let schema = index.index.schema();
	let title_field = schema.get_field("title").unwrap();
	let content_field = schema.get_field("content").unwrap();
	let tags_field = schema.get_field("tags").unwrap();
	let id_field = schema.get_field("id").unwrap();

	let query_parser = tantivy::query::QueryParser::for_index(
		&index.index,
		vec![title_field, content_field, tags_field],
	);
	let parsed: Box<dyn Query> = query_parser.parse_query(query_str)?;

	let query: Box<dyn Query> = if let Some(tag) = tag_filter {
		let term = Term::from_field_text(tags_field, tag);
		let tag_q: Box<dyn Query> = Box::new(TermQuery::new(term, IndexRecordOption::Basic));
		Box::new(BooleanQuery::new(vec![
			(Occur::Must, parsed),
			(Occur::Must, tag_q),
		]))
	} else {
		parsed
	};

	let top_docs = searcher.search(&*query, &tantivy::collector::TopDocs::with_limit(k).order_by_score())?;

	let mut results = Vec::with_capacity(top_docs.len());
	for (score, doc_address) in top_docs {
		let retrieved_doc: TantivyDocument = searcher.doc(doc_address)?;

		let id = retrieved_doc.get_first(id_field).and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
		let title = retrieved_doc.get_first(title_field).and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
		let content = retrieved_doc.get_first(content_field).and_then(|v| v.as_str().map(String::from)).unwrap_or_default();
		let tags: Vec<String> = retrieved_doc
			.get_first(tags_field)
			.and_then(|v| v.as_str().map(String::from))
			.map(|s| s.split_whitespace().map(String::from).collect())
			.unwrap_or_default();

		results.push((
			Document {
				id, title, tags,
				purpose: None,
				source_doc_id: None,
				created_at: String::new(),
				updated_at: String::new(),
				content,
			},
			score,
		));
	}

	Ok(results)
}
