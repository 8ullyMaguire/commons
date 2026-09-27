"""A create publishes itself as `self_published`."""
import sys

p = sys.argv[1]
s = open(p).read()
old = '''    q = q.bind(&id)
        .bind(object_id)
        .bind("unverified")'''
if old not in s:
    # `cargo fmt` may have reflowed the chain; find the bind by its neighbours.
    old = '''    q = q.bind(&id)
        .bind(object_id)
        .bind("unverified")'''
assert 'unverified' in s, "the consent bind was not found"
# Creating an object is not an attestation about it. A create that published
# itself lets a paste of scraped CSV titles publish itself.
s = s.replace('"unverified"', '"self_published"')
open(p, "w").write(s)
