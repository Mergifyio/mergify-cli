from opentelemetry.sdk.trace import ReadableSpan
from opentelemetry.trace import Status
from opentelemetry.trace import StatusCode
import pytest
import respx

from mergify_cli.ci.quarantine import QuarantineFailedError
from mergify_cli.ci.quarantine import check_and_update_failing_spans


API_MERGIFY_BASE_URL = "https://api.mergify.com"


@pytest.mark.respx(base_url=API_MERGIFY_BASE_URL)
async def test_status_code_resp_not_200(
    respx_mock: respx.MockRouter,
) -> None:
    respx_mock.post("/v1/ci/foo/repositories/bar/quarantines/check").respond(
        422,
        json={"detail": "No subscription"},
    )

    spans = [
        ReadableSpan(
            name="test_stuff.py::foo",
            status=Status(status_code=StatusCode.ERROR, description=""),
            attributes={"cicd.test.quarantined": False, "test.scope": "case"},
        ),
    ]

    with pytest.raises(QuarantineFailedError):
        await check_and_update_failing_spans(
            API_MERGIFY_BASE_URL,
            "token",
            "foo/bar",
            "main",
            spans,
        )

    assert spans[0].attributes is not None
    assert "cicd.test.quarantined" in spans[0].attributes
    assert spans[0].attributes["cicd.test.quarantined"] is False


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
    spans = [
        ReadableSpan(
            name="test_me.py::test_mee",
            status=Status(status_code=StatusCode.ERROR, description=""),
            attributes={"test.scope": "case"},
        ),
    ]

    failed_tests_quarantined_test_count = await check_and_update_failing_spans(
        API_MERGIFY_BASE_URL,
        "token",
        "foo/bar",
        "main",
        spans,
    )
    assert failed_tests_quarantined_test_count == 1
    assert spans[0].attributes is not None
    assert "cicd.test.quarantined" in spans[0].attributes
    assert spans[0].attributes["cicd.test.quarantined"] is False


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

    spans = [
        ReadableSpan(
            name="test_me.py::test_me1",
            status=Status(status_code=StatusCode.ERROR, description=""),
            attributes={"test.scope": "case"},
        ),
        ReadableSpan(
            name="test_me.py::test_me2",
            status=Status(status_code=StatusCode.ERROR, description=""),
            attributes={"test.scope": "case"},
        ),
        ReadableSpan(
            name="test_me.py::test_me3",
            status=Status(status_code=StatusCode.OK, description=""),
            attributes={"test.scope": "case"},
        ),
        ReadableSpan(
            name="test_me.py::test_me4",
            status=Status(status_code=StatusCode.OK, description=""),
            attributes=None,
        ),
    ]

    failed_tests_quarantined_count = await check_and_update_failing_spans(
        API_MERGIFY_BASE_URL,
        "token",
        "foo/bar",
        "main",
        spans,
    )
    assert failed_tests_quarantined_count == 1

    assert spans[0].attributes is not None
    assert spans[1].attributes is not None
    assert spans[2].attributes is not None
    assert spans[3].attributes is not None

    assert "cicd.test.quarantined" in spans[0].attributes
    assert "cicd.test.quarantined" in spans[1].attributes
    assert "cicd.test.quarantined" in spans[2].attributes
    assert "cicd.test.quarantined" not in spans[3].attributes

    assert spans[0].attributes["cicd.test.quarantined"] is False
    assert spans[1].attributes["cicd.test.quarantined"] is True
    assert spans[2].attributes["cicd.test.quarantined"] is False


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
                "test_me.py::test_me3",
            ],
            "non_quarantined_tests_names": [],
        },
    )
    spans = [
        ReadableSpan(
            name="test_me.py::test_me1",
            status=Status(status_code=StatusCode.ERROR, description=""),
            attributes={"test.scope": "case"},
        ),
        ReadableSpan(
            name="test_me.py::test_me2",
            status=Status(status_code=StatusCode.ERROR, description=""),
            attributes={"test.scope": "case"},
        ),
        ReadableSpan(
            name="test_me.py::test_me3",
            status=Status(status_code=StatusCode.ERROR, description=""),
            attributes={"test.scope": "case"},
        ),
    ]

    failed_tests_quarantined_count = await check_and_update_failing_spans(
        API_MERGIFY_BASE_URL,
        "token",
        "foo/bar",
        "main",
        spans,
    )
    assert failed_tests_quarantined_count == 0

    assert spans[0].attributes is not None
    assert spans[1].attributes is not None
    assert spans[2].attributes is not None

    assert "cicd.test.quarantined" in spans[0].attributes
    assert "cicd.test.quarantined" in spans[1].attributes
    assert "cicd.test.quarantined" in spans[2].attributes

    assert spans[0].attributes["cicd.test.quarantined"] is True
    assert spans[1].attributes["cicd.test.quarantined"] is True
    assert spans[2].attributes["cicd.test.quarantined"] is True
