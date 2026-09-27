"""Resolve the range against the index's recorded size.

A file replaced since the last scan then gets a Content-Range past its real end,
and the client waits for bytes that were never sent. Kills only once a fixture
has a file whose length disagrees with the row.
"""
import sys

p = sys.argv[1]
s = open(p).read()
old = "resolve(range_header.as_deref(), real_len)"
assert old in s, "the resolve call was not found"
s = s.replace(old, "resolve(range_header.as_deref(), location.size_bytes)")
open(p, "w").write(s)
