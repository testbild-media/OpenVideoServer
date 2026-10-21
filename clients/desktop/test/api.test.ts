// The HTTP client: what it sends, and what it makes of what comes back.

import { describe, expect, it } from 'vitest';

import { Session } from '../src/main/session.js';
import { ApiFailure, ServerApi, serverOrigin, type FetchLike } from '../src/shared/api.js';
import type { IngestFiles } from '../src/shared/ingest.js';

/** Records requests and answers them from a queue. */
function recorder(answers: Array<() => Response>) {
  const calls: Array<{ url: string; init: RequestInit | undefined }> = [];
  const fetchImpl = async (url: string, init?: RequestInit) => {
    calls.push({ url, init });
    const next = answers.shift();
    return next ? next() : new Response(null, { status: 204 });
  };
  return { calls, fetchImpl };
}

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  });

describe('the server address an operator types', () => {
  it('takes a bare address and assumes the port ovsd listens on', () => {
    expect(serverOrigin('10.0.0.12')).toBe('http://10.0.0.12:7420');
  });

  it('takes a full URL, with or without a path', () => {
    expect(serverOrigin('http://ovs.local:7420/console')).toBe('http://ovs.local:7420');
    expect(serverOrigin(' https://ovs.example:8443/ ')).toBe('https://ovs.example:8443');
  });

  it('keeps a port that was typed', () => {
    expect(serverOrigin('ovs.local:9000')).toBe('http://ovs.local:9000');
  });

  it('refuses nothing at all', () => {
    expect(() => serverOrigin('   ')).toThrow(/address is required/);
  });
});

