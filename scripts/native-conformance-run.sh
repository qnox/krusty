#!/usr/bin/env bash
# Run the Native codegen/box lane of one prebuilt conformance binary; see box-lane-run.sh.
exec bash "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/box-lane-run.sh" native "$@"
