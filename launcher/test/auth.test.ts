import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { freshAccount, licenseProblem, needsRenewal, signInWithMsa, type Account } from '../src/auth/account';
import { clientId, DEFAULT_CLIENT_ID } from '../src/auth/config';
import { AuthError } from '../src/auth/http';
import { dashedUuid } from '../src/auth/minecraft';
import { pollDeviceCode, refreshMsa, startDeviceCode, type DeviceCode } from '../src/auth/msa';

type Handler = (request: { url: string; method: string; headers: Headers; body: string }) => { status?: number; body?: unknown };

/** A fake fetch routing by URL; records every request. */
function mockFetch(routes: Record<string, Handler | Handler[]>) {
  const calls: { url: string; method: string; headers: Headers; body: string }[] = [];
  const counters = new Map<string, number>();
  const fetch = vi.fn(async (input: RequestInfo | URL, init: RequestInit = {}) => {
    const url = String(input);
    const request = { url, method: init.method ?? 'GET', headers: new Headers(init.headers), body: init.body ? String(init.body) : '' };
    calls.push(request);
    const key = Object.keys(routes).find((prefix) => url.startsWith(prefix));
    if (!key) return new Response('not mocked', { status: 599 });
    const route = routes[key]!;
    const n = counters.get(key) ?? 0;
    counters.set(key, n + 1);
    const handler = Array.isArray(route) ? route[Math.min(n, route.length - 1)]! : route;
    const { status = 200, body = {} } = handler(request);
    return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
  });
  vi.stubGlobal('fetch', fetch);
  return calls;
}

const MSA = '/proxy/login.microsoftonline.com/consumers/oauth2/v2.0';
const XBL = '/proxy/user.auth.xboxlive.com/user/authenticate';
const XSTS = '/proxy/xsts.auth.xboxlive.com/xsts/authorize';
const MC = '/proxy/api.minecraftservices.com';

const xboxReply = (token: string) => () => ({
  body: { Token: token, NotAfter: '2030-01-01T00:00:00Z', DisplayClaims: { xui: [{ uhs: 'UHS123' }] } },
});

function happyChain(overrides: Record<string, Handler | Handler[]> = {}) {
  return mockFetch({
    [XBL]: xboxReply('xbl-token'),
    [XSTS]: xboxReply('xsts-token'),
    [`${MC}/authentication/login_with_xbox`]: () => ({ body: { access_token: 'mc-token', expires_in: 86400 } }),
    [`${MC}/entitlements/mcstore`]: () => ({ body: { items: [{ name: 'product_minecraft' }, { name: 'game_minecraft' }] } }),
    [`${MC}/minecraft/profile`]: () => ({ body: { id: '069a79f444e94726a5befca90e38aaf5', name: 'Notch', skins: [] } }),
    ...overrides,
  });
}

const msaTokens = { accessToken: 'msa-access', refreshToken: 'msa-refresh', expiresAt: Date.now() + 3600_000 };

beforeEach(() => vi.useRealTimers());
afterEach(() => vi.unstubAllGlobals());

describe('config', () => {
  it('uses VITE_MS_CLIENT_ID over the built-in RustCraft client ID', () => {
    vi.stubEnv('VITE_MS_CLIENT_ID', '');
    expect(clientId()).toBe(DEFAULT_CLIENT_ID);
    expect(DEFAULT_CLIENT_ID).toBe('41f23721-33c5-4533-923d-f981d3f65040');
    vi.stubEnv('VITE_MS_CLIENT_ID', ' 11111111-2222-3333-4444-555555555555 ');
    expect(clientId()).toBe('11111111-2222-3333-4444-555555555555');
    vi.unstubAllEnvs();
  });
});

