/**
 * Share links, client side. Spec §9.5 (#5612), plan T-P5-007 part 2B.
 *
 * The e2e covers a link that opens. This covers the two things a live server
 * makes awkward to test: a denial the client has to turn into a *sentence*,
 * and a reason the client does not recognise.
 *
 * The second is the one that matters. A server that grows a new denial reason
 * must not make an older client show a blank page — and must certainly not make
 * it show a success. So `refusalFrom` falls back to the claim that is safe when
 * the client does not know what happened: "not valid", which asserts less than
 * any specific reason.
 */

import { test, describe, mock } from 'node:test';
import assert from 'node:assert/strict';
import { refusalFrom, stateLabel } from '../src/lib/api/share.js';

describe('a denial becomes a sentence', () => {
  test('expired tells the recipient to ask for a new link', () => {
    // The load-bearing one. "Not available" sends the recipient nowhere; this
    // sends them to their sender with something to say.
    const r = refusalFrom('expired');
    assert.equal(r.kind, 'refused');
    assert.equal(r.reason, 'expired');
    assert.match(r.message, /expired/i);
    assert.match(r.message, /ask/i);
  });

  test('revoked says somebody turned it off, not that it aged out', () => {
    // Somebody deliberately disabled this link. Saying "expired" would be a
    // polite lie that sends the recipient to argue with the wrong person.
    const r = refusalFrom('revoked');
    assert.equal(r.reason, 'revoked');
    assert.match(r.message, /turned off|disabled|no longer/i);
    assert.doesNotMatch(r.message, /expired/i);
  });

  test('password_required is not the same as wrong_password', () => {
    // Two refusals, two different things to show. Collapsing them makes a
    // recipient retype a password they typed correctly.
    const need = refusalFrom('password_required');
    const wrong = refusalFrom('wrong_password');
    assert.notEqual(need.message, wrong.message);
    assert.match(need.message, /password/i);
    assert.match(wrong.message, /not right|incorrect|wrong/i);
  });

  test('every known reason produces a refusal, never an open', () => {
    // The single most important property here. A denial that rendered as an
    // open link would show a recipient a file they are not allowed to see.
    for (const reason of [
      'unknown_token',
      'expired',
      'revoked',
      'password_required',
      'wrong_password'
    ] as const) {
      const r = refusalFrom(reason);
      assert.equal(r.kind, 'refused', `${reason} must not open`);
      assert.equal(r.reason, reason);
      assert.ok(r.message.length > 0, `${reason} needs a sentence`);
    }
  });

  test('an unknown reason falls back to the claim that asserts least', () => {
    // A server that grew a new denial reason. The old client must not show a
    // blank page and must not show a success, so it says the weakest thing it
    // can honestly say: the link is not valid.
    const r = refusalFrom('teapot');
    assert.equal(r.kind, 'refused');
    assert.equal(r.reason, 'unknown_token', 'must not echo an unknown reason');
    assert.equal(r.message, refusalFrom('unknown_token').message);
  });

  test('an empty reason is a refusal too', () => {
    // A 403 with a body that is not JSON at all — an HTML page from a proxy in
    // front of the server. The client has an answer and must use it.
    const r = refusalFrom('');
    assert.equal(r.kind, 'refused');
    assert.equal(r.reason, 'unknown_token');
  });

  test('a reason that merely looks like a known one is not accepted', () => {
    // "expired_at" is not "expired". Mapping by prefix or substring is how a
    // client ends up saying the wrong thing about a live link.
    const r = refusalFrom('expired_at');
    assert.equal(r.reason, 'unknown_token');
  });
});

describe('a link state has a word', () => {
  test('revoked is not "expired"', () => {
    // Same reason as the denial: the owner needs to tell those apart to know
    // whether the link can be replaced or whether the sender is upset.
    assert.notEqual(stateLabel('revoked'), stateLabel('expired'));
  });

  test('all three states are distinct', () => {
    const labels = (['live', 'expired', 'revoked'] as const).map(stateLabel);
    assert.equal(new Set(labels).size, 3);
  });

  test('an unknown state does not throw', () => {
    // A client from before a new state existed. Throwing here would take the
    // whole management page down for one row.
    const label = stateLabel('quarantined');
    assert.equal(typeof label, 'string');
  });
});

