//! Learn pass: link, dedupe, raise questions, QA, promote, feedback.
//! Split into submodules; external API preserved via re-exports below.

pub(crate) mod infra;
pub(crate) mod dedup;
pub(crate) mod links;
pub(crate) mod raise;
pub(crate) mod connect;
pub(crate) mod qa;
pub(crate) mod promote;
pub(crate) mod migrate;
pub(crate) mod feedback;
pub(crate) mod code_links;

pub use infra::{EntityRef, PassConfig};
pub(crate) use infra::read_reason_meta as infra_read_reason_meta;
pub use dedup::find_near_duplicate_entity;
pub use links::link_doc;
pub use links::bury_question;
pub use links::delete_question_with_edges;
pub use links::repoint_inbound_to_conclusion;
pub use qa::run_pass;
pub use feedback::run_feedback_pass;
pub use migrate::migrate_templated_questions;
pub use migrate::migrate_question_lifecycle;
pub use raise::raise_question_from_search_miss;
#[allow(unused_imports)]
pub use promote::cross_topic_pass;
