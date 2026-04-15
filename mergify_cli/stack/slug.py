#
#  Copyright © 2021-2026 Mergify SAS
#
# Licensed under the Apache License, Version 2.0 (the "License"); you may
# not use this file except in compliance with the License. You may obtain
# a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
# WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the
# License for the specific language governing permissions and limitations
# under the License.
from __future__ import annotations

import re


_CONVENTIONAL_COMMIT_RE = re.compile(
    r"^[a-z]+(?:\([^)]*\))?[!]?:\s*",
    re.IGNORECASE,
)

ABBREVIATIONS: dict[str, str] = {
    "application": "app",
    "applications": "apps",
    "authentification": "auth",
    "authentication": "auth",
    "authorization": "authz",
    "command": "cmd",
    "commands": "cmds",
    "configuration": "config",
    "connection": "conn",
    "connections": "conns",
    "dependency": "dep",
    "dependencies": "deps",
    "description": "desc",
    "development": "dev",
    "directory": "dir",
    "documentation": "docs",
    "environment": "env",
    "environments": "envs",
    "function": "func",
    "functions": "funcs",
    "generation": "gen",
    "implement": "impl",
    "implementation": "impl",
    "information": "info",
    "initialization": "init",
    "library": "lib",
    "libraries": "libs",
    "management": "mgmt",
    "message": "msg",
    "messages": "msgs",
    "middleware": "mw",
    "notification": "notif",
    "notifications": "notifs",
    "number": "num",
    "package": "pkg",
    "packages": "pkgs",
    "parameter": "param",
    "parameters": "params",
    "performance": "perf",
    "production": "prod",
    "property": "prop",
    "properties": "props",
    "reference": "ref",
    "references": "refs",
    "repository": "repo",
    "repositories": "repos",
    "request": "req",
    "requests": "reqs",
    "response": "resp",
    "responses": "resps",
    "specification": "spec",
    "specifications": "specs",
    "statistics": "stats",
    "subscription": "sub",
    "subscriptions": "subs",
    "synchronization": "sync",
    "temporary": "tmp",
    "transaction": "tx",
    "transactions": "txs",
    "utilities": "utils",
    "utility": "util",
    "validation": "val",
    "variable": "var",
    "variables": "vars",
}

STOP_WORDS: frozenset[str] = frozenset(
    {
        "a",
        "an",
        "the",
        "and",
        "or",
        "but",
        "in",
        "on",
        "at",
        "to",
        "for",
        "of",
        "with",
        "by",
        "from",
        "is",
        "are",
        "this",
        "that",
        "it",
        "its",
        "into",
        "as",
        "so",
        "be",
        "was",
        "were",
        "not",
        "no",
        "has",
        "have",
        "had",
        "will",
        "would",
        "can",
        "could",
        "should",
        "do",
        "does",
        "did",
        "just",
        "also",
        "when",
        "where",
        "how",
        "if",
        "then",
        "than",
        "more",
        "some",
        "all",
        "each",
        "every",
        "any",
        "both",
        "about",
        "between",
        "through",
        "during",
        "before",
        "after",
        "up",
        "out",
        "new",
    },
)

_MAX_SLUG_LENGTH = 50
_SHORT_HASH_LENGTH = 8


def slugify_title(title: str, changeid: str) -> str:
    """Convert a commit title and Change-Id into a branch-name slug.

    Returns ``{slug}--{hex8}`` where *slug* is derived from *title*
    and *hex8* is the first 8 hex characters of *changeid* (without
    the ``I`` prefix).
    """
    # 1. Strip conventional commit prefix
    text = _CONVENTIONAL_COMMIT_RE.sub("", title)

    # 2. Split into words, apply abbreviations, remove stop words
    # First split on whitespace, then on non-alphanumeric chars within each token
    words: list[str] = []
    for token in text.split():
        # Split token on non-alphanumeric boundaries (e.g. foo_bar -> foo, bar)
        sub_tokens = re.split(r"[^a-zA-Z0-9]+", token)
        for sub in sub_tokens:
            if not sub:
                continue
            lower = sub.lower()
            abbreviated = ABBREVIATIONS.get(lower, lower)
            if abbreviated and abbreviated not in STOP_WORDS:
                words.append(abbreviated)

    # 3. Join (all tokens are already alphanumeric) and slugify
    slug = "-".join(words)
    slug = re.sub(r"[^a-z0-9-]", "-", slug)
    slug = re.sub(r"-{2,}", "-", slug)
    slug = slug.strip("-")

    # 4. Truncate at word boundary
    if len(slug) > _MAX_SLUG_LENGTH:
        truncated = slug[:_MAX_SLUG_LENGTH]
        last_hyphen = truncated.rfind("-")
        slug = truncated[:last_hyphen] if last_hyphen > 0 else truncated
    slug = slug.strip("-")

    # 5. Fallback
    if not slug:
        slug = "change"

    # 6. Append short Change-Id (strip I prefix)
    hex_suffix = changeid[1 : 1 + _SHORT_HASH_LENGTH]
    return f"{slug}--{hex_suffix}"
