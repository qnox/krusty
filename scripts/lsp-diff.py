#!/usr/bin/env python3
"""Compare krusty-lsp's diagnostics against a reference Kotlin language server, file by file.

A language server is judged by what it reports in the editor: every diagnostic must be one the
reference server also reports, at the same place, with the same wording. This drives two servers
over the same project and the same documents and classifies every difference:

  extra    krusty reported it, the reference did not  -- a FALSE POSITIVE, the worst kind
  missing  the reference reported it, krusty did not
  wording  same position, different message text
  moved    same message, different range

Usage:
  scripts/lsp-diff.py <project-root> --reference '<cmd> <args...>' [--krusty '<cmd> <args...>']
                      [--limit N | <file.kt> ...] [--json report.json] [--timeout S]
"""
import argparse
import json
import os
import pathlib
import queue
import shlex
import signal
import subprocess
import sys
import threading
import time

PUSH_SETTLE_SECONDS = 2.0
EMPTY_PULL_SETTLE_SECONDS = 5.0


class Lsp:
    """Minimal LSP client over stdio: initialize, open documents, collect publishDiagnostics."""

    def __init__(self, argv, root, name):
        self.name = name
        self.stderr_log = None
        log_dir = os.environ.get("LSP_DIFF_LOG_DIR")
        if log_dir:
            directory = pathlib.Path(log_dir)
            directory.mkdir(parents=True, exist_ok=True)
            self.stderr_log = (directory / f"{name}.log").open("wb")
        try:
            self.proc = subprocess.Popen(
                argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=self.stderr_log or subprocess.DEVNULL,
                cwd=root,
                # Own process group: the launcher forks a JVM, and killing only the launcher
                # would leave that JVM indexing forever.
                start_new_session=True)
        except Exception:
            if self.stderr_log:
                self.stderr_log.close()
            raise
        # start_new_session makes the child both the session and process-group leader. Keep the
        # group id even if the launcher exits so shutdown can still reap a surviving child JVM.
        self.process_group = self.proc.pid
        self.inbox = queue.Queue()
        self.next_id = 1
        self.pull = False
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        out = self.proc.stdout
        while True:
            headers = {}
            while True:
                line = out.readline()
                if not line:
                    self.inbox.put(None)
                    return
                line = line.strip()
                if not line:
                    break
                key, _, value = line.partition(b":")
                headers[key.strip().lower()] = value.strip()
            length = headers.get(b"content-length")
            if length is None:
                continue
            body = out.read(int(length))
            try:
                self.inbox.put(json.loads(body))
            except ValueError:
                continue

    def send(self, message):
        body = json.dumps(message).encode()
        self.proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        self.proc.stdin.flush()

    def notify(self, method, params):
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def _answer_server_request(self, message):
        result = None
        if message["method"] == "workspace/configuration":
            result = [None] * len(message["params"].get("items", []))
        elif message["method"] in ("window/workDoneProgress/create", "client/registerCapability"):
            result = None
        self.send({"jsonrpc": "2.0", "id": message["id"], "result": result})

    def request(self, method, params, timeout):
        rid = self.next_id
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        deferred = []
        deadline = time.monotonic() + timeout
        try:
            while time.monotonic() < deadline:
                try:
                    message = self.inbox.get(
                        timeout=max(0.001, deadline - time.monotonic()))
                except queue.Empty:
                    break
                if message is None:
                    raise RuntimeError(f"{self.name}: server closed the connection")
                if message.get("id") == rid and ("result" in message or "error" in message):
                    return message
                if "id" in message and "method" in message:
                    self._answer_server_request(message)
                else:
                    deferred.append(message)
        finally:
            # A timed-out request must not silently discard diagnostics/progress notifications that
            # arrived while it was waiting for its own response.
            for held in deferred:
                self.inbox.put(held)
        raise TimeoutError(f"{self.name}: {method} timed out")

    def initialize(self, root, timeout):
        root_uri = pathlib.Path(root).as_uri()
        response = self.request("initialize", {
            "processId": None,
            "rootUri": root_uri,
            "rootPath": root,
            "workspaceFolders": [{"uri": root_uri, "name": os.path.basename(root)}],
            "capabilities": {
                "textDocument": {
                    "publishDiagnostics": {"relatedInformation": True},
                    "diagnostic": {"dynamicRegistration": True, "relatedDocumentSupport": False},
                },
                "workspace": {"workspaceFolders": True, "configuration": True},
            },
        }, timeout)
        if "error" in response:
            raise RuntimeError(f"{self.name}: initialize failed: {response['error']}")
        self.notify("initialized", {})
        # A server may PUSH diagnostics (publishDiagnostics) or serve them on PULL
        # (textDocument/diagnostic). Which one it is decides how this client must ask.
        capabilities = (response.get("result") or {}).get("capabilities") or {}
        self.pull = capabilities.get("diagnosticProvider") is not None
        return response["result"]

    def diagnostics_for(self, path, timeout):
        """Open `path` and return its diagnostics (None if the server never reported any)."""
        uri = pathlib.Path(path).as_uri()
        with open(path, encoding="utf-8", errors="replace") as handle:
            text = handle.read()
        self.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": "kotlin", "version": 1, "text": text}})
        try:
            if self.pull:
                return self._pull_diagnostics(uri, timeout)
            deadline = time.monotonic() + timeout
            latest = None
            settle_deadline = None
            while time.monotonic() < deadline:
                wait_until = min(deadline, settle_deadline) if settle_deadline else deadline
                try:
                    message = self.inbox.get(
                        timeout=max(0.001, wait_until - time.monotonic()))
                except queue.Empty:
                    break
                if message is None:
                    raise RuntimeError(f"{self.name}: server closed the connection")
                if message.get("method") == "textDocument/publishDiagnostics":
                    if message["params"]["uri"] == uri:
                        latest = message["params"]["diagnostics"]
                        # Push servers commonly publish an empty preliminary result followed by the
                        # analyzed one. Keep the latest publication after a short quiet period.
                        settle_deadline = min(
                            deadline, time.monotonic() + PUSH_SETTLE_SECONDS)
                elif "id" in message and "method" in message:
                    self._answer_server_request(message)
            return latest
        finally:
            try:
                self.notify("textDocument/didClose", {"textDocument": {"uri": uri}})
            except (BrokenPipeError, OSError):
                pass

    def _pull_diagnostics(self, uri, timeout):
        """Ask for this document's diagnostics until the server stops answering 'not ready yet'.

        Analysis is asynchronous: right after `didOpen` the server can legitimately answer with an
        empty report because indexing has not reached the file. Re-ask until non-empty reports agree;
        an empty result must also remain stable for a minimum window, so "clean" does not mean "the
        first two requests were both too early".
        """
        deadline = time.monotonic() + timeout
        previous = None
        first_observed_at = None
        while time.monotonic() < deadline:
            response = self.request("textDocument/diagnostic", {
                "textDocument": {"uri": uri}}, max(0.001, deadline - time.monotonic()))
            if "error" in response:
                return None
            report = response.get("result") or {}
            items = report.get("items")
            if items is None:
                return None
            now = time.monotonic()
            if previous is not None and items == previous:
                if items or now - first_observed_at >= EMPTY_PULL_SETTLE_SECONDS:
                    return items
            else:
                previous = items
                first_observed_at = now
            time.sleep(min(2, max(0, deadline - time.monotonic())))
        return previous

    def shutdown(self):
        try:
            self.request("shutdown", {}, 15)
            self.notify("exit", {})
        except Exception:
            pass
        try:
            self.proc.wait(timeout=10)
        except Exception:
            pass
        # Even if the launcher exited cleanly, a forked JVM may still own this process group.
        # Signal only the group created by this Lsp instance; never sweep unrelated editor servers.
        for signal_number in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(self.process_group, signal_number)
            except OSError:
                break
            try:
                self.proc.wait(timeout=5)
            except Exception:
                pass
        for stream in (self.proc.stdin, self.proc.stdout):
            try:
                stream.close()
            except (AttributeError, OSError):
                pass
        if self.stderr_log:
            self.stderr_log.close()


