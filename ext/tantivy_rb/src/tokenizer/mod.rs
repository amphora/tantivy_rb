//! Custom tokenizer registration for Tantivy indexes.
//!
//! Three tokenizer types are available:
//!
//! - `:default` — standard pipeline (whitespace, ASCII fold, lowercase, stop
//!   words, stemmer). Good for general-purpose text.
//! - `:raw` — no tokenization at all. Used for exact-match fields like IDs.
//! - `:compound` — the PatentSafe-specific pipeline with WORD/COMPLEX
//!   classification. Has two modes: `:index` (full expansion) and `:query`
//!   (simplified for search input).

pub mod compound;
pub mod default;

use magnus::{Error, RHash, Symbol};
use tantivy::Index;

/// Register a tokenizer on the given index.
///
/// Ruby: index.register_tokenizer("name", type: :default, stemmer: :english, ...)
pub fn register_tokenizer(index: &Index, name: String, kwargs: RHash) -> Result<(), Error> {
    let type_sym: Symbol = kwargs
        .get(Symbol::new("type"))
        .ok_or_else(|| {
            Error::new(
                magnus::exception::arg_error(),
                "missing keyword argument: type",
            )
        })
        .and_then(|v| magnus::TryConvert::try_convert(v))?;

    let type_name: String = type_sym
        .name()
        .map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to get symbol name: {}", e),
            )
        })?
        .to_string();

    match type_name.as_str() {
        "default" => default::register(index, &name, &kwargs),
        "raw" => {
            index
                .tokenizers()
                .register(&name, tantivy::tokenizer::RawTokenizer::default());
            Ok(())
        }
        "compound" => compound::register(index, &name, &kwargs),
        _ => Err(Error::new(
            magnus::exception::arg_error(),
            format!(
                "Unknown tokenizer type '{}'. Valid types: default, raw, compound",
                type_name
            ),
        )),
    }
}
