#!/usr/bin/env python3
"""Transpile Sonarr's ReportTitleRegex table into patterns fancy-regex accepts.

.NET keeps every capture of a repeated or duplicated named group. Rust engines
keep only the last. Sonarr's parser needs the first and last episode capture and
every season capture, so this script:

  * unrolls each repeated group that contains a named capture into a first copy
    plus a repeated copy, and
  * gives every named group a unique `name__N` suffix in pattern order.

The Rust side then reads `name__*` groups in order to recover first and last.

Usage: gen_sonarr_regexes.py <Parser.cs> <out.rs>
"""
import re
import sys


def extract(src: str, array_name: str):
    start = src.index(array_name + " = new")
    i = src.index("{", start)
    out = []
    pos = i
    end_of_array = re.compile(r"^\s*\};", re.M).search(src, i).start()
    for line_start in iter(lambda: src.find("new Regex(@\"", pos), -1):
        if line_start > end_of_array:
            break
        # skip commented-out entries
        bol = src.rfind("\n", 0, line_start) + 1
        j = line_start + len('new Regex(@"')
        buf = []
        while True:
            c = src[j]
            if c == '"':
                if src[j + 1] == '"':
                    buf.append('"')
                    j += 2
                    continue
                break
            buf.append(c)
            j += 1
        pos = j
        if src[bol:line_start].strip().startswith("//") or src[bol:line_start].strip().startswith("/*"):
            continue
        comment = ""
        prev = src.rfind("\n", 0, bol - 1) + 1
        prev_line = src[prev:bol].strip()
        if prev_line.startswith("//"):
            comment = prev_line[2:].strip()
        out.append((comment, "".join(buf)))
    return out


class Group:
    def __init__(self, prefix):
        self.prefix = prefix  # e.g. "?:", "?<name>", "?=", ""
        self.children = []  # str | Group
        self.quant = ""

    @property
    def name(self):
        m = re.match(r"\?<([A-Za-z_]\w*)>$", self.prefix)
        return m.group(1) if m else None

    @property
    def lookaround(self):
        return self.prefix in ("?=", "?!", "?<=", "?<!")

    def has_named(self):
        if self.name:
            return True
        return any(isinstance(c, Group) and c.has_named() for c in self.children)

    def clone(self):
        g = Group(self.prefix)
        g.quant = self.quant
        g.children = [c.clone() if isinstance(c, Group) else c for c in self.children]
        return g


QUANT = re.compile(r"(\+|\*|\?|\{\d+(?:,\d*)?\})(\?)?")


def parse(p: str):
    root = Group("ROOT")
    stack = [root]
    i = 0
    n = len(p)

    def emit(s):
        stack[-1].children.append(s)

    while i < n:
        c = p[i]
        if c == "\\":
            if p.startswith("\\k<", i):
                j = p.index(">", i) + 1
                emit(p[i:j])
                i = j
            else:
                emit(p[i : i + 2])
                i += 2
        elif c == "[":
            j = i + 1
            if j < n and p[j] == "^":
                j += 1
            if j < n and p[j] == "]":
                j += 1
            while p[j] != "]":
                j += 2 if p[j] == "\\" else 1
            emit(p[i : j + 1])
            i = j + 1
        elif c == "(":
            m = re.match(r"\((\?<[A-Za-z_]\w*>|\?<=|\?<!|\?=|\?!|\?>|\?:|\?[a-z-]+:)?", p[i:])
            prefix = m.group(1) or ""
            g = Group(prefix)
            stack[-1].children.append(g)
            stack.append(g)
            i += m.end()
        elif c == ")":
            g = stack.pop()
            i += 1
            m = QUANT.match(p, i)
            if m:
                g.quant = m.group(0)
                i = m.end()
        else:
            emit(c)
            i += 1
    assert len(stack) == 1, "unbalanced pattern: " + p
    return root


def split_quant(q):
    """Return (rest_quant_or_None, wrap_optional) for unrolling one iteration."""
    lazy = "?" if q.endswith("?") and len(q) > 1 else ""
    core = q[: -1] if lazy else q
    if core == "+":
        return "*" + lazy, False
    if core == "*":
        return "*" + lazy, True
    m = re.match(r"\{(\d+)(,(\d*))?\}", core)
    if m:
        lo = int(m.group(1))
        if m.group(2) is None:
            hi = lo
        elif m.group(3) == "":
            hi = None
        else:
            hi = int(m.group(3))
        if hi is not None and hi <= 1:
            return None, False
        optional = lo == 0
        nlo = max(lo - 1, 0)
        if hi is None:
            return "{%d,}%s" % (nlo, lazy), optional
        return "{%d,%d}%s" % (nlo, hi - 1, lazy), optional
    return None, False


def unroll_children(node):
    new_children = []
    for c in node.children:
        new_children.extend(unroll(c))
    node.children = new_children
    return node


def unroll(node):
    """Return a list of nodes replacing `node`."""
    if not isinstance(node, Group):
        return [node]
    if node.lookaround:
        return [node]
    rest, optional = (None, False)
    if node.quant and node.has_named():
        rest, optional = split_quant(node.quant)
    if rest is None:
        return [unroll_children(node)]
    first = node.clone()
    first.quant = ""
    second = node.clone()
    second.quant = rest
    parts = [unroll_children(first), unroll_children(second)]
    if optional:
        wrap = Group("?:")
        wrap.children = parts
        wrap.quant = "?"
        return [wrap]
    return parts


def render(node, counters):
    if not isinstance(node, Group):
        if node.startswith("\\k<"):
            return "\\k<%s__1>" % node[3:-1]
        return node
    prefix = node.prefix
    if node.name:
        counters[node.name] = counters.get(node.name, 0) + 1
        prefix = "?<%s__%d>" % (node.name, counters[node.name])
    body = "".join(render(c, counters) for c in node.children)
    if node.prefix == "ROOT":
        return body
    return "(" + prefix + body + ")" + node.quant


def transpile(p: str) -> str:
    p = re.sub(r"\\u([0-9A-Fa-f]{4})", r"\\x{\1}", p)
    root = parse(p)
    new_children = []
    for c in root.children:
        new_children.extend(unroll(c))
    root.children = new_children
    return render(root, {})


def main():
    src = open(sys.argv[1], encoding="utf-8-sig").read()
    items = extract(src, "ReportTitleRegex")
    lines = [
        "// Generated by tools/gen_sonarr_regexes.py from Sonarr's Parser.cs (GPLv3). Do not edit.",
        "pub const SONARR_TITLE_REGEXES: &[(&str, &str)] = &[",
    ]
    for comment, pat in items:
        t = transpile(pat)
        lines.append("    (r##\"%s\"##, r##\"(?i)%s\"##)," % (comment.replace('"', "'"), t))
    lines.append("];")
    open(sys.argv[2], "w").write("\n".join(lines) + "\n")
    print("wrote %d patterns" % len(items))


if __name__ == "__main__":
    main()
