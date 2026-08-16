#!/usr/bin/env python3
"""Drive krusty-lsp over stdio against a real project and dump diagnostics.

Usage: lsp-scan.py <project-root> <file1.kt> [file2.kt ...]
       lsp-scan.py <project-root> --limit N   (first N Kotlin files found)

Writes a JSON report to stdout: {file: [diagnostics]}.
"""
import json
import os
import subprocess
import sys
import threading
import queue

SERVER = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "target", "debug", "krusty-lsp")
MAX_MESSAGE = 16 * 1024 * 1024


class Lsp:
    def __init__(self, root, extra_args=None):
        args = [SERVER, "--stdio"] + (extra_args or [])
        self.proc = subprocess.Popen(
            args, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL, cwd=root)
        self.inbox = queue.Queue()
        self.next_id = 1
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

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
            length = int(headers[b"content-length"])
            body = out.read(length)
            self.inbox.put(json.loads(body))

    def send(self, message):
        body = json.dumps(message).encode()
        self.proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        self.proc.stdin.flush()

    def request(self, method, params, timeout=600):
        rid = self.next_id
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        deferred = []
        try:
            while True:
                msg = self.inbox.get(timeout=timeout)
                if msg is None:
                    raise RuntimeError("server closed pipe")
                if msg.get("id") == rid and ("result" in msg or "error" in msg):
                    for m in deferred:
                        self.inbox.put(m)
                    return msg
                if "id" in msg and "method" in msg:
                    # server -> client request; answer null-ish
                    result = None
                    if msg["method"] == "workspace/configuration":
                        result = [None] * len(msg["params"].get("items", []))
                    self.send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
                else:
                    deferred.append(msg)
        except queue.Empty:
            raise TimeoutError(f"request {method} timed out")

    def notify(self, method, params):
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def wait_diagnostics(self, uri, timeout=300):
        """Wait until publishDiagnostics arrives for uri (may already be queued)."""
        import time
        deadline = time.time() + timeout
        deferred = []
        try:
            while time.time() < deadline:
                msg = self.inbox.get(timeout=max(1, deadline - time.time()))
                if msg is None:
                    raise RuntimeError("server closed pipe")
                if (msg.get("method") == "textDocument/publishDiagnostics"
                        and msg["params"]["uri"] == uri):
                    for m in deferred:
                        self.inbox.put(m)
                    return msg["params"]["diagnostics"]
                if "id" in msg and "method" in msg:
                    self.send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
                else:
                    deferred.append(msg)
        except queue.Empty:
            pass
        return None


def find_kotlin_files(root, limit):
    out = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in (".git", "out", "build", "node_modules", "dist")]
        for f in sorted(filenames):
            if f.endswith(".kt"):
                out.append(os.path.join(dirpath, f))
                if len(out) >= limit:
                    return out
    return out


def main():
    root = os.path.abspath(sys.argv[1])
    extra_args = []
    files = []
    args = sys.argv[2:]
    if "--no-jdk" in args:
        args.remove("--no-jdk")
        extra_args.append("-no-jdk")
    if args and args[0] == "--limit":
        files = find_kotlin_files(root, int(args[1]))
    else:
        files = [os.path.abspath(a) for a in args]

    lsp = Lsp(root, extra_args)
    root_uri = "file://" + root
    init = lsp.request("initialize", {
        "processId": None,
        "rootUri": root_uri,
        "capabilities": {},
        "workspaceFolders": [{"uri": root_uri, "name": os.path.basename(root)}],
    }, timeout=1800)
    if "error" in init:
        print(json.dumps({"initialize_error": init["error"]}))
        sys.exit(1)
    lsp.notify("initialized", {})

    report = {}
    for path in files:
        uri = "file://" + path
        with open(path, encoding="utf-8", errors="replace") as fh:
            text = fh.read()
        lsp.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": "kotlin", "version": 1, "text": text}})
        diags = lsp.wait_diagnostics(uri, timeout=600)
        report[os.path.relpath(path, root)] = diags
        lsp.notify("textDocument/didClose", {"textDocument": {"uri": uri}})
        print(f"scanned {path}: {None if diags is None else len(diags)} diagnostics", file=sys.stderr)

    lsp.request("shutdown", {}, timeout=30)
    lsp.notify("exit", {})
    print(json.dumps(report, indent=1))


if __name__ == "__main__":
    main()
