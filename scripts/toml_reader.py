#!/usr/bin/env python3
"""Strict reader for the TOML subset that RMUX release tooling reads.

CPython only ships ``tomllib`` from 3.11 on. The macOS release host runs the
system interpreter (``/usr/bin/python3``, 3.9), so any release gate step that
imports ``tomllib`` cannot run there at all: under ``set -euo pipefail`` the
first such step aborts the gate and every later step is skipped. This module
replaces ``tomllib`` in release tooling.

It is deliberately one implementation on every interpreter - it never delegates
to ``tomllib`` when that module happens to exist. A release gate must not parse
a manifest one way in hosted CI and another way on the release machine, and a
fallback that only runs on one host is a fallback nobody tests.

Accepted subset, which covers ``Cargo.toml`` and
``tests/reference/tmux_compat/divergences.toml``: comments, table headers,
array-of-tables headers, dotted keys, bare/basic/literal keys, basic and
literal strings, integers, floats, booleans, arrays (including multi-line
arrays with a trailing comma) and inline tables. Everything else - multi-line
strings, dates, times - raises :class:`TOMLDecodeError` naming the line, so an
unsupported construct is a loud gate failure and never a silently different
parse. Duplicate keys and redefined tables are rejected, as ``tomllib``
rejects them, because accepting them would change parsed values.
"""

from __future__ import annotations

import re
import sys
from typing import Any, Dict, List, Tuple

__all__ = ["TOMLDecodeError", "load", "loads"]

PYTHON_FLOOR = (3, 9)

if sys.version_info < PYTHON_FLOOR:
    raise RuntimeError(
        "RMUX release tooling requires Python "
        f"{PYTHON_FLOOR[0]}.{PYTHON_FLOOR[1]} or newer; this interpreter is "
        f"{sys.version_info[0]}.{sys.version_info[1]}"
    )


class TOMLDecodeError(ValueError):
    """Raised when the input is not TOML that this reader accepts."""


_BARE_KEY = re.compile(r"[A-Za-z0-9_-]+")
_VALUE_TOKEN = re.compile(r"[^\s,\]}#]+")
_DECIMAL_INT = re.compile(r"[+-]?(?:0|[1-9](?:_?[0-9])+|[1-9])")
_PREFIXED_INT = re.compile(r"0(?:x[0-9A-Fa-f](?:_?[0-9A-Fa-f])*|o[0-7](?:_?[0-7])*|b[01](?:_?[01])*)")
_FLOAT = re.compile(
    r"[+-]?(?:0|[1-9](?:_?[0-9])*)"
    r"(?:\.[0-9](?:_?[0-9])*)?"
    r"(?:[eE][+-]?[0-9](?:_?[0-9])*)?"
)
_DATE_OR_TIME = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}|[0-9]{2}:[0-9]{2}:[0-9]{2}")
_SIMPLE_ESCAPES = {
    '"': '"',
    "\\": "\\",
    "b": "\b",
    "f": "\f",
    "n": "\n",
    "r": "\r",
    "t": "\t",
}


