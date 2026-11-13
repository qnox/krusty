#!/usr/bin/env python3
"""Resolve and hash the runtime Maven classpath for one exact Kotlin compiler release."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import subprocess
import sys
import tempfile
import urllib.request
import xml.etree.ElementTree as ET


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("version")
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    version = args.version
    if version != "2.5.0-RC":
        raise SystemExit(f"this evidence run is pinned to exact 2.5.0-RC, got {version!r}")

    with tempfile.TemporaryDirectory(prefix="krusty-kotlin-pins-") as temp:
        root = Path(temp)
        deps = root / "dependencies"
        pom = root / "pom.xml"
        pom.write_text(
            f"""<project xmlns=\"http://maven.apache.org/POM/4.0.0\"
              xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"
              xsi:schemaLocation=\"http://maven.apache.org/POM/4.0.0 https://maven.apache.org/xsd/maven-4.0.0.xsd\">
              <modelVersion>4.0.0</modelVersion><groupId>dev.krusty.evidence</groupId>
              <artifactId>kotlin-compiler-classpath</artifactId><version>1</version>
              <dependencies>
                <dependency><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-compiler-embeddable</artifactId><version>{version}</version></dependency>
                <dependency><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-reflect</artifactId><version>1.6.10</version></dependency>
                <dependency><groupId>org.jetbrains.kotlinx</groupId><artifactId>kotlinx-coroutines-core-jvm</artifactId><version>1.8.0</version></dependency>
                <dependency><groupId>org.jetbrains</groupId><artifactId>annotations</artifactId><version>13.0</version></dependency>
                <dependency><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-scripting-common</artifactId><version>{version}</version></dependency>
                <dependency><groupId>org.jetbrains.kotlin</groupId><artifactId>kotlin-scripting-jvm</artifactId><version>{version}</version></dependency>
              </dependencies>
            </project>\n""",
            encoding="utf-8",
        )
        subprocess.run(
            [
                "mvn", "-B", "-f", str(pom),
                "org.apache.maven.plugins:maven-dependency-plugin:3.8.1:copy-dependencies",
                "-DincludeScope=runtime", "-DuseRepositoryLayout=true",
                f"-DoutputDirectory={deps}",
            ],
            check=True,
            stdout=sys.stderr,
        )
        jars = sorted(deps.rglob("*.jar"))
        expected = {
            "kotlin-compiler-embeddable", "kotlin-build-tools-api", "kotlin-daemon-embeddable",
            "kotlin-script-runtime", "kotlin-stdlib", "kotlin-reflect",
            "kotlinx-coroutines-core-jvm", "annotations", "kotlin-scripting-common",
            "kotlin-scripting-jvm",
        }
        found = {p.parts[-3] for p in jars}
        if not expected <= found:
            raise SystemExit(f"resolved Maven classpath is missing expected artifacts: {sorted(expected - found)}")

        rows = []
        for jar in jars:
            rel = jar.relative_to(deps)
            parts = rel.parts
            if len(parts) < 4:
                raise SystemExit(f"unexpected Maven repository path: {rel}")
            group = ".".join(parts[:-3])
            artifact, artifact_version = parts[-3], parts[-2]
            if jar.name != f"{artifact}-{artifact_version}.jar":
                raise SystemExit(f"unexpected artifact filename: {rel}")
            data = jar.read_bytes()
            sha256 = hashlib.sha256(data).hexdigest()
            sha1_url = (
                "https://repo.maven.apache.org/maven2/" + "/".join(parts) + ".sha1"
            )
            with urllib.request.urlopen(sha1_url, timeout=60) as response:
                published_sha1 = response.read().decode("ascii").strip().split()[0]
            if hashlib.sha1(data).hexdigest() != published_sha1:
                raise SystemExit(f"Maven Central SHA-1 mismatch for {group}:{artifact}:{artifact_version}")
            rows.append(
                f"{version}\t{group}:{artifact}:{artifact_version}\t{sha256}\t{len(data)}"
            )

        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text("\n".join(rows) + "\n", encoding="utf-8")
        print(f"Resolved {len(rows)} exact Maven artifacts; wrote {args.output}")


if __name__ == "__main__":
    main()
