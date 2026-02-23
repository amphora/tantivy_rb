//! Schema builder for Tantivy indexes, exposed to Ruby as `TantivyRb::Schema`.
//!
//! Wraps Tantivy's `SchemaBuilder` and provides Ruby methods to add typed fields.
//! The schema is consumed (built) when passed to `TantivyRb::Index.open`, after
//! which the builder cannot be reused.

use magnus::{class, function, method, prelude::*, Error, RHash, Symbol, Value};
use std::cell::RefCell;
use tantivy::schema::{
    DateOptions, IndexRecordOption, NumericOptions, Schema, SchemaBuilder, TextFieldIndexing,
    TextOptions, INDEXED, STORED,
};

/// Wraps a Tantivy SchemaBuilder to construct a schema from Ruby.
///
/// Uses `RefCell<Option<>>` for interior mutability: the `Option` allows the
/// builder to be consumed (taken) exactly once when `build()` is called.
/// `RefCell` (not `Mutex`) is safe here because Ruby's GVL serialises all
/// access from Ruby threads — Magnus-wrapped structs are never accessed
/// concurrently.
#[magnus::wrap(class = "TantivyRb::Schema")]
pub struct RbSchema {
    inner: RefCell<Option<SchemaBuilder>>,
}

impl RbSchema {
    fn new() -> Self {
        RbSchema {
            inner: RefCell::new(Some(Schema::builder())),
        }
    }

    /// add_text_field(name, opts = {})
    /// opts: stored:, tokenizer:, fast:
    fn add_text_field(&self, args: &[Value]) -> Result<(), Error> {
        if args.is_empty() {
            return Err(Error::new(
                magnus::exception::arg_error(),
                "add_text_field requires a field name",
            ));
        }
        let name: String = magnus::TryConvert::try_convert(args[0])?;
        let opts: Option<RHash> = if args.len() > 1 {
            Some(magnus::TryConvert::try_convert(args[1])?)
        } else {
            None
        };

        let mut stored = false;
        let mut tokenizer = "default".to_string();
        let mut fast = false;

        if let Some(hash) = opts {
            if let Some(v) = hash_get_bool(&hash, "stored")? {
                stored = v;
            }
            if let Some(v) = hash_get_string(&hash, "tokenizer")? {
                tokenizer = v;
            }
            if let Some(v) = hash_get_bool(&hash, "fast")? {
                fast = v;
            }
        }

        let mut builder = self.inner.borrow_mut();
        let builder = builder.as_mut().ok_or_else(|| {
            Error::new(magnus::exception::runtime_error(), "Schema already built")
        })?;

        let mut text_opts = if tokenizer == "raw" {
            // STRING field: indexed but not tokenized (exact match)
            let indexing = TextFieldIndexing::default()
                .set_tokenizer("raw")
                .set_index_option(IndexRecordOption::Basic);
            TextOptions::default().set_indexing_options(indexing)
        } else {
            let indexing = TextFieldIndexing::default()
                .set_tokenizer(&tokenizer)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions);
            TextOptions::default().set_indexing_options(indexing)
        };
        if stored {
            text_opts = text_opts.set_stored();
        }
        if fast {
            text_opts = text_opts.set_fast(None);
        }

        builder.add_text_field(&name, text_opts);
        Ok(())
    }

    /// add_u64_field(name, opts = {})
    fn add_u64_field(&self, args: &[Value]) -> Result<(), Error> {
        let (name, stored, indexed, fast) = parse_numeric_args(args, "add_u64_field")?;
        let mut builder = self.inner.borrow_mut();
        let builder = builder.as_mut().ok_or_else(|| {
            Error::new(magnus::exception::runtime_error(), "Schema already built")
        })?;
        builder.add_u64_field(&name, build_numeric_opts(stored, indexed, fast));
        Ok(())
    }

    /// add_i64_field(name, opts = {})
    fn add_i64_field(&self, args: &[Value]) -> Result<(), Error> {
        let (name, stored, indexed, fast) = parse_numeric_args(args, "add_i64_field")?;
        let mut builder = self.inner.borrow_mut();
        let builder = builder.as_mut().ok_or_else(|| {
            Error::new(magnus::exception::runtime_error(), "Schema already built")
        })?;
        builder.add_i64_field(&name, build_numeric_opts(stored, indexed, fast));
        Ok(())
    }

    /// add_f64_field(name, opts = {})
    fn add_f64_field(&self, args: &[Value]) -> Result<(), Error> {
        let (name, stored, indexed, fast) = parse_numeric_args(args, "add_f64_field")?;
        let mut builder = self.inner.borrow_mut();
        let builder = builder.as_mut().ok_or_else(|| {
            Error::new(magnus::exception::runtime_error(), "Schema already built")
        })?;
        builder.add_f64_field(&name, build_numeric_opts(stored, indexed, fast));
        Ok(())
    }

    /// add_date_field(name, opts = {})
    fn add_date_field(&self, args: &[Value]) -> Result<(), Error> {
        let (name, stored, indexed, fast) = parse_numeric_args(args, "add_date_field")?;
        let mut builder = self.inner.borrow_mut();
        let builder = builder.as_mut().ok_or_else(|| {
            Error::new(magnus::exception::runtime_error(), "Schema already built")
        })?;
        let mut date_opts = DateOptions::default();
        if stored {
            date_opts = date_opts.set_stored();
        }
        if indexed {
            date_opts = date_opts.set_indexed();
        }
        if fast {
            date_opts = date_opts.set_fast();
        }
        builder.add_date_field(&name, date_opts);
        Ok(())
    }

    /// Consume the builder and return the built Tantivy Schema.
    pub fn build(&self) -> Result<Schema, Error> {
        let mut builder = self.inner.borrow_mut();
        let b = builder.take().ok_or_else(|| {
            Error::new(magnus::exception::runtime_error(), "Schema already built")
        })?;
        Ok(b.build())
    }
}

