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

use crate::index::{parse_date, RbIndex};
use magnus::{prelude::*, r_hash::ForEach, Error, RArray, RHash, RString, Ruby, Symbol, Value};
use std::ops::Bound;
use tantivy::collector::{Count, MultiCollector, TopDocs};
use tantivy::query::{
    BooleanQuery, Occur, PhraseQuery, Query, QueryParser, RangeQuery, RegexQuery, TermQuery,
};
use tantivy::schema::{IndexRecordOption, OwnedValue, Schema};
use tantivy::tokenizer::TokenStream;
use tantivy::{DocAddress, Searcher, TantivyDocument};

/// Parsed search arguments extracted from Ruby kwargs.
struct SearchArgs {
    query_string: String,
    field_names: Vec<String>,
    filter_hash: Option<RHash>,
    limit: usize,
    offset: usize,
    query_tokenizer_name: Option<String>,
}

/// Parse Ruby arguments into a `SearchArgs` struct.
///
/// Expects at least one positional argument (the query string), with an optional
/// kwargs hash containing `:fields`, `:filter`, `:limit`, `:offset`, and
/// `:query_tokenizer`.
fn parse_search_args(ruby_args: &[Value]) -> Result<SearchArgs, Error> {
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
                query_tokenizer_name = Some(magnus::TryConvert::try_convert(qt_val)?);
            }
        }
    }

    Ok(SearchArgs {
        query_string,
        field_names,
        filter_hash,
        limit,
        offset,
        query_tokenizer_name,
    })
}

/// Resolve field names to Tantivy `Field` objects.
///
/// If `field_names` is empty, returns all text fields from the schema.
/// Otherwise, looks up each name and returns an error for unknown fields.
fn resolve_fields(
    schema: &Schema,
    field_names: &[String],
) -> Result<Vec<tantivy::schema::Field>, Error> {
    if field_names.is_empty() {
        Ok(schema
            .fields()
            .filter_map(|(field, entry)| {
                if matches!(entry.field_type(), tantivy::schema::FieldType::Str(_)) {
                    Some(field)
                } else {
                    None
                }
            })
            .collect())
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
            .collect::<Result<Vec<_>, _>>()
    }
}

/// Execute a search query on the index.
///
/// Ruby signature:
///   index.search(query_string, fields: [...], filter: {}, limit: 20, offset: 0)
///
/// Returns: { total: N, hits: [{ score: F, stored_fields: { ... } }, ...] }
pub fn execute_search(rb_index: &RbIndex, ruby_args: &[Value]) -> Result<RHash, Error> {
    let args = parse_search_args(ruby_args)?;
    let fields = resolve_fields(rb_index.schema(), &args.field_names)?;
    let query = build_full_query(rb_index, &args, &fields)?;
    let searcher = rb_index.reader().searcher();
    let (scored_docs, total) = collect_search_results(&searcher, &*query, args.limit, args.offset)?;
    marshal_results(rb_index.schema(), &searcher, &scored_docs, total)
}

/// Build the complete search query: text query + optional filter clauses.
///
/// Constructs the text query using either the built-in `QueryParser` or a custom
/// tokenizer (if `query_tokenizer` was specified), then wraps it with filter
/// term clauses if a `filter:` hash was provided.
fn build_full_query(
    rb_index: &RbIndex,
    args: &SearchArgs,
    fields: &[tantivy::schema::Field],
) -> Result<Box<dyn Query>, Error> {
    let schema = rb_index.schema();

    let text_query: Box<dyn Query> = if let Some(ref tokenizer_name) = args.query_tokenizer_name {
        build_tokenized_query(rb_index, &args.query_string, tokenizer_name, fields)?
    } else {
        let query_parser = QueryParser::for_index(rb_index.index(), fields.to_vec());
        query_parser.parse_query(&args.query_string).map_err(|e| {
            Error::new(
                magnus::exception::arg_error(),
                format!("Failed to parse query '{}': {}", args.query_string, e),
            )
        })?
    };

    if let Some(ref fh) = args.filter_hash {
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        clauses.push((Occur::Must, text_query));

        fh.foreach(|key: Value, value: Value| {
            let field_name: String = magnus::TryConvert::try_convert(key)?;
            if let Some(clause) = build_filter_clause(schema, &field_name, value)? {
                clauses.push((Occur::Must, clause));
            }
            Ok(ForEach::Continue)
        })?;

        Ok(Box::new(BooleanQuery::new(clauses)))
    } else {
        Ok(text_query)
    }
}

