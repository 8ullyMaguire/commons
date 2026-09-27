"""The separator is a plain character, so adjacent fields can be swapped."""
import sys

p = sys.argv[1]
s = open(p).read()
old = '''    let material = [kind, title, date, producer, description].join("\\u{0}");'''
assert old in s, "the join was not found"
# The whole point: `("ab", "c")` and `("a", "bc")` now hash alike, so two
# DIFFERENT objects share one id and the second create is a silent no-op.
s = s.replace(old, '''    let material = [kind, title, date, producer, description].join(",");''')
open(p, "w").write(s)
