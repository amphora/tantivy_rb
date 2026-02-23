use crate::schema::RbSchema;
use crate::search;
use crate::tokenizer;
use magnus::{class, function, method, prelude::*, r_hash::ForEach, Error, RHash, Symbol, Value};
use std::sync::{Arc, Mutex};
use tantivy::schema::Schema;
use tantivy::{DateTime, Index, IndexReader, IndexWriter, TantivyDocument};

/// Thread-safe wrapper around a Tantivy Index + writer + reader.
///
/// The writer is created lazily on first write operation, so opening an index
/// for search does NOT acquire the exclusive file lock. This allows multiple
/// read-only processes (e.g. web server serving searches) to coexist with a
/// single writer process (e.g. rake task rebuilding the index).
#[magnus::wrap(class = "TantivyRb::Index")]
pub struct RbIndex {
    index: Index,
    schema: Schema,
    writer: Arc<Mutex<Option<IndexWriter>>>,
    reader: Arc<Mutex<IndexReader>>,
}

impl RbIndex {
    /// Open or create an index at the given path with the given schema.
    fn open(path: String, kwargs: RHash) -> Result<Self, Error> {
        let rb_schema: &RbSchema = {
            let val: Value = kwargs.get(Symbol::new("schema")).ok_or_else(|| {
                Error::new(
                    magnus::exception::arg_error(),
                    "missing keyword argument: schema",
                )
            })?;
            magnus::TryConvert::try_convert(val)?
        };

        let schema = rb_schema.build()?;

        let dir = std::path::Path::new(&path);
        std::fs::create_dir_all(dir).map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to create index directory: {}", e),
            )
        })?;

        let directory = tantivy::directory::MmapDirectory::open(dir).map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to open directory: {}", e),
            )
        })?;

        let index = Index::open_or_create(directory, schema.clone()).map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to open/create index: {}", e),
            )
        })?;

        let reader = index.reader().map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to create reader: {}", e),
            )
        })?;

        Ok(RbIndex {
            index,
            schema,
            writer: Arc::new(Mutex::new(None)),
            reader: Arc::new(Mutex::new(reader)),
        })
    }

    /// Lazily create or return the IndexWriter. The exclusive file lock is
    /// acquired here, on first write — NOT when the index is opened.
    fn ensure_writer(&self) -> Result<(), Error> {
        let mut guard = self.writer.lock().map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to lock writer mutex: {}", e),
            )
        })?;
        if guard.is_none() {
            let w = self.index.writer(50_000_000).map_err(|e| {
                Error::new(
                    magnus::exception::runtime_error(),
                    format!("Failed to create writer: {}", e),
                )
            })?;
            *guard = Some(w);
        }
        Ok(())
    }

    /// Add a document to the index. Takes a Ruby hash of field_name => value.
    fn add_document(&self, doc_hash: RHash) -> Result<(), Error> {
        let mut doc = TantivyDocument::new();

        doc_hash.foreach(|key: Value, value: Value| {
            let field_name: String = magnus::TryConvert::try_convert(key)?;
            let field = self.schema.get_field(&field_name).map_err(|_| {
                Error::new(
                    magnus::exception::arg_error(),
                    format!("Unknown field: {}", field_name),
                )
            })?;

            let field_entry = self.schema.get_field_entry(field);
            let field_type = field_entry.field_type();

            match field_type {
                tantivy::schema::FieldType::Str(_) => {
                    let s: String = magnus::TryConvert::try_convert(value)?;
                    doc.add_text(field, &s);
                }
                tantivy::schema::FieldType::U64(_) => {
                    let n: u64 = magnus::TryConvert::try_convert(value)?;
                    doc.add_u64(field, n);
                }
                tantivy::schema::FieldType::I64(_) => {
                    let n: i64 = magnus::TryConvert::try_convert(value)?;
                    doc.add_i64(field, n);
                }
                tantivy::schema::FieldType::F64(_) => {
                    let n: f64 = magnus::TryConvert::try_convert(value)?;
                    doc.add_f64(field, n);
                }
                tantivy::schema::FieldType::Date(_) => {
                    if let Ok(s) = <String as magnus::TryConvert>::try_convert(value) {
                        let dt = parse_date(&s)?;
                        doc.add_date(field, dt);
                    } else if let Ok(ts) = <i64 as magnus::TryConvert>::try_convert(value) {
                        let dt = DateTime::from_timestamp_secs(ts);
                        doc.add_date(field, dt);
                    } else {
                        return Err(Error::new(
                            magnus::exception::arg_error(),
                            format!(
                                "Date field '{}' expects ISO 8601 string or Unix timestamp",
                                field_name
                            ),
                        ));
                    }
                }
                _ => {
                    return Err(Error::new(
                        magnus::exception::runtime_error(),
                        format!("Unsupported field type for '{}'", field_name),
                    ));
                }
            }
            Ok(ForEach::Continue)
        })?;

        self.ensure_writer()?;
        let guard = self.writer.lock().map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to lock writer: {}", e),
            )
        })?;
        guard.as_ref().unwrap().add_document(doc).map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to add document: {}", e),
            )
        })?;
        Ok(())
    }

    /// Delete all documents matching field=value.
    fn delete_document(&self, field_name: String, value: String) -> Result<(), Error> {
        let field = self.schema.get_field(&field_name).map_err(|_| {
            Error::new(
                magnus::exception::arg_error(),
                format!("Unknown field: {}", field_name),
            )
        })?;

        let term = tantivy::Term::from_field_text(field, &value);
        self.ensure_writer()?;
        let guard = self.writer.lock().map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to lock writer: {}", e),
            )
        })?;
        guard.as_ref().unwrap().delete_term(term);
        Ok(())
    }

    /// Commit pending changes to the index.
    fn commit(&self) -> Result<(), Error> {
        self.ensure_writer()?;
        let mut guard = self.writer.lock().map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to lock writer: {}", e),
            )
        })?;
        guard.as_mut().unwrap().commit().map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to commit: {}", e),
            )
        })?;
        Ok(())
    }

    /// Reload the reader to pick up committed changes.
    fn reload(&self) -> Result<(), Error> {
        let reader = self.reader.lock().map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to lock reader: {}", e),
            )
        })?;
        reader.reload().map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to reload reader: {}", e),
            )
        })?;
        Ok(())
    }

    /// Search the index. Returns a Ruby hash with :total and :hits.
    fn search(&self, ruby_args: &[Value]) -> Result<RHash, Error> {
        search::execute_search(self, ruby_args)
    }

    /// Register a custom tokenizer on this index.
    fn register_tokenizer(&self, name: String, kwargs: RHash) -> Result<(), Error> {
        tokenizer::register_tokenizer(&self.index, name, kwargs)
    }

    pub fn index(&self) -> &Index {
        &self.index
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn reader(&self) -> &Arc<Mutex<IndexReader>> {
        &self.reader
    }
}

fn parse_date(s: &str) -> Result<DateTime, Error> {
    let ts = chrono::DateTime::parse_from_rfc3339(s)
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                .map(|ndt| ndt.and_utc().fixed_offset())
        })
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map(|nd| {
                nd.and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .fixed_offset()
            })
        })
        .map_err(|e| {
            Error::new(
                magnus::exception::arg_error(),
                format!("Failed to parse date '{}': {}", s, e),
            )
        })?
        .timestamp();

    Ok(DateTime::from_timestamp_secs(ts))
}

pub fn init(module: magnus::RModule) -> Result<(), Error> {
    let class = module.define_class("Index", class::object())?;
    class.define_singleton_method("open", function!(RbIndex::open, 2))?;
    class.define_method("add_document", method!(RbIndex::add_document, 1))?;
    class.define_method("delete_document", method!(RbIndex::delete_document, 2))?;
    class.define_method("commit", method!(RbIndex::commit, 0))?;
    class.define_method("reload", method!(RbIndex::reload, 0))?;
    class.define_method("search", method!(RbIndex::search, -1))?;
    class.define_method(
        "register_tokenizer",
        method!(RbIndex::register_tokenizer, 2),
    )?;
    Ok(())
}
