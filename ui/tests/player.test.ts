/**
 * The player's pure logic. Spec 11.1, 11.5 / plan T-P6-001.
 *
 * Every wrong answer in `player.ts` is a *number*, so none of it is visible in
 * a screenshot: a scrubber that jumps backwards, an A/B loop that never fires, a
 * resume position inside the last two percent of a finished video, a control bar
 * taller than a phone. That is why these are unit tests and not only e2e ones --
 * and why the e2e spec keeps the 360x640 assertion the ticket names, which is
 * the one claim no unit test can make.
 */
import { describe, it } from 'node:test';
import assert from 'node:assert/strict';
import {
  clipPath,
  frameToMs,
  isLoopArmed,
  LONG_PRESS_SPEED,
  loopMarkers,
  longPressSpeed,
  msToFrame,
  needsProxy,
  planControlBar,
  resumePosition,
  SAVE_INTERVAL_MS,
  seekAccuracy,
  shouldDeinterlace,
  shouldSave,
  skipIntroWindow,
  playSource,
  videoStyle,
  HOLD_MS,
  SLOP_PX
} from '../src/lib/player/player.js';

describe('loop arming', () => {
  it('is armed only when both ends are set and ordered', () => {
    assert.equal(isLoopArmed({ a_ms: 1000, b_ms: 4000 }), true);
  });

  it('a loop from the very start is a real loop', () => {
    // Dragging marker A to the start is a loop over the first five seconds,
    // and the server's CHECK allows it because all it requires is a < b. An
    // earlier version also demanded a_ms > 0, which made this inert: the marker
    // was drawn, the badge said nothing, and the file silently did not loop.
    assert.equal(isLoopArmed({ a_ms: 0, b_ms: 5000 }), true);
  });

  it('a half-set loop is inert rather than a zero-length loop', () => {
    // The failure this prevents: a player sitting in a loop of zero length,
    // showing one frame for ever, which looks like a hang. "Unset" is NULL --
    // so the shape that reaches here never carries 0 for a missing marker.
    assert.equal(isLoopArmed({ a_ms: 1500, b_ms: 0 }), false);
    assert.equal(isLoopArmed({ a_ms: 0, b_ms: 0 }), false);
    assert.equal(isLoopArmed(null), false);
    assert.equal(isLoopArmed(undefined), false);
  });

  it('a non-finite or negative marker is inert', () => {
    assert.equal(isLoopArmed({ a_ms: Number.NaN, b_ms: 4000 }), false);
    assert.equal(isLoopArmed({ a_ms: 1000, b_ms: Number.POSITIVE_INFINITY }), false);
    assert.equal(isLoopArmed({ a_ms: -1, b_ms: 4000 }), false);
  });

  it('an inverted loop is inert the instant the markers cross', () => {
    // Not waiting for a 422 from the server: a user can drag B left of A between
    // two saves, and the UI must stop looping immediately.
    assert.equal(isLoopArmed({ a_ms: 4000, b_ms: 1000 }), false);
  });
});

describe('loop markers going to the server', () => {
  it('an unset marker is NULL, never 0', () => {
    // 0 is a position -- the start of the file -- and sending it says the user
    // put a marker there. NULL says they did not.
    assert.deepEqual(loopMarkers(null, 4000), { loop_a_ms: null, loop_b_ms: null });
    assert.deepEqual(loopMarkers(1000, null), { loop_a_ms: null, loop_b_ms: null });
    assert.deepEqual(loopMarkers(null, null), { loop_a_ms: null, loop_b_ms: null });
  });

  it('a set pair is rounded and sent as-is', () => {
    assert.deepEqual(loopMarkers(1000.4, 4000.6), { loop_a_ms: 1000, loop_b_ms: 4001 });
    assert.deepEqual(loopMarkers(0, 5000), { loop_a_ms: 0, loop_b_ms: 5000 });
  });

  it('an inverted pair is stored as both-NULL, not as a 422', () => {
    // A user who drags B left of A has not made a mistake worth an error
    // dialog; they have a loop that is not armed.
    assert.deepEqual(loopMarkers(4000, 1000), { loop_a_ms: null, loop_b_ms: null });
    assert.deepEqual(loopMarkers(1000, 1000), { loop_a_ms: null, loop_b_ms: null });
  });

  it('a non-finite marker is unset rather than NaN', () => {
    // JSON.stringify(NaN) is `null`, so this would have worked by accident -- but
    // only for NaN, and only on this path. Infinity is the case that would not.
    assert.deepEqual(loopMarkers(Number.POSITIVE_INFINITY, 4000), {
      loop_a_ms: null,
      loop_b_ms: null
    });
    assert.deepEqual(loopMarkers(1000, Number.NaN), { loop_a_ms: null, loop_b_ms: null });
  });
});

