# Ruby bindings for the Tantivy full-text search engine.
#
# Provides two classes:
#   TantivyRb::Schema — define index fields (text, numeric, date)
#   TantivyRb::Index  — open/create an index, add/delete/search documents
#
# The native extension is a Rust cdylib built via rb_sys and magnus.
# See the gem README for usage examples and the docs/ directory for
# detailed tokenizer documentation.

require_relative "tantivy_rb/version"

# Load the compiled Rust native extension. rb_sys places the .so under a
# Ruby-version-specific directory (e.g. tantivy_rb/3.3/tantivy_rb.so);
# fall back to the unversioned path for development builds.
begin
  ruby_api_version = RUBY_VERSION[/\d+\.\d+/]
  require "tantivy_rb/#{ruby_api_version}/tantivy_rb"
rescue LoadError
  require "tantivy_rb/tantivy_rb"
end
