use crate::store::Document;
use std::path::Path;
use tantivy::{schema::*, Index};

pub struct SearchIndex {
	index: Index,
	reader: tantivy::IndexReader,
	writer: std::sync::Mutex<tantivy::IndexWriter>,
}

pub fn create_index(path: &Path) -> anyhow::Result<SearchIndex> {
	let mut schema_builder = Schema::builder();
	let _title = schema_builder.add_text_field("title", TEXT | STORED);
	let _content = schema_builder.add_text_field("content", TEXT | STORED);
	let _tags = schema_builder.add_text_field("tags", TEXT | STORED);
	let _id = schema_builder.add_text_field("id", STORED);
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
		writer: std::sync::Mutex::new(writer),
	})
}

pub fn index_document(index: &SearchIndex, doc: &Document) -> anyhow::Result<()> {
	let mut writer = index.writer.lock().unwrap();
	let schema = index.index.schema();

	let title_field = schema.get_field("title").unwrap();
	let content_field = schema.get_field("content").unwrap();
	let tags_field = schema.get_field("tags").unwrap();
	let id_field = schema.get_field("id").unwrap();

	let mut document = tantivy::TantivyDocument::default();
	document.add_text(title_field, &doc.title);
	document.add_text(content_field, &doc.content);
	document.add_text(tags_field, doc.tags.join(" "));
	document.add_text(id_field, &doc.id);

	writer.add_document(document)?;
	writer.commit()?;
	Ok(())
}

pub fn search_documents(index: &SearchIndex, query_str: &str) -> anyhow::Result<Vec<Document>> {
	Ok(search_topk(index, query_str, None, 10)?
		.into_iter()
		.map(|(d, _)| d)
		.collect())
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

	let reader = &index.reader;
	let searcher = reader.searcher();

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

	let mut results = Vec::new();
	for (score, doc_address) in top_docs {
		let retrieved_doc: TantivyDocument = searcher.doc(doc_address)?;

		let id = retrieved_doc
			.get_first(id_field)
			.and_then(|v| v.as_str().map(String::from))
			.unwrap_or_default();
		let title = retrieved_doc
			.get_first(title_field)
			.and_then(|v| v.as_str().map(String::from))
			.unwrap_or_default();
		let content = retrieved_doc
			.get_first(content_field)
			.and_then(|v| v.as_str().map(String::from))
			.unwrap_or_default();
		let tags: Vec<String> = retrieved_doc
			.get_first(tags_field)
			.and_then(|v| v.as_str().map(String::from))
			.map(|s| s.split_whitespace().map(String::from).collect())
			.unwrap_or_default();

		results.push((Document {
			id,
			title,
			tags,
			purpose: None,
			source_doc_id: None,
			created_at: String::new(),
			updated_at: String::new(),
			content,
		}, score));
	}

	Ok(results)
}