describe('resume position', () => {
  it('an unplayed object resumes at the start', () => {
    assert.equal(resumePosition({ position_ms: 0 }, 60_000), 0);
    assert.equal(resumePosition(null, 60_000), 0);
    assert.equal(resumePosition(undefined, 60_000), 0);
  });

  it('a position in the middle resumes there', () => {
    assert.equal(resumePosition({ position_ms: 30_000 }, 60_000), 30_000);
  });

  it('a completed video starts again rather than showing its last frame', () => {
    // Resuming at `duration` shows a finished video and a play button that
    // appears to do nothing.
    assert.equal(resumePosition({ position_ms: 59_000, completed: true }, 60_000), 0);
  });

  it('a position inside the trailing two percent starts again', () => {
    // The case a naive `position > 0` test misses: abandoned ten seconds before
    // the end, the user sees the last scene with no sign it is over.
    assert.equal(resumePosition({ position_ms: 59_500 }, 60_000), 0);
    assert.equal(resumePosition({ position_ms: 59_400 }, 60_000), 0);
    // Just outside the window it is kept, so this is a threshold and not
    // "everything near the end is discarded".
    assert.equal(resumePosition({ position_ms: 58_000 }, 60_000), 58_000);
  });

  it('the tail is a fraction, so a short clip is not swallowed whole', () => {
    // 2% of 30s is 600 ms, so the window starts at 29_400. A fixed two-second
    // threshold would start at 28_000 -- discarding three times as much of a
    // short clip as of a long one, which is backwards.
    assert.equal(resumePosition({ position_ms: 29_400 }, 30_000), 0);
    assert.equal(resumePosition({ position_ms: 29_399 }, 30_000), 29_399);
    // And the fixed-threshold comparison on a long clip, which is the case the
    // fraction is protecting: 2% of an hour is 72 s, not 2 s.
    assert.equal(resumePosition({ position_ms: 3_500_000 }, 3_600_000), 3_500_000);
    assert.equal(resumePosition({ position_ms: 3_590_000 }, 3_600_000), 0);
  });

  it('a position past the end starts again', () => {
    assert.equal(resumePosition({ position_ms: 90_000 }, 60_000), 0);
  });

  it('an unknown duration still restores a position', () => {
    // Common while a manifest loads: seekable, no length yet.
    assert.equal(resumePosition({ position_ms: 12_000 }, null), 12_000);
    // The state's own duration is the fallback.
    assert.equal(resumePosition({ position_ms: 12_000, duration_ms: 60_000 }, null), 12_000);
  });

  it('a zero or negative duration does not divide the tail into nonsense', () => {
    assert.equal(resumePosition({ position_ms: 5_000 }, 0), 5_000);
    assert.equal(resumePosition({ position_ms: 5_000 }, -1), 5_000);
  });

  it('a non-finite position is treated as unplayed', () => {
    assert.equal(resumePosition({ position_ms: Number.NaN }, 60_000), 0);
    assert.equal(resumePosition({ position_ms: Number.POSITIVE_INFINITY }, 60_000), 0);
  });
});

describe('frame-accurate seek', () => {
  it('a frame number converts at the source rate', () => {
    assert.equal(frameToMs(900, 30), 30_000);
    assert.equal(frameToMs(0, 30), 0);
  });

  it('29.97 is not rounded to 30', () => {
    // Rounding 29.97 to 30 is a 100 ms drift by frame 900 -- which is exactly
    // the wrong frame, and the drift is invisible until someone compares.
    const ms = frameToMs(900, 29.97);
    assert.notEqual(ms, 30_000);
    assert.ok(Math.abs(ms - 30_030) < 2, `got ${ms}`);
  });

  it('a missing frame rate reports that it cannot, rather than guessing', () => {
    // A frame-accurate seek with no frame rate is a time seek wearing a label.
    assert.equal(frameToMs(900, null), null);
    assert.equal(frameToMs(900, undefined), null);
    assert.equal(frameToMs(900, 0), null);
    assert.equal(frameToMs(900, Number.NaN), null);
  });

  it('a negative frame is refused rather than seeking before the start', () => {
    assert.equal(frameToMs(-1, 30), null);
  });

  it('the conversion round-trips', () => {
    const ms = frameToMs(1234, 25);
    assert.equal(msToFrame(ms, 25), 1234);
  });

  it('the inverse also refuses an unknown rate', () => {
    assert.equal(msToFrame(1000, null), null);
    assert.equal(msToFrame(1000, 0), null);
    assert.equal(msToFrame(-5, 30), null);
  });

  it('the control only claims frame accuracy when it has a rate', () => {
    assert.equal(seekAccuracy(30), 'frame');
    assert.equal(seekAccuracy(29.97), 'frame');
    assert.equal(seekAccuracy(null), 'time');
    assert.equal(seekAccuracy(undefined), 'time');
    assert.equal(seekAccuracy(0), 'time');
    assert.equal(seekAccuracy(Number.NaN), 'time');
  });
});

