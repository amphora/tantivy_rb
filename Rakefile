require "rb_sys/extensiontask"
require "rake/testtask"

RbSys::ExtensionTask.new("tantivy_rb") do |ext|
  ext.lib_dir = "lib/tantivy_rb"
end

Rake::TestTask.new(:test) do |t|
  t.libs << "test"
  t.test_files = FileList["test/**/*_test.rb"]
end
