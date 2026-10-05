# /// script
# requires-python = "~=3.14.0"
# dependencies = ["regex==2026.9.10"]
# ///
"""Fill in the engine's verdict for every case in `engine_glob_cases.json`.

The verdicts are what the engine says, not what anyone expects it to say:
this runs the engine's own `mergify_engine/rules/globs.py` on Python 3.14
(the engine's version, which `glob.translate` comes from) and matches with
the `regex` module, the engine's judge of a pattern. Both pins follow
`engine/pyproject.toml`: bump them with it. Add a case with any
`engine` value, run this, and review the diff:

    uv run --no-project engine_glob_cases.py <monorepo>/engine/mergify_engine/rules/globs.py

Only `globs.py` is loaded. Its `regexp_engine` import is stubbed with
`regex.compile`, which is the backtracking engine `regexp_engine` always
compiles and whose answer its RE2 path is proven to give.
"""

import importlib.util
import json
import pathlib
import sys
import types

import regex

CASES = pathlib.Path(__file__).with_name("engine_glob_cases.json")


def load_globs(path: str) -> types.ModuleType:
    regexp_engine = types.ModuleType("mergify_engine.rules.regexp_engine")
    regexp_engine.compile_pattern = regex.compile  # type: ignore[attr-defined]
    rules = types.ModuleType("mergify_engine.rules")
    rules.regexp_engine = regexp_engine  # type: ignore[attr-defined]
    sys.modules["mergify_engine"] = types.ModuleType("mergify_engine")
    sys.modules["mergify_engine.rules"] = rules
    sys.modules["mergify_engine.rules.regexp_engine"] = regexp_engine
    spec = importlib.util.spec_from_file_location("mergify_engine.rules.globs", path)
    assert spec is not None and spec.loader is not None
    globs = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(globs)
    return globs


def verdict(globs: types.ModuleType, pattern: str, path: str) -> str:
    # `glob_patterns_to_regexes` is what a scope's `include` / `exclude`
    # compiles through, and `match` is how `filter.glob_match` asks.
    try:
        compiled = globs.glob_patterns_to_regexes((pattern,))
    except (globs.InvalidGlobPatternError, regex.error):
        return "invalid"
    return "match" if any(rx.match(path) for rx in compiled) else "miss"


def dump(cases: list[dict[str, str]]) -> str:
    lines = ",\n".join(f"  {json.dumps(case, ensure_ascii=False)}" for case in cases)
    return f"[\n{lines}\n]\n"


def main() -> None:
    globs = load_globs(sys.argv[1])
    cases = json.loads(CASES.read_text(encoding="utf-8"))
    for case in cases:
        case["engine"] = verdict(globs, case["pattern"], case["path"])
    CASES.write_text(dump(cases), encoding="utf-8")


if __name__ == "__main__":
    main()