describe('the HTTP client', () => {
  it('signs in and carries the token afterwards', async () => {
    const { calls, fetchImpl } = recorder([
      () => json({ token: 'SESSION', account_id: 'A', expires_at: 1 }),
      () => new Response(null, { status: 204 }),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await api.logIn('operator', 'correct horse battery staple');
    expect(api.token).toBe('SESSION');

    await api.updateMaster({ dimmer: 0.5 });
    const headers = calls[1]!.init!.headers as Record<string, string>;
    expect(calls[1]!.url).toBe('http://server:7420/api/v1/live/master');
    expect(headers['authorization']).toBe('Bearer SESSION');
    expect(calls[1]!.init!.body).toBe(JSON.stringify({ dimmer: 0.5 }));
  });

  it('sends no authorization before signing in', async () => {
    const { calls, fetchImpl } = recorder([() => json({ configured: false })]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await api.capabilities();

    const headers = calls[0]!.init!.headers as Record<string, string>;
    expect(headers['authorization']).toBeUndefined();
  });

  it('forgets the token even when signing out fails', async () => {
    // The server may have expired the session already. A client that kept
    // using the token would keep being refused and never say why.
    const { fetchImpl } = recorder([() => json({ code: 'unauthenticated', message: 'no' }, 401)]);
    const api = new ServerApi('http://server:7420', fetchImpl);
    api.setToken('SESSION');

    await expect(api.logOut()).rejects.toBeInstanceOf(ApiFailure);
    expect(api.token).toBeNull();
  });

  it('reports the server error the operator needs to read', async () => {
    const { fetchImpl } = recorder([
      () =>
        json(
          {
            code: 'invalid_show',
            message: 'a layer refers to a slot that does not exist',
            details: [{ path: 'layers[2].slots[0].index', message: 'duplicated' }],
          },
          422,
        ),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    const failure = await api.shows().catch((error: unknown) => error);

    expect(failure).toBeInstanceOf(ApiFailure);
    const typed = failure as ApiFailure;
    expect(typed.code).toBe('invalid_show');
    expect(typed.message).toContain('slot that does not exist');
    expect(typed.details[0]!.path).toBe('layers[2].slots[0].index');
    expect(typed.status).toBe(422);
  });

  it('copes when something in the way answers with a page instead', async () => {
    // "Unexpected token < in JSON" is not something to put in front of an
    // operator during a show.
    const { fetchImpl } = recorder([
      () => new Response('<html>502 Bad Gateway</html>', { status: 502, statusText: 'Bad Gateway' }),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    const failure = (await api.status().catch((error: unknown) => error)) as ApiFailure;

    expect(failure.code).toBe('http_502');
    // What the thing in the way said, and which call it was said about: a
    // refusal with no call named is four words nobody can act on.
    expect(failure.message).toBe('Bad Gateway — GET /server/status');
  });

  it('says a route this server does not publish is that, not a missing thing', async () => {
    // The refusal an operator meets when their client is newer than the server
    // they are talking to. "Not Found" is the same four words whether a show
    // is missing or the route was never built (owner, 2026-09-23).
    const { fetchImpl } = recorder([
      () => new Response('', { status: 404, statusText: 'Not Found' }),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    const failure = (await api
      .captureLiveState('SHOW')
      .catch((error: unknown) => error)) as ApiFailure;

    expect(failure.status).toBe(404);
    expect(failure.message).toContain('POST /shows/SHOW/capture');
    expect(failure.message).toContain('older build');
  });

  it('keeps the server’s own words when the server had some', async () => {
    const { fetchImpl } = recorder([
      () =>
        new Response(JSON.stringify({ code: 'not_found', message: 'no show with id X' }), {
          status: 404,
        }),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    const failure = (await api
      .captureLiveState('X')
      .catch((error: unknown) => error)) as ApiFailure;

    expect(failure.message).toBe('no show with id X');
  });

  it('knows an expired session from any other refusal', async () => {
    const { fetchImpl } = recorder([() => json({ code: 'unauthenticated', message: 'no' }, 401)]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    const failure = (await api.status().catch((error: unknown) => error)) as ApiFailure;

    expect(failure.isUnauthenticated).toBe(true);
  });

  it('accepts an empty body from an endpoint that has nothing to say', async () => {
    const { fetchImpl } = recorder([() => new Response(null, { status: 204 })]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await expect(api.openShow('SHOW')).resolves.toBeUndefined();
  });

  it('fills a slot through the route built for doing it with a show running', async () => {
    // Not `saveShow`: storing a show the server has open reloads it
    // there, which would put every layer back to its opening slot because
    // somebody filled an empty pad (`slot_routes`).
    const { calls, fetchImpl } = recorder([
      () => new Response(JSON.stringify({ updated_at: 7 }), { status: 200 }),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    const answer = await api.assignSlot('SHOW', 'LAYER', 5, {
      kind: 'media',
      media_item_id: 'MEDIA1',
    });

    expect(calls[0]!.url).toBe(
      'http://server:7420/api/v1/shows/SHOW/layers/LAYER/slots/5',
    );
    expect(calls[0]!.init!.method).toBe('PUT');
    expect(calls[0]!.init!.body).toBe(
      JSON.stringify({ source: { kind: 'media', media_item_id: 'MEDIA1' } }),
    );
    expect(answer.updated_at).toBe(7);
  });

  it('empties a slot with a delete to the same path', async () => {
    const { calls, fetchImpl } = recorder([
      () => new Response(JSON.stringify({ updated_at: 8 }), { status: 200 }),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await api.clearSlot('SHOW', 'LAYER', 5);

    expect(calls[0]!.url).toBe(
      'http://server:7420/api/v1/shows/SHOW/layers/LAYER/slots/5',
    );
    expect(calls[0]!.init!.method).toBe('DELETE');
  });

  it('stacks an effect on a slot, and on a layer, through the chain each one has', async () => {
    const { calls, fetchImpl } = recorder([
      () => new Response(JSON.stringify({ updated_at: 1, effect_instance_id: 'E1' }), { status: 200 }),
      () => new Response(JSON.stringify({ updated_at: 2, effect_instance_id: 'E2' }), { status: 200 }),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await api.addEffect('SHOW', 'LAYER', 3, 'ovs.blur', 1);
    await api.addEffect('SHOW', 'LAYER', null, 'ovs.blur', 1);

    expect(calls[0]!.url).toBe(
      'http://server:7420/api/v1/shows/SHOW/layers/LAYER/slots/3/effects',
    );
    expect(calls[1]!.url).toBe(
      'http://server:7420/api/v1/shows/SHOW/layers/LAYER/effects',
    );
    // No parameters. Each has a default in the effect's own manifest, and a
    // copy written into the document stops following it.
    expect(calls[0]!.init!.body).toBe(JSON.stringify({ type_id: 'ovs.blur', version: 1 }));
  });

  it('takes an effect off the chain it is on', async () => {
    const { calls, fetchImpl } = recorder([
      () => new Response(JSON.stringify({ updated_at: 3, effect_instance_id: 'E1' }), { status: 200 }),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await api.removeEffect('SHOW', 'LAYER', 3, 'E1');

    expect(calls[0]!.url).toBe(
      'http://server:7420/api/v1/shows/SHOW/layers/LAYER/slots/3/effects/E1',
    );
    expect(calls[0]!.init!.method).toBe('DELETE');
  });

  it('asks for a bank count on the layer rather than working it out from the slots', async () => {
    const { calls, fetchImpl } = recorder([
      () => new Response(JSON.stringify({ updated_at: 4 }), { status: 200 }),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await api.setBanks('SHOW', 'LAYER', 3);

    expect(calls[0]!.url).toBe('http://server:7420/api/v1/shows/SHOW/layers/LAYER/banks');
    expect(calls[0]!.init!.method).toBe('PUT');
    expect(calls[0]!.init!.body).toBe(JSON.stringify({ banks: 3 }));
  });

  it('moves a slot by naming the pad it leaves in the path and the one it goes to in the body', async () => {
    const { calls, fetchImpl } = recorder([
      () => new Response(JSON.stringify({ updated_at: 4, swapped: false }), { status: 200 }),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    const answer = await api.moveSlot('SHOW', 'LAYER', 1, 10);

    expect(calls[0]!.url).toBe('http://server:7420/api/v1/shows/SHOW/layers/LAYER/slots/1/move');
    expect(calls[0]!.init!.method).toBe('POST');
    expect(calls[0]!.init!.body).toBe(JSON.stringify({ to: 10 }));
    expect(answer.swapped).toBe(false);
  });

  it('names the layer a slot goes to only when it is another one', async () => {
    const { calls, fetchImpl } = recorder([
      () => new Response(JSON.stringify({ updated_at: 4, swapped: true }), { status: 200 }),
      () => new Response(JSON.stringify({ updated_at: 5, swapped: false }), { status: 200 }),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await api.moveSlot('SHOW', 'LAYER', 1, 10, 'OTHER');
    await api.moveSlot('SHOW', 'LAYER', 1, 10, 'LAYER');

    expect(calls[0]!.init!.body).toBe(JSON.stringify({ to: 10, to_layer: 'OTHER' }));
    expect(calls[1]!.init!.body).toBe(JSON.stringify({ to: 10 }));
  });

  it('escapes identifiers rather than pasting them into a path', async () => {
    const { calls, fetchImpl } = recorder([() => new Response(null, { status: 204 })]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await api.selectSlot('a layer/../..', 3);

    expect(calls[0]!.url).toBe('http://server:7420/api/v1/live/layers/a%20layer%2F..%2F../slot');
  });

  it('sends a live effect value to the live route for that instance', async () => {
    const { calls, fetchImpl } = recorder([() => new Response(null, { status: 204 })]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await api.updateEffect('EFFECT1', { overrides: { radius_px: { type: 'float', value: 40 } } });

    expect(calls[0]!.url).toBe('http://server:7420/api/v1/live/effects/EFFECT1');
    expect(calls[0]!.init!.method).toBe('PUT');
    expect(calls[0]!.init!.body).toBe(
      JSON.stringify({ overrides: { radius_px: { type: 'float', value: 40 } } }),
    );
  });

  it('puts an effect back by sending a reset and no values at all', async () => {
    // The configured values would look the same until somebody edited the
    // show, and then the effect would go on running what this client sent.
    const { calls, fetchImpl } = recorder([() => new Response(null, { status: 204 })]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await api.updateEffect('EFFECT1', { reset: true });

    expect(calls[0]!.init!.body).toBe(JSON.stringify({ reset: true }));
  });

  it('reports the manifest’s own words when a live value is refused', async () => {
    // `invalid_parameter` quotes the range the manifest declares, and the
    // operator is shown that rather than a range this client made up.
    const { fetchImpl } = recorder([
      () =>
        json(
          { code: 'invalid_parameter', message: 'radius_px must be a number between 0 and 64' },
          400,
        ),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    const failure = (await api
      .updateEffect('EFFECT1', { overrides: { radius_px: { type: 'float', value: 4_000 } } })
      .catch((error: unknown) => error)) as ApiFailure;

    expect(failure.code).toBe('invalid_parameter');
    expect(failure.message).toBe('radius_px must be a number between 0 and 64');
  });

  it('asks for the catalogues the editor draws its controls from', async () => {
    const { calls, fetchImpl } = recorder([
      () => json({ effects: [] }),
      () => json({ generators: [] }),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await api.effects();
    await api.generators();

    expect(calls.map((call) => call.url)).toEqual([
      'http://server:7420/api/v1/effects',
      'http://server:7420/api/v1/generators',
    ]);
  });

  it('asks the server where a fixture fits, rather than working it out', async () => {
    // The endpoint knows what is already patched, which is the whole question.
    const { calls, fetchImpl } = recorder([() => json({ universe: 3, start_address: 97 })]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await expect(api.suggestAddress('SHOW', 3, 14)).resolves.toEqual({
      universe: 3,
      start_address: 97,
    });
    expect(calls[0]!.url).toBe(
      'http://server:7420/api/v1/shows/SHOW/patch/address?universe=3&footprint=14',
    );
  });

  it('says which fixture is being re-addressed, so it does not block itself', async () => {
    // A fixture does not collide with where it already is, and without this the
    // server could not suggest the address a fixture already has.
    const { calls, fetchImpl } = recorder([() => json({ universe: 1, start_address: 1 })]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await api.suggestAddress('SHOW', 1, 10, '01HQ0000000000000000000003');

    expect(calls[0]!.url).toBe(
      'http://server:7420/api/v1/shows/SHOW/patch/address?universe=1&footprint=10&moving=01HQ0000000000000000000003',
    );
  });

  it('asks for the fixture personalities the patch page draws itself from', async () => {
    const { calls, fetchImpl } = recorder([() => json({ fixtures: [] })]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await api.fixtures();

    expect(calls[0]!.url).toBe('http://server:7420/api/v1/fixtures');
  });

  it('saves a show whole, and reports when the server says it wrote it', async () => {
    // There is no partial-update endpoint. The timestamp is the server's: an
    // editor that stamped the document itself would put a number in it that
    // nothing measured.
    const { calls, fetchImpl } = recorder([() => json({ updated_at: 1_800_000_000_000 })]);
    const api = new ServerApi('http://server:7420', fetchImpl);
    const show = { id: 'SHOW', name: 'Nachtfahrt' } as never;

    await expect(api.saveShow(show)).resolves.toEqual({ updated_at: 1_800_000_000_000 });
    expect(calls[0]!.url).toBe('http://server:7420/api/v1/shows/SHOW');
    expect(calls[0]!.init!.method).toBe('PUT');
    expect(calls[0]!.init!.body).toBe(JSON.stringify({ show }));
  });

  it('points the realtime socket at the same server', () => {
    expect(new ServerApi('http://server:7420', fetch).realtimeUrl()).toBe(
      'ws://server:7420/realtime',
    );
    expect(new ServerApi('https://server:8443', fetch).realtimeUrl()).toBe(
      'wss://server:8443/realtime',
    );
  });
});

/**
 * A filesystem nothing in this file uses.
 *
 * The ingest queue needs one to exist; any call would be a test reaching
 * somewhere it did not mean to, and says so.
 */
const noFiles: IngestFiles = {
  probe: () => Promise.reject(new Error('no file was meant to be read here')),
  size: () => Promise.reject(new Error('no file was meant to be read here')),
  hash: () => Promise.reject(new Error('no file was meant to be read here')),
  read: () => Promise.reject(new Error('no file was meant to be read here')),
  prepare: () => Promise.reject(new Error('no file was meant to be prepared here')),
  discard: () => Promise.reject(new Error('no file was meant to be removed here')),
  encoders: () => Promise.reject(new Error('no encoder was meant to be asked for here')),
  keyframeInterval: () => Promise.reject(new Error('no file was meant to be read here')),
};

/** A session signed in to a server, with a socket that carries nothing. */
async function signedIn(fetchImpl: FetchLike): Promise<Session> {
  const session = new Session(
    {
      fetch: fetchImpl,
      open: (_url, handlers) => ({ send: () => undefined, close: () => handlers.closed('stopped') }),
      clientVersion: '0.0.1-test',
      files: noFiles,
    },
    {
      status: () => undefined,
      view: () => undefined,
      telemetry: () => undefined,
      ndiSources: () => undefined,
      connectors: () => undefined,
      fault: () => undefined,
      ingest: () => undefined,
    },
  );
  await session.connect('10.0.0.12', 'operator', 'correct horse battery staple');
  return session;
}

describe('a live effect change, from the operator’s control to the wire', () => {
  it('is sent as a live change and never as a show edit', async () => {
    const { calls, fetchImpl } = recorder([
      () => json({ token: 'SESSION', account_id: 'A', expires_at: 1 }),
    ]);
    const session = await signedIn(fetchImpl);

    await expect(
      session.run({
        do: 'set_effect',
        effectInstanceId: 'EFFECT1',
        overrides: { radius_px: { type: 'float', value: 40 } },
      }),
    ).resolves.toEqual({ ok: true });

    const sent = calls[calls.length - 1]!;
    expect(sent.url).toBe('http://10.0.0.12:7420/api/v1/live/effects/EFFECT1');
    expect(sent.init!.method).toBe('PUT');
    expect(sent.init!.body).toBe(
      JSON.stringify({ overrides: { radius_px: { type: 'float', value: 40 } } }),
    );
    // The show is not touched, saved or even asked for on the way. A live
    // value sits on top of what the document configured and never reaches it.
    expect(calls.some((call) => call.url.includes('/shows'))).toBe(false);

    session.disconnect();
  });

  it('puts an effect back with a reset rather than with the configured values', async () => {
    const { calls, fetchImpl } = recorder([
      () => json({ token: 'SESSION', account_id: 'A', expires_at: 1 }),
    ]);
    const session = await signedIn(fetchImpl);

    await session.run({ do: 'set_effect', effectInstanceId: 'EFFECT1', reset: true });

    // `reset` and nothing else: the two fields are a map and a bool with serde
    // defaults on the far side, so an absent field is the only way to say "do
    // not", and a null would not deserialise at all.
    expect(calls[calls.length - 1]!.init!.body).toBe(JSON.stringify({ reset: true }));

    session.disconnect();
  });
});

describe('the media endpoints', () => {
  it('sends a chunk as bytes at the offset it belongs at', async () => {
    // Not as JSON: a chunk of a ProRes file encoded as JSON would be a third
    // larger and would arrive as something the server has to decode first.
    const { calls, fetchImpl } = recorder([
      () =>
        json({
          upload_id: 'UPLOAD',
          media_item_id: 'ITEM',
          byte_size: 16,
          received_bytes: 12,
          chunk_size: 8,
        }),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);
    api.setToken('SESSION');

    const session = await api.appendChunk('UPLOAD', 4, new Uint8Array([1, 2, 3, 4]));

    expect(calls[0]!.url).toBe('http://server:7420/api/v1/media/uploads/UPLOAD?offset=4');
    expect(calls[0]!.init!.method).toBe('PUT');
    const headers = calls[0]!.init!.headers as Record<string, string>;
    expect(headers['content-type']).toBe('application/octet-stream');
    expect(headers['authorization']).toBe('Bearer SESSION');
    // Where the next chunk goes, and where a resume starts.
    expect(session.received_bytes).toBe(12);
  });

  it('treats an item with no picture as an item with no picture', async () => {
    // 404 is ordinary here: an item registered before thumbnails existed has
    // none, and one nothing could be read from has none either.
    const { fetchImpl } = recorder([
      () => json({ code: 'not_found', message: 'there is no picture of this item' }, 404),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await expect(api.mediaThumbnail('ITEM')).resolves.toBeNull();
  });

  it('passes on a refusal about a picture that is not a missing one', async () => {
    // A library that drew a name for an item whose picture the server refused
    // would be hiding a refusal.
    const { fetchImpl } = recorder([
      () => json({ code: 'unauthorised', message: 'no session' }, 401),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await expect(api.mediaThumbnail('ITEM')).rejects.toBeInstanceOf(ApiFailure);
  });

  it('asks which shows use an item, and deletes by identifier', async () => {
    const { calls, fetchImpl } = recorder([
      () => json({ used_by_shows: ['Spring Tour'] }),
      () => new Response(null, { status: 204 }),
    ]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await expect(api.mediaUsage('ITEM')).resolves.toEqual({ used_by_shows: ['Spring Tour'] });
    await api.deleteMedia('ITEM');

    expect(calls.map((call) => call.url)).toEqual([
      'http://server:7420/api/v1/media/ITEM/usage',
      'http://server:7420/api/v1/media/ITEM',
    ]);
    expect(calls[1]!.init!.method).toBe('DELETE');
  });

  it('asks for a ticket per surface, at the route the server mints them on', async () => {
    const { calls, fetchImpl } = recorder([() => json({ ticket: 'TICKET', expires_at: 1 })]);
    const api = new ServerApi('http://server:7420', fetchImpl);

    await expect(api.streamTicket('composition')).resolves.toEqual({
      ticket: 'TICKET',
      expires_at: 1,
    });
    expect(calls[0]!.url).toBe('http://server:7420/api/v1/preview/stream/composition/ticket');
    expect(calls[0]!.init!.method).toBe('POST');
  });
});