def start(line, character):
    return (line, character)


def key_of(diagnostic):
    diagnostic_range = diagnostic.get("range", {})
    start_position = diagnostic_range.get("start", {})
    end_position = diagnostic_range.get("end", {})
    return (
        start_position.get("line", -1),
        start_position.get("character", -1),
        end_position.get("line", -1),
        end_position.get("character", -1),
    )


def normalize(message):
    """Collapse whitespace so a wrapped message compares equal to a single-line one."""
    return " ".join((message or "").split())


def compare(reference, krusty):
    """Classify one file's diagnostics into matched / extra / missing / wording / moved."""
    result = {"matched": [], "extra": [], "missing": [], "wording": [], "moved": []}
    remaining = list(krusty)
    for expected in reference:
        position = key_of(expected)
        text = normalize(expected.get("message"))
        same_place = [d for d in remaining if key_of(d) == position]
        exact = next((d for d in same_place if normalize(d.get("message")) == text), None)
        if exact is not None:
            remaining.remove(exact)
            result["matched"].append({"at": position, "message": text})
            continue
        if same_place:
            got = same_place[0]
            remaining.remove(got)
            result["wording"].append({
                "at": position,
                "reference": text,
                "krusty": normalize(got.get("message")),
            })
            continue
        elsewhere = next((d for d in remaining if normalize(d.get("message")) == text), None)
        if elsewhere is not None:
            remaining.remove(elsewhere)
            result["moved"].append({
                "message": text,
                "reference_at": position,
                "krusty_at": key_of(elsewhere),
            })
            continue
        result["missing"].append({"at": position, "message": text})
    for leftover in remaining:
        result["extra"].append({"at": key_of(leftover), "message": normalize(leftover.get("message"))})
    return result


