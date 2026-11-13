import tempfile
import subprocess
import sys
import unittest
from pathlib import Path

from box_selection import (DSL_SOURCE, GENERATORS, Model, extract_models, select_files)


class BoxSelectionTests(unittest.TestCase):
    def test_every_box_model_root_is_included(self):
        source = '''
            val k1 = listOf("multiplatform/k1")
            testGroup("generated", "compiler/testData/codegen") {
                testClass<AbstractFirLightTreeBlackBoxCodegenTest> {
                    model("box", excludeDirs = k1)
                    model("boxJvm")
                }
                testClass<AbstractFirLightTreeBlackBoxCodegenTest>("ModernJdk") {
                    model("boxModernJdk")
                }
                testClass<AbstractFirLightTreeBlackBoxInlineCodegenTest> {
                    model("boxInline")
                }
            }
        '''
        models = extract_models(source, source_name="jvm.kt", target="jvm")
        self.assertEqual(
            [m.root for m in models],
            ["compiler/testData/codegen/" + name for name in
             ["box", "boxJvm", "boxModernJdk", "boxInline"]],
        )
        self.assertEqual(models[0].exclude_dirs, ("multiplatform/k1",))

    def test_native_specific_and_klib_box_roots_are_included(self):
        source = '''
            testGroup("generated", "native/native.tests/testData/codegen") {
                testClass<AbstractNativeCodegenBoxCoreTest> { model() }
            }
            testGroup("generated", "compiler/testData/klib/syntheticAccessors") {
                testClass<AbstractNativeCodegenBoxTest> { model() }
            }
        '''
        self.assertEqual(
            [m.root for m in extract_models(source, source_name="native.kt", target="native")],
            ["native/native.tests/testData/codegen",
             "compiler/testData/klib/syntheticAccessors"],
        )

    def test_wasm_targets_and_filename_patterns_remain_distinct(self):
        source = r'''
            val jvmOnly = listOf("compileKotlinAgainstKotlin")
            val pattern = "^([^_](.+))\\.kt$"
            testGroup("generated", "compiler/testData/codegen") {
                testClass<AbstractFirWasmJsCodegenBoxTest> {
                    model("box", pattern = pattern, excludeDirs = jvmOnly)
                    model("boxInline")
                }
                testClass<AbstractFirWasmWasiCodegenBoxTest> {
                    model("boxWasmWasi")
                }
            }
        '''
        js = extract_models(source, source_name="wasm.kt", target="wasm-js")
        wasi = extract_models(source, source_name="wasm.kt", target="wasm-wasi")
        self.assertEqual([m.root for m in js],
                         ["compiler/testData/codegen/box", "compiler/testData/codegen/boxInline"])
        self.assertEqual([m.root for m in wasi], ["compiler/testData/codegen/boxWasmWasi"])
        self.assertEqual(js[0].pattern, r"^([^_](.+))\.kt$")
        self.assertEqual(js[1].pattern, r"^(.+)\.kt$")

    def test_selection_uses_exact_directory_boundaries_and_per_model_patterns(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            names = ["box/compileKotlinAgainstKotlin/a.kt",
                     "box/compileKotlinAgainstKotlinExtra/ok.kt",
                     "box/a.kt", "box/ok.kt", "box/_helper.kt",
                     "box/multiplatform/k1/old.kt", "box/other.kts",
                     "boxInline/a.kt", "boxInline/_helper.kt"]
            for name in names:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            models = [Model("box", r"^([^_](.+))\.kt$",
                            ("compileKotlinAgainstKotlin", "multiplatform/k1")),
                      Model("boxInline")]
            self.assertEqual(select_files(root, models),
                             ["box/compileKotlinAgainstKotlinExtra/ok.kt", "box/ok.kt",
                              "boxInline/_helper.kt", "boxInline/a.kt"])

    def test_common_inventory_unions_models_while_retaining_target_view(self):
        # Model membership is an upstream runner choice, not a platform directive on every case.
        # Keep the original per-model exclusion while another model contributes the portable case.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dsl = root / DSL_SOURCE
            dsl.parent.mkdir(parents=True)
            dsl.write_text(r'''fun model(extension: String? = "kt") {
                val pattern = if (extension == null) "" else """^(.+)\.$extension$"""
            }''')
            for target, anchors in GENERATORS.items():
                for anchor in anchors:
                    path = root / anchor
                    path.parent.mkdir(parents=True, exist_ok=True)
                    if target == "jvm":
                        models = 'model("box", excludeDirs = listOf("excludedEverywhere"))'
                        runner = "AbstractFirLightTreeBlackBoxCodegenTest"
                    elif target == "native":
                        models = 'model("nativeBox")'
                        runner = "AbstractNativeCodegenBoxTest"
                    else:
                        # The same Wasm generator source declares both target models.
                        models = 'model("box", excludeDirs = listOf("compileKotlinAgainstKotlin", "excludedEverywhere"))'
                        runner = "AbstractFirWasmJsCodegenBoxTest"
                    source = f'''fun main() {{
                        generateTestGroupSuite {{
                            testGroup("generated", "data") {{
                                testClass<{runner}> {{ {models} }}
                            }}
                        }}
                    }}'''
                    if target.startswith("wasm"):
                        source = source.replace(
                            'testClass<AbstractFirWasmJsCodegenBoxTest> { ' + models + ' }',
                            'testClass<AbstractFirWasmJsCodegenBoxTest> { ' + models + ' }\n' +
                            'testClass<AbstractFirWasmWasiCodegenBoxTest> { model("wasiBox") }')
                    path.write_text(source)
            cases = ["box/ordinary.kt", "box/compileKotlinAgainstKotlin/portable.kt",
                     "box/excludedEverywhere/notACase.kt", "nativeBox/native.kt",
                     "wasiBox/wasi.kt"]
            for name in cases:
                path = root / "data" / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('fun box(): String = "OK"\n')
            script = Path(__file__).with_name("box_selection.py")
            def selected(*options):
                result = subprocess.run([sys.executable, str(script), "--checkout", str(root),
                                         *options], capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                return [bytes.fromhex(line).decode() for line in result.stdout.splitlines()]
            self.assertEqual(selected("--target", "wasm-js"), ["data/box/ordinary.kt"])
            self.assertEqual(selected("--common-inventory"),
                             ["data/box/compileKotlinAgainstKotlin/portable.kt",
                              "data/box/ordinary.kt", "data/nativeBox/native.kt",
                              "data/wasiBox/wasi.kt"])

    def test_missing_upstream_root_is_an_error(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "missing upstream box root"):
                select_files(Path(directory), [Model("boxMissing")])

    def test_box_runner_subclasses_keep_their_roots(self):
        source = '''
            testGroup("generated", "compiler/testData/codegen") {
                testClass<AbstractRenamedReflectionTest> { model("boxJvm/reflection") }
            }
        '''
        parents = {"AbstractRenamedReflectionTest": ["AbstractFirPsiBlackBoxCodegenTest"]}
        models = extract_models(source, source_name="reflection.kt", target="jvm",
                                runner_resolver=lambda name: parents.get(name, []))
        self.assertEqual([model.root for model in models], ["compiler/testData/codegen/boxJvm/reflection"])

    def test_unknown_box_runner_is_an_error_not_a_silent_omission(self):
        source = '''
            testGroup("generated", "compiler/testData/codegen") {
                testClass<AbstractNewRunner> { model("boxNew") }
            }
        '''
        with self.assertRaisesRegex(ValueError, "unclassified upstream box runner"):
            extract_models(source, source_name="future.kt", target="jvm")

    def test_debugger_subclasses_are_not_mistaken_for_box_suites(self):
        source = '''
            testGroup("generated", "compiler/testData") {
                testClass<AbstractFirSteppingTest> { model("debug/stepping") }
            }
        '''
        parents = {"AbstractFirSteppingTest": ["AbstractFirPsiBlackBoxCodegenTest"]}
        self.assertEqual(extract_models(source, source_name="debug.kt", target="jvm",
                                       runner_resolver=lambda name: parents.get(name, [])), [])


if __name__ == "__main__":
    unittest.main()