describe('long-press 2x', () => {
  it('a tap is not a hold', () => {
    const v = longPressSpeed({ held: true, heldForMs: 50, driftPx: 0 });
    assert.equal(v.speed, 1);
    assert.equal(v.reason, 'not-yet');
  });

  it('a held finger engages 2x', () => {
    const v = longPressSpeed({ held: true, heldForMs: HOLD_MS, driftPx: 0 });
    assert.equal(v.speed, LONG_PRESS_SPEED);
    assert.equal(v.reason, 'held');
  });

  it('a drifting finger releases rather than flickering', () => {
    // Holding a phone in one hand produces a 2px tremor. Without a slop bound
    // the speed flips off mid-sentence, which is worse than not having it.
    const v = longPressSpeed({ held: true, heldForMs: 5000, driftPx: SLOP_PX + 1 });
    assert.equal(v.speed, 1);
    assert.equal(v.reason, 'drifted-too-far');
  });

  it('drift exactly at the bound still counts as held', () => {
    const v = longPressSpeed({ held: true, heldForMs: 5000, driftPx: SLOP_PX });
    assert.equal(v.speed, LONG_PRESS_SPEED);
  });

  it('a released finger is back to normal', () => {
    assert.deepEqual(longPressSpeed({ held: false, heldForMs: 5000, driftPx: 0 }), {
      speed: 1,
      reason: 'idle'
    });
  });
});

describe('deinterlacing', () => {
  it('off is off, even for a flagged interlaced source', () => {
    assert.equal(shouldDeinterlace('off', { interlaced: true, fieldOrder: 'tt' }), false);
  });

  it('on is on, even for a progressive source', () => {
    assert.equal(shouldDeinterlace('on', { interlaced: false, fieldOrder: 'progressive' }), true);
  });

  it('auto reads the flag and the field order', () => {
    assert.equal(shouldDeinterlace('auto', { interlaced: true }), true);
    assert.equal(shouldDeinterlace('auto', { fieldOrder: 'tt' }), true);
    assert.equal(shouldDeinterlace('auto', { fieldOrder: 'bb' }), true);
    assert.equal(shouldDeinterlace('auto', { interlaced: false, fieldOrder: 'progressive' }), false);
    // An unknown field order is not evidence of interlacing.
    assert.equal(shouldDeinterlace('auto', { fieldOrder: 'unknown' }), false);
  });

  it('an unflagged low-frame-rate source is not deinterlaced', () => {
    // Telecined 24fps content is flagged progressive, and deinterlacing it makes
    // motion worse -- which is why this setting is per-playback.
    assert.equal(shouldDeinterlace('auto', { interlaced: false, fps: 23.976 }), false);
  });
});

describe('crop, pan and flip', () => {
  it('nothing set means no style at all', () => {
    assert.equal(videoStyle(), '');
    assert.equal(videoStyle({}), '');
    assert.equal(clipPath(), '');
    assert.equal(clipPath({ crop: { top: 0, right: 0, bottom: 0, left: 0 } }), '');
  });

  it('flip comes before translate so the pan is in the right axis', () => {
    // The order is load-bearing and easy to get wrong: translate is applied last
    // in a CSS transform list, so panning before flipping moves along the axis
    // the user cannot see.
    const css = videoStyle({ flipH: true, panX: 0.25 });
    const flipAt = css.indexOf('scale(');
    const panAt = css.indexOf('translate(');
    assert.ok(flipAt >= 0, `no scale in: ${css}`);
    assert.ok(panAt > flipAt, `wrong order: ${css}`);
  });

  it('rotation sits between the flip and the pan', () => {
    const css = videoStyle({ flipH: true, rotate: 90, panY: 0.1 });
    assert.ok(css.indexOf('scale(') < css.indexOf('rotate('), css);
    assert.ok(css.indexOf('rotate(') < css.indexOf('translate('), css);
  });

  it('a crop becomes a clip-path, not object-fit cover', () => {
    // `cover` crops silently, so a user who asked for 4:3 gets a different
    // picture than they asked for without being told.
    assert.equal(
      clipPath({ crop: { top: 10, right: 0, bottom: 10, left: 0 } }),
      'clip-path: inset(10% 0% 10% 0%);'
    );
  });
});

