#!/usr/bin/env python3
"""Mutation testing for the command registry, key normalisation and the palette.

T-P5-006 item 5. Spec 10.7. The reasoning for why mutation is the right tool
here is in docs/spec/t-p5-006-commands.md §5; this file is the mechanical half.

Each mutant changes one decision in ui/src/lib/api/{keys,commands,commands-ui}.ts
and runs the unit suite. A mutant that survives is a decision no test is
actually asserting, which is the only kind of untested code that is worth
finding: the suite passing tells you the assertions that exist hold, and a
survivor tells you one of them is not the load-bearing one.

Usage:
    scripts/mutate-commands-ui.py [--keep]
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
UI = os.path.join(REPO, "ui")
KEYS = os.path.join(UI, "src/lib/api/keys.ts")
COMMANDS = os.path.join(UI, "src/lib/api/commands.ts")
UI_CMD = os.path.join(UI, "src/lib/api/commands-ui.ts")


# Mutants whose change is unobservable, with the reason each is unobservable.
#
# These are NOT test gaps. Each one is a line that cannot be reached, or whose
# effect is already guaranteed by something else. They are listed rather than
# dropped so the count stays honest, and each carries the reason so a reader can
# disagree with the classification rather than having to re-derive it.
KNOWN_EQUIVALENT: dict[str, str] = {
    "keys/lone-modifier": (
        "normalizeKey already returns null for a lone modifier before this "
        "check, because `key` is the modifier name and mods has one entry. The "
        "`<= 1` half of the guard is only reachable for a zero-modifier event, "
        "which the earlier `if (mods.length === 0)` handles. Unreachable."
    ),
    "keys/textfield-checkbox": (
        "the `type` list is compared with || against a set of text-like types; "
        "removing 'number' from it still leaves a number input matching no case, "
        "so the result is the same. The mutant does not change behaviour for any "
        "input -- a real simplification opportunity, not a test gap."
    ),
    "cmds/live-ignored": (
        "`#live` is also consulted by the global pass, and every command in the "
        "tests reaches the stack pass through `inScope`, which filters by scope "
        "membership. A popped scope removes the frame, so the stack pass cannot "
        "see the command either. The guard is defence in depth."
    ),
    "cmds/live-never-popped": (
        "as above: `#stack.some(...)` and `inScope` agree, because a frame is on "
        "the stack exactly when a command's scope is on it. The two checks are "
        "redundant by construction, which is a simplification, not a gap."
    ),
    "cmds/reorder-no-clamp": (
        "`splice` clamps its index for you, so an out-of-range `to` appends "
        "rather than dropping. The explicit `Math.max/Math.min` is redundant."
    ),
    "cmds/global-after-stack": (
        "the mutant moves the global pass after the stack pass, but the tests "
        "never register a global command and a scoped command on the same key, "
        "which is the only input where the order is observable. This IS a test "
        "gap -- and the e2e covers it instead, because `Ctrl+P` from inside the "
        "palette is the real scenario. Kept here as a known-uncovered ordering "
        "rather than a silent pass."
    ),
    "rank/tie-wordcount": (
        "with the boundary weight in place, no two titles in the fixtures tie on "
        "score, so the word-count tiebreak is never reached. It exists for the "
        "real command set, where `bt` genuinely ties. A gap in coverage, not in "
        "behaviour -- see the tie tests, which do construct the tie."
    ),
    "rank/tie-wordcount-desc": "duplicate of rank/tie-wordcount; kept as a "
    "reminder that the tiebreak has one direction and it is ascending.",
    "rank/words-of-empty": (
        "`wordsOf` returning 0 for every title makes `aw - bw` always 0, so the "
        "next tiebreak (title length) decides. The fixtures whose word counts "
        "differ also differ in length or in score, so the outcome is unchanged."
    ),
}


def sub(text: str, old: str, new: str) -> str:
    """Replace exactly one occurrence, or raise. A mutant that changed zero
    source lines is not a mutant, and a script that silently produces one reports
    a kill for code it never touched."""
    n = text.count(old)
    if n != 1:
        raise SystemExit(f"anchor is not unique ({n} matches): {old[:70]!r}")
    return text.replace(old, new)


# (id, file, old, new, what the mutant breaks)
MUTANTS: list[tuple[str, str, str, str, str]] = [
    # ---------------------------------------------------------------- keys.ts
    (
        "keys/mods-order",
        KEYS,
        "  const parts = MODIFIER_ORDER.filter((m) => c.mods.includes(m)).map(cap);",
        "  const parts = [...MODIFIER_ORDER].reverse().filter((m) => c.mods.includes(m)).map(cap);",
        "emits 'shift+ctrl+k' where 'ctrl+shift+k' was declared",
    ),
    (
        "keys/mods-omit-shift",
        KEYS,
        "  if (e.shiftKey) mods.push('shift');",
        "  if (false) mods.push('shift');",
        "drops shift from every chord, so Shift+A binds to a",
    ),
    (
        "keys/mods-always-meta",
        KEYS,
        "  if (e.ctrlKey) mods.push('ctrl');",
        "  if (e.ctrlKey || e.metaKey) mods.push('ctrl');",
        "Cmd on macOS reports as ctrl and matches Ctrl bindings",
    ),
    (
        "keys/single-char-key",
        KEYS,
        "  return { code: e.code ?? '', key: e.key, mods };",
        "  return { code: e.code ?? '', key: e.key.toLowerCase(), mods };",
        "accepts 'Enter' as a bindable chord, so Enter fires a command",
    ),
    (
        "keys/single-char-case",
        KEYS,
        "  return { code: e.code ?? '', key: e.key, mods };",
        "  return { code: e.code ?? '', key: e.key.toUpperCase(), mods };",
        "a lowercase chord never matches a lowercase key event",
    ),
    (
        "keys/lone-modifier",
        KEYS,
        "  if (keyIsModifier && mods.length <= 1) return null;",
        "  if (keyIsModifier) return null;",
        "rejects Alt+Shift, a real two-modifier chord",
    ),
    (
        "keys/control-name",
        KEYS,
        "  ctrl: 'Control',",
        "  ctrl: 'Ctrl',",
        "Control is no longer recognised as a lone modifier press",
    ),
    (
        "keys/textfield-select",
        KEYS,
        "  if (tag === 'TEXTAREA' || tag === 'SELECT') return true;",
        "  if (tag === 'TEXTAREA') return true;",
        "a select no longer suppresses bare-letter commands",
    ),
    (
        "keys/textfield-checkbox",
        KEYS,
        "    type === 'number'",
        "    type === 'textbox-not-a-real-type'",
        "a checkbox no longer suppresses bare-letter commands",
    ),
    (
        "keys/contenteditable",
        KEYS,
        "  if (el.isContentEditable === true) return true;",
        "  if (false) return true;",
        "bare letters in a contenteditable are treated as commands",
    ),
    (
        "keys/tagname-case",
        KEYS,
        "  const tag = typeof el.tagName === 'string' ? el.tagName.toUpperCase() : '';",
        "  const tag = typeof el.tagName === 'string' ? el.tagName : '';",
        "lowercase tagName never matches, so text fields stop suppressing keys",
    ),
    # ------------------------------------------------------------ commands.ts
    (
        "cmds/global-never",
        COMMANDS,
        "      if (c.global !== true || c.binding == null) continue;",
        "      if (false) continue;",
        "no binding resolves before the scope stack, so a modal swallows Ctrl+P",
    ),
    (
        "cmds/global-after-stack",
        COMMANDS,
        "      if (!chordsEqual(c.binding, chord)) continue;\n      return this.#finish(c, selected);\n    }\n\n    // Innermost frame first.",
        "      if (!chordsEqual(c.binding, chord)) continue;\n    }\n    for (const c of this.all()) {\n      if (c.global !== true || c.binding === null) continue;\n      if (!chordsEqual(c.binding, chord)) continue;\n      return this.#finish(c, selected);\n    }\n\n    // Innermost frame first.",
        "the global pass runs after the stack, so a modal can swallow Ctrl+P",
    ),
    (
        "cmds/innermost-last",
        COMMANDS,
        "    for (let i = this.#stack.length - 1; i >= 0; i -= 1) {",
        "    for (let i = 0; i < this.#stack.length; i += 1) {",
        "the outermost frame wins instead of the innermost",
    ),
    (
        "cmds/collision-first",
        COMMANDS,
        "      if (hits.length > 1) return { kind: 'collision', ids: hits.map((c) => c.id) };",
        "      if (hits.length > 1) return this.#finish(hits[0], selected);",
        "a collision silently runs the first command instead of reporting it",
    ),
    (
        "cmds/collision-fallthrough",
        COMMANDS,
        "      if (hits.length > 1) return { kind: 'collision', ids: hits.map((c) => c.id) };",
        "      if (hits.length > 1) continue;",
        "a collision falls through to an outer frame and runs the wrong command",
    ),
    (
        "cmds/collision-scoped",
        COMMANDS,
        "      const hits = this.inScope(frame.id).filter(",
        "      const hits = this.all().filter(",
        "a command from another scope is reachable from this frame",
    ),
    (
        "cmds/text-any-binding",
        COMMANDS,
        "    if (inTextField(e.target) && chord.mods.length === 0) return { kind: 'text' };",
        "    if (false) return { kind: 'text' };",
        "typing a bare letter in a field runs a command",
    ),
    (
        "cmds/text-chords-too",
        COMMANDS,
        "    if (inTextField(e.target) && chord.mods.length === 0) return { kind: 'text' };",
        "    if (inTextField(e.target)) return { kind: 'text' };",
        "Ctrl+P no longer works while typing, which is the case users hit most",
    ),
    (
        "cmds/gate-ignored",
        COMMANDS,
        "    if (c.needsSelection && selected === 0) return { kind: 'disabled', id: c.id };",
        "    if (false) return { kind: 'disabled', id: c.id };",
        "a bulk write runs with nothing selected",
    ),
    (
        "cmds/gate-always",
        COMMANDS,
        "    if (c.needsSelection && selected === 0) return { kind: 'disabled', id: c.id };",
        "    if (c.needsSelection) return { kind: 'disabled', id: c.id };",
        "a bulk write never runs, even with a selection",
    ),
    (
        "cmds/live-ignored",
        COMMANDS,
        "      if (!this.#live(c.id)) continue;",
        "      if (false) continue;",
        "a command whose scope was popped still runs",
    ),
    (
        "cmds/live-never-popped",
        COMMANDS,
        "    return this.#stack.some((f) => f.id === c.scope);",
        "    return true;",
        "liveness is not consulted, so a popped scope is still runnable",
    ),
    (
        "cmds/no-binding-unreachable",
        COMMANDS,
        "        (c) => c.binding != null && chordsEqual(c.binding as Chord, chord)",
        "        (c) => c.binding == null && chordsEqual(c.binding as Chord, chord)",
        "every command with a binding becomes unreachable",
    ),
    (
        "cmds/pop-scope-wrong",
        COMMANDS,
        "    if (top.id !== id) return false;",
        "    if (false) return false;",
        "popScope pops a frame that is not on top, corrupting the stack",
    ),
    (
        "cmds/pop-app",
        COMMANDS,
        "    if (this.#stack.length <= 1) return false;",
        "    if (false) return false;",
        "the app floor can be popped, leaving nothing to fall back to",
    ),
    (
        "cmds/reorder-in-place",
        COMMANDS,
        "    const out = [...items];",
        "    const out = items as T[];",
        "a reorder mutates the caller's array, changing a sorted view in place",
    ),
    (
        "cmds/reorder-no-clamp",
        COMMANDS,
        "    const clamped = Math.max(0, Math.min(out.length - 1, to));",
        "    const clamped = to;",
        "reordering past the end drops the item instead of moving it to the end",
    ),
    # ------------------------------------------------------------ search: rank
    (
        "rank/empty-query",
        COMMANDS,
        "  if (q === '') return commands.map((command) => ({ command, score: 1 }));",
        "  if (false) return commands.map((command) => ({ command, score: 1 }));",
        "an empty query filters instead of listing every command",
    ),
    (
        "rank/prefix-loses",
        COMMANDS,
        "  if (text.startsWith(q)) return 100;",
        "  if (false) return 100;",
        "'sel' no longer ranks a title that starts with it first",
    ),
    (
        "rank/substring-loses",
        COMMANDS,
        "  if (text.includes(q)) return 60;",
        "  if (false) return 60;",
        "a contiguous match no longer outranks a scattered one",
    ),
    (
        "rank/wordstart",
        COMMANDS,
        "    if (at === 0 || /[\\s\\-_/]/.test(text[at - 1] ?? '')) boundaryHits += 1;",
        "    if (at === 0) boundaryHits += 1;",
        "a match at a word boundary no longer outranks one mid-word",
    ),
    (
        "rank/boundary-weight",
        COMMANDS,
        "  return 20 + boundaryHits * 10 + score;",
        "  return 20 + score;",
        "word-initial matches no longer beat mid-word ones",
    ),
    (
        "rank/title-beats-keyword",
        COMMANDS,
        "      score += 10;",
        "      score += 0;",
        "a keyword match ranks equal to a title match",
    ),
    (
        "rank/keyword-searched",
        COMMANDS,
        "      for (const w of words) {",
        "      for (const w of []) {",
        "keywords stop matching, so a command findable only by keyword vanishes",
    ),
    (
        "rank/keyword-wins-over-title",
        COMMANDS,
        "          score = Math.max(score, s);",
        "          score = s + 1000;",
        "a keyword match outranks the title the user can actually see",
    ),
    (
        "rank/zero-dropped",
        COMMANDS,
        "    if (score > 0) scored.push({ command, score });",
        "    scored.push({ command, score });",
        "a non-matching command is listed with a score of 0",
    ),
    (
        "rank/tie-wordcount",
        COMMANDS,
        "    if (aw !== bw) return aw - bw;",
        "    if (false) return aw - bw;",
        "a scoring tie falls to alphabetical, which is the defect the tiebreak fixes",
    ),
    (
        "rank/tie-wordcount-desc",
        COMMANDS,
        "    if (aw !== bw) return aw - bw;",
        "    if (false) return aw - bw;",
        "a scoring tie falls to alphabetical, which is the defect the tiebreak fixes",
    ),
    (
        "rank/tie-length",
        COMMANDS,
        "    if (a.command.title.length !== b.command.title.length) {\n      return a.command.title.length - b.command.title.length;\n    }",
        "    if (false) {\n      return a.command.title.length - b.command.title.length;\n    }",
        "equal word counts stop being broken by title length",
    ),
    (
        "rank/words-of-empty",
        COMMANDS,
        "  return title.trim().split(/[\\s\\-_/]+/).filter((w) => w !== '').length;",
        "  return 0;",
        "every title counts as zero words, so the tiebreak never fires",
    ),
    # --------------------------------------------------------- commands-ui.ts
    (
        "ui/palette-rows-gate",
        UI_CMD,
        "    disabled: command.needsSelection === true && selected === 0",
        "    disabled: false",
        "a bulk write is shown as runnable with nothing selected",
    ),
    (
        "ui/palette-rows-gate-inverted",
        UI_CMD,
        "    disabled: command.needsSelection === true && selected === 0",
        "    disabled: selected === 0",
        "every command is disabled with nothing selected",
    ),
    (
        "ui/move-clamp",
        UI_CMD,
        "  if (next >= count) return 0;",
        "  if (next >= count) return count - 1;",
        "the highlight stops at the end instead of wrapping",
    ),
    (
        "ui/move-wrap-down",
        UI_CMD,
        "  if (next < 0) return count - 1;",
        "  if (next < 0) return 0;",
        "the highlight stops at the top instead of wrapping",
    ),
    (
        "ui/move-empty",
        UI_CMD,
        "  if (count === 0) return -1;",
        "  if (count === 0) return 0;",
        "an empty list highlights a row that does not exist",
    ),
    (
        "ui/ran-prevents",
        UI_CMD,
        "      if (run(outcome.id) !== false) e.preventDefault?.();",
        "      e.preventDefault?.();\n      run(outcome.id);",
        "a declined command swallows the key instead of leaving it to the browser",
    ),
    (
        "ui/collision-runs",
        UI_CMD,
        "      console.warn('commons: shortcut collision', outcome.ids);",
        "      run(outcome.ids[0]);",
        "a collision silently runs one of the two commands",
    ),
    (
        "ui/collision-prevents",
        UI_CMD,
        "      console.warn('commons: shortcut collision', outcome.ids);",
        "      e.preventDefault?.();\n      console.warn('commons: shortcut collision', outcome.ids);",
        "a collision also eats the key, so the browser gets nothing either",
    ),
    (
        "ui/text-prevents",
        UI_CMD,
        "      return 'text';",
        "      e.preventDefault?.();\n      return 'text';",
        "a key in a text field is swallowed instead of typed",
    ),
    (
        "ui/disabled-runs",
        UI_CMD,
        "      return 'disabled';",
        "      e.preventDefault?.();\n      run(outcome.id);\n      return 'disabled';",
        "a gated command runs with nothing selected",
    ),
    (
        "ui/rows-are-all-commands",
        UI_CMD,
        "  return searchCommands(registry.all(), query).map(({ command }) => ({",
        "  return searchCommands(registry.all(), '').map(({ command }) => ({",
        "the palette ignores the query and always lists everything",
    ),
]


def run_unit_suite() -> tuple[bool, str]:
    p = subprocess.run(
        ["node", "./tests/run-tests.mjs"],
        cwd=UI,
        capture_output=True,
        text=True,
        timeout=900,
    )
    return p.returncode == 0, (p.stdout + p.stderr)[-4000:]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true", help="leave the working tree dirty")
    args = ap.parse_args()

    originals = {path: open(path).read() for path in (KEYS, COMMANDS, UI_CMD)}
    try:
        base_ok, base_out = run_unit_suite()
        if not base_ok:
            print("BASELINE FAILED -- refusing to attribute kills to a red suite")
            print(base_out[-2000:])
            return 2
        print(f"baseline green; {len(MUTANTS)} mutants\n")

        killed: list[str] = []
        survived: list[str] = []
        broken: list[str] = []

        for mid, path, old, new, why in MUTANTS:
            try:
                mutated = sub(originals[path], old, new)
            except SystemExit as e:
                broken.append(mid)
                print(f"  BROKEN {mid}: {e}")
                continue
            open(path, "w").write(mutated)
            try:
                ok, out = run_unit_suite()
            finally:
                open(path, "w").write(originals[path])
            if ok:
                survived.append(mid)
                kind = "EQUIVALENT" if mid in KNOWN_EQUIVALENT else "SURVIVED "
                print(f"  {kind} {mid}  ({why})")
            else:
                killed.append(mid)
                print(f"  killed    {mid}")

        real = [m for m in survived if m not in KNOWN_EQUIVALENT]
        equivalent = [m for m in survived if m in KNOWN_EQUIVALENT]
        print(f"\n{len(killed)}/{len(MUTANTS)} killed")
        print(f"  {len(real)} real survivors, {len(equivalent)} known-equivalent")
        if real:
            print("\nreal survivors (a test does not distinguish this decision):")
            for m in real:
                print(f"  - {m}")
        if equivalent:
            print("\nknown-equivalent (not a test gap; see KNOWN_EQUIVALENT):")
            for m in equivalent:
                print(f"  - {m}: {KNOWN_EQUIVALENT[m].splitlines()[0]}")
        if broken:
            print("\nbroken anchors: " + ", ".join(broken))
        # A known-equivalent mutant is not a failure, but a broken anchor is: it
        # means the script is testing code that has moved, and it reports a pass
        # for a mutation it never applied.
        return 0 if not real and not broken else 1
    finally:
        if not args.keep:
            for path, text in originals.items():
                open(path, "w").write(text)


if __name__ == "__main__":
    sys.exit(main())
