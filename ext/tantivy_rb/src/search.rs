//! Search execution for `TantivyRb::Index#search`.
//!
//! Supports two query modes:
//!
//! 1. **Default (no `query_tokenizer:`)** — delegates to Tantivy's built-in
//!    `QueryParser`, which supports the standard Tantivy query syntax.
//! 2. **Custom tokenizer (`query_tokenizer: "name"`)** — tokenizes the query
//!    string through the named tokenizer and builds a custom AND-of-ORs query.
//!    Same-position tokens (e.g. stemmed + original) are OR'd as synonyms;
//!    different positions are AND'd. Supports quoted `"phrase"` searches.
//!
//! Both modes support optional field-level term filters via the `filter:` hash.

use crate::index::RbIndex;
use magnus::{prelude::*, r_hash::ForEach, Error, Ruby, RArray, RHash, RString, Symbol, Value};
use tantivy::collector::{Count, MultiCollector, TopDocs};
use tantivy::query::{BooleanQuery, Occur, PhraseQuery, Query, QueryParser, TermQuery};
use tantivy::schema::{IndexRecordOption, OwnedValue};
use tantivy::tokenizer::TokenStream;
use tantivy::TantivyDocument;

/// Execute a search query on the index.
///
/// Ruby signature:
///   index.search(query_string, fields: [...], filter: {}, limit: 20, offset: 0)
///
/// Returns: { total: N, hits: [{ score: F, stored_fields: { ... } }, ...] }
// TODO:: [DEFERRED] Decompose into parse_search_args, execute_query, marshal_results
// Reason: ~177 lines mixing argument parsing, query construction, collection, and marshalling
// See: AMPHTT-730
pub fn execute_search(rb_index: &RbIndex, ruby_args: &[Value]) -> Result<RHash, Error> {
    // SAFETY: This function is only ever called from Ruby via the magnus method!
    // macro, which guarantees execution on a Ruby GVL thread after magnus::init
    // has completed. Ruby::get_unchecked() requires exactly this invariant.
    let ruby = unsafe { Ruby::get_unchecked() };

    if ruby_args.is_empty() {
        return Err(Error::new(
            magnus::exception::arg_error(),
            "search requires at least a query string",
        ));
    }

    let query_string: String = magnus::TryConvert::try_convert(ruby_args[0])?;

    let mut field_names: Vec<String> = Vec::new();
    let mut filter_hash: Option<RHash> = None;
    let mut limit: usize = 20;
    let mut offset: usize = 0;
    let mut query_tokenizer_name: Option<String> = None;

    if ruby_args.len() > 1 {
        if let Ok(kwargs) = <RHash as magnus::TryConvert>::try_convert(ruby_args[1]) {
            if let Some(fields_val) = kwargs.get(Symbol::new("fields")) {
                let arr: RArray = magnus::TryConvert::try_convert(fields_val)?;
                for val in arr.into_iter() {
                    let s: String = magnus::TryConvert::try_convert(val)?;
                    field_names.push(s);
                }
            }
            if let Some(filter_val) = kwargs.get(Symbol::new("filter")) {
                let fh: RHash = magnus::TryConvert::try_convert(filter_val)?;
                filter_hash = Some(fh);
            }
            if let Some(limit_val) = kwargs.get(Symbol::new("limit")) {
                limit = magnus::TryConvert::try_convert(limit_val)?;
            }
            if let Some(offset_val) = kwargs.get(Symbol::new("offset")) {
                offset = magnus::TryConvert::try_convert(offset_val)?;
            }
            if let Some(qt_val) = kwargs.get(Symbol::new("query_tokenizer")) {
                query_tokenizer_name =
                    Some(magnus::TryConvert::try_convert(qt_val)?);
            }
        }
    }

    let schema = rb_index.schema();

    let fields: Vec<tantivy::schema::Field> = if field_names.is_empty() {
        schema
            .fields()
            .filter_map(|(field, entry)| {
                if matches!(entry.field_type(), tantivy::schema::FieldType::Str(_)) {
                    Some(field)
                } else {
                    None
                }
            })
            .collect()
    } else {
        field_names
            .iter()
            .map(|name| {
                schema.get_field(name).map_err(|_| {
                    Error::new(
                        magnus::exception::arg_error(),
                        format!("Unknown field: {}", name),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    let text_query: Box<dyn Query> = if let Some(ref tokenizer_name) = query_tokenizer_name {
        build_tokenized_query(rb_index, &query_string, tokenizer_name, &fields)?
    } else {
        let query_parser = QueryParser::for_index(rb_index.index(), fields.clone());
        query_parser.parse_query(&query_string).map_err(|e| {
            Error::new(
                magnus::exception::arg_error(),
                format!("Failed to parse query '{}': {}", query_string, e),
            )
        })?
    };

    let final_query: Box<dyn Query> = if let Some(fh) = filter_hash {
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        clauses.push((Occur::Must, text_query));

        fh.foreach(|key: Value, value: Value| {
            let field_name: String = magnus::TryConvert::try_convert(key)?;
            let field_value: String = magnus::TryConvert::try_convert(value)?;
            let field = schema.get_field(&field_name).map_err(|_| {
                Error::new(
                    magnus::exception::arg_error(),
                    format!("Unknown filter field: {}", field_name),
                )
            })?;
            let field_entry = schema.get_field_entry(field);
            if !matches!(field_entry.field_type(), tantivy::schema::FieldType::Str(_)) {
                return Err(Error::new(
                    magnus::exception::arg_error(),
                    format!(
                        "Filter field '{}' is not a text field. Only text fields are supported in filter:",
                        field_name
                    ),
                ));
            }
            let term = tantivy::Term::from_field_text(field, &field_value);
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
            ));
            Ok(ForEach::Continue)
        })?;

        Box::new(BooleanQuery::new(clauses))
    } else {
        text_query
    };

    let searcher = rb_index.reader().searcher();

    // Use MultiCollector to gather TopDocs and Count in a single index scan.
    let fetch_count = limit + offset;
    let mut multi = MultiCollector::new();
    let top_docs_handle = multi.add_collector(TopDocs::with_limit(fetch_count));
    let count_handle = multi.add_collector(Count);

    let mut multi_fruit = searcher.search(&*final_query, &multi).map_err(|e| {
        Error::new(
            magnus::exception::runtime_error(),
            format!("Search failed: {}", e),
        )
    })?;

    let top_docs = top_docs_handle.extract(&mut multi_fruit);
    let total = count_handle.extract(&mut multi_fruit);

    let hits = RArray::new();
    for (score, doc_address) in top_docs.into_iter().skip(offset) {
        let doc: TantivyDocument = searcher.doc(doc_address).map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to retrieve doc: {}", e),
            )
        })?;

        let hit = RHash::new();
        hit.aset(
            Symbol::new("score"),
            ruby.float_from_f64(score as f64).as_value(),
        )?;

        let stored = RHash::new();
        for (field, entry) in schema.fields() {
            if !entry.is_stored() {
                continue;
            }
            let field_name = entry.name();
            let values: Vec<&OwnedValue> = doc.get_all(field).collect();
            if values.is_empty() {
                continue;
            }
            if values.len() == 1 {
                let ruby_val = owned_value_to_ruby(values[0], &ruby)?;
                stored.aset(field_name.to_string(), ruby_val)?;
            } else {
                let arr = RArray::new();
                for v in values {
                    arr.push(owned_value_to_ruby(v, &ruby)?)?;
                }
                stored.aset(field_name.to_string(), arr)?;
            }
        }
        hit.aset(Symbol::new("stored_fields"), stored)?;
        hits.push(hit)?;
    }

    let result = RHash::new();
    result.aset(
        Symbol::new("total"),
        ruby.integer_from_i64(total as i64).as_value(),
    )?;
    result.aset(Symbol::new("hits"), hits)?;
    Ok(result)
}

/// A segment of a query string, either a quoted phrase or unquoted terms.
#[derive(Debug, PartialEq)]
enum QuerySegment {
    /// Text between matched double-quotes — becomes a phrase query.
    Phrase(String),
    /// Unquoted text — becomes the standard AND-of-ORs term query.
    Terms(String),
}

/// Parse a query string into segments of quoted phrases and unquoted terms.
///
/// - Matched pairs of `"` delimit phrase segments.
/// - Text outside quotes becomes term segments.
/// - An unmatched trailing `"` is ignored (remainder treated as terms).
/// - Empty phrases (`""`) are skipped.
fn parse_query_segments(query: &str) -> Vec<QuerySegment> {
    let mut segments = Vec::new();
    let mut chars = query.char_indices().peekable();
    let mut current_start = 0;
    let mut in_phrase = false;

    while let Some(&(i, c)) = chars.peek() {
        if c == '"' {
            if in_phrase {
                // Closing quote — emit the phrase
                let phrase_text = &query[current_start..i];
                if !phrase_text.trim().is_empty() {
                    segments.push(QuerySegment::Phrase(phrase_text.to_string()));
                }
                chars.next();
                current_start = i + 1;
                in_phrase = false;
            } else {
                // Opening quote — emit any preceding terms
                let terms_text = &query[current_start..i];
                if !terms_text.trim().is_empty() {
                    segments.push(QuerySegment::Terms(terms_text.to_string()));
                }
                chars.next();
                current_start = i + 1;
                in_phrase = true;
            }
        } else {
            chars.next();
        }
    }

    // Handle remainder after last quote (or entire string if no quotes)
    let remainder = &query[current_start..];
    if !remainder.trim().is_empty() {
        // Unmatched opening quote — treat remainder as terms, not a phrase
        segments.push(QuerySegment::Terms(remainder.to_string()));
    }

    segments
}

/// Build a query by tokenizing the query string with a named tokenizer.
///
/// Supports quoted phrase search: `"exact phrase"` matches terms in order.
/// Unquoted terms use the standard AND-of-ORs behaviour where all terms are
/// required and same-position synonyms (stemmed + original) are OR'd.
///
/// Mixed queries work: `"phrase one" other terms "phrase two"` requires all
/// three clauses (both phrases AND all loose terms).
fn build_tokenized_query(
    rb_index: &RbIndex,
    query_string: &str,
    tokenizer_name: &str,
    fields: &[tantivy::schema::Field],
) -> Result<Box<dyn Query>, Error> {
    let segments = parse_query_segments(query_string);

    // Fast path: no quotes found — single Terms segment, use existing logic.
    if segments.len() == 1 {
        if let QuerySegment::Terms(ref text) = segments[0] {
            return Ok(build_terms_query(rb_index, text, tokenizer_name, fields)?
                .unwrap_or_else(|| Box::new(tantivy::query::EmptyQuery)));
        }
    }

    // Build a clause for each segment and AND them together.
    let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

    for segment in &segments {
        let maybe_query = match segment {
            QuerySegment::Terms(text) => {
                build_terms_query(rb_index, text, tokenizer_name, fields)?
            }
            QuerySegment::Phrase(text) => {
                build_phrase_query(rb_index, text, tokenizer_name, fields)?
            }
        };
        if let Some(q) = maybe_query {
            clauses.push((Occur::Must, q));
        }
    }

    if clauses.is_empty() {
        return Ok(Box::new(tantivy::query::EmptyQuery));
    }
    if clauses.len() == 1 {
        return Ok(clauses.remove(0).1);
    }

    Ok(Box::new(BooleanQuery::new(clauses)))
}

/// Build a phrase query for a quoted segment.
///
/// Tokenizes the phrase text, takes the first (stemmed) token at each position,
/// and builds a PhraseQuery for each searchable field. The per-field phrase
/// queries are OR'd together (the phrase could appear in any field).
///
/// Returns `None` if the tokenizer produces no tokens (e.g. all stop words).
fn build_phrase_query(
    rb_index: &RbIndex,
    phrase_text: &str,
    tokenizer_name: &str,
    fields: &[tantivy::schema::Field],
) -> Result<Option<Box<dyn Query>>, Error> {
    let tokenizer_manager = rb_index.index().tokenizers();
    let mut tokenizer = tokenizer_manager.get(tokenizer_name).ok_or_else(|| {
        Error::new(
            magnus::exception::arg_error(),
            format!("Unknown query tokenizer: '{}'", tokenizer_name),
        )
    })?;

    // Collect tokens grouped by position. We only need the first token at each
    // position (the stemmed form) for the phrase query.
    let mut token_stream = tokenizer.token_stream(phrase_text);
    let mut position_terms: std::collections::BTreeMap<usize, String> =
        std::collections::BTreeMap::new();
    while token_stream.advance() {
        let tok = token_stream.token();
        // Take only the first token at each position (stemmed form).
        position_terms.entry(tok.position).or_insert_with(|| tok.text.clone());
    }

    if position_terms.is_empty() {
        return Ok(None);
    }

    // Single token — fall back to a term query (phrase needs ≥2 terms).
    if position_terms.len() == 1 {
        let token_text = position_terms.into_values().next()
            .expect("len checked to be 1");
        let mut field_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for &field in fields {
            let term = tantivy::Term::from_field_text(field, &token_text);
            field_clauses.push((
                Occur::Should,
                Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)),
            ));
        }
        return Ok(Some(Box::new(BooleanQuery::new(field_clauses))));
    }

    // Build a PhraseQuery per field, OR'd together.
    let ordered_texts: Vec<String> = position_terms.into_values().collect();
    let mut field_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

    for &field in fields {
        let terms: Vec<tantivy::Term> = ordered_texts
            .iter()
            .map(|text| tantivy::Term::from_field_text(field, text))
            .collect();
        field_clauses.push((
            Occur::Should,
            Box::new(PhraseQuery::new(terms)),
        ));
    }

    Ok(Some(Box::new(BooleanQuery::new(field_clauses))))
}