/// Build a single filter clause for one `(field_name, value)` entry in the `filter:` hash.
///
/// Dispatches on the Ruby value type:
/// - `Array` → OR-joined `TermQuery` clauses via a Should-BooleanQuery.
/// - `Hash` with `:gte`/`:gt`/`:lte`/`:lt` keys → `RangeQuery` on a date field.
/// - (Future commits) `Hash` with `:prefix` key → prefix query.
/// - `String` → exact-match `TermQuery` on a text field.
///
/// Returns `Ok(None)` when the entry is a no-op (e.g. empty array, range with no
/// bounds). The caller should skip — no clause is added to the outer
/// BooleanQuery.
///
/// Field-type validation (e.g. "text field only" for term filters, "date field
/// only" for range filters) is performed inside this function.
fn build_filter_clause(
    schema: &Schema,
    field_name: &str,
    value: Value,
) -> Result<Option<Box<dyn Query>>, Error> {
    let field = schema.get_field(field_name).map_err(|_| {
        Error::new(
            magnus::exception::arg_error(),
            format!("Unknown filter field: {}", field_name),
        )
    })?;

    // Array-valued filter → OR of TermQueries. Checked before String so an
    // accidental Array input can't be stringified.
    if let Ok(arr) = <RArray as magnus::TryConvert>::try_convert(value) {
        return build_array_filter(schema, field, field_name, arr);
    }

    // Hash-valued filter → range or prefix query, dispatched on inner keys.
    if let Ok(hash) = <RHash as magnus::TryConvert>::try_convert(value) {
        return build_hash_filter(schema, field, field_name, hash);
    }

    // String-valued filter → exact-match TermQuery (existing behaviour).
    if let Ok(s) = <String as magnus::TryConvert>::try_convert(value) {
        return build_term_filter(schema, field, field_name, &s).map(Some);
    }

    Err(Error::new(
        magnus::exception::arg_error(),
        format!(
            "Unsupported filter value for field '{}': expected a String, Array, or Hash",
            field_name
        ),
    ))
}

/// Dispatch a Hash-valued filter by inspecting its keys.
///
/// Accepted key sets:
/// - Any of `:gte`, `:gt`, `:lte`, `:lt` → range query (date fields only).
/// - `:prefix` → prefix query (raw-tokenized text fields only).
///
/// Unknown or empty key sets raise a descriptive error listing the accepted
/// keys, so a typo like `{ "grt" => "..." }` fails fast rather than silently
/// producing an empty result set.
fn build_hash_filter(
    schema: &Schema,
    field: tantivy::schema::Field,
    field_name: &str,
    hash: RHash,
) -> Result<Option<Box<dyn Query>>, Error> {
    let has_prefix = hash.get(Symbol::new("prefix")).is_some();
    let has_gte = hash.get(Symbol::new("gte")).is_some();
    let has_gt = hash.get(Symbol::new("gt")).is_some();
    let has_lte = hash.get(Symbol::new("lte")).is_some();
    let has_lt = hash.get(Symbol::new("lt")).is_some();

    if has_prefix {
        return build_prefix_filter(schema, field, field_name, &hash);
    }

    if has_gte || has_gt || has_lte || has_lt {
        return build_range_filter(schema, field, field_name, &hash);
    }

    Err(Error::new(
        magnus::exception::arg_error(),
        format!(
            "Unsupported filter hash for field '{}' — expected :prefix, :gte, :gt, :lte, or :lt",
            field_name
        ),
    ))
}

/// Build an exact-match `TermQuery` for a text field.
fn build_term_filter(
    schema: &Schema,
    field: tantivy::schema::Field,
    field_name: &str,
    value: &str,
) -> Result<Box<dyn Query>, Error> {
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
    let term = tantivy::Term::from_field_text(field, value);
    Ok(Box::new(TermQuery::new(term, IndexRecordOption::Basic)))
}

