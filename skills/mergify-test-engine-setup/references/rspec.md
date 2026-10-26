# RSpec: `rspec-mergify`

## Requirements

- Ruby 3.1 or later, `rspec-core` 3.12 or later.

## Install

Add the gem to the test group of the `Gemfile` (or as a development dependency
in the gemspec) and run `bundle install` with the Bundler version the
`Gemfile.lock` was written with (its `BUNDLED WITH` line):

```ruby
group :test do
  gem 'rspec-mergify'
end
```

**Bundler does not load this gem by itself**: its entry file is
`rspec_mergify`, which `Bundler.require` does not look for, and a plain
`bundle exec rspec` never calls `Bundler.require`. Require it in `.rspec`:

```text
--require rspec_mergify
```

Without it, RSpec runs as usual and nothing reaches Test Engine.

**Matrix with Rubies older than 3.1**: gate both the gem and the require on the
Ruby version, and require it from `spec/spec_helper.rb` rather than `.rspec`
(a `.rspec` require fails where the gem is absent):

```ruby
# Gemfile
gem 'rspec-mergify' if RUBY_VERSION >= '3.1'
```

```ruby
# spec/spec_helper.rb
require 'rspec_mergify' if RUBY_VERSION >= '3.1'
```

Check that a frozen `bundle install` still passes on the older Ruby, and say in
the pull request which legs cannot report.

## CI

- GitHub Actions: `MERGIFY_TOKEN: ${{ secrets.MERGIFY_TOKEN }}` in the `env` of
  the step that runs RSpec.
- Matrix: `MERGIFY_TEST_JOB_NAME` per leg. Shards of one suite (a
  `ci_node_index` matrix, Knapsack, `parallel_tests`): keep one name for all
  shards; the default (the job id) already does.
- No `continue-on-error`: quarantine is applied inside RSpec.
- An empty or missing `MERGIFY_TOKEN` (fork pull requests) makes the gem warn
  that the token is not set; tests are not affected.

## Variables

| Variable | Purpose |
|---|---|
| `MERGIFY_TOKEN` | the `ci` application key |
| `MERGIFY_TEST_JOB_NAME` | the job name to report under |
| `MERGIFY_CI_ENABLE` | report even when `CI` is not set (was `RSPEC_MERGIFY_ENABLE`) |
| `MERGIFY_CI_DEBUG` | print the spans instead of uploading them (was `RSPEC_MERGIFY_DEBUG`) |

Check: `MERGIFY_CI_ENABLE=true MERGIFY_CI_DEBUG=true bundle exec rspec`
prints a `Mergify CI:` section.