class _Parser:
    """Single-pass scanner over a TOML document."""

    def __init__(self, text: str) -> None:
        self._text = text
        self._pos = 0
        self._root: Dict[str, Any] = {}
        self._current: Dict[str, Any] = self._root
        self._explicit_tables: set = set()
        self._array_tables: set = set()

    # -- diagnostics -----------------------------------------------------

    def _fail(self, message: str) -> "TOMLDecodeError":
        line = self._text.count("\n", 0, self._pos) + 1
        return TOMLDecodeError(f"line {line}: {message}")

    # -- character helpers ------------------------------------------------

    def _eof(self) -> bool:
        return self._pos >= len(self._text)

    def _peek(self) -> str:
        return "" if self._eof() else self._text[self._pos]

    def _starts_with(self, prefix: str) -> bool:
        return self._text.startswith(prefix, self._pos)

    def _skip_spaces(self) -> None:
        while not self._eof() and self._text[self._pos] in " \t":
            self._pos += 1

    def _skip_comment(self) -> None:
        if self._peek() == "#":
            end = self._text.find("\n", self._pos)
            self._pos = len(self._text) if end < 0 else end

    def _skip_blanks(self) -> None:
        """Skip spaces, newlines and comments (used between statements)."""
        while not self._eof():
            char = self._text[self._pos]
            if char in " \t\r\n":
                self._pos += 1
            elif char == "#":
                self._skip_comment()
            else:
                return

    def _expect(self, char: str) -> None:
        if self._peek() != char:
            raise self._fail(f"expected {char!r}, found {self._peek()!r}")
        self._pos += 1

    def _end_of_statement(self) -> None:
        self._skip_spaces()
        self._skip_comment()
        if self._eof():
            return
        if self._text[self._pos] in "\r\n":
            self._pos += 1
            return
        raise self._fail(f"unexpected trailing input {self._text[self._pos]!r}")

    # -- keys -------------------------------------------------------------

    def _parse_key_segment(self) -> str:
        char = self._peek()
        if char == '"':
            return self._parse_basic_string()
        if char == "'":
            return self._parse_literal_string()
        match = _BARE_KEY.match(self._text, self._pos)
        if match is None:
            raise self._fail(f"expected a key, found {char!r}")
        self._pos = match.end()
        return match.group()

    def _parse_key_path(self) -> Tuple[str, ...]:
        parts = [self._parse_key_segment()]
        while True:
            self._skip_spaces()
            if self._peek() != ".":
                return tuple(parts)
            self._pos += 1
            self._skip_spaces()
            parts.append(self._parse_key_segment())

    # -- strings ----------------------------------------------------------

    def _parse_basic_string(self) -> str:
        if self._starts_with('"""'):
            raise self._fail("multi-line basic strings are not supported")
        self._expect('"')
        chunks: List[str] = []
        while True:
            if self._eof() or self._text[self._pos] in "\r\n":
                raise self._fail("unterminated basic string")
            char = self._text[self._pos]
            if char == '"':
                self._pos += 1
                return "".join(chunks)
            if char != "\\":
                chunks.append(char)
                self._pos += 1
                continue
            self._pos += 1
            if self._eof():
                raise self._fail("unterminated escape sequence")
            escape = self._text[self._pos]
            self._pos += 1
            if escape in _SIMPLE_ESCAPES:
                chunks.append(_SIMPLE_ESCAPES[escape])
                continue
            if escape in "uU":
                width = 4 if escape == "u" else 8
                digits = self._text[self._pos : self._pos + width]
                if len(digits) != width or any(
                    digit not in "0123456789abcdefABCDEF" for digit in digits
                ):
                    raise self._fail(f"invalid \\{escape} escape sequence")
                self._pos += width
                chunks.append(chr(int(digits, 16)))
                continue
            raise self._fail(f"unsupported escape sequence \\{escape}")

    def _parse_literal_string(self) -> str:
        if self._starts_with("'''"):
            raise self._fail("multi-line literal strings are not supported")
        self._expect("'")
        end = self._pos
        while end < len(self._text) and self._text[end] not in "'\r\n":
            end += 1
        if end >= len(self._text) or self._text[end] != "'":
            raise self._fail("unterminated literal string")
        value = self._text[self._pos : end]
        self._pos = end + 1
        return value

    # -- values -----------------------------------------------------------

    def _parse_value(self) -> Any:
        char = self._peek()
        if char == "":
            raise self._fail("expected a value")
        if char == '"':
            return self._parse_basic_string()
        if char == "'":
            return self._parse_literal_string()
        if char == "[":
            return self._parse_array()
        if char == "{":
            return self._parse_inline_table()
        match = _VALUE_TOKEN.match(self._text, self._pos)
        if match is None:
            raise self._fail(f"expected a value, found {char!r}")
        token = match.group()
        self._pos = match.end()
        return self._scalar(token)

    def _scalar(self, token: str) -> Any:
        if token == "true":
            return True
        if token == "false":
            return False
        if _DATE_OR_TIME.search(token):
            raise self._fail(f"date and time values are not supported: {token}")
        if _PREFIXED_INT.fullmatch(token):
            base = {"x": 16, "o": 8, "b": 2}[token[1]]
            return int(token[2:].replace("_", ""), base)
        if _DECIMAL_INT.fullmatch(token):
            return int(token.replace("_", ""))
        if _FLOAT.fullmatch(token) and ("." in token or "e" in token or "E" in token):
            return float(token.replace("_", ""))
        raise self._fail(f"unsupported value: {token}")

    def _parse_array(self) -> List[Any]:
        self._expect("[")
        values: List[Any] = []
        while True:
            self._skip_blanks()
            if self._peek() == "]":
                self._pos += 1
                return values
            values.append(self._parse_value())
            self._skip_blanks()
            char = self._peek()
            if char == ",":
                self._pos += 1
                continue
            if char == "]":
                self._pos += 1
                return values
            raise self._fail(f"expected ',' or ']' in array, found {char!r}")

    def _parse_inline_table(self) -> Dict[str, Any]:
        self._expect("{")
        table: Dict[str, Any] = {}
        self._skip_spaces()
        if self._peek() == "}":
            self._pos += 1
            return table
        while True:
            self._skip_spaces()
            path = self._parse_key_path()
            self._skip_spaces()
            self._expect("=")
            self._skip_spaces()
            self._assign(table, path, self._parse_value())
            self._skip_spaces()
            char = self._peek()
            if char == ",":
                self._pos += 1
                continue
            if char == "}":
                self._pos += 1
                return table
            raise self._fail(f"expected ',' or '}}' in inline table, found {char!r}")

    # -- structure --------------------------------------------------------

    def _assign(
        self, table: Dict[str, Any], path: Tuple[str, ...], value: Any
    ) -> None:
        target = table
        for part in path[:-1]:
            child = target.get(part)
            if child is None:
                child = {}
                target[part] = child
            elif not isinstance(child, dict):
                raise self._fail(f"cannot extend non-table key {part!r}")
            target = child
        if path[-1] in target:
            raise self._fail(f"duplicate key {'.'.join(path)!r}")
        target[path[-1]] = value

    def _open_table(self, path: Tuple[str, ...], array: bool) -> None:
        container = self._root
        for depth, part in enumerate(path[:-1], start=1):
            child = container.get(part)
            if child is None:
                child = {}
                container[part] = child
            elif isinstance(child, list):
                if path[:depth] not in self._array_tables:
                    raise self._fail(f"cannot descend into non-table key {part!r}")
                child = child[-1]
            elif not isinstance(child, dict):
                raise self._fail(f"cannot descend into non-table key {part!r}")
            container = child

        name = path[-1]
        existing = container.get(name)
        dotted = ".".join(path)
        if array:
            if existing is None:
                table: Dict[str, Any] = {}
                container[name] = [table]
                self._array_tables.add(path)
            elif isinstance(existing, list) and path in self._array_tables:
                table = {}
                existing.append(table)
            else:
                raise self._fail(f"cannot redefine {dotted!r} as an array of tables")
        else:
            if path in self._explicit_tables or path in self._array_tables:
                raise self._fail(f"table {dotted!r} is defined twice")
            if existing is None:
                table = {}
                container[name] = table
            elif isinstance(existing, dict):
                table = existing
            else:
                raise self._fail(f"cannot redefine {dotted!r} as a table")
            self._explicit_tables.add(path)
        self._current = table

    def _parse_header(self) -> None:
        array = self._starts_with("[[")
        self._pos += 2 if array else 1
        self._skip_spaces()
        path = self._parse_key_path()
        self._skip_spaces()
        if array:
            if not self._starts_with("]]"):
                raise self._fail("expected ']]' closing an array-of-tables header")
            self._pos += 2
        else:
            self._expect("]")
        self._open_table(path, array)
        self._end_of_statement()

    def parse(self) -> Dict[str, Any]:
        while True:
            self._skip_blanks()
            if self._eof():
                return self._root
            if self._peek() == "[":
                self._parse_header()
                continue
            path = self._parse_key_path()
            self._skip_spaces()
            self._expect("=")
            self._skip_spaces()
            self._assign(self._current, path, self._parse_value())
            self._end_of_statement()


