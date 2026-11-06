#!/usr/bin/env python3
"""Read-only incident inventory, never permission to retry an immutable publication.

GET schema: https://central.sonatype.com/api-doc (listDeployments).
Authentication: https://central.sonatype.org/publish/publish-portal-api/.
"""
import base64
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
from urllib.parse import unquote, urlsplit

VERSION = "2.4.21-build.704"
NAMESPACE = "dev.krustythecompiler"
# The original704 uploader supplies this name; the API filter is only a substring match.
DEPLOYMENT_NAME = "krusty-gradle-plugin-" + VERSION
SOURCE = "df118ba68648201b30538c1b022d1c39ebd2dc09"
ARTIFACTS = {"krusty-gradle-plugin", "dev.krustythecompiler.gradle.plugin"}
STATES = {"VALIDATING", "VALIDATED", "PUBLISHING", "PUBLISHED", "FAILED"}
ENDPOINT = "https://central.sonatype.com/api/v1/publisher/deployments"
SIZE, MAX_PAGES, SECONDS = 20, 20, 180


def require(condition, message):
    if not condition:
        raise ValueError(message)


def coordinate(purl):
    require(isinstance(purl, str), "component purl must be a string")
    parsed = urlsplit(purl)
    parts = parsed.path.split("/")
    require(parsed.scheme == "pkg" and len(parts) == 3 and parts[0] == "maven",
            "unsupported component purl")
    artifact, separator, version = parts[2].rpartition("@")
    require(separator and artifact and version, "component purl lacks exact version")
    return tuple(unquote(value) for value in (parts[1], artifact, version))


