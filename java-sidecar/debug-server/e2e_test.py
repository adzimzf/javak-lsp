#!/usr/bin/env python3
"""End-to-end check of javak-lsp's debugger, driven the way nvim-jdtls and
vscode-java-debug drive jdtls' debugger:

  LSP  workspace/executeCommand vscode.java.resolveMainClass,
       resolveClasspath, resolveJavaExecutable, buildWorkspace,
       startDebugSession (-> a port)
  DAP  over that port: initialize, launch, setBreakpoints (a plain, a
       conditional one and a logpoint), configurationDone, stopped,
       threads, stackTrace, scopes, variables, evaluate, continue, exited.

Usage: python3 e2e_test.py [path/to/javak-lsp]
Needs `java` and `mvn` on the PATH and the debug server jar built
(`mvn -q package` here).
"""

import json
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import threading
import queue
import time

HERE = pathlib.Path(__file__).resolve().parent
SERVER = sys.argv[1] if len(sys.argv) > 1 else str(HERE.parent.parent / "target" / "debug" / "javak-lsp")

MAIN = """package com.demo;

import java.util.ArrayList;
import java.util.List;

public class Main {
    static int total = 0;

    public static void main(String[] args) {
        Greeter greeter = new Greeter("world");
        List<String> seen = new ArrayList<>();
        for (int count = 0; count < 5; count++) {
            String name = greeter.greet(count);
            seen.add(name);
            total += count;
        }
        System.out.println("done " + seen.size());
    }
}

class Greeter {
    private final String who;

    Greeter(String who) {
        this.who = who;
    }

    String greet(int times) {
        return "hello " + who + " #" + times;
    }
}
"""

POM = """<project xmlns="http://maven.apache.org/POM/4.0.0">
  <modelVersion>4.0.0</modelVersion>
  <groupId>com.demo</groupId><artifactId>demo</artifactId><version>1</version>
  <properties>
    <maven.compiler.release>17</maven.compiler.release>
    <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
  </properties>
</project>
"""

failures = []


def check(what, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + what + (f"  ({detail})" if detail and not ok else ""))
    if not ok:
        failures.append(what)


class Lsp:
    def __init__(self, command, root):
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL)
        self.next_id = 0
        self.root = root

    def send(self, message):
        body = json.dumps(message).encode()
        self.process.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        self.process.stdin.flush()

    def read(self):
        length = 0
        while True:
            line = self.process.stdout.readline().decode().strip()
            if not line:
                break
            if line.lower().startswith("content-length:"):
                length = int(line.split(":")[1])
        return json.loads(self.process.stdout.read(length))

    def request(self, method, params):
        self.next_id += 1
        self.send({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params})
        while True:
            message = self.read()
            if message.get("id") == self.next_id and "method" not in message:
                return message
            if "method" in message and "id" in message:
                # A request from the server (registerCapability, progress): accept it.
                self.send({"jsonrpc": "2.0", "id": message["id"], "result": None})

    def command(self, command, *arguments):
        return self.request("workspace/executeCommand", {"command": command, "arguments": list(arguments)})


