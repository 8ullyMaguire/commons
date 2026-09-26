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

# Values the scanner is meant to allow: local dev credentials, not leaks.
ALLOWED = ["smoke_pw", "postgres://postgres:smoke_pw@127.0.0.1/postgres"]


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
    print(f"PASS: all {len(PLANTS)} planted patterns caught, allowed values ignored")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
