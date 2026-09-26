"""Scan the whole history for credentials before it goes anywhere.

The repo is about to be published for the first time. Its own history contains
a verified failure mode: `scripts/verify.sh` carries a real local development
`DATABASE_URL` in plaintext. That is a loopback credential on a throwaway
local database, so it is not a leak in itself -- but it is a live string of
exactly the shape a scanner is built to catch, and publishing a history that
contains one makes every future scan of this project noisier than it needs to
be.

Two things the first version got wrong, both of which made it report "clean"
without having looked at anything:

  - the object list came out of an `awk` inside a shell string, and the quoting
    did not survive, so it scanned 0 blobs and said so cheerfully;
  - blobs were read in text mode, and the repository holds generated fixture
    archives, so the first binary one killed the run.

It prints a count of what it scanned, and a scan of nothing is a failure.

Blobs are read as bytes. Nothing prints a value: labels, paths, and a redacted
shape only.
"""
import re, subprocess

REPO = "/home/alvaro/code-local/rust/commons"

PATTERNS = [
    ("github token", rb"gh[pousr]_[A-Za-z0-9]{20,}"),
    ("openai key", rb"sk-[A-Za-z0-9_-]{20,}"),
    ("aws access key", rb"AKIA[0-9A-Z]{16}"),
    ("slack token", rb"xox[abprs]-[A-Za-z0-9-]{10,}"),
    ("private key block", rb"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    ("jwt", rb"eyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\."),
    ("url with inline password", rb"[a-z][a-z0-9+.-]*://[^\s/:@]+:([^\s/@]{6,})@"),
    (
        "assigned secret",
        rb"(?i)(password|passwd|secret|token|api[_-]?key)\s*[:=]\s*[\"']?([^\s\"'#,]{8,})",
    ),
]

# The values that are local-only development credentials, so a hit on them is a
# decision rather than an accident.
KNOWN_LOCAL = {b"smoke_pw", b"postgres", b"changeme", b"placeholder"}


def redact(b):
    s = b.decode("utf-8", "replace")
    if len(s) <= 4:
        return "*" * len(s)
    return s[:2] + "*" * (len(s) - 4) + s[-2:]


def blob_list():
    """Every blob ever committed, with a path, via one batch-check call."""
    out = subprocess.run(
        ["git", "-C", REPO, "rev-list", "--objects", "--all"],
        capture_output=True, text=True, check=True, timeout=590,
    ).stdout
    pairs = []
    for line in out.split("\n"):
        if not line.strip():
            continue
        parts = line.split(" ", 1)
        pairs.append(parts[0])
    return pairs


def main():
    shas = blob_list()
    if not shas:
        print("FAIL: no blobs found -- the scan looked at nothing")
        return 1

    # `git cat-file --batch-check` in one process, fed on stdin, so there is no
    # shell in the middle to mangle the quoting.
    proc = subprocess.run(
        ["git", "-C", REPO, "cat-file", "--batch-check"],
        input="\n".join(shas),
        capture_output=True, text=True, check=True, timeout=590,
    ).stdout
    blobs = []
    for line in proc.split("\n"):
        parts = line.split()
        if len(parts) == 3 and parts[1] == "blob":
            blobs.append((parts[0], parts[2]))
        elif len(parts) == 2 and parts[1] == "blob":
            blobs.append((parts[0], "<no path>"))

    hits, scanned, binary = {}, 0, 0
    for sha, path in blobs:
        out = subprocess.run(
            ["git", "-C", REPO, "cat-file", "blob", sha],
            capture_output=True, timeout=120,
        ).stdout
        scanned += 1
        if b"\x00" in out[:4096]:
            binary += 1
            # still scanned: a PNG can carry an appended password as easily as
            # a source file can
        for label, pat in PATTERNS:
            for m in re.finditer(pat, out):
                val = m.group(m.lastindex or 0)
                if val.strip().lower() in KNOWN_LOCAL:
                    continue
                hits.setdefault((label, path), set()).add(redact(val))

    print(f"scanned {scanned} blobs ({binary} binary), {len(hits)} finding(s)\n")
    if scanned == 0:
        print("FAIL: scanned 0 blobs -- the scan looked at nothing")
        return 1
    if not hits:
        return 0
    for (label, path), vals in sorted(hits.items()):
        print(f"  {label:26} {path}")
        for v in sorted(vals)[:4]:
            print(f"      {v}")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
