"""Recognize source test cases, excluding statically disabled declarations.

Unknown Rust cfg predicates remain eligible across the manifest's cfg/feature
scope. A possibly applied ignore/skip decorator cannot establish conformance.
No test source or decorator is executed by this static reader.
"""
import ast

from surface_syntax import Token, literal, split


def cfg_value(tokens):
    if len(tokens) != 2 or tokens[0].kind != 'atom' or tokens[1].kind != '(':
        return None
    name, rows = tokens[0].value, split(tokens[1].value)
    values = [cfg_value(row) for row in rows]
    if name == 'all':
        return False if False in values else None if None in values else True
    if name == 'any':
        return True if True in values else None if None in values else False
    if name == 'not' and len(values) == 1:
        return None if values[0] is None else not values[0]
    return None


def attributes(rows):
    result = []
    for row in rows:
        if len(row) == 2 and row[0] == Token('atom', 'cfg_attr') and row[1].kind == '(':
            fields = split(row[1].value)
            if fields and cfg_value(fields[0]) is not False:
                result.extend(attributes(fields[1:]))
        else:
            result.append(row)
    return result


def enabled(rows):
    return not any(len(row) == 2 and row[0] == Token('atom', 'cfg')
                   and row[1].kind == '(' and cfg_value(row[1].value) is False
                   for row in attributes(rows))


def rust_items(tokens, kind):
    found, rows, i = [], [], 0
    # Inner attributes apply to the complete enclosing module.
    for j in range(len(tokens) - 2):
        if tokens[j:j+2] == (Token('atom', '#'), Token('atom', '!')) and tokens[j+2].kind == '[':
            if not enabled([tokens[j+2].value]):
                return []
    while i < len(tokens):
        token = tokens[i]
        if token == Token('atom', '#') and i + 1 < len(tokens) and tokens[i+1].kind == '[':
            rows.append(tokens[i+1].value)
            i += 2
            continue
        active = enabled(rows)
        if token == Token('atom', kind) and i + 1 < len(tokens):
            names = [row[0].value for row in attributes(rows) if row and row[0].kind == 'atom']
            if active and (kind == 'mod' or 'test' in names) and 'ignore' not in names:
                if kind == 'mod':
                    paths = [literal(row[2:]) for row in attributes(rows)
                             if len(row) == 3 and row[:2] == (Token('atom', 'path'), Token('atom', '='))]
                    found.append((tokens[i+1].value, paths[0] if len(paths) == 1 else None))
                else:
                    found.append(tokens[i+1].value)
        if token.kind == '{':
            if active and not any(row and row[0] == Token('atom', 'ignore') for row in attributes(rows)):
                found.extend(rust_items(token.value, kind))
            rows = []
        elif token == Token('atom', ';'):
            rows = []
        i += 1
    return found


def rust_cases(tokens):
    return rust_items(tokens, 'fn')


def rust_modules(tokens):
    return rust_items(tokens, 'mod')


def truth(node):
    try:
        return bool(ast.literal_eval(node))
    except (ValueError, TypeError, SyntaxError):
        if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.Not):
            value = truth(node.operand)
            return None if value is None else not value
        return None


def python_cases(source):
    tree = ast.parse(source)
    aliases = {}
    for node in tree.body:
        if isinstance(node, ast.ImportFrom) and node.module == 'unittest':
            aliases.update((item.asname or item.name, item.name) for item in node.names)

    def skipped(node):
        for decorator in node.decorator_list:
            target = decorator.func if isinstance(decorator, ast.Call) else decorator
            name = target.attr if isinstance(target, ast.Attribute) else aliases.get(
                target.id, target.id) if isinstance(target, ast.Name) else ''
            if name == 'skip':
                return True
            if name in ('skipIf', 'skipUnless'):
                condition = truth(decorator.args[0]) if isinstance(decorator, ast.Call) and decorator.args else None
                if condition is None or condition == (name == 'skipIf'):
                    return True
        return False

    def classes(body):
        result = []
        for node in body:
            if isinstance(node, ast.ClassDef):
                result.append(node)
            elif isinstance(node, ast.If):
                condition = truth(node.test)
                if condition is not False:
                    result.extend(classes(node.body))
                if condition is not True:
                    result.extend(classes(node.orelse))
        return result

    definitions = classes(tree.body)
    by_name = {node.name: node for node in definitions}

    def bases(node):
        return [base.attr if isinstance(base, ast.Attribute) else aliases.get(base.id, base.id)
                if isinstance(base, ast.Name) else '' for base in node.bases]

    def testcase(node, seen=()):
        return node.name not in seen and any(name == 'TestCase' or name in by_name and
                   testcase(by_name[name], (*seen, node.name)) for name in bases(node))

    def class_skipped(node, seen=()):
        if node.name in seen or skipped(node):
            return True
        for statement in node.body:
            if isinstance(statement, ast.Assign) and any(isinstance(t, ast.Name) and
                    t.id == '__unittest_skip__' for t in statement.targets):
                if truth(statement.value) is not False:
                    return True
        return any(name in by_name and class_skipped(by_name[name], (*seen, node.name))
                   for name in bases(node))

    return [method.name for node in definitions if testcase(node) and not class_skipped(node)
            for method in node.body if isinstance(method, (ast.FunctionDef, ast.AsyncFunctionDef))
            and method.name.startswith('test_') and not skipped(method)]
