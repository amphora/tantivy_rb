//! Index management for Tantivy, exposed to Ruby as `TantivyRb::Index`.
//!
//! Provides document indexing (add/delete/commit), reader management, search
//! dispatch, and custom tokenizer registration. The writer is lazily created on
//! first write so that read-only processes (e.g. the web server) never acquire
//! the exclusive file lock.

use crate::schema::RbSchema;
use crate::search;
use crate::tokenizer;
use magnus::{class, function, method, prelude::*, r_hash::ForEach, Error, RHash, Symbol, Value};
use std::sync::{Arc, Mutex, MutexGuard};
use tantivy::schema::Schema;
use tantivy::{DateTime, Index, IndexReader, IndexWriter, TantivyDocument};

/// Thread-safe wrapper around a Tantivy Index + writer + reader.
///
/// The writer is created lazily on first write operation, so opening an index
/// for search does NOT acquire the exclusive file lock. This allows multiple
/// read-only processes (e.g. web server serving searches) to coexist with a
/// single writer process (e.g. rake task rebuilding the index).
///
/// The reader is stored without a Mutex because `IndexReader` is `Sync` —
/// `searcher()` and `reload()` both take `&self` and are safe to call
/// concurrently.
#[magnus::wrap(class = "TantivyRb::Index")]
pub struct RbIndex {
    index: Index,
    schema: Schema,
    writer: Arc<Mutex<Option<IndexWriter>>>,
    reader: IndexReader,
    read_only: bool,
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
            reader,
            read_only: false,
        })
    }

    /// Open an existing index at the given path in read-only mode.
    ///
    /// Uses `Index::open_in_dir` which reads the schema from `meta.json`
    /// and never touches the write lock file. This allows read-only
    /// processes (console, runner, rake tasks) to coexist with a running
    /// writer process without lock contention.
    ///
    /// The index directory must already exist (no auto-creation).
    /// Write operations (`add_document`, `delete_document`, `commit`) will
    /// return an error.
    fn open_readonly(path: String) -> Result<Self, Error> {
        let dir = std::path::Path::new(&path);

        let directory = tantivy::directory::MmapDirectory::open(dir).map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to open directory: {}", e),
            )
        })?;

        let index = Index::open(directory).map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to open index (does it exist?): {}", e),
            )
        })?;

        let schema = index.schema();

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
            reader,
            read_only: true,
        })
    }

    /// Lock the writer mutex and lazily create the IndexWriter if needed.
    ///
    /// Returns the held `MutexGuard` so the caller can use the writer without
    /// releasing and re-acquiring the lock (which would create a TOCTOU gap
    /// and require an unsafe unwrap on the second acquisition).
    ///
    /// The exclusive Tantivy file lock is acquired here on first write — NOT
    /// when the index is opened.
    fn lock_writer(&self) -> Result<MutexGuard<'_, Option<IndexWriter>>, Error> {
        if self.read_only {
            return Err(Error::new(
                magnus::exception::runtime_error(),
                "Cannot write to a read-only index",
            ));
        }
        let mut guard = self.writer.lock().map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!(
                    "Failed to lock writer mutex (poisoned): a previous operation panicked while \
                     holding the lock, leaving the index in a potentially inconsistent state. \
                     Consider rebuilding the index. Original error: {}",
                    e
                ),
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
        Ok(guard)
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

        let guard = self.lock_writer()?;
        let writer = guard.as_ref().expect("lock_writer guarantees Some");
        writer.add_document(doc).map_err(|e| {
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
        let guard = self.lock_writer()?;
        let writer = guard.as_ref().expect("lock_writer guarantees Some");
        writer.delete_term(term);
        Ok(())
    }

    /// Commit pending changes to the index.
    fn commit(&self) -> Result<(), Error> {
        let mut guard = self.lock_writer()?;
        let writer = guard.as_mut().expect("lock_writer guarantees Some");
        writer.commit().map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!("Failed to commit: {}", e),
            )
        })?;
        Ok(())
    }

    /// Reload the reader to pick up committed changes.
    fn reload(&self) -> Result<(), Error> {
        self.reader.reload().map_err(|e| {
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

    /// Release the index writer, dropping the exclusive file lock.
    ///
    /// After calling this, further write operations will lazily create a new
    /// writer (and re-acquire the lock). This is used by `SearchService.reset_default!`
    /// to ensure the old singleton's file lock is released before the reference
    /// is dropped, avoiding `LockBusy` errors when a new singleton is created
    /// before GC collects the old one.
    fn release_writer(&self) -> Result<(), Error> {
        let mut guard = self.writer.lock().map_err(|e| {
            Error::new(
                magnus::exception::runtime_error(),
                format!(
                    "Failed to lock writer mutex (poisoned): a previous operation panicked while \
                     holding the lock, leaving the index in a potentially inconsistent state. \
                     Consider rebuilding the index. Original error: {}",
                    e
                ),
            )
        })?;
        *guard = None;
        Ok(())
    }

    /// Register a custom tokenizer on this index.
    fn register_tokenizer(&self, name: String, kwargs: RHash) -> Result<(), Error> {
        tokenizer::register_tokenizer(&self.index, name, kwargs)
    }

    /// Access the underlying Tantivy `Index` (for tokenizer registration and query parsing).
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// Access the built schema (for field lookup during document add and search).
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Access the reader (for search). `IndexReader` is `Sync`, so `searcher()`
    /// can be called concurrently from multiple threads without locking.
    pub fn reader(&self) -> &IndexReader {
        &self.reader
    }
}

/// Parse a date string into a Tantivy `DateTime`.
///
/// Accepts three formats, tried in order:
/// 1. RFC 3339 with timezone — `"2024-01-15T10:30:00+00:00"`
/// 2. ISO 8601 without timezone (assumed UTC) — `"2024-01-15T10:30:00"`
/// 3. Date only (midnight UTC) — `"2024-01-15"`
fn parse_date(s: &str) -> Result<DateTime, Error> {
    let ts = parse_date_to_timestamp(s).map_err(|msg| {
        Error::new(magnus::exception::arg_error(), msg)
    })?;
    Ok(DateTime::from_timestamp_secs(ts))
}

/// Pure date parsing logic, free of Magnus dependencies.
///
/// Returns a Unix timestamp (seconds since epoch) or an error message string.
/// Extracted from `parse_date` so that `#[cfg(test)]` code can exercise the
/// parsing branches without pulling Ruby symbols into the test binary.
fn parse_date_to_timestamp(s: &str) -> Result<i64, String> {
    chrono::DateTime::parse_from_rfc3339(s)
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                .map(|ndt| ndt.and_utc().fixed_offset())
        })
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").map(|nd| {
                nd.and_hms_opt(0, 0, 0)
                    .expect("midnight (0,0,0) is always valid")
                    .and_utc()
                    .fixed_offset()
            })
        })
        .map(|dt| dt.timestamp())
        .map_err(|e| format!("Failed to parse date '{}': {}", s, e))
}

