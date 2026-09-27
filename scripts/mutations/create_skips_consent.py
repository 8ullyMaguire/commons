"""A created object gets no consent row, so nobody can find it again."""
import sys

p = sys.argv[1]
s = open(p).read()
old = '''    if inserted {
        // The consent row, in the same batch.'''
assert old in s, "the consent branch was not found"
# The object row is written; the consent row is not. Every tier list in
# `filter_ast::consent_clause` is a `c.tier IN (...)` join, so a missing row
# matches nothing -- the object exists on disk and is invisible to its creator.
new = '''    if false {
        // The consent row, in the same batch.'''
s = s.replace(old, new)
open(p, "w").write(s)
