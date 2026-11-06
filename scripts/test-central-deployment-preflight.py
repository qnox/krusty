#!/usr/bin/env python3
"""Exercise the actual GET client with isolated curl responses, without network or real secrets."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("central-deployment-preflight.py")
VERSION = "2.4.21-build.704"
NAME = "krusty-gradle-plugin-" + VERSION
NS = "dev.krustythecompiler"


def item(identity="deployment-704", version=VERSION, marker=True):
    artifacts = ["krusty-gradle-plugin", "dev.krustythecompiler.gradle.plugin"]
    return {"deploymentId": identity, "deploymentName": "krusty-gradle-plugin-" + version,
            "namespace": NS, "deploymentState": "PUBLISHING",
            "createTimestamp": "2026-11-06T10:00:00.000Z", "updateTimestamp": "2026-11-06T10:01:00.000Z",
            "deploymentComponents": [{"purl": f"pkg:maven/{NS}/{name}@{version}", "name": name + ".pom",
                                      "path": "dev/krustythecompiler/", "errors": [], "warnings": []}
                                     for name in artifacts[:2 if marker else 1]]}


def pages(items):
    count = (len(items) + 19) // 20
    return [{"deployments": items[offset:offset + 20], "page": offset // 20, "pageSize": 20,
             "pageCount": count, "totalResultCount": len(items)}
            for offset in range(0, max(1, len(items)), 20)]


CURL = r"""#!/usr/bin/env python3
import json,os,sys,stat
from pathlib import Path
args=sys.argv[1:]
assert args[args.index('--request')+1]=='GET' and '--get' in args
assert '--location' not in args and '--form' not in args and '--data' not in args
assert args[-1]=='https://central.sonatype.com/api/v1/publisher/deployments'
assert 'MAVEN_CENTRAL_USERNAME' not in os.environ and 'MAVEN_CENTRAL_PASSWORD' not in os.environ
header=Path(args[args.index('--header')+1][1:])
assert stat.S_IMODE(header.stat().st_mode)==0o600
assert header.read_text()=='Authorization: Bearer ZHVtbXktdXNlcjpkdW1teS1wYXNz\n'
queries=[args[i+1] for i,arg in enumerate(args) if arg=='--data-urlencode']
assert queries[0]=='namespace=dev.krustythecompiler'
assert queries[1]=='deploymentName=krusty-gradle-plugin-2.4.21-build.704'
assert queries[3:]==['size=20','sortField=createTimestamp','sortDirection=asc']
page=int(queries[2].split('=')[1])
fixture=json.loads(Path(os.environ['FIXTURE']).read_text())
with open(os.environ['REQUESTS'],'a') as log:log.write(json.dumps({'method':'GET','page':page,'queries':queries})+'\n')
body=fixture.get('body',json.dumps(fixture['pages'][page]))
Path(args[args.index('--output')+1]).write_text(body)
print(fixture.get('http','200'),end='')
sys.exit(fixture.get('exit',0))
"""


class Preflight(unittest.TestCase):
    def run_client(self, responses, code=0, **transport):
        with tempfile.TemporaryDirectory(prefix="central-client-test-") as stage:
            root = Path(stage)
            curl = root / "curl"
            curl.write_text(CURL)
            curl.chmod(0o755)
            fixture = root / "fixture.json"
            fixture.write_text(json.dumps({"pages": responses, **transport}))
            requests = root / "requests.jsonl"
            env = {"PATH": str(root) + os.pathsep + os.environ["PATH"], "TMPDIR": str(root),
                   "MAVEN_CENTRAL_USERNAME": "dummy-user", "MAVEN_CENTRAL_PASSWORD": "dummy-pass",
                   "FIXTURE": str(fixture), "REQUESTS": str(requests)}
            output = subprocess.run(["python3", "-B", str(SCRIPT), str(root / "out")], env=env,
                                    capture_output=True, timeout=10)
            self.assertEqual(output.returncode, code, output)
            summary = json.loads((root / "out/summary.json").read_text())
            self.assertFalse(summary["retry_authorized"])
            self.assertFalse(summary["payload_verified"])
            self.assertEqual(summary["source_commit"], "df118ba68648201b30538c1b022d1c39ebd2dc09")
            self.assertEqual(list(root.glob("central-read-*")), [])
            self.assertNotIn(b"dummy-pass", output.stdout + output.stderr)
            return summary, [json.loads(line) for line in requests.read_text().splitlines()]

    def test_empty_inventory_is_observation_not_retry_permission(self):
        summary, calls = self.run_client(pages([]))
        self.assertEqual(summary["finding"], "NO_MATCH_IN_VISIBLE_INVENTORY")
        self.assertTrue(summary["inventory_complete"])
        self.assertEqual(len(calls), 1)

    def test_each_observed_state_keeps_the_exact_candidate(self):
        for state in ["VALIDATING", "VALIDATED", "PUBLISHING", "PUBLISHED", "FAILED"]:
            with self.subTest(state=state):
                row = item(); row["deploymentState"] = state
                summary, _ = self.run_client(pages([row]))
                self.assertEqual(summary["finding"], "UNIQUE_CANDIDATE_REQUIRES_PAYLOAD_CHECK")
                self.assertEqual(summary["matching_deployments"], [row])

    def test_substring_name_does_not_match_a_different_version(self):
        summary, _ = self.run_client(pages([item(version=VERSION + "1")]))
        self.assertEqual(summary["matching_deployments"], [])

    def test_all_pages_are_read_before_selecting_exact_version(self):
        rows = [item(str(i), VERSION + str(i + 1)) for i in range(20)] + [item()]
        summary, calls = self.run_client(pages(rows))
        self.assertEqual([call["page"] for call in calls], [0, 1])
        self.assertEqual(summary["matching_deployments"], [rows[-1]])

    def test_duplicate_or_partial_candidates_fail_closed(self):
        for rows in [[item("one"), item("two")], [item(marker=False)]]:
            summary, _ = self.run_client(pages(rows), code=2)
            self.assertIn("error", summary)

    def test_pagination_gaps_duplicates_and_mutations_fail(self):
        original = pages([item(str(i), VERSION + str(i + 1)) for i in range(20)] + [item()])
        for change in ["gap", "duplicate", "changed-total", "wrong-page"]:
            with self.subTest(change=change):
                data = json.loads(json.dumps(original))
                if change == "gap": data[1]["deployments"] = []
                if change == "duplicate": data[1]["deployments"][0]["deploymentId"] = "0"
                if change == "changed-total": data[1]["totalResultCount"] += 1
                if change == "wrong-page": data[1]["page"] = 0
                summary, _ = self.run_client(data, code=2)
                self.assertFalse(summary["inventory_complete"])

    def test_page_cap_and_missing_component_schema_are_not_absence(self):
        oversized = pages([item()]); oversized[0].update(pageCount=21, totalResultCount=401)
        row = item(); row["purls"] = [component["purl"] for component in row.pop("deploymentComponents")]
        for data in [oversized, pages([row])]:
            summary, calls = self.run_client(data, code=2)
            self.assertFalse(summary["inventory_complete"])
            self.assertEqual(len(calls), 1)

    def test_auth_namespace_network_and_json_failures_are_not_absence(self):
        for transport in [{"http": status} for status in ["401", "403", "404", "500", "303"]] + [
                {"http": "000", "exit": 7}, {"body": "not-json"}]:
            with self.subTest(transport=transport):
                summary, _ = self.run_client(pages([]), code=2, **transport)
                self.assertFalse(summary["inventory_complete"])


if __name__ == "__main__":
    unittest.main()
