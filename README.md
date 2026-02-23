# tantivy_rb

Ruby bindings for the [Tantivy](https://github.com/quickwit-oss/tantivy) full-text search engine, built as a native Rust extension via [magnus](https://github.com/matsadler/magnus) and [rb_sys](https://github.com/oxidize-rb/rb-sys).

This gem is part of the PatentSafe AI application and is not published to RubyGems. It lives in `gems/tantivy_rb/` within the main repository.

## What it provides

- **`TantivyRb::Schema`** — build a Tantivy schema from Ruby with typed fields (text, u64, i64, f64, date)
- **`TantivyRb::Index`** — open or create an on-disk index, add/delete/search documents, register custom tokenizers
- **Custom tokenizers** — a compound tokenizer ported from the Java PatentSafe search analysers, with WORD/COMPLEX classification, n-gram expansion, stemming, and stop-word filtering

## Building

The gem requires a Rust toolchain (stable) and the `rb_sys` gem.

```sh
# From the Rails root — bundler handles the build:
bundle install

# Or build the native extension directly:
cd gems/tantivy_rb
bundle exec rake compile
```

After building, the compiled `.so` is placed in `lib/tantivy_rb/<ruby_version>/tantivy_rb.so`.

### Development builds

When iterating on the Rust code:

```sh
cd gems/tantivy_rb/ext/tantivy_rb

# Type-check without a full build:
cargo check

# Run Rust-level unit tests (no Ruby required):
cargo test

# Full compile + install into the gem's lib directory:
cd ../..
bundle exec rake compile
```

> **Important:** After changing Rust code, bump the version in
> `lib/tantivy_rb/version.rb` to verify the new `.so` is actually loaded at
> runtime. Ruby may cache the old extension otherwise.

## Ruby API

### Schema

Define the fields your index will contain:

```ruby
schema = TantivyRb::Schema.new

# Text fields — options: stored:, tokenizer:, fast:
schema.add_text_field("title",    stored: true, tokenizer: "default")
schema.add_text_field("body",     stored: false)
schema.add_text_field("doc_id",   stored: true, tokenizer: "raw")  # exact match, no tokenization

# Numeric fields — options: stored:, indexed:, fast:
schema.add_u64_field("page_count", stored: true, indexed: true)
schema.add_i64_field("score",      stored: true)
schema.add_f64_field("relevance",  fast: true)

# Date fields — options: stored:, indexed:, fast:
schema.add_date_field("created_at", stored: true)
```

The schema is consumed when passed to `Index.open` and cannot be reused.

### Index

Open or create an index on disk:

```ruby
index = TantivyRb::Index.open("/path/to/index", schema: schema)
```

The writer is created lazily — opening an index for search does **not** acquire the exclusive file lock. This allows multiple read-only processes (e.g. web servers) to coexist with a single writer process.

#### Adding documents

Pass a hash of field name to value:

```ruby
index.add_document({
  "title"      => "Experiment E21634-016",
  "body"       => "Results of the compound analysis...",
  "doc_id"     => "E21634-016",
  "page_count" => 12,
  "created_at" => "2024-01-15T10:30:00+00:00"  # ISO 8601 string or Unix timestamp
})

index.commit   # flush pending writes to disk
index.reload   # refresh the reader to see committed changes
```

#### Deleting documents

Delete all documents matching a term (exact match on the specified field):

```ruby
index.delete_document("doc_id", "E21634-016")
index.commit
index.reload
```

#### Searching

```ruby
result = index.search("compound analysis",
  fields: ["title", "body"],    # which text fields to search (default: all text fields)
  limit: 20,                    # max results to return (default: 20)
  offset: 0,                    # skip N results for pagination (default: 0)
  filter: { "state" => "active" },      # exact-match term filters (optional)
  query_tokenizer: "ps_query"           # tokenizer for the query string (optional)
)

result[:total]  # => Integer — total matching documents
result[:hits]   # => Array of hashes:
# [
#   {
#     score: 1.23,
#     stored_fields: { "title" => "Experiment E21634-016", "doc_id" => "E21634-016", ... }
#   },
#   ...
# ]
```

**Query modes:**

- Without `query_tokenizer:` — uses Tantivy's built-in query parser (supports the standard Tantivy query syntax).
- With `query_tokenizer:` — tokenizes the query through the named tokenizer and builds a custom AND-of-ORs query. Supports quoted `"phrase"` searches. This is the mode used by PatentSafe's `SearchService`.

### Tokenizers

Register custom tokenizers on an index:

```ruby
# Standard pipeline: whitespace -> ASCII fold -> lowercase -> stop words -> stemmer
index.register_tokenizer("my_default",
  type: :default,
  stemmer: :english,
  stop_words: :english)

# Raw: no tokenization (for exact-match fields)
index.register_tokenizer("my_raw", type: :raw)

# Compound index tokenizer (PatentSafe-specific):
index.register_tokenizer("ps_index",
  type: :compound,
  mode: :index,
  stemmer: :english,
  stop_words: :english,
  leading_strip: ".,;:\")<>}]~+",
  trailing_strip: ".,;:\"(<>[{%")

# Compound query tokenizer:
index.register_tokenizer("ps_query",
  type: :compound,
  mode: :query,
  stemmer: :english,
  stop_words: :english)
```

See [`docs/TOKENIZERS.md`](docs/TOKENIZERS.md) for a detailed explanation of the compound tokenizer pipeline.

#### Supported stemmer languages

`:english`, `:french`, `:german`, `:spanish`, `:italian`, `:portuguese`, `:dutch`, `:swedish`, `:norwegian`, `:danish`, `:finnish`, `:hungarian`, `:romanian`, `:russian`, `:turkish`, `:arabic`

#### Stop words

The `stop_words:` option accepts either a language symbol (`:english`) or an array of custom words (`["a", "the", "is"]`). The built-in English list matches Lucene 3.6's `ENGLISH_STOP_WORDS_SET` for compatibility with the Java application.

## Architecture

```
Ruby (TantivyRb module)
  │
  ├── Schema  ──── schema.rs ──── Tantivy SchemaBuilder
  │
  └── Index   ──── index.rs  ──── Tantivy Index + Writer + Reader
                     │
                     ├── search.rs ──── Query building + execution
                     │                  (QueryParser or custom AND-of-ORs)
                     │
                     └── tokenizer/
                           ├── mod.rs ──── Tokenizer registration dispatch
                           ├── default.rs ──── Standard pipeline + shared helpers
                           └── compound/
                                 ├── mod.rs ──── Index tokenizer (WORD/COMPLEX)
                                 ├── query.rs ──── Query tokenizer (simplified)
                                 ├── classifier.rs ──── Token classification
                                 ├── expander.rs ──── N-gram expansion
                                 └── stop_words.rs ──── Stop word checking
```

The Rust code uses [magnus](https://github.com/matsadler/magnus) to bridge between Ruby and Rust. The `#[magnus::wrap]` macro creates Ruby-visible classes, and `method!`/`function!` macros expose Rust methods as Ruby methods.

## Testing

### Rust tests

```sh
cd gems/tantivy_rb/ext/tantivy_rb
cargo test
```

69 tests covering the tokenizer pipelines, including tests ported from the Java test suite (ComplexTokenFilter, FullIndexingAnalyser, PatentSafeQueryAnalyser, BlockTokenParser).

### Ruby integration tests

From the Rails root:

```sh
bin/rails test test/services/tantivy_rb_smoke_test.rb
bin/rails test test/services/tantivy_rb_search_test.rb
bin/rails test test/services/tantivy_rb_index_test.rb
bin/rails test test/services/tantivy_rb_compound_test.rb
```

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `tantivy` | 0.22 | Full-text search engine |
| `magnus` | 0.7 | Ruby-Rust bindings (with `rb-sys` feature) |
| `rb-sys` | 0.9 | Low-level Ruby C API bindings |
| `rust-stemmers` | 1.2 | Snowball stemming algorithms |
| `chrono` | 0.4 | Date parsing for date fields |

## License

MIT