describe('device code flow', () => {
  it('starts with the Xbox Live scope through the proxy', async () => {
    const calls = mockFetch({
      [`${MSA}/devicecode`]: () => ({
        body: { user_code: 'ABCD1234', device_code: 'dc', verification_uri: 'https://www.microsoft.com/link', expires_in: 900, interval: 5, message: 'm' },
      }),
    });
    const code = await startDeviceCode('client');
    expect(code).toMatchObject({ userCode: 'ABCD1234', deviceCode: 'dc', verificationUri: 'https://www.microsoft.com/link', interval: 5 });
    const params = new URLSearchParams(calls[0]!.body);
    expect(calls[0]!.method).toBe('POST');
    expect(params.get('client_id')).toBe('client');
    expect(params.get('scope')).toBe('XboxLive.signin offline_access');
  });

  const code: DeviceCode = { userCode: 'U', deviceCode: 'dc', verificationUri: '', expiresAt: Date.now() + 900_000, interval: 5, message: '' };
  const noWait = vi.fn(async () => {});

  it('polls through pending and slow_down until tokens arrive', async () => {
    const calls = mockFetch({
      [`${MSA}/token`]: [
        () => ({ status: 400, body: { error: 'authorization_pending' } }),
        () => ({ status: 400, body: { error: 'slow_down' } }),
        () => ({ body: { access_token: 'a', refresh_token: 'r', expires_in: 3600 } }),
      ],
    });
    noWait.mockClear();
    const tokens = await pollDeviceCode('client', code, undefined, noWait);
    expect(tokens).toMatchObject({ accessToken: 'a', refreshToken: 'r' });
    expect(calls).toHaveLength(3);
    expect(new URLSearchParams(calls[0]!.body).get('grant_type')).toBe('urn:ietf:params:oauth:grant-type:device_code');
    // slow_down adds five seconds to the interval.
    expect(noWait.mock.calls.map((c) => (c as unknown[])[0])).toEqual([5000, 5000, 10000]);
  });

  it('reports a declined or expired code', async () => {
    mockFetch({ [`${MSA}/token`]: () => ({ status: 400, body: { error: 'authorization_declined' } }) });
    await expect(pollDeviceCode('client', code, undefined, noWait)).rejects.toMatchObject({ code: 'authorization_declined' });
    mockFetch({ [`${MSA}/token`]: () => ({ status: 400, body: { error: 'expired_token' } }) });
    await expect(pollDeviceCode('client', code, undefined, noWait)).rejects.toMatchObject({ code: 'expired_token' });
  });

  it('refreshes, keeping the old refresh token when none is returned', async () => {
    const calls = mockFetch({ [`${MSA}/token`]: () => ({ body: { access_token: 'a2', expires_in: 3600 } }) });
    const tokens = await refreshMsa('client', 'old-refresh');
    expect(tokens).toMatchObject({ accessToken: 'a2', refreshToken: 'old-refresh' });
    expect(new URLSearchParams(calls[0]!.body).get('grant_type')).toBe('refresh_token');
  });

  it('asks to sign in again when the refresh token is rejected', async () => {
    mockFetch({ [`${MSA}/token`]: () => ({ status: 400, body: { error: 'invalid_grant', error_description: 'AADSTS70000: expired' } }) });
    await expect(refreshMsa('client', 'r')).rejects.toMatchObject({ code: 'relogin' });
  });
});

