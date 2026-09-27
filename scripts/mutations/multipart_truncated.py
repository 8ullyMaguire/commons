"""Answer a 206 carrying only the first of several ranges.

The truncation that the `Multiple` variant exists to prevent: a body the client
cannot align with its request, and the browser's media stack retries forever.
"""
import sys

p = sys.argv[1]
s = open(p).read()
old = (
    'tracing::debug!(object_id = %object_id, "multipart range requested; serving the whole file");\n'
    "            match read_range(&path, 0, real_len).await {\n"
    "                Ok(bytes) => full_response(&path, bytes, real_len),"
)
assert old in s, "the multipart arm was not found"
new = (
    "match read_range(&path, 0, 3.min(real_len)).await {\n"
    "                Ok(bytes) => partial_response(&path, bytes, ByteRange { start: 0, end: 3.min(real_len) }, real_len),"
)
s = s.replace(old, new)
open(p, "w").write(s)