/// Build an OR-joined filter from a Ruby Array of string values.
///
/// Each element becomes a `TermQuery` joined with `Occur::Should` inside a
/// `BooleanQuery`, so a document matches if ANY of the listed terms matches.
/// The outer filter loop then joins this (as `Must`) with the text query and
/// other filter clauses, yielding `text AND (term1 OR term2 OR ...)`.
///
/// Degenerate cases:
/// - Empty array → `Ok(None)` (no clause added — equivalent to "no filter on
///   this field"). This lets callers pass an empty multi-select without
///   special-casing at the call site.
/// - Single-element array → behaves identically to a bare string value.
fn build_array_filter(
    schema: &Schema,
    field: tantivy::schema::Field,
    field_name: &str,
    arr: RArray,
) -> Result<Option<Box<dyn Query>>, Error> {
    let field_entry = schema.get_field_entry(field);
    if !matches!(field_entry.field_type(), tantivy::schema::FieldType::Str(_)) {
        return Err(Error::new(
            magnus::exception::arg_error(),
            format!(
                "Filter field '{}' is not a text field. Only text fields are supported in array filter:",
                field_name
            ),
        ));
    }

    if arr.is_empty() {
        return Ok(None);
    }

    let mut term_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::with_capacity(arr.len());
    for element in arr.into_iter() {
        let s: String = magnus::TryConvert::try_convert(element).map_err(|_| {
            Error::new(
                magnus::exception::arg_error(),
                format!(
                    "Array filter for field '{}' contains a non-String element",
                    field_name
                ),
            )
        })?;
        let term = tantivy::Term::from_field_text(field, &s);
        term_clauses.push((
            Occur::Should,
            Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
        ));
    }

    Ok(Some(Box::new(BooleanQuery::new(term_clauses))))
}

/// Build a `RangeQuery` from a Hash with `:gte` / `:gt` / `:lte` / `:lt` keys.
///
/// Currently restricted to date fields — the only range-queryable field in the
/// PatentSafe schema is `created_at`. Numeric range support can be added later
/// when a caller arrives; the value-parsing surface (integer vs float vs date)
/// is deliberately kept small for now.
///
/// Conflicting bound combinations (both `:gte` and `:gt`, or both `:lte` and
/// `:lt`) are rejected: they're almost always a caller bug, and silently
/// picking one would mask the mistake.
///
/// Range bounds use `Term::from_field_date_for_search`, which truncates the
/// DateTime to the precision used at index time. The bare `Term::from_field_date`
/// (used when adding documents) encodes at microsecond precision and does NOT
/// match the indexed term at search time.
fn build_range_filter(
    schema: &Schema,
    field: tantivy::schema::Field,
    field_name: &str,
    hash: &RHash,
) -> Result<Option<Box<dyn Query>>, Error> {
    let field_entry = schema.get_field_entry(field);
    if !matches!(
        field_entry.field_type(),
        tantivy::schema::FieldType::Date(_)
    ) {
        return Err(Error::new(
            magnus::exception::arg_error(),
            format!(
                "Range filter on field '{}' requires a date field",
                field_name
            ),
        ));
    }

    let gte = parse_date_bound(hash, "gte", field_name)?;
    let gt = parse_date_bound(hash, "gt", field_name)?;
    let lte = parse_date_bound(hash, "lte", field_name)?;
    let lt = parse_date_bound(hash, "lt", field_name)?;

    if gte.is_some() && gt.is_some() {
        return Err(Error::new(
            magnus::exception::arg_error(),
            format!(
                "Range filter for field '{}' cannot specify both :gte and :gt",
                field_name
            ),
        ));
    }
    if lte.is_some() && lt.is_some() {
        return Err(Error::new(
            magnus::exception::arg_error(),
            format!(
                "Range filter for field '{}' cannot specify both :lte and :lt",
                field_name
            ),
        ));
    }

    let lower = match (gte, gt) {
        (Some(v), None) => Bound::Included(tantivy::Term::from_field_date_for_search(field, v)),
        (None, Some(v)) => Bound::Excluded(tantivy::Term::from_field_date_for_search(field, v)),
        (None, None) => Bound::Unbounded,
        (Some(_), Some(_)) => unreachable!("guarded above"),
    };
    let upper = match (lte, lt) {
        (Some(v), None) => Bound::Included(tantivy::Term::from_field_date_for_search(field, v)),
        (None, Some(v)) => Bound::Excluded(tantivy::Term::from_field_date_for_search(field, v)),
        (None, None) => Bound::Unbounded,
        (Some(_), Some(_)) => unreachable!("guarded above"),
    };

    // Unreachable: build_hash_filter only dispatches here when at least one of
    // :gte/:gt/:lte/:lt is present, so one of the bounds above will be bounded.
    // Kept as a defensive guard; a Bound::Unbounded pair would construct a
    // match-everything RangeQuery that's both semantically wrong and expensive.
    debug_assert!(
        !matches!(lower, Bound::Unbounded) || !matches!(upper, Bound::Unbounded),
        "build_range_filter called with no bounds — build_hash_filter dispatch is broken",
    );

    Ok(Some(Box::new(RangeQuery::new(lower, upper))))
}