def inventory(out, authorization):
    deadline = time.monotonic() + SECONDS
    seen, deployments, pagination = set(), [], None
    child_env = {key: value for key, value in os.environ.items()
                 if key not in {"MAVEN_CENTRAL_USERNAME", "MAVEN_CENTRAL_PASSWORD"}}
    for page in range(MAX_PAGES):
        remaining = int(deadline - time.monotonic())
        require(remaining > 0, "inventory deadline exceeded")
        limit = min(30, remaining)
        response = out / f"page-{page:03}.json"
        command = ["curl", "--silent", "--show-error", "--request", "GET", "--get",
                   "--connect-timeout", str(min(10, limit)), "--max-time", str(limit),
                   "--max-filesize", "1048576", "--header", "@" + str(authorization),
                   "--output", str(response), "--write-out", "%{http_code}"]
        for query in ["namespace=" + NAMESPACE, "deploymentName=" + DEPLOYMENT_NAME,
                      f"page={page}", f"size={SIZE}", "sortField=createTimestamp", "sortDirection=asc"]:
            command.extend(["--data-urlencode", query])
        command.append(ENDPOINT)
        result = subprocess.run(command, env=child_env, capture_output=True, timeout=limit + 5)
        (out / f"page-{page:03}.stderr").write_bytes(result.stderr)
        status = result.stdout.decode("ascii", errors="strict")
        (out / f"page-{page:03}.http.json").write_text(json.dumps(
            {"method": "GET", "endpoint": ENDPOINT, "page": page,
             "http_status": status, "curl_exit": result.returncode}) + "\n")
        require(result.returncode == 0 and status == "200", "deployment list request failed")
        require(response.stat().st_size <= 1048576, "deployment page exceeds retained bound")
        data = json.loads(response.read_bytes())
        require(isinstance(data, dict), "deployment page must be an object")
        for key in ["page", "pageSize", "pageCount", "totalResultCount"]:
            require(type(data.get(key)) is int and data[key] >= 0, "invalid pagination field " + key)
        require(data["page"] == page and data["pageSize"] == SIZE, "unexpected pagination coordinate")
        pages, total = data["pageCount"], data["totalResultCount"]
        require((pages == (total + SIZE - 1) // SIZE) or (total == 0 and pages == 1),
                "inconsistent page count")
        require(pages <= MAX_PAGES, "inventory exceeds bounded page count; no absence conclusion")
        current = (pages, total)
        require(pagination is None or pagination == current, "inventory changed while paginating")
        pagination = current
        rows = data.get("deployments")
        require(isinstance(rows, list) and len(rows) == min(SIZE, max(0, total - page * SIZE)),
                "incomplete page")
        for row in rows:
            require(isinstance(row, dict), "invalid deployment record")
            identity = row.get("deploymentId")
            require(isinstance(identity, str) and identity and identity not in seen,
                    "missing or duplicated deployment identity")
            require(row.get("namespace") == NAMESPACE, "unexpected deployment namespace")
            name = row.get("deploymentName")
            require(isinstance(name, str) and DEPLOYMENT_NAME.casefold() in name.casefold(),
                    "unexpected deployment-name filter result")
            require(row.get("deploymentState") in STATES, "unknown deployment state")
            components = row.get("deploymentComponents")
            require(isinstance(components, list) and components, "component inventory unavailable")
            for component in components:
                require(isinstance(component, dict), "invalid deployment component")
                coordinate(component.get("purl"))
            seen.add(identity)
            deployments.append(row)
        if page + 1 >= pages:
            require(len(deployments) == total, "inventory total mismatch")
            return deployments
    raise ValueError("pagination did not complete")


def main():
    require(len(sys.argv) == 2, "usage: central-deployment-preflight.py OUTPUT_DIRECTORY")
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=False)
    summary = {"version": VERSION, "namespace": NAMESPACE, "source_commit": SOURCE,
               "release_id": 436350079, "tag": "v" + VERSION, "original_run": 42187960174,
               "controller_revision": os.environ.get("GITHUB_SHA"), "retry_authorized": False,
               "query_filters": {"namespace_exact": NAMESPACE, "deployment_name_substring": DEPLOYMENT_NAME},
               "payload_verified": False, "inventory_complete": False,
               "existing_zip_evidence": {"asset_id": 716727043, "files": 43,
                   "sha256": "a2214d0f06298545eba060cddc5f26922ee4e3ed1ea27c05d33dbcd609e02426",
                   "qualification": "Previously verified incident evidence; this list probe does not reverify payload bytes."},
               "qualification": "Current visible inventory only. Missing journal/list entries do not prove an upload was never accepted. A candidate still requires payload/provenance reconciliation."}
    code = 2
    try:
        username = os.environ.get("MAVEN_CENTRAL_USERNAME", "")
        password = os.environ.get("MAVEN_CENTRAL_PASSWORD", "")
        require(username and password, "Central read credentials are required")
        token = base64.b64encode((username + ":" + password).encode()).decode()
        if os.environ.get("GITHUB_ACTIONS") == "true":
            print("::add-mask::" + token, flush=True)
        with tempfile.TemporaryDirectory(prefix="central-read-") as stage:
            authorization = Path(stage) / "authorization"
            descriptor = os.open(authorization, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "w") as stream:
                stream.write("Authorization: Bearer " + token + "\n")
            deployments = inventory(out, authorization)
        summary["inventory_complete"] = True
        matches = []
        for row in deployments:
            coordinates = {coordinate(component["purl"]) for component in row["deploymentComponents"]}
            names = {name for group, name, version in coordinates
                     if group == NAMESPACE and version == VERSION}
            if names:
                require(names == ARTIFACTS, "partial or unexpected version deployment")
                matches.append(row)
            elif row["deploymentName"].casefold() == DEPLOYMENT_NAME.casefold():
                raise ValueError("named incident deployment lacks the expected exact component versions")
        summary["matching_deployments"] = matches
        require(len(matches) <= 1, "multiple matching deployments; manual reconciliation required")
        summary["finding"] = "UNIQUE_CANDIDATE_REQUIRES_PAYLOAD_CHECK" if matches else "NO_MATCH_IN_VISIBLE_INVENTORY"
        code = 0
    except (ValueError, TypeError, OSError, subprocess.SubprocessError) as error:
        summary["error"] = str(error)
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({key: summary[key] for key in ["inventory_complete", "retry_authorized"]}))
    return code


if __name__ == "__main__":
    sys.exit(main())
