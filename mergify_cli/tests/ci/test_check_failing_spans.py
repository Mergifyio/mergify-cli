import pytest
import respx

from mergify_cli.ci.cli import check_failing_spans_with_quarantine


API_MERGIFY_BASE_URL = "https://api.mergify.com"


@pytest.mark.respx(base_url=API_MERGIFY_BASE_URL)
async def test_status_code_resp_not_200(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.post("/v1/ci/foo/repositories/bar/quarantines/check").respond(
        422,
        json={"detail": "No subscription"},
    )

    with pytest.raises(SystemExit) as exc:
        await check_failing_spans_with_quarantine(
            API_MERGIFY_BASE_URL,
            "token",
            "foo/bar",
            "main",
            ["test_stuff.py::foo"],
        )

    assert exc.value.code == 1


@pytest.mark.respx(base_url=API_MERGIFY_BASE_URL)
async def test_no_failing_tests_quarantined(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.post("/v1/ci/foo/repositories/bar/quarantines/check").respond(
        200,
        json={
            "quarantined_tests_names": [],
            "non_quarantined_tests_names": ["test_me.py::test_mee"],
        },
    )

    with pytest.raises(SystemExit) as exc:
        await check_failing_spans_with_quarantine(
            API_MERGIFY_BASE_URL,
            "token",
            "foo/bar",
            "main",
            ["test_me.py::test_mee"],
        )

    assert exc.value.code == 1


@pytest.mark.respx(base_url=API_MERGIFY_BASE_URL)
async def test_some_failing_tests_quarantined(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.post("/v1/ci/foo/repositories/bar/quarantines/check").respond(
        200,
        json={
            "quarantined_tests_names": ["test_me.py::test_me2"],
            "non_quarantined_tests_names": ["test_me.py::test_me1"],
        },
    )

    with pytest.raises(SystemExit) as exc:
        await check_failing_spans_with_quarantine(
            API_MERGIFY_BASE_URL,
            "token",
            "foo/bar",
            "main",
            [
                "test_me.py::test_me1",
                "test_me.py::test_me2",
            ],
        )

    assert exc.value.code == 1


@pytest.mark.respx(base_url=API_MERGIFY_BASE_URL)
async def test_all_failing_tests_quarantined(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.post("/v1/ci/foo/repositories/bar/quarantines/check").respond(
        200,
        json={
            "quarantined_tests_names": [
                "test_me.py::test_me1",
                "test_me.py::test_me2",
            ],
            "non_quarantined_tests_names": [],
        },
    )

    await check_failing_spans_with_quarantine(
        API_MERGIFY_BASE_URL,
        "token",
        "foo/bar",
        "main",
        [
            "test_me.py::test_me1",
            "test_me.py::test_me2",
        ],
    )
