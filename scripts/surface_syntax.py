"""Token trees for the literal Rust registries read by surface_manifest.

This is not a Rust evaluator. Readers accept a closed declarative grammar and
refuse unknown rows; comments, strings and nested groups cannot manufacture rows.
"""
from dataclasses import dataclass
import re


@dataclass(frozen=True)
class Token:
    kind: str
    value: object


def require(condition, message):
    if not condition:
        raise ValueError(message)


def _string(text):
    def escape(match):
        value = match.group(1)
        if value.startswith("u{"):
            return chr(int(value[2:-1].replace("_", ""), 16))
        if value.startswith("x"):
            return chr(int(value[1:], 16))
        if value[0].isspace():
            return ""
        return {"n": "\n", "r": "\r", "t": "\t", "0": "\0",
                "\\": "\\", '"': '"', "'": "'"}[value]
    return re.sub(r'\\(u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|\s+|.)', escape, text)


def parse(text):
    stack = [[]]
    opened = []
    offset = 0
    while offset < len(text):
        tail = text[offset:]
        if tail[0].isspace():
            offset += 1
            continue
        if tail.startswith("//"):
            end = text.find("\n", offset)
            offset = len(text) if end < 0 else end + 1
            continue
        if tail.startswith("/*"):
            depth = 1
            offset += 2
            while depth and offset < len(text):
                if text.startswith("/*", offset):
                    depth += 1
                    offset += 2
                elif text.startswith("*/", offset):
                    depth -= 1
                    offset += 2
                else:
                    offset += 1
            require(depth == 0, "unterminated Rust comment")
            continue
        raw = re.match(r'(?:b|c)?r(#{0,255})"', tail)
        if raw:
            end_marker = '"' + raw[1]
            end = text.find(end_marker, offset + raw.end())
            require(end >= 0, "unterminated Rust raw string")
            stack[-1].append(Token("string", text[offset + raw.end():end]))
            offset = end + len(end_marker)
            continue
        literal = re.match(r'(?:b|c)?"(?:[^"\\]|\\[\s\S])*"', tail)
        if literal:
            value = literal[0]
            start = value.index('"') + 1
            stack[-1].append(Token("string", _string(value[start:-1])))
            offset += literal.end()
            continue
        require(tail[0] != '"', "unterminated Rust string")
        char = re.match(r"(?:b)?'(?:[^'\\\n]|\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.))'", tail)
        if char:
            stack[-1].append(Token("char", char[0]))
            offset += char.end()
            continue
        current = tail[0]
        if current in "([{":
            opened.append(current)
            stack.append([])
            offset += 1
            continue
        if current in ")]}":
            require(opened and opened[-1] == {")": "(", "]": "[", "}": "{"}[current],
                    "unbalanced Rust token tree")
            group = Token(opened.pop(), tuple(stack.pop()))
            stack[-1].append(group)
            offset += 1
            continue
        atom = re.match(r'[A-Za-z_][A-Za-z_0-9]*|[0-9][A-Za-z_0-9]*', tail)
        value = atom[0] if atom else current
        stack[-1].append(Token("atom", value))
        offset += len(value)
    require(not opened, "unterminated Rust token tree")
    return tuple(stack[0])


def matches(tokens, words):
    return len(tokens) == len(words) and all(t.kind == "atom" and t.value == w
                                            for t, w in zip(tokens, words))


def split(tokens, separator=","):
    result, row = [], []
    for token in tokens:
        if token == Token("atom", separator):
            require(row, "empty registry field")
            result.append(tuple(row))
            row = []
        else:
            row.append(token)
    if row:
        result.append(tuple(row))
    return result


def group(tokens, prefix, kind):
    found = [tokens[i + len(prefix)] for i in range(len(tokens) - len(prefix))
             if matches(tokens[i:i + len(prefix)], prefix)
             and tokens[i + len(prefix)].kind == kind]
    require(len(found) == 1, "missing or ambiguous registry: " + " ".join(prefix))
    return found[0].value


def initializer(tokens, name):
    positions = [i for i, t in enumerate(tokens) if t == Token("atom", name)
                 and i and tokens[i - 1] == Token("atom", "const")]
    require(len(positions) == 1, "missing or ambiguous constant: " + name)
    rest = tokens[positions[0] + 1:]
    declaration = rest[:rest.index(Token("atom", ";"))]
    equal = declaration.index(Token("atom", "="))
    return declaration[equal + 1:]


def array(tokens, name):
    value = initializer(tokens, name)
    require(len(value) == 2 and value[0] == Token("atom", "&") and value[1].kind == "[",
            "unsupported array declaration: " + name)
    return value[1].value


def function(tokens, name):
    positions = [i for i in range(len(tokens) - 2)
                 if matches(tokens[i:i+2], ["fn", name]) and tokens[i+2].kind == "("]
    require(len(positions) == 1, "missing or ambiguous function: " + name)
    return next(t.value for t in tokens[positions[0]+3:] if t.kind == "{")


def literal(tokens):
    require(len(tokens) == 1 and tokens[0].kind == "string", "expected literal string")
    return tokens[0].value


def atom(tokens):
    require(len(tokens) == 1 and tokens[0].kind == "atom", "expected literal identifier")
    return tokens[0].value


def strings(tokens):
    require(len(tokens) == 2 and tokens[0] == Token("atom", "&")
            and tokens[1].kind == "[", "expected literal string array")
    return [literal(row) for row in split(tokens[1].value)]


def optional_string(tokens):
    if matches(tokens, ["None"]):
        return None
    require(len(tokens) == 2 and tokens[0] == Token("atom", "Some")
            and tokens[1].kind == "(", "expected optional string")
    return literal(tokens[1].value)


def calls(tokens, name):
    rows = []
    for row in split(tokens):
        require(len(row) == 2 and row[0] == Token("atom", name) and row[1].kind == "(",
                "unsupported registry row: " + name)
        rows.append(split(row[1].value))
    require(rows, "empty registry: " + name)
    return rows


def text(tokens):
    return " ".join('"' + t.value + '"' if t.kind == "string" else
                    t.kind + text(t.value) + {"(": ")", "[": "]", "{": "}"}[t.kind]
                    if t.kind in ("(", "[", "{") else str(t.value)
                    for t in tokens)
