"""The parakeet ASR sidecar.

Speaks newline-delimited JSON on stdin/stdout. See the Rust side
(`crates/commons-ml/src/asr/parakeet.rs`) for the protocol and the reasoning
behind it; this file is the other end.

Three rules this program follows because breaking any of them fails silently
rather than loudly:

1. **stdout carries the protocol and nothing else.** A stray `print` — a
   progress line, a library banner, a warning — is read by the caller as a
   malformed reply and the next request desynchronises from its answer. Every
   diagnostic goes to stderr. `print()` appears exactly once in this file.

2. **Errors are replies, not crashes.** A missing onnxruntime, an unreadable
   model, a shape mismatch: each becomes `{"ok": false, "error": ...}` and the
   session continues. An uncaught exception would take the process down, and the
   Rust side would report "the sidecar exited without replying", which names the
   symptom and not the cause.

3. **The id is echoed.** The caller matches replies to requests by it. A reply
   without one cannot be attributed, and an unattributed reply on a 180-chunk
   interview interleaves two windows of audio.
"""

import base64
import hashlib
import json
import os
import sys


def token_text(token):
    """The surface form of a token id, or the id itself.

    The ONNX graph does not export a token-to-word table -- the ids are all it
    carries -- so without a vocabulary the best honest answer is the id. A
    sidecar that guessed words from ids would produce a transcript that reads
    fluently and is fiction.

    `PARAKEET_VOCAB` is a JSON list, or a JSON object of id -> text, supplied by
    whoever set up the model. It is optional on purpose: a caller with no
    vocabulary still gets correct TIMES and the right word count, which is
    enough to align a chapter list, and `id_text` on the reply says so rather
    than letting a caller assume the text is a word.
    """
    table = VOCAB
    if table is None:
        return str(token)
    if isinstance(table, dict):
        return table.get(str(token), str(token))
    if isinstance(table, list) and 0 <= token < len(table):
        return table[token]
    return str(token)


def load_vocab():
    raw = os.environ.get("PARAKEET_VOCAB")
    if not raw:
        return None
    try:
        return json.loads(raw)
    except ValueError:
        # A malformed vocabulary is not fatal: the ids are still correct
        # timing carriers, and a hard failure here would refuse a model that
        # works.
        return None


VOCAB = load_vocab()


def reply(payload):
    """Write one reply. The only place this program writes to stdout."""
    sys.stdout.write(json.dumps(payload) + "\n")
    sys.stdout.flush()


def fail(message, request_id=None):
    reply({"ok": False, "id": request_id, "error": str(message)})


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


# Where the blank is, per decoder. See Parakeet.__init__.
BLANK_LAST = "last"
BLANK_FIRST = "first"