def loads(text: str) -> Dict[str, Any]:
    """Parse a TOML document from ``text``."""

    if not isinstance(text, str):
        raise TypeError("loads() expects str; use load() for a binary file")
    return _Parser(text).parse()


def load(handle: Any) -> Dict[str, Any]:
    """Parse a TOML document from a file object opened in binary or text mode."""

    data = handle.read()
    if isinstance(data, bytes):
        data = data.decode("utf-8")
    return loads(data)


_SELF_TEST_DOCUMENT = """
# leading comment
schema = 1
enabled = true
disabled = false
ratio = 1.5
scaled = 2e3
hex = 0x2A
underscored = 1_000
escaped = "tab\\tquote\\"unicode\\u0041"
literal = 'raw\\not-an-escape'

[workspace]
members = [
  "a",     # trailing comment inside an array
  "b",
]

[workspace.package]
version = "0.10.0"
edition.workspace = true

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "0.61.2", features = ["Win32_Foundation"] }

[[entry]]
id = "C-D1"
tests = []

[[entry]]
id = "C-D2"
tests = ["tests/a.rs::b_product_divergence"]
"""

_SELF_TEST_REJECTIONS = (
    ('a = 1\na = 2\n', "duplicate key"),
    ('[a]\nk = 1\n[a]\n', "defined twice"),
    ('a = """x"""\n', "multi-line basic strings"),
    ("a = '''x'''\n", "multi-line literal strings"),
    ("a = 1979-05-27\n", "date and time values"),
    ("a = 07:32:00\n", "date and time values"),
    ('a = "unterminated\n', "unterminated basic string"),
    ("a = 'unterminated\n", "unterminated literal string"),
    ("a 1\n", "expected '='"),
    ('a = "x" trailing\n', "unexpected trailing input"),
    ('a = "\\q"\n', "unsupported escape sequence"),
    ("a = [1, 2\n", "expected ',' or ']'"),
    ("a = { b = 1\n", "expected ',' or '}'"),
    ("a = nope\n", "unsupported value"),
    ("[[a]]\nb = 1\n[a]\n", "defined twice"),
    ("[a]\nb = 1\n[[a]]\n", "as an array of tables"),
)


