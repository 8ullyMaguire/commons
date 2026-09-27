"""Claim the whole file in Content-Range while the body stays the range.

Works whether or not `cargo fmt` has reflowed the call onto one line, which the
first version did not: it matched the literal `content_range(range, len)` and,
after fmt split the call across lines, matched only a PREFIX of the real text --
so the replacement spliced a `ByteRange { ... }` into a half-matched expression
and produced a brace error rather than a clean failure. A mutation that breaks
the build is not a mutation.
"""
import re
import sys

p = sys.argv[1]
s = open(p).read()
m = re.search(r"content_range\(\s*range,\s*len\s*\)", s)
if not m:
    sys.exit("the content_range call was not found")
s = s[: m.start()] + "content_range(ByteRange { start: 0, end: len }, len)" + s[m.end() :]
open(p, "w").write(s)
