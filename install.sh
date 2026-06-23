#!/bin/sh
# Install the `mergify` CLI from a GitHub Release.
#
# After installing, prints the commands to enable shell completions
# for the detected shell. It does not write them itself — matching
# rustup/starship, the installer drops a single binary and leaves the
# user's shell dirs untouched.
#
# Default usage:
#
#   curl -fsSL https://raw.githubusercontent.com/Mergifyio/mergify-cli/main/install.sh | sh
#
# Environment overrides:
#
#   MERGIFY_INSTALL_DIR   Install directory (default: $HOME/.local/bin).
#   MERGIFY_VERSION       Release tag to install (default: latest).
#   MERGIFY_BASE_URL      Base URL for asset downloads (default: the
#                         GitHub Releases endpoint for Mergifyio/mergify-cli).
#                         Overriding this is how the install-script
#                         smoke test points at a local fixture server.
#
# POSIX sh — no bash-only constructs. The script is intentionally
# straightforward so a security-conscious user can `curl ... | less`
# before piping to `sh`.

set -eu

REPO="Mergifyio/mergify-cli"
INSTALL_DIR="${MERGIFY_INSTALL_DIR:-${HOME}/.local/bin}"
VERSION="${MERGIFY_VERSION:-latest}"

die() {
    printf 'error: %s\n' "$1" >&2
    exit 1
}

# Map `uname -s` / `uname -m` to the Rust target triple the release
# workflow tags its assets with. Anything we don't ship a binary for
# bails — no auto-build fallback because that would imply a working
# Rust toolchain on the user's machine, which defeats the point of a
# prebuilt binary installer.
detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "${os}" in
        Linux)  os_part="unknown-linux-gnu" ;;
        Darwin) os_part="apple-darwin"      ;;
        # git-bash, MSYS2 and Cygwin all report a `*_NT-*` kernel
        # name from `uname -s`; they run a POSIX shell on Windows.
        MINGW*|MSYS*|CYGWIN*) os_part="pc-windows-msvc" ;;
        *) die "unsupported OS '${os}' — see https://github.com/${REPO}/releases for available assets" ;;
    esac
    case "${arch}" in
        x86_64|amd64)  arch_part="x86_64"  ;;
        arm64|aarch64) arch_part="aarch64" ;;
        *) die "unsupported architecture '${arch}'" ;;
    esac
    # We only publish an x86_64 Windows binary, so reject other
    # Windows arches up front instead of 404ing on an asset that
    # was never built. x86_64 git-bash runs fine on ARM Windows
    # under emulation, so this rarely bites in practice.
    if [ "${os_part}" = "pc-windows-msvc" ] && [ "${arch_part}" != "x86_64" ]; then
        die "unsupported architecture '${arch}' on Windows — only x86_64 is published"
    fi
    printf '%s-%s' "${arch_part}" "${os_part}"
}

# `sha256sum` (GNU coreutils) on Linux, `shasum -a 256` on macOS.
# Tests for both at use-site instead of caching because we already
# have at most one fetch round-trip; cost is negligible.
sha256_check() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum -c "$1"
    elif command -v shasum > /dev/null 2>&1; then
        shasum -a 256 -c "$1"
    else
        die "neither sha256sum nor shasum found — install one and retry"
    fi
}

# Print the commands to enable tab completion for the detected shell.
# We print rather than write: matching rustup/starship, the installer
# stays a single binary drop and never touches the user's shell dirs.
# Auto-writing is also unreliable — ~/.zfunc is off zsh's $fpath by
# default — so the user runs `mergify completions <shell>` themselves.
print_completion_hint() {
    shell_name=$(basename "${SHELL:-}")
    case "${shell_name}" in
        bash)
            printf '\nTo enable shell completions for bash:\n'
            printf '  mkdir -p ~/.local/share/bash-completion/completions\n'
            printf '  mergify completions bash > ~/.local/share/bash-completion/completions/mergify\n'
            ;;
        zsh)
            printf '\nTo enable shell completions for zsh:\n'
            printf '  mkdir -p ~/.zfunc && mergify completions zsh > ~/.zfunc/_mergify\n'
            printf '  # then add to ~/.zshrc:  fpath+=~/.zfunc; autoload -Uz compinit; compinit\n'
            ;;
        fish)
            printf '\nTo enable shell completions for fish:\n'
            printf '  mergify completions fish > ~/.config/fish/completions/mergify.fish\n'
            ;;
        *)
            printf '\nTo enable shell completions:\n'
            printf '  mergify completions <bash|zsh|fish|elvish|powershell>\n'
            ;;
    esac
}

