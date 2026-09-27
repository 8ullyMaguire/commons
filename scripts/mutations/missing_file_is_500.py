"""Answer 500 when the file is gone from disk.

A library mid-rescan is entirely absent files, so this is a failed page load per
absent file for the duration of every scan.
"""
import sys

p = sys.argv[1]
s = open(p).read()
old = """"media file could not be opened");
            return not_found();"""
assert old in s, "the open-failure branch was not found"
new = """"media file could not be opened");
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, "media_read_failed", "");"""
s = s.replace(old, new)
open(p, "w").write(s)
