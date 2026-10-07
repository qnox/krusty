import importlib.util
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace


SCRIPT = Path(__file__).parents[1] / "bench-kmp-ios-compile.py"
SPEC = importlib.util.spec_from_file_location("bench_kmp_ios_compile", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
BENCHMARK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCHMARK)


class Generator:
    @staticmethod
    def generate_kotlin_files(destination: Path, _count: int) -> None:
        (destination / "Generated0001.kt").write_text("val value: Int = 1,\n")


class BenchmarkOrchestrationTests(unittest.TestCase):
    def bench(self, root: Path):
        bench = BENCHMARK.Bench.__new__(BENCHMARK.Bench)
        bench.args = SimpleNamespace(kotlinc="tools/kotlinc")
        bench.env = {"KRUSTY_KOTLINC": str(Path("tools/kotlinc").resolve())}
        bench.gen = Generator()
        bench.root = root
        bench.root.mkdir()
        bench.rows = []
        return bench

    def test_kotlin_trials_compile_then_edit_and_rebuild_the_same_tree(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            bench = self.bench(Path(temp) / "bench")
            calls = []

            def compile_source(source: Path, output: Path):
                text = (source / "Generated0001.kt").read_text()
                calls.append((source, output, text))
                return 0, float(len(calls)), "", 1

            clean, rebuild, classes = bench.kotlin_trials(1, "compiler", compile_source)

            self.assertEqual(classes, 1)
            self.assertEqual(clean, [3.0, 5.0, 7.0])
            self.assertEqual(rebuild, [4.0, 6.0, 8.0])
            self.assertEqual(len(calls), 8)
            for baseline, edited in zip(calls[::2], calls[1::2]):
                self.assertEqual(baseline[:2], edited[:2])
                self.assertIn("= 1,", baseline[2])
                self.assertIn("= 1001,", edited[2])

    def test_selected_kotlinc_is_the_krusty_distribution(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            generator = Path(temp) / "generator.py"
            generator.write_text("def generate_kotlin_files(destination, count): pass\n")
            work = Path(temp) / "work"
            selected = Path(temp) / "kotlinc" / "bin" / "kotlinc"
            args = SimpleNamespace(generator=str(generator), work=str(work), kotlinc=str(selected))

            bench = BENCHMARK.Bench(args)

            self.assertEqual(bench.env["KRUSTY_KOTLINC"], str(selected.resolve()))


if __name__ == "__main__":
    unittest.main()
