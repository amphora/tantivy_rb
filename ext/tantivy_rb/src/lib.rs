//! Ruby bindings for the Tantivy full-text search engine.
//!
//! This crate exposes two Ruby classes under the `TantivyRb` module:
//!
//! - `TantivyRb::Schema` — builds a Tantivy schema from Ruby, supporting text,
//!   numeric, and date fields with per-field options (stored, tokenizer, fast).
//! - `TantivyRb::Index` — opens or creates a Tantivy index on disk and provides
//!   document indexing, deletion, commit/reload, search, and custom tokenizer
//!   registration.
//!
//! The crate also includes custom tokenizer pipelines (`tokenizer` module) that
//! replicate the Java PatentSafe search analysers, including a compound tokenizer
//! with WORD/COMPLEX classification and n-gram expansion for technical documents.

mod index;
mod schema;
mod search;
mod tokenizer;

use magnus::{define_module, Error};

/// Magnus init entry point — called when Ruby loads the native extension.
///
/// Defines the top-level `TantivyRb` module and registers the `Schema` and
/// `Index` classes with their methods.
#[magnus::init]
fn init() -> Result<(), Error> {
    let module = define_module("TantivyRb")?;
    schema::init(module)?;
    index::init(module)?;
    Ok(())
}
