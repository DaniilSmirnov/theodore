"""python3 _pytest_lite.py TESTFILE: run every argument-less test_* function.

For checks that cannot install pytest: a stand-in `pytest` module with
`raises` (all the test files run this way use), then each test called in
file order. Exit 1 on the first failure.
"""

import contextlib
import importlib.util
import inspect
import sys
import traceback
import types

stub = types.ModuleType("pytest")


@contextlib.contextmanager
def raises(exc, match=None):
    try:
        yield
    except exc:
        return
    raise AssertionError(f"did not raise {exc.__name__}")


class _Mark:
    """`@pytest.mark.<anything>(...)` leaves the function as it is; a
    parametrized test takes arguments, so it is skipped below."""

    def __getattr__(self, _name):
        def mark(*args, **kwargs):
            if len(args) == 1 and callable(args[0]) and not kwargs:
                return args[0]
            return lambda fn: fn

        return mark


stub.raises = raises
stub.mark = _Mark()
sys.modules["pytest"] = stub

path = sys.argv[1]
spec = importlib.util.spec_from_file_location("lite_target", path)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

ran = 0
for name, fn in inspect.getmembers(mod, inspect.isfunction):
    if not name.startswith("test_") or fn.__module__ != mod.__name__:
        continue
    if inspect.signature(fn).parameters:
        continue
    try:
        fn()
    except Exception:
        traceback.print_exc()
        print(f"FAILED {path}::{name}", file=sys.stderr)
        sys.exit(1)
    ran += 1
print(f"{ran} tests passed in {path}")
sys.exit(0 if ran else 1)
