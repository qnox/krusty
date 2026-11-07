"""Selection from the pinned Kotlin codegen-box test generators."""
from __future__ import annotations

import dataclasses
import argparse
import json
import re
import sys
from pathlib import Path


@dataclasses.dataclass(frozen=True)
class Model:
    root: str
    pattern: str = r"^(.+)\.kt$"
    exclude_dirs: tuple[str, ...] = ()
    excluded_pattern: str | None = None
    exclude_recursive: tuple[str, ...] = ()
    test_class: str = ""
    source: str = ""
    target: str = ""
    recursive: bool = True


@dataclasses.dataclass(frozen=True)
class Token:
    value: str
    quoted: bool = False
    line: int = 0


def tokens(source: str) -> list[Token]:
    result = []
    cursor = 0
    line = 1
    while cursor < len(source):
        start = cursor
        if source[cursor].isspace():
            cursor += 1
        elif source.startswith("//", cursor):
            end = source.find("\n", cursor)
            cursor = len(source) if end < 0 else end
        elif source.startswith("/*", cursor):
            cursor += 2
            depth = 1
            while depth and cursor < len(source):
                if source.startswith("/*", cursor):
                    depth += 1
                    cursor += 2
                elif source.startswith("*/", cursor):
                    depth -= 1
                    cursor += 2
                else:
                    cursor += 1
            if depth:
                raise ValueError("unterminated Kotlin generator comment")
        elif source.startswith('"""', cursor):
            end = source.find('"""', cursor + 3)
            if end < 0:
                raise ValueError("unterminated Kotlin generator raw string")
            result.append(Token(source[cursor + 3:end], True, line))
            cursor = end + 3
        elif source[cursor] == "'":
            cursor += 1
            while cursor < len(source):
                if source[cursor] == "\\":
                    cursor += 2
                elif source[cursor] == "'":
                    cursor += 1
                    break
                else:
                    cursor += 1
            result.append(Token(source[start:cursor], True, line))
        elif source[cursor] == '"':
            cursor += 1
            while cursor < len(source):
                if source[cursor] == "\\":
                    cursor += 2
                elif source[cursor] == '"':
                    cursor += 1
                    break
                else:
                    cursor += 1
            encoded = source[start:cursor].replace(r"\$", "$")
            result.append(Token(json.loads(encoded), True, line))
        elif source[cursor].isalpha() or source[cursor] == "_":
            cursor += 1
            while cursor < len(source) and (source[cursor].isalnum() or source[cursor] == "_"):
                cursor += 1
            result.append(Token(source[start:cursor], False, line))
        else:
            result.append(Token(source[cursor], False, line))
            cursor += 1
        line += source[start:cursor].count("\n")
    return result


def matching(items: list[Token], start: int) -> int:
    pairs = {"(": ")", "{": "}", "[": "]", "<": ">"}
    opening = items[start].value
    depth = 1
    for index in range(start + 1, len(items)):
        item = items[index]
        if item.quoted:
            continue
        if item.value == opening:
            depth += 1
        elif item.value == pairs[opening]:
            depth -= 1
            if depth == 0:
                return index
    raise ValueError(f"unclosed generator delimiter {opening}")


def arguments(items: list[Token]) -> tuple[list[list[Token]], dict[str, list[Token]]]:
    parts = []
    start = 0
    cursor = 0
    while cursor < len(items):
        item = items[cursor]
        if not item.quoted and item.value in ("(", "{", "["):
            cursor = matching(items, cursor) + 1
        elif not item.quoted and item.value == ",":
            if start < cursor:
                parts.append(items[start:cursor])
            cursor += 1
            start = cursor
        else:
            cursor += 1
    if start < len(items):
        parts.append(items[start:])
    positional = []
    named = {}
    for part in parts:
        if len(part) >= 2 and part[1].value == "=" and not part[1].quoted:
            if part[0].value in named:
                raise ValueError(f"duplicate model argument {part[0].value}")
            named[part[0].value] = part[2:]
        else:
            positional.append(part)
    return positional, named


