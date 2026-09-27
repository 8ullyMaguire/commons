"""Remove Accept-Ranges from the 200.

Replaces the header with a VARY of the same value type, because replacing it
with a bare `""` gives a mismatched-types error instead of a mutation. A client
that cannot see the token will not send `Range` at all, and then never seeks.
"""
import sys

p = sys.argv[1]
s = open(p).read()
old = "(header::ACCEPT_RANGES, ACCEPT_RANGES.to_owned()),"
assert old in s, "the Accept-Ranges header was not found"
s = s.replace(old, '(header::VARY, "Accept-Encoding".to_owned()),')
open(p, "w").write(s)
