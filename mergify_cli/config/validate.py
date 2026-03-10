from __future__ import annotations

import dataclasses
import pathlib
import typing

import yaml


if typing.TYPE_CHECKING:
    import httpx


SCHEMA_URL = "https://docs.mergify.com/mergify-configuration-schema.json"


@dataclasses.dataclass
class ValidationError:
    path: str
    message: str


@dataclasses.dataclass
class ValidationResult:
    errors: list[ValidationError]

    @property
    def is_valid(self) -> bool:
        return len(self.errors) == 0


def load_yaml(path: str) -> dict[str, typing.Any]:
    with pathlib.Path(path).open(encoding="utf-8") as f:
        data = yaml.safe_load(f)
    if data is None:
        return {}
    if not isinstance(data, dict):
        msg = f"Expected a YAML mapping at the top level, got {type(data).__name__}"
        raise TypeError(msg)
    return data


def read_raw(path: str) -> str:
    return pathlib.Path(path).read_text(encoding="utf-8")


def fetch_schema(client: httpx.Client) -> dict[str, typing.Any]:
    response = client.get(SCHEMA_URL)
    response.raise_for_status()
    return response.json()  # type: ignore[no-any-return]


def validate_config(
    config: dict[str, typing.Any],
    schema: dict[str, typing.Any],
) -> ValidationResult:
    import jsonschema

    validator_cls = jsonschema.validators.validator_for(schema)
    validator = validator_cls(schema)
    errors = []
    for error in sorted(
        validator.iter_errors(config),
        key=lambda e: [str(p) for p in e.path],
    ):
        path = ".".join(str(p) for p in error.absolute_path) or "(root)"
        errors.append(ValidationError(path=path, message=error.message))
    return ValidationResult(errors=errors)


@dataclasses.dataclass
class SimulatorResult:
    success: bool
    message: str


async def simulate_config(
    client: httpx.AsyncClient,
    repository: str,
    mergify_yml: str,
) -> SimulatorResult:
    import httpx as httpx_mod

    try:
        response = await client.post(
            f"/v1/repos/{repository}/configuration-simulator",
            json={"mergify_yml": mergify_yml},
        )
    except httpx_mod.HTTPStatusError as e:
        if e.response.status_code == 422:
            try:
                data = e.response.json()
                detail = data.get("detail", str(data))
            except ValueError:
                detail = e.response.text
            return SimulatorResult(success=False, message=str(detail))
        raise

    data = response.json()
    return SimulatorResult(success=True, message=data["message"])
