"""Send the whole file in a 206.

The inverse of the header/body agreement: the client asks for a range, the
header says a range, and the body is the whole file. Extra bytes after the
declared end are dropped by the client, so this is the milder direction -- which
is why the header-claims-more mutation below is the one that hangs.
"""
import sys

p = sys.argv[1]
s = open(p).read()
old = "read_range(&path, range.start, range.end).await"
assert old in s, "the range read was not found"
s = s.replace(old, "read_range(&path, 0, real_len).await")
open(p, "w").write(s)