/// Read a date-valued bound from the hash under the given symbol key.
///
/// Returns `None` if the key is absent, `Some(DateTime)` if present and parseable,
/// or an Error with field context if the value isn't a String or fails to parse.
fn parse_date_bound(
    hash: &RHash,
    key: &str,
    field_name: &str,
) -> Result<Option<tantivy::DateTime>, Error> {
    let Some(v) = hash.get(Symbol::new(key)) else {
        return Ok(None);
    };
    let s: String = magnus::TryConvert::try_convert(v).map_err(|_| {
        Error::new(
            magnus::exception::arg_error(),
            format!(
                "Range filter for field '{}' expects a String for :{} bound",
                field_name, key
            ),
        )
    })?;
    parse_date(&s).map(Some)
}

/// Build a prefix-match filter from a Hash with a `:prefix` key.
///
/// Produces a `RegexQuery` of the shape `escaped_prefix.*`. The tantivy_fst
/// regex engine matches the WHOLE token by default (implicit anchoring), so
/// `"EXP\\-2026.*"` matches any token that starts with `"EXP-2026"` — exactly
/// the prefix semantics we want.
///
/// Restricted to text fields with the `"raw"` tokenizer. On a tokenized field
/// (e.g. `label`, which uses `ps_index`), the stored tokens are per-word, not
/// per-document-id, and a prefix query would match any word starting with the
/// prefix — confusing and rarely what the caller means. Rather than silently
/// producing odd results, reject with a clear error.
///
/// Edge cases:
/// - Empty prefix → `Ok(None)` (no clause added — equivalent to "no filter").
/// - Non-String value for `:prefix` → ArgumentError.
fn build_prefix_filter(
    schema: &Schema,
    field: tantivy::schema::Field,
    field_name: &str,
    hash: &RHash,
) -> Result<Option<Box<dyn Query>>, Error> {
    let field_entry = schema.get_field_entry(field);
    let tokenizer = match field_entry.field_type() {
        tantivy::schema::FieldType::Str(text_opts) => text_opts
            .get_indexing_options()
            .map(|opts| opts.tokenizer()),
        _ => {
            return Err(Error::new(
                magnus::exception::arg_error(),
                format!(
                    "Prefix filter on field '{}' requires a text field",
                    field_name
                ),
            ));
        }
    };
    if tokenizer != Some("raw") {
        return Err(Error::new(
            magnus::exception::arg_error(),
            format!(
                "Prefix filter on field '{}' requires the 'raw' tokenizer",
                field_name
            ),
        ));
    }

    // Invariant: build_hash_filter only dispatches here when :prefix is present.
    let prefix_val = hash
        .get(Symbol::new("prefix"))
        .expect("build_prefix_filter requires :prefix key");
    let prefix: String = magnus::TryConvert::try_convert(prefix_val).map_err(|_| {
        Error::new(
            magnus::exception::arg_error(),
            format!(
                "Prefix filter for field '{}' expects a String for :prefix",
                field_name
            ),
        )
    })?;

    if prefix.is_empty() {
        return Ok(None);
    }

    let pattern = format!("{}.*", escape_prefix_to_regex(&prefix));
    let query = RegexQuery::from_pattern(&pattern, field).map_err(|e| {
        Error::new(
            magnus::exception::runtime_error(),
            format!(
                "Failed to build prefix RegexQuery for field '{}': {}",
                field_name, e
            ),
        )
    })?;
    Ok(Some(Box::new(query)))
}

