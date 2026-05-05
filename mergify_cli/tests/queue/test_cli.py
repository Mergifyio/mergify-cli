from __future__ import annotations

import datetime
from unittest.mock import patch

from mergify_cli.queue.cli import _relative_time


class TestRelativeTime:
    def test_seconds(self) -> None:
        now = datetime.datetime(2025, 1, 1, 12, 0, 30, tzinfo=datetime.UTC)
        with patch("mergify_cli.queue.cli.datetime") as mock_dt:
            mock_dt.datetime.now.return_value = now
            mock_dt.datetime.fromisoformat = datetime.datetime.fromisoformat
            mock_dt.UTC = datetime.UTC
            assert _relative_time("2025-01-01T12:00:00Z") == "30s ago"

    def test_minutes(self) -> None:
        now = datetime.datetime(2025, 1, 1, 12, 5, 0, tzinfo=datetime.UTC)
        with patch("mergify_cli.queue.cli.datetime") as mock_dt:
            mock_dt.datetime.now.return_value = now
            mock_dt.datetime.fromisoformat = datetime.datetime.fromisoformat
            mock_dt.UTC = datetime.UTC
            assert _relative_time("2025-01-01T12:00:00Z") == "5m ago"

    def test_hours(self) -> None:
        now = datetime.datetime(2025, 1, 1, 14, 0, 0, tzinfo=datetime.UTC)
        with patch("mergify_cli.queue.cli.datetime") as mock_dt:
            mock_dt.datetime.now.return_value = now
            mock_dt.datetime.fromisoformat = datetime.datetime.fromisoformat
            mock_dt.UTC = datetime.UTC
            assert _relative_time("2025-01-01T12:00:00Z") == "2h ago"

    def test_days(self) -> None:
        now = datetime.datetime(2025, 1, 4, 12, 0, 0, tzinfo=datetime.UTC)
        with patch("mergify_cli.queue.cli.datetime") as mock_dt:
            mock_dt.datetime.now.return_value = now
            mock_dt.datetime.fromisoformat = datetime.datetime.fromisoformat
            mock_dt.UTC = datetime.UTC
            assert _relative_time("2025-01-01T12:00:00Z") == "3d ago"

    def test_future(self) -> None:
        now = datetime.datetime(2025, 1, 1, 12, 0, 0, tzinfo=datetime.UTC)
        with patch("mergify_cli.queue.cli.datetime") as mock_dt:
            mock_dt.datetime.now.return_value = now
            mock_dt.datetime.fromisoformat = datetime.datetime.fromisoformat
            mock_dt.UTC = datetime.UTC
            assert _relative_time("2025-01-01T12:30:00Z", future=True) == "~30m"

    def test_none(self) -> None:
        assert not _relative_time(None)

    def test_empty(self) -> None:
        assert not _relative_time("")
