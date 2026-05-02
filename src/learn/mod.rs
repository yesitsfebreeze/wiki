//! Learn pass: link, dedupe, raise questions, QA, promote, feedback.
//! Split into submodules; external API preserved via re-exports below.

pub(crate) mod infra;
pub(crate) mod dedup;
pub(crate) mod links;
pub(crate) mod raise;
pub(crate) mod qa;
pub(crate) mod promote;
pub(crate) mod migrate;
pub(crate) mod feedback;

pub use infra::{EntityRef, PassConfig};
pub use dedup::find_near_duplicate_entity;
pub use links::link_doc;
pub use qa::run_pass;
pub use feedback::run_feedback_pass;
pub use migrate::migrate_templated_questions;
#[allow(unused_imports)]
pub use promote::cross_topic_pass;