/// Build the standard AND-of-ORs term query for unquoted text.
///
/// Tokens at the **same position** are treated as synonyms (OR'd together),
/// matching how Lucene/Java handles same-position tokens from the analyzer.
/// Tokens at **different positions** are AND'd together, so multi-word queries
/// require all terms. Example: "running experiments" ->
///   Must(run OR running across fields) AND Must(experi OR experiments across fields)
///
/// Returns `None` if the tokenizer produces no tokens (e.g. all stop words).
fn build_terms_query(
    rb_index: &RbIndex,
    query_string: &str,
    tokenizer_name: &str,
    fields: &[tantivy::schema::Field],
) -> Result<Option<Box<dyn Query>>, Error> {
    let tokenizer_manager = rb_index.index().tokenizers();
    let mut tokenizer = tokenizer_manager.get(tokenizer_name).ok_or_else(|| {
        Error::new(
            magnus::exception::arg_error(),
            format!("Unknown query tokenizer: '{}'", tokenizer_name),
        )
    })?;

    // Collect tokens with their positions so we can group synonyms.
    let mut token_stream = tokenizer.token_stream(query_string);
    let mut positioned_tokens: Vec<(usize, String)> = Vec::new();
    while token_stream.advance() {
        let tok = token_stream.token();
        positioned_tokens.push((tok.position, tok.text.clone()));
    }

    if positioned_tokens.is_empty() {
        return Ok(None);
    }

    // Group tokens by position. Same-position tokens are synonyms (OR).
    // Use a BTreeMap so positions are processed in order.
    let mut position_groups: std::collections::BTreeMap<usize, Vec<String>> =
        std::collections::BTreeMap::new();
    for (pos, text) in positioned_tokens {
        position_groups.entry(pos).or_default().push(text);
    }

    // For each position group, create: Must(synonym1_in_field1 OR synonym1_in_field2 OR synonym2_in_field1 ...)
    // All synonyms at the same position are OR'd across all fields.
    // Different positions are AND'd together.
    let mut position_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
    for synonyms in position_groups.values() {
        let mut field_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for token_text in synonyms {
            for &field in fields {
                let term = tantivy::Term::from_field_text(field, token_text);
                field_clauses.push((
                    Occur::Should,
                    Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)),
                ));
            }
        }
        position_clauses.push((Occur::Must, Box::new(BooleanQuery::new(field_clauses))));
    }

    Ok(Some(Box::new(BooleanQuery::new(position_clauses))))
}

