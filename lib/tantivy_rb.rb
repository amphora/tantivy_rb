# Ruby bindings for the Tantivy full-text search engine.
#
# Provides two classes:
#   TantivyRb::Schema — define index fields (text, numeric, date)
#   TantivyRb::Index  — open/create an index, add/delete/search documents
#
# The native extension is a Rust cdylib built via rb_sys and magnus.
# See README.md for usage examples and docs/TOKENIZERS.md for
# detailed tokenizer documentation.

require_relative "tantivy_rb/version"

module TantivyRb
  # Load the compiled Rust native extension. rb_sys places the .so under a
  # Ruby-version-specific directory (e.g. tantivy_rb/3.3/tantivy_rb.so);
  # fall back to the unversioned path for development builds.
  #
  # The +loader+ parameter accepts any callable that behaves like +require+.
  # Production code uses the default (Kernel#require); tests inject a fake
  # to simulate LoadError on specific paths.
  def self.load_native_extension(loader: method(:require))
    ruby_api_version = RUBY_VERSION[/\d+\.\d+/]
    loader.call("tantivy_rb/#{ruby_api_version}/tantivy_rb")
  rescue LoadError
    begin
      loader.call("tantivy_rb/tantivy_rb")
    rescue LoadError => e
      raise LoadError,
        "Failed to load tantivy_rb native extension. " \
        "Tried #{ruby_api_version}/ and unversioned paths. " \
        "Run `cd ext/tantivy_rb && cargo build --release` to compile. " \
        "Original error: #{e.message}"
    end
  end
end

TantivyRb.load_native_extension