describe('the request helpers', () => {
  // A `fetch` stub rather than a real server: what is under test is the
  // classification of a response, and a real server would make that depend on
  // network timing as well.

  // `fetch` is stubbed at the GLOBAL, because the transport in `client.ts` is
  // what calls it and that is the only module permitted to. The alternative --
  // injecting a transport -- is what `setTransport` already exists for, but
  // that only covers the GraphQL path; the REST verbs use the global directly,
  // so a test that stubs the transport instead of the global would be testing a
  // seam this code does not have.
  function stubFetch(status: number, body: unknown, ok = status < 400) {
    const calls: string[] = [];
    let lastBody = '';
    const original = globalThis.fetch;
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      calls.push(String(input));
      lastBody = String(init?.body ?? '');
      return {
        status,
        ok,
        json: async () => body
      } as Response;
    }) as typeof fetch;
    return {
      calls,
      get lastBody() {
        return lastBody;
      },
      restore: () => {
        globalThis.fetch = original;
      }
    };
  }

  test('a token with an awkward character is encoded into the path', async () => {
    // A token pasted with whitespace or a stray character must not become a
    // path that silently addresses a different route.
    const { createShare, openShare } = await import('../src/lib/api/share.js');
    const f = stubFetch(200, { target_id: 'o', can_download: false });
    try {
      await openShare('a b/c?d');
      assert.ok(
        f.calls[0].includes('%20') && f.calls[0].includes('%2F'),
        `expected encoding, got ${f.calls[0]}`
      );
      assert.ok(!f.calls[0].includes('/c'), 'the slash must be encoded');
    } finally {
      f.restore();
    }
    assert.equal(typeof createShare, 'function');
  });

  test('a 403 becomes a refusal, not a throw', async () => {
    const { openShare } = await import('../src/lib/api/share.js');
    const f = stubFetch(403, { error: 'expired' });
    try {
      const r = await openShare('x.y');
      assert.equal(r.kind, 'refused');
      assert.equal(r.kind === 'refused' && r.reason, 'expired');
    } finally {
      f.restore();
    }
  });

  test('a 500 throws, because "the link is dead" is a lie the server did not tell', async () => {
    // A refusal claims something about the LINK. A 500 says nothing about the
    // link at all, so rendering it as "this link has expired" sends the
    // recipient to their sender when the actual problem is on this side.
    const { openShare } = await import('../src/lib/api/share.js');
    const f = stubFetch(500, {});
    try {
      await assert.rejects(() => openShare('x.y'), /share: 500/);
    } finally {
      f.restore();
    }
  });

  test('a 403 with an unreadable body still refuses', async () => {
    const { openShare } = await import('../src/lib/api/share.js');
    const original = globalThis.fetch;
    globalThis.fetch = (async () => ({
      status: 403,
      ok: false,
      json: async () => {
        throw new Error('not json');
      }
    })) as typeof fetch;
    try {
      const r = await openShare('x.y');
      assert.equal(r.kind, 'refused');
      assert.equal(r.kind === 'refused' && r.reason, 'unknown_token');
    } finally {
      globalThis.fetch = original;
    }
  });

  test('opening a link reports whether the bytes are fetchable', async () => {
    // The client must not infer download rights from the fact that the link
    // opened: `view` opens fine and serves nothing, and a UI that showed a
    // download button would produce a 403 the user cannot explain.
    const { openShare } = await import('../src/lib/api/share.js');
    const f = stubFetch(200, {
      target_kind: 'object',
      target_id: 'obj-1',
      can_download: true,
      needs_password: false
    });
    try {
      const r = await openShare('x.y');
      assert.equal(r.kind, 'opened');
      if (r.kind === 'opened') {
        assert.equal(r.canDownload, true);
        assert.equal(r.targetId, 'obj-1');
      }
    } finally {
      f.restore();
    }
  });

});

describe('create carries the right field names', () => {
  test('camelCase in, snake_case on the wire', async () => {
    // Named separately from the block above because it is the assertion that
    // matters: a camelCase key sent to a `deny_unknown_fields` body is a 422,
    // so a rename here breaks link creation for every user with a confusing
    // error about a field they never typed.
    let sent = '';
    const original = globalThis.fetch;
    globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit) => {
      sent = String(init?.body ?? '');
      return {
        status: 201,
        ok: true,
        json: async () => ({ id: 'x', url: 'u', expires_at: 't' })
      } as Response;
    }) as typeof fetch;
    try {
      const { createShare } = await import('../src/lib/api/share.js');
      await createShare({
        targetKind: 'object',
        targetId: 'obj-1',
        scope: 'view_download',
        expiresInHours: 24,
        password: 'hunter2'
      });
      const body = JSON.parse(sent) as Record<string, unknown>;
      assert.equal(body.target_kind, 'object');
      assert.equal(body.target_id, 'obj-1');
      assert.equal(body.scope, 'view_download');
      assert.equal(body.expires_in_hours, 24);
      assert.equal(body.password, 'hunter2');
      assert.equal(body.targetKind, undefined, 'client names must not leak');
    } finally {
      globalThis.fetch = original;
    }
  });

  test('an omitted password is omitted, not sent as null', async () => {
    // The server treats a JSON null differently from an absent key on a
    // `deny_unknown_fields` body, and sending `password: null` is the kind of
    // thing that makes "no password" mean "the password is the string null".
    let sent = '';
    const original = globalThis.fetch;
    globalThis.fetch = (async (_input: RequestInfo | URL, init?: RequestInit) => {
      sent = String(init?.body ?? '');
      return { status: 201, ok: true, json: async () => ({ id: 'x' }) } as Response;
    }) as typeof fetch;
    try {
      const { createShare } = await import('../src/lib/api/share.js');
      await createShare({ targetKind: 'object', targetId: 'o', scope: 'view' });
      const body = JSON.parse(sent) as Record<string, unknown>;
      assert.equal('password' in body, false);
      assert.equal('expires_in_hours' in body, false);
    } finally {
      globalThis.fetch = original;
    }
  });

  test('a 422 throws with the server field name in the message', async () => {
    // The difference between a UI that says which field is wrong and one that
    // says "could not create link", which is a bug report rather than a fix.
    const original = globalThis.fetch;
    globalThis.fetch = (async () => ({
      status: 422,
      ok: false,
      json: async () => ({ error: 'expires_in_hours must be between 1 and 2160' })
    })) as typeof fetch;
    try {
      const { createShare } = await import('../src/lib/api/share.js');
      await assert.rejects(
        () => createShare({ targetKind: 'object', targetId: 'o', scope: 'view' }),
        /expires_in_hours/
      );
    } finally {
      globalThis.fetch = original;
    }
  });
});

mock.reset();