def _check(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(f"error: toml reader self-test: {message}")


def _self_test() -> int:
    parsed = loads(_SELF_TEST_DOCUMENT)
    _check(parsed["schema"] == 1, "integer value")
    _check(parsed["enabled"] is True and parsed["disabled"] is False, "boolean values")
    _check(parsed["ratio"] == 1.5 and parsed["scaled"] == 2000.0, "float values")
    _check(parsed["hex"] == 42 and parsed["underscored"] == 1000, "integer notations")
    _check(parsed["escaped"] == 'tab\tquote"unicodeA', "basic string escapes")
    _check(parsed["literal"] == "raw\\not-an-escape", "literal string")
    _check(parsed["workspace"]["members"] == ["a", "b"], "multi-line array")
    _check(parsed["workspace"]["package"]["version"] == "0.10.0", "nested table")
    _check(
        parsed["workspace"]["package"]["edition"] == {"workspace": True},
        "dotted key",
    )
    _check(
        parsed["target"]["cfg(windows)"]["dependencies"]["windows-sys"]
        == {"version": "0.61.2", "features": ["Win32_Foundation"]},
        "quoted header segment and inline table",
    )
    _check(
        [entry["id"] for entry in parsed["entry"]] == ["C-D1", "C-D2"],
        "array of tables",
    )

    for document, expected in _SELF_TEST_REJECTIONS:
        try:
            loads(document)
        except TOMLDecodeError as error:
            _check(
                expected in str(error),
                f"rejection message for {document!r} was {error}",
            )
        else:
            _check(False, f"accepted invalid document {document!r}")

    print(
        "toml-reader-self-test=ok "
        f"cases={len(_SELF_TEST_REJECTIONS) + 1} "
        f"python={sys.version_info[0]}.{sys.version_info[1]}.{sys.version_info[2]} "
        f"floor={PYTHON_FLOOR[0]}.{PYTHON_FLOOR[1]}"
    )
    return 0


def main(argv: List[str]) -> int:
    if len(argv) == 2 and argv[1] == "--self-test":
        return _self_test()
    print(f"usage: {argv[0]} --self-test", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