class Source:
    def __init__(self, source: str):
        self.items = tokens(source)
        self.bindings = {}
        self.resolving = set()
        for index in range(len(self.items) - 2):
            if self.items[index].value not in ("val", "var") or self.items[index].quoted:
                continue
            if self.items[index + 2].value != "=":
                continue
            name = self.items[index + 1].value
            start = index + 3
            end = start
            while end < len(self.items):
                item = self.items[end]
                if not item.quoted and item.value in ("(", "["):
                    end = matching(self.items, end) + 1
                    continue
                if not item.quoted and item.value in ("}", ";"):
                    break
                if end > start and item.line > self.items[end - 1].line:
                    if item.value not in ("+", ".") and self.items[end - 1].value != "+":
                        break
                end += 1
            self.bindings.setdefault(name, []).append(self.items[start:end])

    def evaluate(self, items: list[Token]):
        if not items:
            raise ValueError("empty generator expression")
        values = []
        start = 0
        cursor = 0
        while cursor < len(items):
            item = items[cursor]
            if not item.quoted and item.value in ("(", "["):
                cursor = matching(items, cursor) + 1
            elif not item.quoted and item.value == "+":
                values.append(self.atom(items[start:cursor]))
                cursor += 1
                start = cursor
            else:
                cursor += 1
        values.append(self.atom(items[start:]))
        result = values[0]
        for value in values[1:]:
            if isinstance(result, list) and isinstance(value, str):
                result = [*result, value]
            elif isinstance(result, type(value)) and isinstance(result, (str, list)):
                result += value
            else:
                raise ValueError("unsupported generator concatenation")
        return result

    def atom(self, items: list[Token]):
        if len(items) == 1:
            item = items[0]
            if item.quoted:
                return self.interpolate(item.value)
            if item.value in ("true", "false", "null"):
                return {"true": True, "false": False, "null": None}[item.value]
            if item.value in self.resolving:
                raise ValueError(f"recursive generator binding {item.value}")
            matches = self.bindings.get(item.value, [])
            if len(matches) != 1:
                raise ValueError(f"generator binding {item.value} has {len(matches)} definitions")
            self.resolving.add(item.value)
            try:
                return self.evaluate(matches[0])
            finally:
                self.resolving.remove(item.value)
        if len(items) >= 3 and items[1].value == "(" and matching(items, 1) == len(items) - 1:
            positional, named = arguments(items[2:-1])
            if named:
                raise ValueError("named arguments in generator constant")
            if items[0].value in ("listOf", "arrayOf", "setOf", "mutableListOf"):
                return [self.evaluate(part) for part in positional]
            if items[0].value == "detectDirsWithTestsMapFileOnly":
                # The upstream helper excludes directories containing only test-map JSON.
                # They contribute no Kotlin files to a file inventory.
                if len(positional) != 1 or not isinstance(self.evaluate(positional[0]), str):
                    raise ValueError("unsupported spec test-map directory filter")
                return []
        raise ValueError("unsupported generator expression: " + " ".join(t.value for t in items))

    def interpolate(self, text: str) -> str:
        def replace_first(match):
            value = self.evaluate([Token(match.group(1))])
            if not isinstance(value, list) or not value or not isinstance(value[0], str):
                raise ValueError("non-list interpolation in box root")
            return value[0]
        text = re.sub(r"\$\{(\w+)\.first\(\)\}", replace_first, text)
        if "$" + "{" in text:
            raise ValueError(f"unsupported interpolation in generator constant {text!r}")
        return text


IGNORED_NON_BOX_RUNNERS = {
    "AbstractDirectivesValidatorTest",
    "AbstractFirLightTreeHeaderModeCodegenTest",
    "AbstractNativeIrDeserializationTest",
    "AbstractNativeIrDeserializationWithInlinedFunInKlibTest",
}


def _is_box_runner(name: str, runner_resolver=None, seen=frozenset()) -> bool:
    if name in IGNORED_NON_BOX_RUNNERS:
        return False
    if "Box" in name or name == "AbstractFirWasmJsTranslatorTest":
        return True
    if name in seen:
        raise ValueError(f"recursive upstream runner inheritance: {name}")
    parents = runner_resolver(name) if runner_resolver else []
    return any(_is_box_runner(parent, runner_resolver, seen | {name}) for parent in parents)


