#!/usr/bin/env python3
"""check-matrix-schema.py -- this repository's harness config, against the harness's own schemas.

The runner deserialises a scenario with a catch-all for unknown keys, so a
matrix the harness's schema rejects still runs. The schema is the contract,
and the runner is merely the looser of the two: this check holds the matrix
to the schema so the two cannot drift apart again unnoticed.

It does not restate any rule. It loads fs-windows-test-harness's
tests/validate-configs.py from the sibling checkout (at HARNESS_REF in
chores.yml) and runs its `consumer_errors` on this repository, which checks
fs-windows-test-harness.toml against schemas/harness.schema.json,
test-matrix.json against schemas/test-matrix.schema.json, and that every
recipe op is declared in [ops].

A missing sibling or a missing `jsonschema` is a failure naming what would
provide it, never a skip.

Needs Python 3.11+ (tomllib) and the `jsonschema` package.
"""

import importlib.util
import pathlib
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
HARNESS = REPO.parent / "fs-windows-test-harness"
VALIDATOR = HARNESS / "tests" / "validate-configs.py"


def main():
    if not VALIDATOR.is_file():
        print(
            f"check-matrix-schema: {VALIDATOR} not found; "
            "run `chore siblings` to check out fs-windows-test-harness",
            file=sys.stderr,
        )
        return 1

    spec = importlib.util.spec_from_file_location("validate_configs", VALIDATOR)
    module = importlib.util.module_from_spec(spec)
    try:
        spec.loader.exec_module(module)
    except ModuleNotFoundError as e:
        print(
            f"check-matrix-schema: {e}; install it with `pip install jsonschema`",
            file=sys.stderr,
        )
        return 1

    errors = module.consumer_errors(REPO)
    if errors:
        print(f"check-matrix-schema: {len(errors)} error(s)")
        for e in errors:
            print(f"  {e}")
        return 1

    print("check-matrix-schema: fs-windows-test-harness.toml and test-matrix.json are valid")
    return 0


if __name__ == "__main__":
    sys.exit(main())