/// Escape regex metacharacters in a prefix string so it matches literally.
///
/// PatentSafe IDs contain characters like `-` that are not regex metacharacters
/// in most dialects but ARE in the tantivy_fst regex grammar (inside character
/// classes). To be safe and consistent with well-known regex escape semantics,
/// escape every character that has special meaning in standard regex syntax:
///
///   \  .  +  *  ?  (  )  |  [  ]  {  }  ^  $  #  &  -  ~
///
/// This matches the set used by `regex_syntax::escape_into`, so behaviour is
/// predictable for callers familiar with Rust's `regex` crate.
///
/// `RegexQuery` in tantivy 0.24 compiles the pattern through `tantivy_fst::Regex`,
/// which accepts a subset of `regex` syntax; the escape set above is a superset
/// of what `tantivy_fst` treats as special, so all metachars are covered. If the
/// tantivy / tantivy_fst version changes and introduces new metacharacters, this
/// set must be re-verified.
fn escape_prefix_to_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '.'
                | '+'
                | '*'
                | '?'
                | '('
                | ')'
                | '|'
                | '['
                | ']'
                | '{'
                | '}'
                | '^'
                | '$'
                | '#'
                | '&'
                | '-'
                | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Execute the search query and collect scored documents + total count.
///
/// Uses `MultiCollector` to gather `TopDocs` and `Count` in a single index scan.
/// The `offset` is applied by fetching `limit + offset` results and skipping the
/// first `offset` entries.
fn collect_search_results(
    searcher: &Searcher,
    query: &dyn Query,
    limit: usize,
    offset: usize,
) -> Result<(Vec<(f32, DocAddress)>, usize), Error> {
    let fetch_count = limit + offset;
    let mut multi = MultiCollector::new();
    let top_docs_handle = multi.add_collector(TopDocs::with_limit(fetch_count));
    let count_handle = multi.add_collector(Count);

    let mut multi_fruit = searcher.search(query, &multi).map_err(|e| {
        Error::new(
            magnus::exception::runtime_error(),
            format!("Search failed: {}", e),
        )
    })?;

    let top_docs = top_docs_handle.extract(&mut multi_fruit);
    let total = count_handle.extract(&mut multi_fruit);

    let scored_docs: Vec<(f32, DocAddress)> = top_docs.into_iter().skip(offset).collect();
    Ok((scored_docs, total))
}

