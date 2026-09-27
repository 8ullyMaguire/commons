"""Prove the secret scanner can fail.

A scanner that reports "0 findings" is only evidence if it can report something
otherwise. This plants each pattern it claims to look for into a blob, runs the
scanner, and requires it to catch every one. A pattern that silently stopped
matching -- a refactor, a regex typo -- turns a clean scan into a false all-clear,
and the only way to know is to make it fire on purpose.

The planted values are in a scratch repo, never in the project.
"""
import re, shutil, subprocess, sys, tempfile, os

HERE = os.path.dirname(os.path.abspath(__file__))
SCANNER = os.path.join(HERE, "scan-history-secrets.py")

PLANTS = {
    "ghp_deadbeefdeadbeefdeadbeefdeadbeef1234": "github token",
    "sk-abcdefghijklmnopqrstuvwxyz0123456789": "openai key",
    "AKIAIOSFODNN7EXAMPLE": "aws access key",
    "xoxb-1234567890-abcdefghijkl": "slack token",
    "-----BEGIN RSA PRIVATE KEY-----": "private key block",
    "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.abc": "jwt",
    "postgres://someone:verysecretpw@10.0.0.5/db": "url with inline password",
    'password = "hunter2hunter2"': "assigned secret",
}

# Values the scanner is meant to allow.
#
# The last two are a PAIR and the pair is the point. `${VAR}` in a URL has the
# shape of a password and none of the content, so it is allowed. `hunter2${VAR}`
# is six-plus characters of a real literal with an expansion stuck on the end, and
# it is NOT -- because the day the scanner accepts that, the exclusion has become
# a way to stop it finding things, which is the failure mode a scanner exists to
# prevent. A test that only checks the allowed case would let that happen.
ALLOWED = [
    "smoke_pw",
    "postgres://postgres:smoke_pw@127.0.0.1/postgres",
    "postgres://postgres:${COMMONS_PGPW:?set it}@127.0.0.1/postgres",
]

# Values that must still be CAUGHT even though each contains an allowed one, and
# each resembles an allowed value closely enough to be a plausible refactor.
MUST_STILL_FIRE = [
    "postgres://postgres:hunter2${PGPW}@127.0.0.1/postgres",
    "postgres://postgres:realpassword${X}@127.0.0.1/postgres",
]


def run(repo):
    src = open(SCANNER).read().replace(
        'REPO = "/home/alvaro/code-local/rust/commons"', f'REPO = {repo!r}'
    )
    tmp = os.path.join(repo, "..", "_scanner_probe.py")
    with open(tmp, "w") as fh:
        fh.write(src)
    try:
        r = subprocess.run([sys.executable, tmp], capture_output=True, text=True, timeout=590)
        return r.stdout
    finally:
        os.unlink(tmp)


def main():
    tmp = tempfile.mkdtemp()
    repo = os.path.join(tmp, "probe")
    os.makedirs(repo)
    subprocess.run(["git", "init", "-q", repo], check=True)
    subprocess.run(["git", "-C", repo, "config", "user.email", "p@example.com"], check=True)
    subprocess.run(["git", "-C", repo, "config", "user.name", "probe"], check=True)

    for i, (value, label) in enumerate(PLANTS.items()):
        with open(os.path.join(repo, f"f{i}.txt"), "w") as fh:
            fh.write(f"# {label}\n{value}\n")
    # a binary blob, because the real repository has them
    with open(os.path.join(repo, "blob.bin"), "wb") as fh:
        fh.write(bytes(range(256)) * 4)
    for v in ALLOWED:
        with open(os.path.join(repo, f"allowed{ALLOWED.index(v)}.txt"), "w") as fh:
            fh.write(f"# local only\nDATABASE_URL=postgres://postgres:{v}@127.0.0.1/postgres\n")
    # Each of these CONTAINS an allowed value, so a filter written too loosely --
    # "skip the line", "skip anything with a dollar sign" -- stops catching them.
    for v in MUST_STILL_FIRE:
        with open(os.path.join(repo, f"mustfire{MUST_STILL_FIRE.index(v)}.txt"), "w") as fh:
            fh.write(f"# LOOKS allowed, IS NOT\nDATABASE_URL={v}\n")

    subprocess.run(["git", "-C", repo, "add", "-A"], check=True)
    subprocess.run(["git", "-C", repo, "commit", "-qm", "probe"], check=True)

    out = run(os.path.realpath(repo))
    print(out)
    shutil.rmtree(tmp, ignore_errors=True)

    missed = [label for value, label in PLANTS.items() if label not in out]
    if missed:
        print("FAIL: the scanner did not catch:", ", ".join(missed))
        return 1
    if "scanned 0 blobs" in out:
        print("FAIL: the scanner reported scanning nothing")
        return 1

    # The near-misses are reported BY NAME. A filter that is one character too
    # generous shows up here as a missing filename, which says which filter went
    # wrong -- not just that "something" is wrong.
    not_fired = [f"mustfire{i}" for i in range(len(MUST_STILL_FIRE)) if f"mustfire{i}" not in out]
    if not_fired:
        print(
            "FAIL: the scanner stopped catching these because it now allows "
            "shell expansions --", ", ".join(not_fired)
        )
        return 1

    # And the allowed ones are genuinely not reported, by name.
    wrongly = [f"allowed{i}" for i in range(len(ALLOWED)) if f"allowed{i}" in out]
    if wrongly:
        print("FAIL: the scanner reported these allowed values:", ", ".join(wrongly))
        return 1

    print(
        f"PASS: all {len(PLANTS)} planted patterns caught, "
        f"{len(ALLOWED)} allowed values ignored, "
        f"{len(MUST_STILL_FIRE)} near-misses still caught"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
