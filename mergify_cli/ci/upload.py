import contextlib
import io
import logging
import pathlib
import typing

from opentelemetry.exporter.otlp.proto.http import Compression
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
from opentelemetry.sdk.trace import ReadableSpan
from opentelemetry.sdk.trace import export

from mergify_cli import console
from mergify_cli.ci import detector
from mergify_cli.ci import junit


class UploadError(Exception):
    pass


@contextlib.contextmanager
def capture_log(logger: logging.Logger) -> typing.Generator[io.StringIO, None, None]:
    # Create a string stream to capture logs
    log_capture_string = io.StringIO()

    # Create a stream handler for the logger
    stream_handler = logging.StreamHandler(log_capture_string)
    stream_handler.setLevel(logging.WARNING)  # Set the desired logging level
    logger.setLevel(logging.WARNING)
    logger.addHandler(stream_handler)

    yield log_capture_string

    logger.removeHandler(stream_handler)
    stream_handler.close()


def upload_spans(
    api_url: str,
    token: str,
    repository: str,
    spans: list[ReadableSpan],
) -> None:
    exporter = OTLPSpanExporter(
        endpoint=f"{api_url}/v1/repos/{repository}/ci/traces",
        headers={"Authorization": f"Bearer {token}"},
        compression=Compression.Gzip,
    )
    with capture_log(
        logging.getLogger("opentelemetry.exporter.otlp.proto.http.trace_exporter"),
    ) as logstr:
        result = exporter.export(spans)

        if result == export.SpanExportResult.FAILURE:
            raise UploadError(logstr.getvalue())


def connect_traces(run_id: str) -> None:
    if detector.get_ci_provider() == "github_actions":
        console.print(
            f"::notice title=Mergify CI::MERGIFY_TEST_RUN_ID={run_id}",
            soft_wrap=True,
        )


async def upload(  # noqa: PLR0913, PLR0917
    api_url: str,
    token: str,
    repository: str,
    files: tuple[str, ...],
    test_language: str | None = None,
    test_framework: str | None = None,
) -> None:
    spans = []

    run_id = junit.ID_GENERATOR.generate_span_id().to_bytes(8, "big").hex()

    for filename in files:
        try:
            spans.extend(
                await junit.junit_to_spans(
                    run_id,
                    pathlib.Path(filename).read_bytes(),
                    test_language=test_language,
                    test_framework=test_framework,
                ),
            )
        except junit.InvalidJunitXMLError as e:
            console.log(
                f"Error converting JUnit XML file to spans: {e.details}",
                style="red",
            )

    if spans:
        try:
            upload_spans(api_url, token, repository, spans)
        except UploadError as e:
            console.log(f"Error uploading spans: {e}", style="red")
        else:
            connect_traces(run_id)
            console.log("[green]:tada: File(s) uploaded[/]")
    else:
        console.log("[orange]No tests were detected in the JUnit file(s)[/]")
