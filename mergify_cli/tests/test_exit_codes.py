from __future__ import annotations

from mergify_cli.exit_codes import ExitCode


class TestExitCode:
    def test_values_are_unique(self) -> None:
        values = [m.value for m in ExitCode.__members__.values()]
        assert len(values) == len(set(values))

    def test_success_is_zero(self) -> None:
        assert int(ExitCode.SUCCESS) == 0

    def test_generic_error_is_one(self) -> None:
        assert int(ExitCode.GENERIC_ERROR) == 1

    def test_code_2_reserved_for_click(self) -> None:
        """Code 2 is reserved for Click usage errors and should not be in the enum."""
        assert 2 not in [e.value for e in ExitCode]

    def test_all_codes_are_small_integers(self) -> None:
        for code in ExitCode:
            assert 0 <= code <= 127

    def test_int_compatibility(self) -> None:
        """ExitCode values work with sys.exit() as plain ints."""
        assert int(ExitCode.STACK_NOT_FOUND) == 3
        assert int(ExitCode.CONFLICT) == 4
        assert int(ExitCode.GITHUB_API_ERROR) == 5
        assert int(ExitCode.MERGIFY_API_ERROR) == 6
        assert int(ExitCode.INVALID_STATE) == 7
        assert int(ExitCode.CONFIGURATION_ERROR) == 8