// ---------------------------------------------------------------------------
// Helper functions for extracting typed values from Ruby keyword-argument hashes.
// These handle the Symbol-keyed RHash that Ruby passes for `opts = {}` arguments.
// ---------------------------------------------------------------------------

/// Extract a boolean value from an RHash by symbol key, returning `None` if absent.
fn hash_get_bool(hash: &RHash, key: &str) -> Result<Option<bool>, Error> {
    let sym = Symbol::new(key);
    match hash.get(sym) {
        Some(val) => {
            let b: bool = magnus::TryConvert::try_convert(val)?;
            Ok(Some(b))
        }
        None => Ok(None),
    }
}

/// Extract a string value from an RHash by symbol key, returning `None` if absent.
fn hash_get_string(hash: &RHash, key: &str) -> Result<Option<String>, Error> {
    let sym = Symbol::new(key);
    match hash.get(sym) {
        Some(val) => {
            let s: String = magnus::TryConvert::try_convert(val)?;
            Ok(Some(s))
        }
        None => Ok(None),
    }
}

/// Parse the common argument pattern for numeric/date field methods.
///
/// All numeric `add_*_field` methods accept `(name, opts = {})` where opts
/// can contain `:stored`, `:indexed`, and `:fast`. Returns the parsed tuple.
fn parse_numeric_args(args: &[Value], method_name: &str) -> Result<(String, bool, bool, bool), Error> {
    if args.is_empty() {
        return Err(Error::new(
            magnus::exception::arg_error(),
            format!("{} requires a field name", method_name),
        ));
    }
    let name: String = magnus::TryConvert::try_convert(args[0])?;
    let mut stored = false;
    let mut indexed = true;
    let mut fast = false;

    if args.len() > 1 {
        let hash: RHash = magnus::TryConvert::try_convert(args[1])?;
        if let Some(v) = hash_get_bool(&hash, "stored")? {
            stored = v;
        }
        if let Some(v) = hash_get_bool(&hash, "indexed")? {
            indexed = v;
        }
        if let Some(v) = hash_get_bool(&hash, "fast")? {
            fast = v;
        }
    }
    Ok((name, stored, indexed, fast))
}

/// Build a Tantivy `NumericOptions` from the parsed boolean flags.
fn build_numeric_opts(stored: bool, indexed: bool, fast: bool) -> NumericOptions {
    let mut opts = NumericOptions::default();
    if stored {
        opts = opts | STORED;
    }
    if indexed {
        opts = opts | INDEXED;
    }
    if fast {
        opts = opts.set_fast();
    }
    opts
}

/// Register `TantivyRb::Schema` and its methods on the given Ruby module.
pub fn init(module: magnus::RModule) -> Result<(), Error> {
    let class = module.define_class("Schema", class::object())?;
    class.define_singleton_method("new", function!(RbSchema::new, 0))?;
    class.define_method("add_text_field", method!(RbSchema::add_text_field, -1))?;
    class.define_method("add_u64_field", method!(RbSchema::add_u64_field, -1))?;
    class.define_method("add_i64_field", method!(RbSchema::add_i64_field, -1))?;
    class.define_method("add_f64_field", method!(RbSchema::add_f64_field, -1))?;
    class.define_method("add_date_field", method!(RbSchema::add_date_field, -1))?;
    Ok(())
}
