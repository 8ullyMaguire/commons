"""`created` and `existing` collapse into one number.

The user-visible bug this models is the one the spec calls out: someone pastes
40 rows, 35 of which are already in the library, and the report says "created
40" because the action only ever reports what it wrote.
"""
import sys

p = sys.argv[1]
s = open(p).read()
old = '''        match insert_if_absent(store, draft, &id).await? {
            true => outcome.created += 1,
            false => outcome.existing += 1,
        }'''
assert old in s, "the outcome match was not found"
new = '''        if insert_if_absent(store, draft, &id).await? {
            outcome.created += 1;
        }'''
s = s.replace(old, new)
open(p, "w").write(s)
