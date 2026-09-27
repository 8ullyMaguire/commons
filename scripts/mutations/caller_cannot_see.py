"""Ask as a caller with no account.

With `account_id: None` the consent clause resolves to ConsentTiers::PUBLIC,
which excludes `unverified` -- so a route that asked as a visitor cannot serve
a freshly scanned file. Kills only once a fixture is seeded at `unverified`.
"""
import sys

p = sys.argv[1]
s = open(p).read()
old = 'account_id: Some("local".to_owned()),'
assert old in s, "the account_id was not found"
s = s.replace(old, "account_id: None,")
open(p, "w").write(s)
