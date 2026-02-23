require_relative "tantivy_rb/version"

begin
  RUBY_VERSION =~ /(\d+\.\d+)/
  require "tantivy_rb/#{$1}/tantivy_rb"
rescue LoadError
  require "tantivy_rb/tantivy_rb"
end