def find_kotlin_files(root, limit):
    found = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [
            d for d in dirnames
            if d not in (".git", ".gradle", "out", "build", "target", "node_modules")
        ]
        dirnames.sort()
        for name in sorted(filenames):
            if name.endswith(".kt"):
                found.append(os.path.join(dirpath, name))
                if len(found) >= limit:
                    return found
    return found


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("root")
    parser.add_argument("files", nargs="*")
    parser.add_argument("--reference", required=True,
                        help="command line of the reference Kotlin language server")
    parser.add_argument("--krusty", default=None,
                        help="command line of krusty-lsp (default: target/gate/krusty-lsp --stdio)")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--timeout", type=float, default=120.0, help="per-document seconds")
    parser.add_argument("--init-timeout", type=float, default=900.0)
    parser.add_argument("--json")
    args = parser.parse_args()

    if args.timeout <= 0 or args.init_timeout <= 0:
        raise SystemExit("timeouts must be positive")
    if args.limit is not None and args.limit <= 0:
        raise SystemExit("--limit must be positive")

    root = os.path.abspath(args.root)
    if not os.path.isdir(root):
        raise SystemExit(f"project root is not a directory: {root}")
    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    krusty_argv = shlex.split(
        args.krusty or f"{os.path.join(repo, 'target', 'gate', 'krusty-lsp')} --stdio")
    reference_argv = shlex.split(args.reference)
    if not krusty_argv or not reference_argv:
        raise SystemExit("language-server command must not be empty")

    files = [
        os.path.abspath(f if os.path.isabs(f) else os.path.join(root, f))
        for f in args.files
    ]
    if not files:
        files = find_kotlin_files(root, args.limit or 25)
    if not files:
        raise SystemExit("no Kotlin files selected")

    servers = {}
    try:
        for name, argv in (("reference", reference_argv), ("krusty", krusty_argv)):
            # Register BEFORE initializing: a server that dies during `initialize` has already
            # forked its JVM, and the finally below is the only thing that will stop it.
            servers[name] = Lsp(argv, root, name)
            print(f"[lsp-diff] {name}: initializing {argv[0]}", file=sys.stderr, flush=True)
            servers[name].initialize(root, args.init_timeout)

        report = {"root": root, "files": {}}
        # Every exit path must stop the servers: a stray keeps indexing and eats a core.
        totals = {"matched": 0, "extra": 0, "missing": 0, "wording": 0, "moved": 0,
                  "unanswered": 0, "files": 0}
        for path in files:
            relative = os.path.relpath(path, root)
            try:
                reference = servers["reference"].diagnostics_for(path, args.timeout)
                krusty = servers["krusty"].diagnostics_for(path, args.timeout)
            except TimeoutError as timeout:
                # One slow document must not lose the whole run's results.
                totals["unanswered"] += 1
                report["files"][relative] = {"timeout": str(timeout)}
                print(f"[lsp-diff] {relative}: {timeout}", file=sys.stderr, flush=True)
                continue
            if reference is None or krusty is None:
                totals["unanswered"] += 1
                report["files"][relative] = {"unanswered": {
                    "reference": reference is None, "krusty": krusty is None}}
                print(f"[lsp-diff] {relative}: no diagnostics published "
                      f"(reference={reference is None} krusty={krusty is None})",
                      file=sys.stderr, flush=True)
                continue
            comparison = compare(reference, krusty)
            totals["files"] += 1
            for kind in ("matched", "extra", "missing", "wording", "moved"):
                totals[kind] += len(comparison[kind])
            report["files"][relative] = comparison
            print(f"[lsp-diff] {relative}: matched={len(comparison['matched'])} "
                  f"extra={len(comparison['extra'])} missing={len(comparison['missing'])} "
                  f"wording={len(comparison['wording'])} moved={len(comparison['moved'])}",
                  file=sys.stderr, flush=True)


    finally:
        # A language server keeps indexing after the client walks away: always stop both.
        for server in servers.values():
            server.shutdown()

    report["totals"] = totals
    print("lsp-diff: " + " ".join(f"{key}={value}" for key, value in totals.items()))
    if args.json:
        with open(args.json, "w", encoding="utf-8") as handle:
            json.dump(report, handle, indent=1, sort_keys=True)
    return 0 if totals["extra"] == 0 and totals["wording"] == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