describe('the control bar fits the viewport', () => {
  it('a roomy viewport keeps everything', () => {
    assert.deepEqual(planControlBar({ width: 1280, height: 800 }), {
      showTitle: true,
      showSecondary: true,
      showReadouts: true,
      showPrimary: true,
      dropped: []
    });
  });

  it('the 360x640 case drops the title and the secondary controls', () => {
    // stash#6526, and the ticket's Done-when. A bar with a title, a scrubber and
    // eight buttons is taller than the video and covers the thing the user
    // opened it to watch.
    const p = planControlBar({ width: 360, height: 640 });
    assert.equal(p.showTitle, false);
    assert.equal(p.showSecondary, false);
    assert.ok(p.dropped.includes('title'));
  });

  it('the play button and scrubber are NEVER dropped', () => {
    // A player you cannot pause is worse than one showing less information.
    for (const vp of [
      { width: 360, height: 640 },
      { width: 320, height: 480 },
      { width: 414, height: 400 },
      { width: 1920, height: 200 },
      { width: 280, height: 300 }
    ]) {
      assert.equal(planControlBar(vp).showPrimary, true, JSON.stringify(vp));
    }
  });

  it('narrow but tall drops controls rather than rows', () => {
    const p = planControlBar({ width: 360, height: 900 });
    assert.equal(p.showPrimary, true);
    assert.equal(p.showSecondary, false);
    // The title survives: there is vertical room, and it is the cheapest thing
    // to lose information-wise.
    assert.equal(p.showTitle, true);
  });
});

describe('needing the proxy', () => {
  const playable = { container: 'mov,mp4,m4a,3gp,3g2,mj2', videoCodec: 'h264', audioCodec: 'aac' };

  it("ffprobe's container is a comma-joined list, not one name", () => {
    // Matching the whole string against 'mp4' never matches anything, and the
    // cost is a transcode for every file in the library.
    assert.equal(needsProxy(playable), false);
  });

  it('a silent playable file needs no proxy', () => {
    assert.equal(needsProxy({ ...playable, audioCodec: '' }), false);
    assert.equal(needsProxy({ ...playable, audioCodec: null }), false);
  });

  it('h264 muxed into matroska is not playable, whatever the list says', () => {
    assert.equal(
      needsProxy({ container: 'matroska,webm', videoCodec: 'h264', audioCodec: 'aac' }),
      true
    );
  });

  it('vp9 in a matroska/webm is playable', () => {
    assert.equal(
      needsProxy({ container: 'matroska,webm', videoCodec: 'vp9', audioCodec: 'opus' }),
      false
    );
  });

  it('an unrecognised codec is proxied rather than trusted', () => {
    // Trusting an unknown codec means a <video> that never fires canplay and a
    // user looking at a black rectangle.
    assert.equal(
      needsProxy({ container: 'matroska,webm', videoCodec: 'mpeg4', audioCodec: '' }),
      true
    );
    assert.equal(needsProxy({ container: 'avi', videoCodec: 'h264', audioCodec: 'aac' }), true);
  });

  it('a recognised codec in the wrong container is still unplayable', () => {
    // Every part is recognised and the browser still will not play it -- the
    // case a per-field whitelist alone misses.
    assert.equal(needsProxy({ container: 'mov,mp4', videoCodec: 'vp9', audioCodec: 'opus' }), true);
  });

  it('missing metadata is proxied, because a guess is not a decision', () => {
    assert.equal(needsProxy({}), true);
    assert.equal(needsProxy({ container: null, videoCodec: null, audioCodec: null }), true);
  });
});

