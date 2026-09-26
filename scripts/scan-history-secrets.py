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
    """Every blob ever committed, with the path it had.

    `rev-list --objects` prints `<sha> <path>` for a blob and `<sha>` for a
    commit, so the path is the second field when there is one. The first
    version piped this into `cat-file --batch-check` to learn the type, which
    asked git to resolve the *path string* as an object name: every commit came
    back "missing" and every blob's size landed in the path column, so findings
    were reported against files called "286" and "3238".

    Two lines of parsing, no second git process, and a path that is a path.
    """
    out = subprocess.run(
        ["git", "-C", REPO, "rev-list", "--objects", "--all"],
        capture_output=True, text=True, check=True, timeout=590,
    ).stdout
    shas = set()
    paths = {}
    for line in out.split("\n"):
        if not line.strip():
            continue
        parts = line.split(" ", 1)
        shas.add(parts[0])
        if len(parts) > 1:
            paths[parts[0]] = parts[1]
    return sorted(shas), paths


def main():
    shas, paths = blob_list()
    if not shas:
        print("FAIL: no objects found -- the scan looked at nothing")
        return 1

    # One `batch-check` for the type of every object, fed on stdin: no shell to
    # mangle quoting, and one process instead of one per object.
    proc = subprocess.run(
        ["git", "-C", REPO, "cat-file", "--batch-check"],
        input="\n".join(shas),
        capture_output=True, text=True, check=True, timeout=590,
    ).stdout
    blobs = []
    for line in proc.split("\n"):
        parts = line.split()
        if len(parts) >= 2 and parts[1] == "blob":
            blobs.append((parts[0], paths.get(parts[0], "<no path>")))

    # The self-test commits blobs whose whole purpose is to look like
    # credentials, so scanning them finds its own fixtures. Skipped by path,
    # and named here rather than in a list at the top, because an exclusion
    # nobody can see the reason for is one a future reader is afraid to
    # remove -- or, worse, generalises to the next exclusion.
    SELF = "scan-history-secrets-selftest.py"

    hits, scanned, binary, skipped = {}, 0, 0, 0
    for sha, path in blobs:
        out = subprocess.run(
            ["git", "-C", REPO, "cat-file", "blob", sha],
            capture_output=True, timeout=120,
        ).stdout
        if SELF in path:
            skipped += 1
            continue
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

    print(
        f"scanned {scanned} blobs ({binary} binary), "
        f"{skipped} skipped as self-test fixtures, {len(hits)} finding(s)\n"
    )
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
