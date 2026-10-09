#!/usr/bin/env python3
"""Extract NUnit [TestCase(...)] rows from Radarr/Sonarr parser fixtures into JSON.

Usage: extract_fixtures.py <ParserTests dir> <out.json>
Output: {fixture_file: {method: {"quality": str|None, "cases": [[arg, ...], ...]}}}
"""
import json, os, re, sys


def parse_args(s):
    """Parse a C# argument list into Python values. Raises on anything unusual."""
    out, i, n = [], 0, len(s)

    def ws():
        nonlocal i
        while i < n and s[i] in " \t\r\n":
            i += 1

    def value():
        nonlocal i
        ws()
        if s.startswith('@"', i):
            i += 2
            buf = []
            while True:
                if s[i] == '"':
                    if s[i + 1 : i + 2] == '"':
                        buf.append('"'); i += 2; continue
                    i += 1
                    break
                buf.append(s[i]); i += 1
            return "".join(buf)
        if s[i] == '"':
            i += 1
            buf = []
            while s[i] != '"':
                if s[i] == "\\":
                    c = s[i + 1]
                    if c == "u":
                        buf.append(chr(int(s[i + 2 : i + 6], 16))); i += 6; continue
                    buf.append({"n": "\n", "t": "\t", "r": "\r", "0": "\0"}.get(c, c)); i += 2
                else:
                    buf.append(s[i]); i += 1
            i += 1
            ws()
            if s.startswith("+", i):  # string concatenation
                i += 1
                return "".join(buf) + value()
            return "".join(buf)
        m = re.match(r"new\s*(?:\w+)?\s*\[\s*\]\s*\{", s[i:])
        if m:
            i += m.end()
            arr = []
            ws()
            while s[i] != "}":
                arr.append(value()); ws()
                if s[i] == ",":
                    i += 1; ws()
            i += 1
            return arr
        m = re.match(r"-?\d+(\.\d+)?", s[i:])
        if m:
            i += m.end()
            return float(m.group(0)) if "." in m.group(0) else int(m.group(0))
        m = re.match(r"[A-Za-z_][\w.]*", s[i:])
        if m:
            i += m.end()
            w = m.group(0)
            return {"true": True, "false": False, "null": None}.get(w, w)
        raise ValueError("cannot parse at: " + s[i : i + 40])

    while True:
        ws()
        if i >= n:
            break
        out.append(value()); ws()
        if i < n and s[i] == ",":
            i += 1
            m = re.match(r"\s*\w+\s*=", s[i:])  # named attribute args (Description = ...)
            if m:
                break
    return out


def find_close(src, start):
    """Index of the ')' closing the '(' at start, honouring string literals."""
    depth, i = 0, start
    while True:
        c = src[i]
        if c == '"':
            verbatim = src[i - 1] == "@"
            i += 1
            while True:
                if src[i] == '"':
                    if verbatim and src[i + 1] == '"':
                        i += 2; continue
                    break
                if src[i] == "\\" and not verbatim:
                    i += 1
                i += 1
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return i
        i += 1


def extract(path):
    src = open(path, encoding="utf-8-sig").read()
    result, pending, pos = {}, [], 0
    token = re.compile(r"^\s*\[TestCase\(|^\s*public\s+[\w<>\[\]]+\s+(\w+)\s*\(", re.M)
    while True:
        m = token.search(src, pos)
        if not m:
            break
        if m.group(1) is None:
            line_start = src.rfind("\n", 0, m.start() + 1) + 1
            if src[line_start:m.end()].lstrip().startswith("//"):
                pos = m.end(); continue
            op = m.end() - 1
            cl = find_close(src, op)
            try:
                pending.append(parse_args(src[op + 1 : cl]))
            except Exception as e:
                print("skip", os.path.basename(path), e, file=sys.stderr)
            pos = cl
        else:
            if pending:
                nxt = token.search(src, m.end())
                body = src[m.end() : nxt.start() if nxt else len(src)]
                q = re.search(r"Quality\.(\w+)", body)
                result[m.group(1)] = {"quality": q.group(1) if q else None, "cases": pending}
            pending = []
            pos = m.end()
    return result


def main():
    d, out = sys.argv[1], sys.argv[2]
    data = {}
    for f in sorted(os.listdir(d)):
        if f.endswith(".cs"):
            r = extract(os.path.join(d, f))
            if r:
                data[f[:-3]] = r
    json.dump(data, open(out, "w"), ensure_ascii=False, indent=0)
    print(out, sum(len(m["cases"]) for f in data.values() for m in f.values()), "cases")


if __name__ == "__main__":
    main()
