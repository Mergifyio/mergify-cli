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

import pytest

from mergify_cli.stack.slug import slugify_title


@pytest.mark.parametrize(
    ("title", "change_id", "expected"),
    [
        # Basic slugification
        (
            "Add user model",
            "I29617d37762fd69809c255d7e7073cb11f8fbf50",
            "add-user-model--29617d37",
        ),
        # Conventional commit prefix stripped
        (
            "feat(stack): improve comment design",
            "I29617d37762fd69809c255d7e7073cb11f8fbf50",
            "improve-comment-design--29617d37",
        ),
        # Conventional commit without scope
        (
            "fix: handle missing change id",
            "I29617d37762fd69809c255d7e7073cb11f8fbf50",
            "handle-missing-change-id--29617d37",
        ),
        # Stop words removed
        (
            "Fix the bug in the parser",
            "I29617d37762fd69809c255d7e7073cb11f8fbf50",
            "fix-bug-parser--29617d37",
        ),
        # Abbreviations applied
        (
            "Add user authentication model",
            "I29617d37762fd69809c255d7e7073cb11f8fbf50",
            "add-user-auth-model--29617d37",
        ),
        # Multiple abbreviations
        (
            "Implement repository synchronization",
            "I29617d37762fd69809c255d7e7073cb11f8fbf50",
            "impl-repo-sync--29617d37",
        ),
        # Non-ASCII characters replaced
        (
            "Fix l'authentification du modèle",
            "I29617d37762fd69809c255d7e7073cb11f8fbf50",
            "fix-l-auth-du-mod-le--29617d37",
        ),
        # Special characters replaced
        (
            "Add foo_bar & baz.qux",
            "I29617d37762fd69809c255d7e7073cb11f8fbf50",
            "add-foo-bar-baz-qux--29617d37",
        ),
        # Short title
        (
            "fix typo",
            "I29617d37762fd69809c255d7e7073cb11f8fbf50",
            "fix-typo--29617d37",
        ),
        # Fallback: only stop words
        (
            "It is the",
            "I29617d37762fd69809c255d7e7073cb11f8fbf50",
            "change--29617d37",
        ),
        # Fallback: empty after processing
        (
            "feat: ",
            "I29617d37762fd69809c255d7e7073cb11f8fbf50",
            "change--29617d37",
        ),
    ],
)
def test_slugify_title(title: str, change_id: str, expected: str) -> None:
    assert slugify_title(title, change_id) == expected


def test_slugify_title_truncation() -> None:
    long_title = "word " * 20  # 100 chars of words
    result = slugify_title(long_title, "I29617d37762fd69809c255d7e7073cb11f8fbf50")
    # Slug part (before --) should be <= 50 chars
    slug_part = result.rsplit("--", 1)[0]
    assert len(slug_part) <= 50
    # Should end with the hex suffix
    assert result.endswith("--29617d37")
    # Should not end slug part with a hyphen
    assert not slug_part.endswith("-")


def test_slugify_title_different_change_ids() -> None:
    title = "Add feature"
    result1 = slugify_title(title, "Iaaaaaaa0762fd69809c255d7e7073cb11f8fbf50")
    result2 = slugify_title(title, "Ibbbbbbbb762fd69809c255d7e7073cb11f8fbf50")
    assert result1 == "add-feature--aaaaaaa0"
    assert result2 == "add-feature--bbbbbbbb"
    assert result1 != result2
