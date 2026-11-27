"""Stable spelling of the Python 3.14 AST format used by the frozen R5-B pins."""
import ast


def dump(node):
    """Omit empty sequence/optional fields, retaining every parsed AST value.

    Python 3.12's ast.dump includes empty sequences; 3.14 omits them. Spell the
    frozen format explicitly so the same source has the same pin on both hosts.
    Location attributes remain excluded, as in the original pin calculation.
    """
    if isinstance(node, ast.AST):
        fields = []
        for name, value in ast.iter_fields(node):
            if isinstance(value, list) and not value:
                continue
            if value is None and getattr(type(node), name, ...) is None:
                continue
            fields.append(name + "=" + dump(value))
        return type(node).__name__ + "(" + ", ".join(fields) + ")"
    if isinstance(node, list):
        return "[" + ", ".join(dump(value) for value in node) + "]"
    return repr(node)
