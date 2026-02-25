# Mergify CLI

Mergify CLI is a tool that automates the creation and management of stacked
pull requests on GitHub as well as handling CI results upload.

## Features

### Stacked Pull Requests

Create and manage stacked pull requests to break down large changes into
smaller, reviewable pieces. Learn more in the
[documentation](https://docs.mergify.com/stacks/).

### CI Insights

Upload and analyze CI results to get better visibility into your CI pipeline.
Learn more about [CI Insights in the
documentation](https://docs.mergify.com/ci-insights/).

## Installation

```shell
pip install mergify-cli
```

## Usage

```shell
# Show available commands
mergify --help

# Manage stacked pull requests
mergify stack --help

# Upload CI results
mergify ci --help
```

## Contributing

We welcome and appreciate contributions from the open-source community to make
this project better. Whether you're a developer, designer, tester, or just
someone with a good idea, we encourage you to get involved.

## License

This project is licensed under the Apache License 2.0 - see the
[LICENSE](LICENSE) file for details.
