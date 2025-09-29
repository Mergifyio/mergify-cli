import contextlib
import io
import logging
import typing

from opentelemetry.exporter.otlp.proto.http import Compression
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
from opentelemetry.sdk.trace import ReadableSpan
from opentelemetry.sdk.trace import export

from mergify_cli import console


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


def upload(
    api_url: str,
    token: str,
    repository: str,
    spans: list[ReadableSpan],
) -> None:
    console.log("")
    console.log("☁️ Upload")
    console.log(f"• Owner/Repo: {repository}")
    if spans:
        try:
            upload_spans(api_url, token, repository, spans)
        except UploadError as e:
            console.log(f"• ❌ Error uploading spans: {e}", style="red")
        else:
            console.log("• [green]🎉 File(s) uploaded[/]")
    else:
        console.log("• [orange]🟠 No tests were detected in the JUnit file(s)[/]")
