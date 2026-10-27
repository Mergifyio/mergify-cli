# Minitest: `minitest-mergify`

## Requirements

- Ruby 3.1 or later with Minitest 5.19 or later; Minitest 6 needs Ruby 3.2.
- It runs on `mergify-ci-core`, which Bundler installs with it, precompiled for
  CRuby 3.1 up to the latest release (not a development `ruby-head`) on Linux
  (glibc 2.30 or newer, and musl), macOS and Windows. Where Bundler takes the
  source gem instead (JRuby, TruffleRuby, a Ruby newer than the core, a job that
  sets `force_ruby_platform`), it has no compiled core and sends nothing: gate
  the gem on `RUBY_ENGINE == "ruby"` too, and list those legs as cannot
  report.

## Install

Add the gem to the test group of the `Gemfile`, and run `bundle install` with
the Bundler version of the `Gemfile.lock` (its `BUNDLED WITH` line):

```ruby
group :test do
  gem 'minitest-mergify'
end
```

Then require it where the suite sets up Minitest, before its tests are
defined, usually `test/test_helper.rb`:

```ruby
require 'minitest/mergify'
```

Minitest 6 loads only the plugins a suite asks for, so the line is needed
there; keep it with Minitest 5 too.

**Matrix with Rubies older than 3.1**: gate the gem and the require on the Ruby
version, check that a frozen `bundle install` still passes there, and say in
the pull request which legs cannot report:

```ruby
gem 'minitest-mergify' if RUBY_ENGINE == 'ruby' && RUBY_VERSION >= '3.1'      # Gemfile
require 'minitest/mergify' if RUBY_ENGINE == 'ruby' && RUBY_VERSION >= '3.1'  # test helper
```

- It covers tests in classes and in `describe` blocks, run in one process, in
  Minitest's threads (`parallelize_me!`) or in Rails' parallel workers.
- A quarantined test's failure counts as a skip, so the run is not failed by
  it: no `continue-on-error`.

## CI

- GitHub Actions: `MERGIFY_TOKEN: ${{ secrets.MERGIFY_TOKEN }}` on the step
  that runs the tests (`bin/rails test`, `rake test`, …).
- Matrix: `MERGIFY_TEST_JOB_NAME` per leg.

## Variables

| Variable | Purpose |
|---|---|
| `MERGIFY_TOKEN` | the `ci` application key |
| `MERGIFY_TEST_JOB_NAME` | the job name to report under |
| `MERGIFY_CI_ENABLE` | report even when `CI` is not set |
| `MERGIFY_CI_DEBUG` | print the spans instead of sending them |

Check: `MERGIFY_CI_ENABLE=true MERGIFY_CI_DEBUG=true bundle exec rake test`
(or `bin/rails test`) prints the spans and a `Mergify CI` summary.