def extract_models(source: str, *, source_name: str, target: str, runner_resolver=None) -> list[Model]:
    parsed = Source(source)
    items = parsed.items
    result = []

    def walk(start, end, base=None, test_class=None):
        cursor = start
        while cursor < end:
            item = items[cursor]
            if item.quoted:
                cursor += 1
                continue
            if item.value == "{":
                close = matching(items, cursor)
                walk(cursor + 1, close, base, test_class)
                cursor = close + 1
                continue
            if item.value not in ("testGroup", "testClass", "model"):
                cursor += 1
                continue
            kind = item.value
            position = cursor + 1
            class_name = None
            if kind == "testClass":
                if position >= end or items[position].value != "<":
                    if position < end and items[position].value == "(":
                        raise ValueError("unsupported non-reified testClass registration")
                    cursor += 1
                    continue
                close = matching(items, position)
                names = items[position + 1:close]
                if len(names) != 1:
                    raise ValueError("unsupported testClass type")
                class_name = names[0].value
                position = close + 1
            elif position >= end or items[position].value != "(":
                cursor += 1
                continue
            call_args = []
            if position < end and items[position].value == "(":
                close = matching(items, position)
                call_args = items[position + 1:close]
                position = close + 1
            positional, named = arguments(call_args)
            if kind == "model":
                if test_class is None or base is None:
                    raise ValueError(f"model outside a testGroup/testClass in {source_name}:{item.line}")
                relative = parsed.evaluate(named.get("relativeRootPath", positional[0] if positional else [Token("", True)]))
                data_root = parsed.evaluate(base)
                if not isinstance(relative, str) or not isinstance(data_root, str):
                    raise ValueError("non-string upstream box root")
                root = str(Path(data_root) / relative).rstrip("/")
                has_box_path = any(part.startswith("box") for part in Path(root).parts)
                if not has_box_path and not _is_box_runner(test_class):
                    # Debugger, TypeScript-export and other specialized suites also reuse the
                    # box base classes. They are not codegen-box model registrations.
                    cursor = position
                    continue
                if not _is_box_runner(test_class, runner_resolver):
                    if has_box_path and test_class not in IGNORED_NON_BOX_RUNNERS:
                        raise ValueError(f"unclassified upstream box runner {test_class}: {root}")
                    cursor = position
                    continue
                wasm_target = "wasm-wasi" if "Wasi" in test_class else "wasm-js"
                if target.startswith("wasm") and wasm_target != target:
                    cursor = position
                    continue
                if "extension" in named and parsed.evaluate(named["extension"]) != "kt":
                    raise ValueError(f"unsupported box model extension in {test_class}")
                if "excludeParentDirs" in named and parsed.evaluate(named["excludeParentDirs"]):
                    raise ValueError(f"unsupported directory-based box model {test_class}")
                pattern = parsed.evaluate(named["pattern"]) if "pattern" in named else r"^(.+)\.kt$"
                excluded = parsed.evaluate(named["excludedPattern"]) if "excludedPattern" in named else None
                dirs = parsed.evaluate(named["excludeDirs"]) if "excludeDirs" in named else []
                recursive_dirs = parsed.evaluate(named["excludeDirsRecursively"]) if "excludeDirsRecursively" in named else []
                recursive = parsed.evaluate(named["recursive"]) if "recursive" in named else True
                if not isinstance(pattern, str) or (excluded is not None and not isinstance(excluded, str)):
                    raise ValueError("non-string box filename pattern")
                if not all(isinstance(value, str) for value in [*dirs, *recursive_dirs]):
                    raise ValueError("non-string box directory exclusion")
                if Path(root).is_absolute() or ".." in Path(root).parts:
                    raise ValueError(f"upstream box root escapes checkout: {root}")
                result.append(Model(root, pattern, tuple(dirs), excluded, tuple(recursive_dirs),
                                    test_class, source_name, target, recursive))
                cursor = position
                continue
            if position >= end or items[position].value != "{":
                raise ValueError(f"missing generator body after {kind}")
            close = matching(items, position)
            if kind == "testGroup":
                group_base = named.get("testDataRoot", positional[1] if len(positional) > 1 else None)
                if group_base is None:
                    raise ValueError("testGroup has no data root")
                walk(position + 1, close, group_base, None)
            else:
                walk(position + 1, close, base, class_name)
            cursor = close + 1

    walk(0, len(items))
    return result


def select_files(checkout: Path, models: list[Model]) -> list[str]:
    selected = set()
    for model in models:
        directory = checkout / model.root
        if not directory.is_dir():
            raise ValueError(f"missing upstream box root {model.root}")
        pattern = re.compile(model.pattern)
        excluded = re.compile(model.excluded_pattern) if model.excluded_pattern else None
        candidates = directory.rglob("*") if model.recursive else directory.iterdir()
        for file in candidates:
            if not file.is_file() or not pattern.fullmatch(file.name):
                continue
            relative = file.relative_to(directory)
            if any(relative == Path(path) or Path(path) in relative.parents for path in model.exclude_dirs):
                continue
            if any(part in model.exclude_recursive for part in relative.parts[:-1]):
                continue
            if excluded and excluded.fullmatch(file.name):
                continue
            selected.add(file.relative_to(checkout).as_posix())
    return sorted(selected)


FIR_GENERATOR = "compiler/fir/fir2ir/testFixtures/org/jetbrains/kotlin/test/TestGeneratorForFir2IrTests.kt"
SCOPES_GENERATOR = "compiler/tests-common-new/testFixtures/org/jetbrains/kotlin/test/TestGeneratorForTestCommonNew.kt"
NATIVE_GENERATOR = "native/native.tests/codegen-box/testFixtures/org/jetbrains/kotlin/generators/tests/GenerateNativeCodegenBoxTests.kt"
NATIVE_KLIB_GENERATOR = "native/native.tests/klib-ir-inliner/testFixtures/org/jetbrains/kotlin/generators/tests/GenerateKlibNativeTests.kt"
WASM_GENERATOR = "wasm/wasm.tests/testFixtures/org/jetbrains/kotlin/generators/tests/GenerateWasmTests.kt"
DSL_SOURCE = "generators/test-generator/testFixtures/org/jetbrains/kotlin/generators/dsl/TestGenerationDSL.kt"
SPEC_UTILS = "compiler/tests-spec/testFixtures/org/jetbrains/kotlin/spec/utils/tasks/GenerateSpecTestsUtils.kt"

