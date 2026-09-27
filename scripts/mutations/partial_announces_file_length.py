"""A 206 announces the whole file's length in Content-Length.

The client reads Content-Length, waits for that many bytes, and gets a range.
"""
import sys

p = sys.argv[1]
s = open(p).read()
old = """(header::CONTENT_LENGTH, bytes.len().to_string()),
            (header::CONTENT_RANGE, content_range(range, len)),"""
assert old in s, "the 206 headers were not found"
new = """(header::CONTENT_LENGTH, len.to_string()),
            (header::CONTENT_RANGE, content_range(range, len)),"""
s = s.replace(old, new)
open(p, "w").write(s)
