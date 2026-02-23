use crate::index::RbIndex;
use magnus::{prelude::*, r_hash::ForEach, Error, Ruby, RArray, RHash, RString, Symbol, Value};
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{IndexRecordOption, OwnedValue};
use tantivy::tokenizer::TokenStream;
use tantivy::TantivyDocument;

/// Execute a search query on the index.
///
/// Ruby signature:
///   index.search(query_string, fields: [...], filter: {}, limit: 20, offset: 0)
///
/// Returns: { total: N, hits: [{ score: F, stored_fields: { ... } }, ...] }
pub fn execute_search(rb_index: &RbIndex, ruby_args: &[Value]) -> Result<RHash, Error> {
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

    let reader_guard = rb_index.reader().lock().map_err(|e| {
        Error::new(
            magnus::exception::runtime_error(),
            format!("Failed to lock reader: {}", e),
        )
    })?;
    let searcher = reader_guard.searcher();

    let fetch_count = limit + offset;
    let top_docs = searcher
        .search(&*final_query, &TopDocs::with_limit(fetch_count))
        .map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Search failed: {}", e),
            )
        })?;

    let total = searcher.search(&*final_query, &Count).map_err(|e| {
        Error::new(
            magnus::exception::runtime_error(),
            format!("Count failed: {}", e),
        )
    })?;

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

/// Build a query by tokenizing the query string with a named tokenizer,
/// then constructing TermQuery objects for each token × field combination.
///
/// This allows using a different tokenizer at query time than at index time.
/// The index tokenizer may expand tokens (e.g. n-gram sub-spans for COMPLEX tokens),
/// while the query tokenizer produces only the canonical form, ensuring that
/// searches match the full indexed token rather than noisy sub-spans.
///
/// Tokens at the **same position** are treated as synonyms (OR'd together),
/// matching how Lucene/Java handles same-position tokens from the analyzer.
/// Tokens at **different positions** are AND'd together, so multi-word queries
/// require all terms. Example: "running experiments" →
///   Must(run OR running across fields) AND Must(experi OR experiments across fields)
fn build_tokenized_query(
    rb_index: &RbIndex,
    query_string: &str,
    tokenizer_name: &str,
    fields: &[tantivy::schema::Field],
) -> Result<Box<dyn Query>, Error> {
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
        return Ok(Box::new(tantivy::query::EmptyQuery));
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
    for (_pos, synonyms) in &position_groups {
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

    Ok(Box::new(BooleanQuery::new(position_clauses)))
}

fn owned_value_to_ruby(val: &OwnedValue, ruby: &Ruby) -> Result<Value, Error> {
    Ok(match val {
        OwnedValue::Str(s) => RString::new(s).as_value(),
        OwnedValue::U64(n) => ruby.integer_from_i64(*n as i64).as_value(),
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
