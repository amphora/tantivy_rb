require_relative "test_helper"

class TantivyRbTest < Minitest::Test
  def test_version_is_a_non_empty_semver_string
    assert_match(/\A\d+\.\d+\.\d+\z/, TantivyRb::VERSION)
  end

  def test_load_versioned_path_succeeds
    versioned_path = "tantivy_rb/#{RUBY_VERSION[/\d+\.\d+/]}/tantivy_rb"
    loader, attempted = build_loader(succeed_on: [ versioned_path ])

    TantivyRb.load_native_extension(loader: loader)

    assert_equal [ versioned_path ], attempted
  end

  def test_fallback_to_unversioned_when_versioned_path_fails
    versioned_path = "tantivy_rb/#{RUBY_VERSION[/\d+\.\d+/]}/tantivy_rb"
    unversioned_path = "tantivy_rb/tantivy_rb"
    loader, attempted = build_loader(succeed_on: [ unversioned_path ])

    TantivyRb.load_native_extension(loader: loader)

    assert_equal [ versioned_path, unversioned_path ], attempted
  end

  def test_raises_descriptive_error_when_both_paths_fail
    loader, _attempted = build_loader(succeed_on: [])

    error = assert_raises(LoadError) do
      TantivyRb.load_native_extension(loader: loader)
    end

    assert_includes error.message, "Failed to load tantivy_rb native extension"
    assert_includes error.message, "Tried #{RUBY_VERSION[/\d+\.\d+/]}/ and unversioned paths"
  end

  private

  def build_loader(succeed_on:)
    attempted = []
    loader = ->(path) {
      attempted << path
      raise LoadError, "cannot load such file -- #{path}" unless succeed_on.include?(path)
      true
    }
    [ loader, attempted ]
  end
end