/// Register `TantivyRb::Index` and its methods on the given Ruby module.
pub fn init(module: magnus::RModule) -> Result<(), Error> {
    let class = module.define_class("Index", class::object())?;
    class.define_singleton_method("open", function!(RbIndex::open, 2))?;
    class.define_singleton_method("open_readonly", function!(RbIndex::open_readonly, 1))?;
    class.define_method("add_document", method!(RbIndex::add_document, 1))?;
    class.define_method("delete_document", method!(RbIndex::delete_document, 2))?;
    class.define_method("commit", method!(RbIndex::commit, 0))?;
    class.define_method("reload", method!(RbIndex::reload, 0))?;
    class.define_method("search", method!(RbIndex::search, -1))?;
    class.define_method("release_writer", method!(RbIndex::release_writer, 0))?;
    class.define_method(
        "register_tokenizer",
        method!(RbIndex::register_tokenizer, 2),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_date_rfc3339() {
        let ts = parse_date_to_timestamp("2024-01-15T10:30:00+00:00").unwrap();
        assert_eq!(ts, 1705314600); // 2024-01-15 10:30:00 UTC
    }

    #[test]
    fn test_parse_date_iso8601_no_tz() {
        let ts = parse_date_to_timestamp("2024-01-15T10:30:00").unwrap();
        assert_eq!(ts, 1705314600); // No TZ → assumed UTC
    }

    #[test]
    fn test_parse_date_date_only() {
        let ts = parse_date_to_timestamp("2024-01-15").unwrap();
        assert_eq!(ts, 1705276800); // Midnight UTC on 2024-01-15
    }

    #[test]
    fn test_parse_date_invalid() {
        let result = parse_date_to_timestamp("not-a-date");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Failed to parse date"));
    }
}

// TODO:: [DEFERRED] Add Ruby-dependent unit tests (requires magnus::embed or Ruby linking)
// Targets: lock_writer lazy init / read-only rejection, add_document field-type dispatch
// Reason: Constructing RbIndex triggers #[magnus::wrap] trait code that references Ruby
// symbols, causing linker errors in pure-Rust test binaries. Needs either embed feature
// flag or a refactor to decouple testable logic from the Magnus wrapper.
// Scope: 3
// See: AMPHTT-731
