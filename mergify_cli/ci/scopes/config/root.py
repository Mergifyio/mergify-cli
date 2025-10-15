import pathlib
import typing

import pydantic
import yaml

from mergify_cli.ci.scopes import exceptions
from mergify_cli.ci.scopes.config.scopes import Scopes


class ConfigInvalidError(exceptions.ScopesError):
    pass


class Config(pydantic.BaseModel):
    model_config = pydantic.ConfigDict(extra="ignore")

    scopes: Scopes

    @classmethod
    def from_dict(
        cls,
        data: dict[str, typing.Any] | typing.Any,  # noqa: ANN401
    ) -> typing.Self:
        try:
            return cls.model_validate(data)
        except pydantic.ValidationError as e:
            raise ConfigInvalidError(e)

    @classmethod
    def from_yaml(cls, path: str) -> typing.Self:
        with pathlib.Path(path).open(encoding="utf-8") as f:
            try:
                data = yaml.safe_load(f) or {}
            except yaml.YAMLError as e:
                raise ConfigInvalidError(e)

            return cls.from_dict(data)
