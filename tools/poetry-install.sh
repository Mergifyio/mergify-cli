#!/bin/bash

if [ "$CI" ]; then
    export POETRY_VIRTUALENVS_OPTIONS_NO_PIP=true
    export POETRY_VIRTUALENVS_OPTIONS_NO_SETUPTOOLS=true
    poetry install --sync --no-cache
else
    # NOTE: Outside the CI we keep pip/setuptools because most IDE
    # (pycharm/vscode) didn't yet support virtualenv without them installed.
    poetry install --sync
fi
