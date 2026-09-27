"""Answer 403 for a denied object.

The exact thing 14.1 is about: a 403 confirms the object exists, which is the
information the gate is withholding.
"""
import sys

p = sys.argv[1]
s = open(p).read()
old = 'json_error(StatusCode::NOT_FOUND, "not_found", "")'
assert old in s, "not_found was not found"
s = s.replace(old, 'json_error(StatusCode::FORBIDDEN, "forbidden", "")')
open(p, "w").write(s)