main() {
    command -v curl > /dev/null 2>&1 || die "curl is required"

    # Resolve VERSION to the actual tag so we can embed it in the
    # asset filename. When MERGIFY_BASE_URL is set (fixture mode used
    # by the CI smoke test) the fixture already serves a
    # `latest-release.json` stub; otherwise resolve against GitHub.
    if [ "${VERSION}" = "latest" ]; then
        if [ -n "${MERGIFY_BASE_URL:-}" ]; then
            # Fixture mode: the stub JSON lives next to the assets.
            VERSION=$(curl -fsSL "${MERGIFY_BASE_URL}/latest-release.json" \
                | grep -o '"tag_name":[[:space:]]*"[^"]*"' \
                | sed 's/.*"tag_name":[[:space:]]*"//; s/".*//')
        else
            # Follow the `releases/latest` redirect to its
            # `releases/tag/<tag>` target and read the tag off the
            # final URL. The `api.github.com` JSON endpoint is
            # unauthenticated and per-IP rate-limited, so it 403s on
            # shared CI runner IPs; the plain github.com redirect has
            # no such limit and needs no API token.
            # A repo with no releases redirects to `.../releases`
            # (HTTP 200, no `/tag/`), so require the segment before
            # stripping rather than letting the full URL flow into the
            # asset name.
            VERSION=$(curl -fsSL -o /dev/null -w '%{url_effective}' \
                "https://github.com/${REPO}/releases/latest")
            case "${VERSION}" in
                */tag/*) VERSION="${VERSION##*/tag/}" ;;
                *)       VERSION="" ;;
            esac
        fi
        [ -n "${VERSION}" ] || die "could not resolve latest release version"
    fi

    # With the version known, build the per-version asset name and
    # the base URL for this release.
    BASE_URL="${MERGIFY_BASE_URL:-https://github.com/${REPO}/releases/download/${VERSION}}"

    target=$(detect_target)
    # Windows releases ship a `.zip` holding `mergify.exe`; every
    # other target ships a `.tar.gz` holding `mergify`. Branch on the
    # triple the same way the release workflow does so the names line
    # up bit-for-bit with what it published.
    case "${target}" in
        *windows*) ext="zip";    bin="mergify.exe" ;;
        *)         ext="tar.gz"; bin="mergify"     ;;
    esac

    # Need the matching extractor available. `tar` is everywhere we
    # ship a tarball; `unzip` is the portable choice on the POSIX
    # shells that run on Windows (git-bash, MSYS2, Cygwin).
    case "${ext}" in
        zip) command -v unzip > /dev/null 2>&1 \
                || die "unzip is required to install the Windows binary" ;;
        *)   command -v tar   > /dev/null 2>&1 || die "tar is required" ;;
    esac

    asset="mergify-${VERSION}-${target}.${ext}"
    url="${BASE_URL}/${asset}"
    sums_url="${BASE_URL}/SHA256SUMS"

    tmp=$(mktemp -d)
    # POSIX-compatible cleanup. `trap` runs on EXIT regardless of
    # how the script exits (success, die, signal).
    trap 'rm -rf "${tmp}"' EXIT INT HUP TERM

    printf 'Downloading %s\n' "${url}"
    curl -fsSL "${url}" -o "${tmp}/${asset}"

    printf 'Downloading checksums\n'
    curl -fsSL "${sums_url}" -o "${tmp}/SHA256SUMS"

    # `SHA256SUMS` is one line per asset; pull ours out by exact
    # second-field match so the lookup can't accidentally pick up
    # a sibling entry (`awk '$2 == asset'` does literal comparison
    # — using `grep " ${asset}$"` would let `.tar.gz`'s `.`s match
    # any char and weaken the fail-closed guarantee). Then
    # validate the canonical line shape (64 hex chars + two spaces
    # + filename) before handing it to sha256sum: GNU
    # `sha256sum -c` only *warns* on a malformed entry and still
    # exits 0 if no real mismatch was found, which would let a
    # corrupted SHA256SUMS slip past the check.
    printf 'Verifying checksum\n'
    cd "${tmp}"
    awk -v asset="${asset}" '$2 == asset { print; found=1 } END { exit !found }' \
        SHA256SUMS > scoped.sums \
        || die "no checksum entry for ${asset} in SHA256SUMS"
    grep -qE '^[0-9a-fA-F]{64}  ' scoped.sums \
        || die "malformed checksum entry for ${asset} in SHA256SUMS"
    sha256_check scoped.sums > /dev/null \
        || die "checksum verification failed for ${asset}"
    cd - > /dev/null

    case "${ext}" in
        zip) unzip -oq "${tmp}/${asset}" -d "${tmp}" ;;
        *)   tar -xzf "${tmp}/${asset}" -C "${tmp}"  ;;
    esac

    mkdir -p "${INSTALL_DIR}"
    # `install -m 0755` is in coreutils on Linux and BSD install on
    # macOS; both honour `-m`. Avoids a separate chmod step. On
    # Windows git-bash the mode is cosmetic (the exec bit is
    # meaningless there) but `install` still copies the file.
    install -m 0755 "${tmp}/${bin}" "${INSTALL_DIR}/${bin}"

    printf '\nmergify installed to %s/%s\n' "${INSTALL_DIR}" "${bin}"
    "${INSTALL_DIR}/${bin}" --version

    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *) printf '\nWarning: %s is not on your PATH.\n' "${INSTALL_DIR}"
           # The `$PATH` reference here is meant to land verbatim in
           # the user's shell config — they expand it, not us.
           # shellcheck disable=SC2016
           printf 'Add it to your shell config:  export PATH="%s:$PATH"\n' "${INSTALL_DIR}" ;;
    esac

    print_completion_hint
}

main "$@"