describe('choosing a source', () => {
  const PLAYABLE = { container: 'mov,mp4', videoCodec: 'h264', audioCodec: 'aac' };

  it("the server's answer wins, and needsProxy is not consulted", () => {
    // The whole point of the caps endpoint. The server says "no proxy needed"
    // for a source the local rule would call unplayable -- and it is right,
    // because it ran the same probe. Trusting the local rule here produces a
    // 422 from the proxy route, which reads as a broken player.
    const caps = { container: 'matroska,webm', video_codec: 'vp9', audio_codec: 'opus', rung: null };
    const r = playSource('o1', caps, { container: 'avi', videoCodec: 'mpeg4', audioCodec: '' });
    assert.equal(r.proxied, false);
    assert.equal(r.url, '/media/o1');
  });

  it('a rung from the server becomes the proxy url and the height', () => {
    const caps = { container: 'matroska,webm', video_codec: 'mpeg4', audio_codec: '', rung: 720 };
    assert.deepEqual(playSource('o1', caps, {}), {
      proxied: true,
      url: '/media/o1/proxy.m3u8?h=720'
    });
  });

  it('a height above the rung is reduced rather than sent', () => {
    // A rung the source does not need is an upscale. The server clamps it
    // anyway, and a url asking for the impossible is worse in a log.
    const caps = { container: 'matroska,webm', video_codec: 'mpeg4', audio_codec: '', rung: 480 };
    assert.equal(playSource('o1', caps, {}, 1080).url, '/media/o1/proxy.m3u8?h=480');
    // Below the rung is honoured, because the client may want less than the
    // decision requires -- less work, not more.
    assert.equal(playSource('o1', caps, {}, 360).url, '/media/o1/proxy.m3u8?h=360');
  });

  it('a zero or absent height uses the rung the server chose', () => {
    const caps = { container: 'matroska,webm', video_codec: 'mpeg4', audio_codec: '', rung: 720 };
    assert.equal(playSource('o1', caps, {}, 0).url, '/media/o1/proxy.m3u8?h=720');
    assert.equal(playSource('o1', caps, {}, null).url, '/media/o1/proxy.m3u8?h=720');
  });

  it('with no caps the local rule decides, and says proxy when it cannot tell', () => {
    // Asymmetric on purpose: a wrong "direct" is a video that does not play, and
    // a wrong "proxy" is one extra transcode.
    assert.equal(playSource('o1', null, PLAYABLE).proxied, false);
    assert.equal(playSource('o1', null, {}).proxied, true);
    assert.equal(playSource('o1', null, PLAYABLE).url, '/media/o1');
    assert.equal(playSource('o1', null, { container: 'avi' }).url, '/media/o1/proxy.m3u8');
  });

  it('an object id cannot become a path of its own', () => {
    // The id goes through encodeURIComponent, so a slash is %2F and stays inside
    // the one segment. The literal ".." that remains is inert: it is not a
    // separator, so it cannot walk up a directory. The route has exactly four
    // path parts regardless of what the id looks like.
    const url = playSource('a/../../etc', null, { container: 'avi' }).url;
    const after = url.split('/media/')[1].replace('/proxy.m3u8', '');
    assert.equal(after, 'a%2F..%2F..%2Fetc');
    assert.equal(url.split('/').length, 4, url);
  });
});

describe('saving', () => {
  it('the first save happens immediately', () => {
    assert.equal(shouldSave(0, null, 'tick'), true);
  });

  it('a tick inside the interval does not save', () => {
    assert.equal(shouldSave(1000, 0, 'tick'), false);
    assert.equal(shouldSave(SAVE_INTERVAL_MS - 1, 0, 'tick'), false);
  });

  it('a tick at the interval saves', () => {
    assert.equal(shouldSave(SAVE_INTERVAL_MS, 0, 'tick'), true);
  });

  it('leaving the page always saves, whatever the interval says', () => {
    // `pagehide` and not `beforeunload`: the latter is not fired when mobile
    // Safari discards a tab, which is precisely the case a resume position
    // exists for.
    assert.equal(shouldSave(1, 0, 'pagehide'), true);
  });
});

describe('skip-intro', () => {
  it('a marker applies only when this file has one', () => {
    // Per source (stash#634): a marker set on one file must not silently skip
    // the first thirty seconds of a different one.
    const marker = { startMs: 30_000, endMs: 90_000 };
    assert.deepEqual(skipIntroWindow(true, marker), marker);
    assert.equal(skipIntroWindow(false, marker), null);
    assert.equal(skipIntroWindow(true, null), null);
  });

  it('a malformed marker is ignored rather than skipping nothing visible', () => {
    assert.equal(skipIntroWindow(true, { startMs: 90_000, endMs: 30_000 }), null);
    assert.equal(skipIntroWindow(true, { startMs: -1, endMs: 30_000 }), null);
    assert.equal(skipIntroWindow(true, { startMs: 0, endMs: 0 }), null);
  });
});