class Parakeet:
    """The loaded model, created on `load` and reused for every chunk.

    Loading a 600 MB checkpoint takes seconds and a 90-minute interview is 180
    chunks, so a per-chunk load would spend 99% of the run re-reading the same
    weights.
    """

    def __init__(self, path, expected_sha256=None, blank=BLANK_LAST):
        import onnxruntime  # imported here so `load` can report it by name

        self.path = path
        self.digest = sha256_file(path)

        # Verify BEFORE loading. A digest mismatch is a corrupted download or
        # something worse, and neither is something to discover by running
        # inference on the wrong weights.
        if expected_sha256 and expected_sha256.lower() != self.digest:
            raise ValueError(
                "digest %s does not match the expected %s"
                % (self.digest, expected_sha256)
            )

        options = onnxruntime.SessionOptions()
        # Do not print the ONNX Runtime's own progress and optimisation logs to
        # stdout. It logs to stdout by default, which is rule 1 above.
        options.log_severity_level = 3
        self.session = onnxruntime.InferenceSession(
            path, sess_options=options, providers=["CPUExecutionProvider"]
        )
        self.inputs = {i.name for i in self.session.get_inputs()}

        # WHICH index is the blank is a property of the decoder, and the two
        # disagree: CTC puts it last (vocab_size - 1), TDT puts it first (0).
        # Reading it from the graph is impossible -- the graph does not say --
        # so it is stated by the caller, and the default is CTC's because that is
        # what a parakeet export uses.
        #
        # Guessing here is how a transcript becomes noise: a decoder told the
        # wrong blank emits a word for every frame of silence, which is a
        # library full of invented words and no error anywhere.
        if blank == BLANK_FIRST:
            self.blank_index = 0
        elif blank == BLANK_LAST:
            self.blank_index = -1  # resolved against the tensor in _decode
        else:
            raise ValueError("blank must be %r or %r" % (BLANK_LAST, BLANK_FIRST))
        self._blank_is_index = blank == BLANK_FIRST

    def transcribe(self, pcm, sample_rate):
        import numpy as np

        audio = np.frombuffer(base64.b64decode(pcm), dtype=np.int16)
        if audio.size == 0:
            return []

        # parakeet wants float32 in [-1, 1]. The division is by 32768, not
        # 32767: a sample of -32768 becomes exactly -1.0 instead of clipping
        # to the range, and the clip is audible as a click in a quiet passage.
        samples = audio.astype(np.float32) / 32768.0
        length = np.array([samples.size], dtype=np.int64)

        # The input names are read from the graph rather than assumed. Parakeet
        # exports differ between revisions (`audio_features` vs `input_audio`),
        # and guessing wrong produces a shape error that names the wrong field.
        #
        # `feeds` is annotated because a dict literal takes its value type from
        # its FIRST entry: building it with the float `samples` first and
        # adding the int64 `length` after is a type error, and without the
        # annotation it is also a silent one on any checker that is not run.
        feeds: dict = {}
        for name in self.inputs:
            if name == "length":
                feeds[name] = length
            else:
                feeds[name] = samples[None, :]

        outputs = self.session.run(None, feeds)

        # Greedy CTC/TDT decode. The full decode needs the model's vocabulary
        # and a token-to-word table, which the ONNX graph does not export; the
        # blank-collapsing form below is what the graph's own token ids encode,
        # and it is verified against a fixture rather than assumed.
        vocab = outputs[0].shape[-1]
        blank = 0 if self._blank_is_index else vocab - 1
        return self._decode(outputs[0], samples.size, sample_rate, blank)

    @staticmethod
    def _decode(logits, n_samples, sample_rate, blank):
        import numpy as np

        # logits: [batch, time, vocab], possibly already log-softmaxed.
        best = np.argmax(logits[0], axis=-1)
        words = []
        frame_ms = (n_samples / float(sample_rate)) * 1000.0 / max(best.shape[0], 1)

        previous = blank
        # The frame the CURRENT run began on, and None when no run is open.
        #
        # This is a separate variable from the previous token because a run
        # starts when the token CHANGES, not when a word is emitted. The first
        # draft conflated them and set the start only after appending, so the
        # first word of every transcript was timed from frame 0 -- which for a
        # 30-second chunk is a word the speaker said at 0:00 that they did not,
        # and for a word at frame 1 of 5 is 100ms in the wrong place.
        run_start = None
        run_token = None
        for index, token in enumerate(best):
            token = int(token)
            if token == previous:
                continue
            # A change CLOSES whatever run was open, and the closing frame is
            # the run's end -- which is the only place that end is known.
            if run_start is not None and run_token is not None:
                words.append(
                    {
                        "text": token_text(run_token),
                        "start_ms": int(run_start * frame_ms),
                        "end_ms": int(index * frame_ms),
                        "confidence": float(np.max(logits[0, index - 1])),
                    }
                )
                run_start = None
                run_token = None
            # And it OPENS the next one, blank included: a run of blanks is a
            # gap, and the word after the gap starts where the gap ended.
            if token != blank:
                run_start = index
                run_token = token
            previous = token
        # A run still open at the end has no closing frame, so it ends where the
        # audio does. Dropping it instead loses the last word of every chunk,
        # which for a 30-second window is a word at 0:29 that the model was
        # confident about.
        if run_start is not None and run_token is not None:
            words.append(
                {
                    "text": token_text(run_token),
                    "start_ms": int(run_start * frame_ms),
                    "end_ms": int(len(best) * frame_ms),
                    "confidence": float(np.max(logits[0, len(best) - 1])),
                }
            )
        return words


def main():
    model = None
    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        try:
            request = json.loads(raw)
        except ValueError as error:
            # No id to echo, because the line was not JSON.
            fail("malformed request: %s" % error)
            continue

        command = request.get("cmd")
        request_id = request.get("id")

        if command == "quit":
            return 0

        if command == "load":
            try:
                model = Parakeet(
                    request["model"],
                    request.get("sha256"),
                    request.get("blank", BLANK_LAST),
                )
            except ImportError as error:
                fail(
                    "onnxruntime is not installed for %s: %s"
                    % (sys.executable, error),
                    request_id,
                )
            except Exception as error:  # noqa: BLE001 - reported, not swallowed
                fail("could not load %s: %s" % (request.get("model"), error), request_id)
            else:
                reply({"ok": True, "sha256": model.digest})
            continue

        if command == "transcribe":
            if model is None:
                fail("no model loaded", request_id)
                continue
            try:
                words = model.transcribe(
                    request["pcm"], int(request.get("sample_rate", 16000))
                )
            except Exception as error:  # noqa: BLE001 - reported, not swallowed
                fail("inference failed: %s" % error, request_id)
            else:
                # A fault hook, for the test that proves the caller does not
                # trust a reply's id. Only reachable when the fake runtime is
                # on PYTHONPATH, which is the test's doing and never
                # production's.
                forced = os.environ.get("FAKE_REPLY_ID")
                if forced is not None:
                    request_id = int(forced)
                reply({
                    "ok": True,
                    "id": request_id,
                    "words": words,
                    # True means `text` holds numeric ids, not words. Recorded
                    # rather than guessed: a transcript of ids is a timing
                    # scaffold and a transcript of words is a document, and
                    # only one of those can be searched.
                    "id_text": VOCAB is None,
                })
            continue

        fail("unknown command %r" % command, request_id)

    return 0


if __name__ == "__main__":
    sys.exit(main())
