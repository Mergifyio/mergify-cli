# Security Policy

This policy covers the `mergify` CLI, its installer (`install.sh`), and the
agent skills and plugins published in this repository.

## Reporting a vulnerability

Please do not report security vulnerabilities through public GitHub issues,
discussions or pull requests.

Report them privately through Mergify's bug bounty program on HackerOne:
<https://hackerone.com/mergify>. For other security questions, write to
<security@mergify.com>.

A useful report says which version you ran (`mergify --version`), how to
reproduce the issue and what an attacker gains from it.

Mergify's vulnerability disclosure policy is published at
<https://docs.mergify.com/security/#vulnerability-disclosure>.

## Supported versions

Security fixes ship in a new release of the CLI. `mergify self-update`,
the installer and the Homebrew tap all install the latest release.
