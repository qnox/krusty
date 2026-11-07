import tempfile
import unittest
from pathlib import Path

from box_selection import Model, extract_models, select_files


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