/// Convert search results into a Ruby hash with `:total` and `:hits` keys.
///
/// Each hit contains `:score` (float) and `:stored_fields` (hash of field
/// name -> value for all stored fields in the document).
fn marshal_results(
    schema: &Schema,
    searcher: &Searcher,
    scored_docs: &[(f32, DocAddress)],
    total: usize,
) -> Result<RHash, Error> {
    // SAFETY: This function is only ever called from Ruby via the magnus method!
    // macro, which guarantees execution on a Ruby GVL thread after magnus::init
    // has completed. Ruby::get_unchecked() requires exactly this invariant.
    let ruby = unsafe { Ruby::get_unchecked() };

    let hits = RArray::new();
    for &(score, doc_address) in scored_docs {
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
            let values: Vec<OwnedValue> = doc.get_all(field).map(OwnedValue::from).collect();
            if values.is_empty() {
                continue;
            }
            if values.len() == 1 {
                let ruby_val = owned_value_to_ruby(&values[0], &ruby)?;
                stored.aset(field_name.to_string(), ruby_val)?;
            } else {
                let arr = RArray::new();
                for v in &values {
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
            QuerySegment::Terms(text) => build_terms_query(rb_index, text, tokenizer_name, fields)?,
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
        position_terms
            .entry(tok.position)
            .or_insert_with(|| tok.text.clone());
    }

    if position_terms.is_empty() {
        return Ok(None);
    }

    // Single token — fall back to a term query (phrase needs ≥2 terms).
    if position_terms.len() == 1 {
        if let Some(token_text) = position_terms.into_values().next() {
            let mut field_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
            for &field in fields {
                let term = tantivy::Term::from_field_text(field, &token_text);
                field_clauses.push((
                    Occur::Should,
                    Box::new(TermQuery::new(term, IndexRecordOption::WithFreqs)),
                ));
            }
            return Ok(Some(Box::new(BooleanQuery::new(field_clauses))));
        } else {
            return Ok(None);
        }
    }

    // Build a PhraseQuery per field, OR'd together.
    let ordered_texts: Vec<String> = position_terms.into_values().collect();
    let mut field_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

    for &field in fields {
        let terms: Vec<tantivy::Term> = ordered_texts
            .iter()
            .map(|text| tantivy::Term::from_field_text(field, text))
            .collect();
        field_clauses.push((Occur::Should, Box::new(PhraseQuery::new(terms))));
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
        OwnedValue::U64(n) => match i64::try_from(*n) {
            Ok(i) => ruby.integer_from_i64(i).as_value(),
            Err(_) => RString::new(&n.to_string()).as_value(),
        },
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
        assert_eq!(
            segments,
            vec![
                QuerySegment::Phrase("exact phrase".into()),
                QuerySegment::Terms(" other terms".into()),
            ]
        );
    }

    #[test]
    fn test_phrase_with_leading_terms() {
        let segments = parse_query_segments("other terms \"exact phrase\"");
        assert_eq!(
            segments,
            vec![
                QuerySegment::Terms("other terms ".into()),
                QuerySegment::Phrase("exact phrase".into()),
            ]
        );
    }

    #[test]
    fn test_mixed_phrases_and_terms() {
        let segments = parse_query_segments("\"phrase one\" middle \"phrase two\"");
        assert_eq!(
            segments,
            vec![
                QuerySegment::Phrase("phrase one".into()),
                QuerySegment::Terms(" middle ".into()),
                QuerySegment::Phrase("phrase two".into()),
            ]
        );
    }

    #[test]
    fn test_unmatched_quote_treated_as_terms() {
        let segments = parse_query_segments("\"unmatched quote");
        assert_eq!(
            segments,
            vec![QuerySegment::Terms("unmatched quote".into())]
        );
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
        assert_eq!(
            segments,
            vec![
                QuerySegment::Phrase("first phrase".into()),
                QuerySegment::Phrase("second phrase".into()),
            ]
        );
    }

    #[test]
    fn test_real_world_query() {
        let segments = parse_query_segments("\"Cryptographic Controls and Key Management Policy\"");
        assert_eq!(
            segments,
            vec![QuerySegment::Phrase(
                "Cryptographic Controls and Key Management Policy".into()
            ),]
        );
    }

    #[test]
    fn test_escape_prefix_alphanumeric_noop() {
        assert_eq!(escape_prefix_to_regex("EXP2026"), "EXP2026");
        assert_eq!(escape_prefix_to_regex("abcXYZ123"), "abcXYZ123");
    }

    #[test]
    fn test_escape_prefix_patentsafe_id() {
        assert_eq!(escape_prefix_to_regex("EXP-2026"), "EXP\\-2026");
        assert_eq!(escape_prefix_to_regex("DEVC01-000"), "DEVC01\\-000");
    }

    #[test]
    fn test_escape_prefix_dot_escaped() {
        assert_eq!(escape_prefix_to_regex("v1.2"), "v1\\.2");
    }

    #[test]
    fn test_escape_prefix_all_regex_metacharacters() {
        // Every metachar gets a leading backslash.
        assert_eq!(
            escape_prefix_to_regex(".+*?()|[]{}^$#&-~"),
            "\\.\\+\\*\\?\\(\\)\\|\\[\\]\\{\\}\\^\\$\\#\\&\\-\\~"
        );
    }

    #[test]
    fn test_escape_prefix_backslash_escaped() {
        assert_eq!(escape_prefix_to_regex("a\\b"), "a\\\\b");
    }

    #[test]
    fn test_escape_prefix_empty_string() {
        assert_eq!(escape_prefix_to_regex(""), "");
    }

    #[test]
    fn test_escape_prefix_preserves_non_ascii() {
        // Non-ASCII characters like é or 中 have no regex meaning and pass through.
        assert_eq!(escape_prefix_to_regex("café"), "café");
        assert_eq!(escape_prefix_to_regex("中文"), "中文");
    }

    // TODO:: [DEFERRED] Add Ruby-dependent unit tests (requires magnus::embed or Ruby linking)
    // Targets: parse_search_args, resolve_fields, build_full_query, collect_search_results,
    // marshal_results, build_terms_query, build_phrase_query, owned_value_to_ruby
    // Reason: These functions use Magnus types (RHash, Value, Error) or take &RbIndex which is
    // #[magnus::wrap]-annotated. Constructing these in tests causes linker errors due to
    // unresolved Ruby symbols. Needs either embed feature flag or a refactor to accept plain
    // Tantivy types instead of Magnus wrappers.
    // Scope: 3
    // See: AMPHTT-731
}