GENERATORS = {
    "jvm": [FIR_GENERATOR, SCOPES_GENERATOR],
    "native": [NATIVE_GENERATOR, NATIVE_KLIB_GENERATOR],
    "wasm-js": [WASM_GENERATOR],
    "wasm-wasi": [WASM_GENERATOR],
}


def runner_resolver(checkout: Path):
    manifest = Path(__file__).resolve().parent.parent / "tests/box_selection_sources.txt"
    directories = sorted({(checkout / name).parent for name in manifest.read_text().splitlines() if name})
    directories = [directory for directory in directories
                   if not any(parent != directory and parent in directory.parents for parent in directories)]
    memo = {}
    source_files = None

    def resolve(name):
        nonlocal source_files
        if name in memo:
            return memo[name]
        paths = {path for directory in directories for path in directory.rglob(name + ".kt")}
        if not paths:
            if source_files is None:
                source_files = sorted({path for directory in directories for path in directory.rglob("*.kt")})
            declaration = re.compile(r"\bclass\s+" + re.escape(name) + r"\b")
            paths = {path for path in source_files if declaration.search(path.read_text())}
        parents = []
        for path in sorted(paths):
            items = tokens(path.read_text())
            for index in range(len(items) - 1):
                if items[index].value != "class" or items[index + 1].value != name:
                    continue
                position = index + 2
                if position < len(items) and items[position].value == "<":
                    position = matching(items, position) + 1
                if position < len(items) and items[position].value == "(":
                    position = matching(items, position) + 1
                if position < len(items) and items[position].value == ":":
                    parents.append(items[position + 1].value)
        memo[name] = parents
        return parents
    return resolve


def load_models(checkout: Path, target: str) -> list[Model]:
    dsl = (checkout / DSL_SOURCE).read_text()
    compact = re.sub(r"\s+", "", dsl)
    if 'extension:String?="kt"' not in compact or 'else"""^(.+)\\.$extension$"""' not in compact:
        raise ValueError("upstream default model filename pattern changed")
    models = []
    resolve = runner_resolver(checkout)
    for source_name in generator_sources(checkout, target):
        source = (checkout / source_name).read_text()
        if "detectDirsWithTestsMapFileOnly" in source:
            helper = re.sub(r"\s+", "", (checkout / SPEC_UTILS).read_text())
            if "allMatch{it.endsWith(TESTS_MAP_FILENAME)}" not in helper:
                raise ValueError("upstream spec test-map directory filter changed")
        models.extend(extract_models(source, source_name=source_name, target=target, runner_resolver=resolve))
    if not models:
        raise ValueError(f"upstream has no box models for {target}")
    return models


def generator_sources(checkout: Path, target: str) -> list[str]:
    result = set()
    for anchor in GENERATORS[target]:
        path = checkout / anchor
        if not path.is_file():
            raise ValueError(f"missing upstream generator {anchor}")
        # Include additional generator entry points in the same core test modules, so a new
        # source file there cannot silently add a box directory outside our inventory.
        for candidate in path.parent.glob("*.kt"):
            source = candidate.read_text()
            if re.search(r"(?m)^fun main\(", source) and "generateTestGroupSuite" in source:
                result.add(candidate.relative_to(checkout).as_posix())
    return sorted(result)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--checkout", type=Path, required=True)
    parser.add_argument("--target", choices=[*GENERATORS, "all"], default="all")
    parser.add_argument("--required-roots", action="store_true")
    parser.add_argument("--rules", action="store_true")
    parser.add_argument("--generators", action="store_true")
    args = parser.parse_args()
    targets = list(GENERATORS) if args.target == "all" else [args.target]
    if args.generators:
        for source in sorted({source for target in targets for source in generator_sources(args.checkout, target)}):
            print(source.encode().hex())
        return
    models = [model for target in targets for model in load_models(args.checkout, target)]
    if args.required_roots:
        for root in sorted({model.root for model in models}):
            print(root)
    elif args.rules:
        for model in models:
            fields = [model.target, model.root, model.pattern, model.excluded_pattern or "",
                      "\0".join(model.exclude_dirs), "\0".join(model.exclude_recursive),
                      model.test_class, model.source, str(int(model.recursive))]
            print("\t".join(value.encode().hex() for value in fields))
    else:
        for file in select_files(args.checkout, models):
            print(file.encode().hex())


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, re.error) as error:
        print(f"box selection: {error}", file=sys.stderr)
        sys.exit(1)
