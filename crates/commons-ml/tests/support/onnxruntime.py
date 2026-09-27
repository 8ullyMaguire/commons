"""A stand-in for `onnxruntime`, so the sidecar's PROTOCOL can be tested here.

This is not a stub of parakeet. It is a stub of the *runtime*, producing logits
from a known input, so that what the tests exercise is the sidecar's framing,
its error handling, its decoding arithmetic and its digest check -- the parts
that are ours. The model itself is NVIDIA's and cannot be faked meaningfully.

`make_session(logits_for)` is what the tests install: a factory taking a
function of the audio, so a test can decide what the "model" heard.
"""

import hashlib
import json
import os
import sys
import tempfile

# The CTC vocabulary this stands in for: blank + a few symbols. The ids are the
# "words" the decode emits, and the sidecar is under test for placing them in
# time, not for mapping ids to English.
VOCAB = 6
BLANK = VOCAB - 1


class SessionOptions:
    def __init__(self):
        self.log_severity_level = 0


class _IO:
    def __init__(self, name):
        self.name = name


class Session:
    def __init__(self, path, sess_options=None, providers=None):
        self.path = path
        # The sidecar must never be handed a model whose bytes it did not
        # verify, and the test asserts exactly this: the sidecar reads the file
        # and hashes it before constructing the session.
        with open(path, "rb") as handle:
            self._bytes = handle.read()
        self.digest = hashlib.sha256(self._bytes).hexdigest()
        # A factory installed by the test: audio bytes -> logits array.
        self._factory = _factory_from_environment()
        self._inputs = ["audio_features", "length"]

    def get_inputs(self):
        return [_IO(name) for name in self._inputs]

    def run(self, _outputs, feeds):
        import numpy as np

        audio = np.asarray(feeds["audio_features"], dtype=np.float32).reshape(-1)
        if self._factory is not None:
            return self._factory(audio)
        return [np.zeros((1, 4, VOCAB), dtype=np.float32)]


def InferenceSession(path, sess_options=None, providers=None):
    return Session(path, sess_options, providers)


def _factory_from_environment():
    """The logits the test chose, read from `FAKE_LOGITS`.

    The environment rather than a file, and the reason is specific: the sidecar
    imports this module when it loads a model, so a marker file written after
    the harness started would never be read and the tests would pass while
    asserting nothing.
    """
    import numpy as np

    raw = os.environ.get("FAKE_LOGITS")
    if not raw:
        return None
    # [batch, time, vocab], which is what the sidecar indexes: `logits[0]` for
    # the batch and `logits.shape[-1]` for the vocabulary, whose LAST index is
    # the blank. A 2-D array works until `shape[-1]` silently reads the wrong
    # axis, so the extra axis is part of what the fake is faking.
    frames = np.array(json.loads(raw), dtype=np.float32)[None, :, :]
    # A list OF THE ARRAY, because `session.run` returns a list of outputs and
    # the sidecar does `outputs[0].shape`. Returning the array itself is the
    # classic off-by-one-list and fails with 'list' object has no attribute
    # 'shape'.
    return lambda audio: [frames]


def make_model_file(contents=b"fake-parakeet-weights"):
    handle = tempfile.NamedTemporaryFile(delete=False, suffix=".onnx")
    handle.write(contents)
    handle.close()
    return handle.name


def digest_of(path):
    with open(path, "rb") as handle:
        return hashlib.sha256(handle.read()).hexdigest()


if __name__ == "__main__":
    # A convenience for manual poking: emit a model file path on stdout.
    print(make_model_file())