/// Convert a Tantivy `OwnedValue` (from a stored field) into a Ruby `Value`.
///
/// Mapping:
/// - `Str` → Ruby `String`
/// - `U64` → Ruby `Integer` (cast to i64)
/// - `I64` → Ruby `Integer`
/// - `F64` → Ruby `Float`
/// - `Date` → Ruby `Integer` (Unix timestamp in seconds)
/// - `Bool` → Ruby `true`/`false`
/// - Everything else → `nil`
fn owned_value_to_ruby(val: &OwnedValue, ruby: &Ruby) -> Result<Value, Error> {
    Ok(match val {
        OwnedValue::Str(s) => RString::new(s).as_value(),
        OwnedValue::U64(n) => {
            match i64::try_from(*n) {
                Ok(i) => ruby.integer_from_i64(i).as_value(),
                Err(_) => RString::new(&n.to_string()).as_value(),
            }
        }
        OwnedValue::I64(n) => ruby.integer_from_i64(*n).as_value(),
        OwnedValue::F64(n) => ruby.float_from_f64(*n).as_value(),
        OwnedValue::Date(dt) => {
            let ts = dt.into_timestamp_secs();
            ruby.integer_from_i64(ts).as_value()
        }
        OwnedValue::Bool(b) => {
            if *b {
                ruby.qtrue().as_value()
            } else {
                ruby.qfalse().as_value()
            }
        }
        _ => ruby.qnil().as_value(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_quotes() {
        let segments = parse_query_segments("hello world");
        assert_eq!(segments, vec![QuerySegment::Terms("hello world".into())]);
    }

    #[test]
    fn test_single_phrase() {
        let segments = parse_query_segments("\"exact phrase\"");
        assert_eq!(segments, vec![QuerySegment::Phrase("exact phrase".into())]);
    }

    #[test]
    fn test_phrase_with_trailing_terms() {
        let segments = parse_query_segments("\"exact phrase\" other terms");
        assert_eq!(segments, vec![
            QuerySegment::Phrase("exact phrase".into()),
            QuerySegment::Terms(" other terms".into()),
        ]);
    }

    #[test]
    fn test_phrase_with_leading_terms() {
        let segments = parse_query_segments("other terms \"exact phrase\"");
        assert_eq!(segments, vec![
            QuerySegment::Terms("other terms ".into()),
            QuerySegment::Phrase("exact phrase".into()),
        ]);
    }

    #[test]
    fn test_mixed_phrases_and_terms() {
        let segments = parse_query_segments("\"phrase one\" middle \"phrase two\"");
        assert_eq!(segments, vec![
            QuerySegment::Phrase("phrase one".into()),
            QuerySegment::Terms(" middle ".into()),
            QuerySegment::Phrase("phrase two".into()),
        ]);
    }

    #[test]
    fn test_unmatched_quote_treated_as_terms() {
        let segments = parse_query_segments("\"unmatched quote");
        assert_eq!(segments, vec![QuerySegment::Terms("unmatched quote".into())]);
    }

    #[test]
    fn test_empty_quotes_skipped() {
        let segments = parse_query_segments("\"\" hello");
        assert_eq!(segments, vec![QuerySegment::Terms(" hello".into())]);
    }

    #[test]
    fn test_only_empty_quotes() {
        let segments = parse_query_segments("\"\"");
        assert_eq!(segments, vec![] as Vec<QuerySegment>);
    }

    #[test]
    fn test_multiple_phrases_no_terms() {
        let segments = parse_query_segments("\"first phrase\" \"second phrase\"");
        assert_eq!(segments, vec![
            QuerySegment::Phrase("first phrase".into()),
            QuerySegment::Phrase("second phrase".into()),
        ]);
    }

    #[test]
    fn test_real_world_query() {
        let segments = parse_query_segments(
            "\"Cryptographic Controls and Key Management Policy\""
        );
        assert_eq!(segments, vec![
            QuerySegment::Phrase("Cryptographic Controls and Key Management Policy".into()),
        ]);
    }

    // TODO:: [DEFERRED] Add Ruby-dependent unit tests (requires magnus::embed or Ruby linking)
    // Targets: build_terms_query, build_phrase_query, owned_value_to_ruby, filter-clause
    // construction
    // Reason: These functions take &RbIndex which is #[magnus::wrap]-annotated. Constructing
    // RbIndex in tests causes linker errors due to unresolved Ruby symbols. Needs either
    // embed feature flag or a refactor to accept &Index instead of &RbIndex.
    // Scope: 3
    // See: AMPHTT-731
}