describe('Xbox → Minecraft chain', () => {
  it('signs in, checks the license and reads the profile', async () => {
    const calls = happyChain();
    const account = await signInWithMsa('client', msaTokens);
    expect(account).toMatchObject({
      clientId: 'client',
      minecraft: { accessToken: 'mc-token' },
      profile: { id: '069a79f444e94726a5befca90e38aaf5', name: 'Notch' },
      licensed: true,
    });
    expect(licenseProblem(account)).toBeNull();

    const xbl = JSON.parse(calls[0]!.body);
    expect(xbl.Properties.RpsTicket).toBe('d=msa-access');
    expect(xbl.RelyingParty).toBe('http://auth.xboxlive.com');
    const xsts = JSON.parse(calls[1]!.body);
    expect(xsts.Properties.UserTokens).toEqual(['xbl-token']);
    expect(xsts.RelyingParty).toBe('rp://api.minecraftservices.com/');
    expect(JSON.parse(calls[2]!.body).identityToken).toBe('XBL3.0 x=UHS123;xsts-token');
    expect(calls[3]!.headers.get('Authorization')).toBe('Bearer mc-token');
    expect(calls[4]!.url).toBe(`${MC}/minecraft/profile`);
  });

  it('refuses accounts without the game', async () => {
    happyChain({
      [`${MC}/entitlements/mcstore`]: () => ({ body: { items: [] } }),
      [`${MC}/minecraft/profile`]: () => ({ status: 404, body: { error: 'NOT_FOUND' } }),
    });
    const account = await signInWithMsa('client', msaTokens);
    expect(account.licensed).toBe(false);
    expect(licenseProblem(account)).toBe('noLicense');
  });

  it('refuses owners without a profile yet', async () => {
    happyChain({ [`${MC}/minecraft/profile`]: () => ({ status: 404, body: {} }) });
    const account = await signInWithMsa('client', msaTokens);
    expect(account.licensed).toBe(false);
    expect(licenseProblem(account)).toBe('noProfile');
  });

  it('explains XSTS errors', async () => {
    happyChain({ [XSTS]: () => ({ status: 401, body: { XErr: 2148916233 } }) });
    await expect(signInWithMsa('client', msaTokens)).rejects.toMatchObject({ key: 'xstsNoXbox', code: 'xsts_2148916233' });
    happyChain({ [XSTS]: () => ({ status: 401, body: { XErr: 1 } }) });
    await expect(signInWithMsa('client', msaTokens)).rejects.toMatchObject({ key: 'xstsFailed', args: [401, '1'] });
  });

  it('explains an Azure app not approved for the Minecraft API', async () => {
    happyChain({ [`${MC}/authentication/login_with_xbox`]: () => ({ status: 403, body: { errorMessage: 'Invalid app registration' } }) });
    const error = await signInWithMsa('client', msaTokens).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(AuthError);
    expect(error).toMatchObject({ code: 'app_not_approved', status: 403, key: 'mcNotApproved' });
  });
});

describe('renewal', () => {
  const account = (minecraftExpiresAt: number, msaExpiresAt: number): Account => ({
    clientId: 'client',
    msa: { accessToken: 'old-msa', refreshToken: 'r', expiresAt: msaExpiresAt },
    minecraft: { accessToken: 'old-mc', expiresAt: minecraftExpiresAt },
    xuid: '0',
    entitlements: ['game_minecraft'],
    profile: { id: '069a79f444e94726a5befca90e38aaf5', name: 'Notch' },
    licensed: true,
    checkedAt: 0,
  });

  it('keeps a valid token', async () => {
    const calls = mockFetch({});
    const current = account(Date.now() + 3600_000, 0);
    expect(needsRenewal(current)).toBe(false);
    expect(await freshAccount(current)).toBe(current);
    expect(calls).toHaveLength(0);
  });

  it('refreshes the Microsoft token, then reruns the chain', async () => {
    const calls = happyChain({ [`${MSA}/token`]: () => ({ body: { access_token: 'new-msa', refresh_token: 'r2', expires_in: 3600 } }) });
    const renewed = await freshAccount(account(Date.now() - 1, Date.now() - 1));
    expect(renewed.minecraft.accessToken).toBe('mc-token');
    expect(renewed.msa).toMatchObject({ accessToken: 'new-msa', refreshToken: 'r2' });
    expect(JSON.parse(calls[1]!.body).Properties.RpsTicket).toBe('d=new-msa');
  });
});

describe('uuid', () => {
  it('adds dashes', () => {
    expect(dashedUuid('069a79f444e94726a5befca90e38aaf5')).toBe('069a79f4-44e9-4726-a5be-fca90e38aaf5');
  });
});
