#!/usr/bin/env bash
# Regenerate the test fixture corpus.
#
# The fixtures are real media files produced by real tools, not hand-crafted
# byte strings, because the point of the fixture table is to prove the sniffer
# agrees with ffmpeg, zip, and a text editor about what these files are. A
# hand-built file only proves the test agrees with itself.
#
# Every fixture here is used by a test that names it. `scene_offset.mp4` and
# `evil.zip` look unused to T-P1-001 and are not: they are used by T-P1-002 and
# T-P1-004 respectively, which is why this script generates them too.
#
# Determinism, precisely: the zip fixtures and everything ffmpeg writes to a
# raw stream are byte-stable across runs, so re-running leaves them untouched in
# `git diff`. The two exceptions are `clip.mkv` and `clip.webm`, because
# Matroska writes a random SegmentUID and a writing-date into the header. That
# is not worth fighting -- it is why the fixtures are checked in rather than
# built in `build.rs`, and a test asserting on a segment UID would be testing
# the encoder.
#
# Usage: scripts/make-fixtures.sh [output-dir]
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:-$HERE/../crates/commons-scan/tests/fixtures}"
mkdir -p "$OUT"

FFMPEG="${FFMPEG:-ffmpeg}"
if ! command -v "$FFMPEG" >/dev/null 2>&1; then
  echo "error: ffmpeg not found. Set FFMPEG=/path/to/ffmpeg or put it on PATH." >&2
  exit 1
fi
command -v python3 >/dev/null 2>&1 || {
  echo "error: python3 required (it builds the zip fixtures)." >&2
  exit 1
}

# Small, fast, and boring on purpose: these files exist to be sniffed, not to
# look like anything. -preset ultrafast keeps regeneration under a second.
gen() { echo "  $1"; "$FFMPEG" -y -loglevel error "$@" 2>&1 | sed 's/^/    /' || true; }

echo "video and audio"
gen -f lavfi -i testsrc=duration=3:size=320x240:rate=10 \
    -pix_fmt yuv420p -c:v libx264 -preset ultrafast "$OUT/scene_3s.mp4"
gen -f lavfi -i testsrc=duration=2:size=160x120:rate=10 \
    -pix_fmt yuv420p -c:v libx264 -preset ultrafast \
    -output_ts_offset 5 "$OUT/scene_offset.mp4"   # stash#7229: non-zero start_time
gen -f lavfi -i testsrc=duration=1:size=160x120:rate=5 \
    -pix_fmt yuv420p -c:v libx264 -preset ultrafast "$OUT/clip.mkv"
gen -f lavfi -i testsrc=duration=1:size=160x120:rate=5 \
    -pix_fmt yuv420p -c:v libvpx "$OUT/clip.webm"
gen -f lavfi -i "sine=frequency=440:duration=5.5" -c:a libmp3lame "$OUT/tone.mp3"
gen -f lavfi -i "sine=frequency=330:duration=2.5" -c:a flac "$OUT/tone.flac"
gen -f lavfi -i "sine=frequency=330:duration=1.5" -c:a pcm_s16le "$OUT/tone.wav"

echo "images"
gen -f lavfi -i testsrc=duration=1:size=300x200:rate=1 -frames:v 1 "$OUT/photo.jpg"
gen -f lavfi -i "color=c=red@0.5:size=64x64,format=rgba" -frames:v 1 "$OUT/alpha.png"
# A one-frame GIF is the stash#5111 "image" case. -frames:v 1 is what makes it
# one frame; a duration of 0.04s alone produces an empty file.
gen -f lavfi -i color=c=red:s=64x64 -frames:v 1 "$OUT/still.gif"
# A many-frame GIF is the stash#5111 "scene" case, resolved in T-P1-002.
gen -f lavfi -i testsrc=duration=4:size=200x200:rate=10 \
    -vf "fps=10,scale=100:-1:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse" \
    -loop 0 "$OUT/animated.gif"

echo "text and sidecars"
printf 'A story.\n\nIt has a few lines.\n' > "$OUT/story.txt"
cat > "$OUT/clip.funscript" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<funscript version="1.0">
  <metadata><title>test</title><version>1</version></metadata>
  <actions>
    <action t="0" pos="10"/>
    <action t="1000" pos="90"/>
  </actions>
</funscript>
XML

echo "archives"
python3 - "$OUT" <<'PY'
import os, sys, zipfile
out = sys.argv[1]

# ZipInfo carries a modification time that `write()` would otherwise take from
# the source file, so a regenerated archive differs from the committed one even
# when its contents are identical. Pinning it makes the zips byte-stable.
EPOCH = (2026, 1, 1, 0, 0, 0)

def put(z, arcname, srcpath):
    info = zipfile.ZipInfo(arcname, date_time=EPOCH)
    info.compress_type = z.compression
    with open(srcpath, "rb") as fh:
        z.writestr(info, fh.read())

# A gallery: a zip whose members are all images.
with zipfile.ZipFile(f"{out}/gallery.zip", "w", zipfile.ZIP_DEFLATED) as z:
    put(z, "001.jpg", f"{out}/photo.jpg")
    put(z, "002.png", f"{out}/alpha.png")
    put(z, "003.jpg", f"{out}/photo.jpg")

# A comic: identical mechanism, and deliberately out of order so the page
# sorter has something to fix (img_2 must precede img_10).
with zipfile.ZipFile(f"{out}/book.cbz", "w", zipfile.ZIP_STORED) as z:
    for name in ("img_10.jpg", "img_2.jpg", "img_1.jpg", "cover.png"):
        put(z, name, f"{out}/photo.jpg" if name.endswith(".jpg") else f"{out}/alpha.png")

# The Zip-Slip corpus for T-P1-004. Four members, three of them hostile:
#   ../../etc/passwd   relative traversal
#   /absolute/escape.txt  absolute path
#   link.jpg            a symlink member pointing outside the root
#   nested/../ok.txt    traversal that lands back inside, which is legal and
#                       must be accepted -- a validator that refuses everything
#                       is not a validator, it is a denial of service.
with zipfile.ZipFile(f"{out}/evil.zip", "w") as z:
    for hostile in ("../../etc/passwd", "/absolute/escape.txt", "nested/../ok.txt"):
        info = zipfile.ZipInfo(hostile, date_time=EPOCH)
        z.writestr(info, b"payload\n")
    info = zipfile.ZipInfo("link.jpg", date_time=EPOCH)
    info.create_system = 3                      # unix
    info.external_attr = (0o120777) << 16       # S_IFLNK | 0777
    z.writestr(info, "/etc/passwd")
PY

echo
echo "fixtures written to $OUT:"
ls -la "$OUT"
echo
echo "verify with:"
echo "  cargo test -p commons-scan --test fixture_table"