class Dap:
    def __init__(self, port):
        self.socket = socket.create_connection(("127.0.0.1", port))
        self.file = self.socket.makefile("rb")
        self.seq = 0
        self.events = queue.Queue()
        self.responses = {}
        self.condition = threading.Condition()
        threading.Thread(target=self.reader, daemon=True).start()

    def reader(self):
        try:
            while True:
                length = 0
                while True:
                    line = self.file.readline().decode().strip()
                    if not line:
                        break
                    if line.lower().startswith("content-length:"):
                        length = int(line.split(":")[1])
                if length == 0:
                    return
                message = json.loads(self.file.read(length))
                if message["type"] == "event":
                    self.events.put(message)
                elif message["type"] == "response":
                    with self.condition:
                        self.responses[message["request_seq"]] = message
                        self.condition.notify_all()
                elif message["type"] == "request":
                    # runInTerminal and the like: decline, java-debug falls back.
                    self.write({"type": "response", "request_seq": message["seq"], "success": False,
                                "command": message["command"]})
        except (OSError, ValueError):
            return

    def write(self, message):
        self.seq += 1
        message["seq"] = self.seq
        body = json.dumps(message).encode()
        self.socket.sendall(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        return self.seq

    def request(self, command, arguments=None, timeout=60):
        seq = self.write({"type": "request", "command": command, "arguments": arguments or {}})
        deadline = time.time() + timeout
        with self.condition:
            while seq not in self.responses:
                remaining = deadline - time.time()
                if remaining <= 0:
                    raise TimeoutError(command)
                self.condition.wait(remaining)
            return self.responses.pop(seq)

    def event(self, name, timeout=60, collect=None):
        deadline = time.time() + timeout
        while True:
            message = self.events.get(timeout=max(0.1, deadline - time.time()))
            if message["event"] == name:
                return message
            if collect is not None:
                collect.append(message)


def main():
    root = pathlib.Path(tempfile.mkdtemp(prefix="javak-debug-e2e-"))
    source = root / "src" / "main" / "java" / "com" / "demo" / "Main.java"
    source.parent.mkdir(parents=True)
    source.write_text(MAIN)
    (root / "pom.xml").write_text(POM)
    lines = MAIN.splitlines()
    line_of = lambda text: next(i for i, line in enumerate(lines, 1) if text in line)

    lsp = Lsp([SERVER], root)
    init = lsp.request("initialize", {"processId": os.getpid(), "rootUri": root.as_uri(), "capabilities": {},
                                      "initializationOptions": {"dependencySync": "off"}})
    commands = init["result"]["capabilities"].get("executeCommandProvider", {}).get("commands", [])
    check("the server offers vscode.java.startDebugSession", "vscode.java.startDebugSession" in commands)
    lsp.send({"jsonrpc": "2.0", "method": "initialized", "params": {}})

    found = lsp.command("vscode.java.resolveMainClass", root.as_uri())["result"]
    check("resolveMainClass finds com.demo.Main", [m["mainClass"] for m in found] == ["com.demo.Main"], found)
    project = found[0]["projectName"] if found else ""

    built = lsp.command("vscode.java.buildWorkspace", json.dumps({"isFullBuild": False}))["result"]
    check("buildWorkspace compiles the project", built == 1, built)

    classpath = lsp.command("vscode.java.resolveClasspath", "com.demo.Main", project)["result"]
    check("resolveClasspath has target/classes", str(root / "target" / "classes") in classpath[1], classpath)
    java = lsp.command("vscode.java.resolveJavaExecutable", "com.demo.Main", project)["result"]
    check("resolveJavaExecutable names a java", bool(java) and os.path.isfile(java), java)
    lsp.command("vscode.java.updateDebugSettings", json.dumps({"showToString": True}))

    port = lsp.command("vscode.java.startDebugSession")["result"]
    check("startDebugSession answers a port", isinstance(port, int) and port > 0, port)
    again = lsp.command("vscode.java.startDebugSession")["result"]
    check("a second session reuses the debug server", again == port, again)

    dap = Dap(port)
    initialized = dap.request("initialize", {"adapterID": "java", "linesStartAt1": True, "columnsStartAt1": True,
                                             "pathFormat": "path", "supportsRunInTerminalRequest": False})
    check("DAP initialize", initialized["success"], initialized)
    launch = dap.request("launch", {
        "type": "java", "request": "launch", "mainClass": "com.demo.Main", "projectName": project,
        "classPaths": classpath[1], "modulePaths": classpath[0], "javaExec": java, "cwd": str(root),
        "console": "internalConsole", "args": "", "vmArgs": "", "noDebug": False,
    })
    check("DAP launch", launch["success"], launch.get("message"))
    dap.event("initialized")

    greet_line = line_of('return "hello "')
    add_line = line_of("seen.add(name)")
    total_line = line_of("total += count")
    response = dap.request("setBreakpoints", {
        "source": {"path": str(source)},
        "breakpoints": [{"line": greet_line}, {"line": add_line, "condition": "count == 3"},
                        {"line": total_line, "logMessage": "count={count} name={name.length()}"}],
    })
    breakpoints = response["body"]["breakpoints"] if response["success"] else []
    check("setBreakpoints accepts three", len(breakpoints) == 3, response)
    dap.request("setExceptionBreakpoints", {"filters": []})
    dap.request("configurationDone")

    # The first stop: in Greeter.greet, a class other than the file's first.
    stopped = dap.event("stopped")
    thread = stopped["body"]["threadId"]
    trace = dap.request("stackTrace", {"threadId": thread, "startFrame": 0, "levels": 20})["body"]["stackFrames"]
    top = trace[0]
    check("stopped in Greeter.greet", "greet" in top["name"], top["name"])
    check("the frame's line is the breakpoint's", top["line"] == greet_line, top["line"])
    check("the frame's source is the workspace file", top.get("source", {}).get("path") == str(source),
          top.get("source"))
    check("the caller is Main.main", len(trace) > 1 and "main" in trace[1]["name"], [f["name"] for f in trace])

    scopes = dap.request("scopes", {"frameId": top["id"]})["body"]["scopes"]
    variables = dap.request("variables", {"variablesReference": scopes[0]["variablesReference"]})["body"]["variables"]
    by_name = {v["name"]: v["value"] for v in variables}
    check("a local is listed", by_name.get("times") == "0", by_name)
    evaluate = lambda expression: dap.request("evaluate", {"expression": expression, "frameId": top["id"],
                                                           "context": "watch"})
    result = evaluate("who + \"!\"")
    check("evaluate concatenates a field", result["success"] and result["body"]["result"] == '"world!"', result)
    result = evaluate("who.length() * 2 + times")
    check("evaluate calls a method", result["success"] and result["body"]["result"] == "10", result)
    result = evaluate("nope")
    check("evaluate reports an unknown name", not result["success"], result)

    # Drop the plain breakpoint: the next stop is the conditional one.
    dap.request("setBreakpoints", {"source": {"path": str(source)}, "breakpoints": [
        {"line": add_line, "condition": "count == 3"},
        {"line": total_line, "logMessage": "count={count} name={name.length()}"}]})
    output = []
    dap.request("continue", {"threadId": thread})
    stopped = dap.event("stopped", collect=output)
    trace = dap.request("stackTrace", {"threadId": thread, "levels": 1})["body"]["stackFrames"]
    check("the conditional breakpoint stops where it holds", trace[0]["line"] == add_line, trace[0])
    count = dap.request("evaluate", {"expression": "count", "frameId": trace[0]["id"], "context": "repl"})
    check("... with count == 3", count["success"] and count["body"]["result"] == "3", count)
    logged = [e["body"]["output"].strip() for e in output if e["event"] == "output"]
    check("the logpoint logged the earlier rounds", "count=0 name=14" in logged and "count=2 name=14" in logged,
          logged)

    dap.request("setBreakpoints", {"source": {"path": str(source)}, "breakpoints": []})
    rest = []
    dap.request("continue", {"threadId": thread})
    dap.event("terminated", collect=rest)
    printed = "".join(e["body"].get("output", "") for e in rest if e["event"] == "output")
    check("the program ran to its end", "done 5" in printed, printed)

    lsp.request("shutdown", None)
    lsp.send({"jsonrpc": "2.0", "method": "exit"})
    print()
    print("FAILED: " + ", ".join(failures) if failures else "all checks passed")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
