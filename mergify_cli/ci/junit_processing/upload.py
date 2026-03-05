from __future__ import annotations

import typing

from opentelemetry.exporter.otlp.proto.http import Compression
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
from opentelemetry.sdk.trace import export

from mergify_cli import console


if typing.TYPE_CHECKING:
    from opentelemetry.sdk.trace import ReadableSpan


class UploadError(Exception):
    pass


class _OTLPSpanExporterWithBody(OTLPSpanExporter):
    last_failure_status: int | None = None
    last_failure_body: str | None = None

    def _export(
        self,
        serialized_data: bytes,
        timeout_sec: float | None = None,
    ) -> typing.Any:
        resp = super()._export(serialized_data, timeout_sec)
        if not resp.ok:
            self.last_failure_status = resp.status_code
            self.last_failure_body = resp.text
        return resp


def upload_spans(
    api_url: str,
    token: str,
    repository: str,
    spans: list[ReadableSpan],
) -> None:
    exporter = _OTLPSpanExporterWithBody(
        endpoint=f"{api_url}/v1/repos/{repository}/ci/traces",
        headers={"Authorization": f"Bearer {token}"},
        compression=Compression.Gzip,
    )
    result = exporter.export(spans)

    if result == export.SpanExportResult.FAILURE:
        if exporter.last_failure_status is not None:
            raise UploadError(
                f"Failed to export span batch code: {exporter.last_failure_status}, "
                f"reason: {exporter.last_failure_body}",
            )
        raise UploadError("Failed to export span batch")


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
