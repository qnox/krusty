import importlib.util
import pathlib
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "route-request.py"
spec = importlib.util.spec_from_file_location("route_request", SCRIPT)
route = importlib.util.module_from_spec(spec)
spec.loader.exec_module(route)

SELF = "${{ (github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name != github.repository) && 'ubuntu-latest' || fromJSON('[\"self-hosted\", \"rust-4c-8gb\"]') }}"


def run(jobs, outputs=None):
    return route.request({"jobs": jobs}, outputs or {})["classes"]


class RouteRequestTest(unittest.TestCase):
    def test_hosted_job_may_run_only_on_github(self):
        self.assertEqual(
            run({"ci": {"runs-on": "ubuntu-latest"}}),
            [{"key": "ci", "name": "ci", "default": ["ubuntu-latest"], "hosted": ["ubuntu-latest"], "needs": []}],
        )

    def test_self_hosted_job_may_run_on_its_labels_or_github(self):
        self.assertEqual(
            run({"lint": {"runs-on": SELF, "needs": "ci"}, "ci": {"runs-on": "ubuntu-latest"}})[0],
            {
                "key": "lint",
                "name": "lint",
                "default": ["self-hosted", "rust-4c-8gb"],
                "hosted": ["ubuntu-latest"],
                "self_allowed": [["self-hosted", "rust-4c-8gb"]],
                "needs": ["ci"],
            },
        )

    def test_unnamed_matrix_rows_take_the_matrix_keys_in_order(self):
        (c,) = run({"bazel": {"runs-on": SELF, "strategy": {"matrix": {"bazel": ["8.4.2", "9.3.1"], "os": ["x"]}}}})
        self.assertEqual(c["name"], "bazel")
        self.assertEqual(c["matrix_keys"], ["bazel", "os"])
        self.assertEqual(c["rows"], [{"bazel": "8.4.2", "os": "x"}, {"bazel": "9.3.1", "os": "x"}])

    def test_matrix_from_a_given_output(self):
        matrix = {"version": "${{ fromJson(needs.bins.outputs.matrix) }}", "target": ["jvm"]}
        job = {"name": "conformance (${{ matrix.version }}, ${{ matrix.target }})", "runs-on": SELF, "strategy": {"matrix": matrix}}
        (c,) = run({"conformance": job}, {"bins.matrix": ["2.4.0", "2.4.10"]})
        self.assertEqual(c["name"], "conformance")
        self.assertEqual(c["matrix_keys"], ["version", "target"])
        self.assertEqual(c["rows"], [{"version": "2.4.0", "target": "jvm"}, {"version": "2.4.10", "target": "jvm"}])

    def test_matrix_from_an_output_not_given_is_left_out(self):
        job = {"runs-on": SELF, "strategy": {"matrix": {"v": "${{ fromJson(needs.bins.outputs.matrix) }}"}}}
        self.assertEqual(run({"conformance": job}), [])

    def test_include_extends_matching_rows_and_adds_the_rest(self):
        matrix = {
            "kotlin": ["2.4.0", "2.4.10"],
            "gradle": ["9.7.0"],
            "include": [{"kotlin": "2.4.0", "ksp": True}, {"kotlin": "2.4.0", "gradle": "7.6.3", "ksp": False}],
        }
        (c,) = run({"gradle": {"runs-on": SELF, "strategy": {"matrix": matrix}}})
        self.assertEqual(c["matrix_keys"], ["kotlin", "gradle", "ksp"])
        self.assertEqual(
            c["rows"],
            [
                {"kotlin": "2.4.0", "gradle": "9.7.0", "ksp": True},
                {"kotlin": "2.4.10", "gradle": "9.7.0"},
                {"kotlin": "2.4.0", "gradle": "7.6.3", "ksp": False},
            ],
        )

    def test_exclude_drops_matching_rows(self):
        matrix = {"a": [1, 2], "b": ["x", "y"], "exclude": [{"a": 2, "b": "y"}]}
        (c,) = run({"j": {"runs-on": SELF, "strategy": {"matrix": matrix}}})
        self.assertEqual(c["rows"], [{"a": 1, "b": "x"}, {"a": 1, "b": "y"}, {"a": 2, "b": "x"}])

    def test_the_asking_job_is_not_placed(self):
        jobs = {"route": {"runs-on": "ubuntu-latest"}, "ci": {"runs-on": "ubuntu-latest"}}
        self.assertEqual([c["key"] for c in route.request({"jobs": jobs}, {}, "route")["classes"]], ["ci"])

    def test_conditional_job_and_its_dependents_are_left_out(self):
        jobs = {
            "ci": {"runs-on": "ubuntu-latest"},
            "build-release": {"runs-on": "ubuntu-latest", "if": "github.ref == 'refs/heads/master'", "needs": ["ci"]},
            "release": {"runs-on": "ubuntu-latest", "needs": ["build-release"]},
            "gate": {"runs-on": SELF, "if": "always()", "needs": ["ci"]},
        }
        self.assertEqual([c["key"] for c in run(jobs)], ["ci", "gate"])

    def test_non_scalar_matrix_and_unreadable_labels_are_left_out(self):
        jobs = {
            "release": {"runs-on": "${{ matrix.target.os }}", "strategy": {"matrix": {"target": [{"os": "ubuntu-latest"}]}}},
            "odd": {"runs-on": "${{ inputs.runner }}"},
        }
        self.assertEqual(run(jobs), [])

    def test_matrix_name_with_other_text_is_left_out(self):
        job = {"name": "conf ${{ matrix.v }}", "runs-on": SELF, "strategy": {"matrix": {"v": ["1"]}}}
        self.assertEqual(run({"conf": job}), [])

    def test_the_ci_workflow_routes_every_unconditional_job(self):
        workflow = route.yaml.safe_load((SCRIPT.parents[1] / ".github/workflows/ci.yml").read_text())
        classes = route.request(workflow, {"build-tool-bins.matrix": ["2.4.0"]})["classes"]
        routed = {c["key"] for c in classes}
        conditional = {k for k, j in workflow["jobs"].items() if j.get("if") not in (None, "always()")}
        self.assertTrue(conditional)
        self.assertFalse(routed & conditional)
        names = [c["name"] for c in classes if not c.get("matrix_keys")]
        self.assertEqual(len(names), len(set(names)))


if __name__ == "__main__":
    unittest.main()
