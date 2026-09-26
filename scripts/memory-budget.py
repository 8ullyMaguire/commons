#!/usr/bin/env python3
"""Idle memory harness (T-P0-008, spec §4.3).

The budget is a hard product constraint, not an aspiration: Commons has to be
usable on a laptop as a local library, and the rejected alternative was
Electron precisely because of what it costs to sit idle. §4.3 sets the target
at 210 MB for the whole idle tree, and this script exists so that number is
measured rather than believed.

How it works
------------
A process's RSS includes shared pages -- most of a server binary's text is
mapped from the same file. Summing RSS across a process tree double-counts those
pages and makes a number go up for a reason that has nothing to do with memory
use, so the honest figure for a multi-process system is PSS (proportional set
size) from /proc/PID/smaps_rollup: each shared page is divided among the
processes mapping it. Where smaps_rollup is unavailable the script falls back to
USS (private pages only) and says which it used, because a budget checked
against a different metric than the one it was written for is not a check.

Sampling
--------
RSS grows for reasons unrelated to the budget -- page cache, lazy first-touch of
mapped regions, a GC cycle mid-flight. The reported figure is the median of
samples taken after the process has settled, and the maximum is reported
alongside it: if the peak exceeds the budget while the median does not, that is
a real finding and not a sampling artifact, so both are shown.

Usage
-----
    scripts/memory-budget.py --budget-mb 210 -- commons-server --mode library ...
    scripts/memory-budget.py --list            # show the named scenarios
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

BUDGET_MB_DEFAULT = 210.0

# How long to let a process settle before sampling. A Rust server is ready in
# well under a second; the delay is mostly so a Tauri shell can open its window
# and so the ML runtime is past its first allocation, which is where a
# regression would show up anyway.
SETTLE_SECONDS = 3.0
SAMPLE_INTERVAL = 0.25
SAMPLE_COUNT = 12

# Cold start matters as much as steady state: a library the user opens should
# not spend ten seconds getting ready.
COLD_START_SECONDS = 15.0


class MeasurementError(RuntimeError):
    pass


@dataclass
class Sample:
    rss_mb: float
    pss_mb: float | None
    uss_mb: float | None
    threads: int
    at: float


@dataclass
class Report:
    scenario: str
    command: list[str]
    samples: list[Sample] = field(default_factory=list)
    cold_start_seconds: float | None = None
    exited: bool = False
    exit_code: int | None = None
    stderr_tail: str = ""

    @property
    def metric(self) -> str:
        if any(s.pss_mb is not None for s in self.samples):
            return "pss"
        return "uss"

    def values(self) -> list[float]:
        key = "pss_mb" if self.metric == "pss" else "uss_mb"
        got = [getattr(s, key) for s in self.samples if getattr(s, key) is not None]
        return got or [s.rss_mb for s in self.samples]

    @property
    def median_mb(self) -> float:
        vals = sorted(self.values())
        if not vals:
            return 0.0
        mid = len(vals) // 2
        if len(vals) % 2:
            return vals[mid]
        return (vals[mid - 1] + vals[mid]) / 2

    @property
    def peak_mb(self) -> float:
        return max(self.values()) if self.values() else 0.0

    def to_dict(self, budget_mb: float) -> dict:
        return {
            "scenario": self.scenario,
            "command": self.command,
            "metric": self.metric,
            "median_mb": round(self.median_mb, 1),
            "peak_mb": round(self.peak_mb, 1),
            "rss_peak_mb": round(max((s.rss_mb for s in self.samples), default=0.0), 1),
            "budget_mb": budget_mb,
            "median_within_budget": self.median_mb <= budget_mb,
            "peak_within_budget": self.peak_mb <= budget_mb,
            "cold_start_seconds": (
                round(self.cold_start_seconds, 2) if self.cold_start_seconds else None
            ),
            "cold_start_within_budget": (
                None
                if self.cold_start_seconds is None
                else self.cold_start_seconds <= COLD_START_SECONDS
            ),
            "samples": len(self.samples),
            "threads": self.samples[-1].threads if self.samples else 0,
            "exited_early": self.exited,
            "exit_code": self.exit_code,
            "stderr_tail": self.stderr_tail,
        }


def _read_status_kb(path: Path, key: str) -> int | None:
    """Read a specific `Key:  1234 kB` line from /proc/PID/status.

    The key must be named. The first `kB` line in `status` is `VmPeak`, not
    `VmRSS`, so a generic "first match" regex silently reports peak virtual size
    as resident size -- a 1 GB figure for a 15 MB process, which is exactly what
    it did before this was named explicitly.
    """
    try:
        text = path.read_text()
    except (OSError, UnicodeDecodeError):
        return None
    m = re.search(rf"^{key}:\s+(\d+)\s+kB", text, re.M)
    return int(m.group(1)) if m else None


def sample_tree(pid: int) -> Sample | None:
    """Sample the process and every descendant.

    Threads share an address space, so they are not walked separately -- the
    memory is already counted in the parent. Only processes are.
    """
    pids = [pid] + descendants(pid)
    rss_kb = 0
    pss_kb = 0
    uss_kb = 0
    have_pss = False
    have_uss = False
    threads = 0

    for p in pids:
        proc = Path(f"/proc/{p}")
        if not proc.exists():
            continue
        rss_kb += _read_status_kb(proc / "status", "VmRSS") or 0
        # smaps_rollup is one read for the whole process, unlike smaps.
        rollup = proc / "smaps_rollup"
        if rollup.exists():
            pss_v, uss_v = _parse_rollup(rollup)
            if pss_v is not None:
                pss_kb += pss_v
                have_pss = True
            if uss_v is not None:
                uss_kb += uss_v
                have_uss = True
        else:
            # smaps_rollup needs kernel 4.14+; fall back to the private pages
            # from smaps, which is USS and is the honest conservative number.
            uss = sum_private_from_smaps(proc / "smaps")
            if uss is not None:
                uss_kb += uss
                have_uss = True
        threads += count_threads(proc / "task")

    if rss_kb == 0:
        return None

    return Sample(
        rss_mb=rss_kb / 1024,
        pss_mb=pss_kb / 1024 if have_pss else None,
        uss_mb=uss_kb / 1024 if have_uss else None,
        threads=threads,
        at=time.time(),
    )


def _parse_rollup(path: Path) -> tuple[int | None, int | None]:
    """(Pss, Uss) in kB from one read of smaps_rollup.

    Uss is the private pages -- the ones no other process could have caused to be
    resident -- so it is Clean + Dirty + Hugetlb. Summing the whole `Private_*`
    family would be the same three, but spelled out here so the intent is
    explicit rather than a prefix match that breaks on a new kernel field.
    """
    pss = uss = 0
    got_pss = got_uss = False
    try:
        for line in path.read_text().splitlines():
            key, _, rest = line.partition(":")
            if not rest.strip():
                continue
            try:
                kb = int(rest.split()[0])
            except (ValueError, IndexError):
                continue
            if key == "Pss":
                pss, got_pss = kb, True
            elif key in ("Private_Clean", "Private_Dirty", "Private_Hugetlb"):
                uss += kb
                got_uss = True
    except (OSError, UnicodeDecodeError):
        return None, None
    return (pss if got_pss else None), (uss if got_uss else None)


def sum_private_from_smaps(path: Path) -> int | None:
    """USS from a full `smaps` walk, for kernels without smaps_rollup (<4.14)."""
    total = 0
    seen = False
    try:
        for line in path.read_text().splitlines():
            if line.startswith(
                ("Private_Clean:", "Private_Dirty:", "Private_Hugetlb:")
            ):
                total += int(line.split()[1])
                seen = True
    except (OSError, ValueError, IndexError, UnicodeDecodeError):
        return None
    return total if seen else None


def count_threads(task_dir: Path) -> int:
    try:
        return len(list(task_dir.iterdir()))
    except OSError:
        return 0


def descendants(pid: int) -> list[int]:
    """Every descendant pid, read from /proc rather than `ps --ppid` so this
    works in a container without procps."""
    children: dict[int, list[int]] = {}
    try:
        entries = os.listdir("/proc")
    except OSError:
        return []
    for entry in entries:
        if not entry.isdigit():
            continue
        try:
            stat = Path(f"/proc/{entry}/stat").read_text()
            # comm can contain spaces and parentheses, so parse after the last
            # ')': ppid is the field after state.
            after = stat[stat.rindex(")") + 1 :].split()
            ppid = int(after[1])
        except (OSError, ValueError, IndexError):
            continue
        children.setdefault(ppid, []).append(int(entry))

    out: list[int] = []
    stack = list(children.get(pid, []))
    while stack:
        cur = stack.pop()
        out.append(cur)
        stack.extend(children.get(cur, []))
    return out


def wait_until_serving(proc: subprocess.Popen, timeout: float) -> float:
    """Seconds until the server answers /healthz, or `timeout` if it never does.

    Readiness is checked over HTTP rather than by sleeping, so a cold start that
    got slower is reported as a number rather than hidden inside a fixed delay.
    """
    start = time.monotonic()
    while time.monotonic() - start < timeout:
        if proc.poll() is not None:
            raise MeasurementError(
                f"process exited with {proc.returncode} before becoming ready"
            )
        if probe_healthz():
            return time.monotonic() - start
        time.sleep(0.1)
    raise MeasurementError(f"no /healthz response within {timeout}s")


def probe_healthz(port: int = 9999) -> bool:
    import socket

    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.5) as s:
            s.sendall(
                b"GET /healthz HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n"
            )
            return b"200" in s.recv(64)
    except OSError:
        return False


def measure(
    scenario: str, command: list[str], env: dict | None = None, settle: float = SETTLE_SECONDS
) -> Report:
    report = Report(scenario=scenario, command=command)
    proc = subprocess.Popen(
        command,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        start_new_session=True,  # its own process group, so one signal cleans up
    )
    try:
        report.cold_start_seconds = wait_until_serving(proc, COLD_START_SECONDS)
        time.sleep(settle)
        for _ in range(SAMPLE_COUNT):
            s = sample_tree(proc.pid)
            if s is None:
                report.exited = True
                break
            report.samples.append(s)
            time.sleep(SAMPLE_INTERVAL)
    except MeasurementError as e:
        report.exited = True
        report.stderr_tail = str(e)
    finally:
        stop(proc)
    return report


def stop(proc: subprocess.Popen) -> None:
    """SIGTERM the whole process group, then SIGKILL what is left.

    A child that outlives its parent would be measured by the next scenario, so
    the group is signalled rather than the pid."""
    if proc.poll() is not None:
        drain(proc)
        return
    try:
        os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            proc.kill()
        proc.wait(timeout=5)
    drain(proc)


def drain(proc: subprocess.Popen) -> None:
    if proc.stderr:
        try:
            proc.stderr.close()
        except OSError:
            pass


SCENARIOS = {
    "server-library": {
        "command": ["commons-server", "--mode", "library"],
        "why": "The web server with a local library. Phase 0 baseline.",
    },
    "server-library-metrics": {
        "command": ["commons-server", "--mode", "library", "--metrics"],
        "why": "Metrics on. The counter set is small but the endpoint is public "
        "on an index, so the cost is worth knowing.",
    },
}


def main() -> int:
    ap = argparse.ArgumentParser(
        description="Measure idle memory against the §4.3 budget.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="Named scenarios:\n"
        + "\n".join(f"  {k:<26} {v['why']}" for k, v in SCENARIOS.items()),
    )
    ap.add_argument("--budget-mb", type=float, default=BUDGET_MB_DEFAULT)
    ap.add_argument("--settle", type=float, default=SETTLE_SECONDS)
    ap.add_argument("--scenario", action="append", help="repeatable")
    ap.add_argument("--list", action="store_true", help="list scenarios and exit")
    ap.add_argument("--json", action="store_true", help="machine-readable output")
    ap.add_argument(
        "command",
        nargs=argparse.REMAINDER,
        help="command to measure; use -- to separate it from the flags",
    )
    args = ap.parse_args()

    if args.list:
        for name, spec in SCENARIOS.items():
            print(f"{name}\n  {' '.join(spec['command'])}\n  {spec['why']}\n")
        return 0

    reports: list[Report] = []

    if args.command and args.command != ["--"]:
        cmd = args.command[1:] if args.command[0] == "--" else args.command
        reports.append(measure("(argv)", cmd, settle=args.settle))
    else:
        wanted = args.scenario or list(SCENARIOS)
        for name in wanted:
            if name not in SCENARIOS:
                print(f"unknown scenario {name!r}; try --list", file=sys.stderr)
                return 2
            exe = shutil.which(SCENARIOS[name]["command"][0])
            if exe is None:
                print(f"skipping {name}: {SCENARIOS[name]['command'][0]} not on PATH")
                continue
            reports.append(measure(name, SCENARIOS[name]["command"], settle=args.settle))

    if not reports:
        print("nothing measured", file=sys.stderr)
        return 2

    if args.json:
        print(json.dumps([r.to_dict(args.budget_mb) for r in reports], indent=2))
    else:
        for r in reports:
            d = r.to_dict(args.budget_mb)
            print(f"\n{r.scenario}  ({d['metric'].upper()})")
            print(f"  {' '.join(r.command)}")
            print(
                f"  median {d['median_mb']:>7.1f} MB   peak {d['peak_mb']:>7.1f} MB"
                f"   (rss peak {d['rss_peak_mb']:.1f} MB)"
            )
            print(
                f"  budget {d['budget_mb']:.0f} MB -> "
                f"median {'OK' if d['median_within_budget'] else 'OVER'}, "
                f"peak {'OK' if d['peak_within_budget'] else 'OVER'}"
            )
            if d["cold_start_seconds"] is not None:
                print(
                    f"  cold start {d['cold_start_seconds']:.2f}s "
                    f"(limit {COLD_START_SECONDS:.0f}s)"
                )
            print(f"  {d['samples']} samples, {d['threads']} threads")
            if d["exited_early"]:
                print(f"  exited early: {r.stderr_tail}")

    # The budget is enforced on the median, with the peak reported. A peak over
    # budget with a median under it is a finding worth reading, not a failure to
    # hide -- but a median over budget means the design is wrong, and the exit
    # code says so.
    failed = [
        r
        for r in reports
        if r.median_mb > args.budget_mb or r.exited
    ]
    if failed:
        print(
            f"\nFAIL: over the {args.budget_mb:.0f} MB budget or exited early: "
            + ", ".join(r.scenario for r in failed),
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
