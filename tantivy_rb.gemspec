require_relative "lib/tantivy_rb/version"

Gem::Specification.new do |spec|
  spec.name = "tantivy_rb"
  spec.version = TantivyRb::VERSION
  spec.authors = ["Amphora Research Systems"]
  spec.license = "MIT"

  spec.summary = "Ruby bindings for the Tantivy full-text search engine"
  spec.description = "A native Ruby extension wrapping Tantivy, a fast full-text search engine " \
                     "written in Rust. Includes standard and compound tokenizers for technical " \
                     "and scientific document search."
  spec.homepage = "https://github.com/amphora-research/tantivy_rb"
  spec.required_ruby_version = ">= 3.1.0"

  spec.files = Dir[
    "lib/**/*.rb",
    "ext/**/*.{rs,toml,rb}",
    "Cargo.toml",
    "LICENSE",
    "README.md"
  ]

  spec.require_paths = ["lib"]
  spec.extensions = ["ext/tantivy_rb/extconf.rb"]

  spec.add_dependency "rb_sys", "~> 0.9"
end
