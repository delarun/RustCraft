// Microsoft account sign-in: the OAuth 2.0 device authorization grant. The
// user opens microsoft.com/link (on any device) and enters a short code while
// we poll the token endpoint. It works for public (desktop) app registrations,
// which cannot use a redirect to this page; see config.ts.

import { MSA_AUTHORITY, MSA_SCOPE } from './config';
import { AuthError, num, postForm, str } from './http';

export interface DeviceCode {
  userCode: string;
  deviceCode: string;
  verificationUri: string;
  /** Epoch ms. */
  expiresAt: number;
  /** Seconds between polls. */
  interval: number;
  message: string;
}

export interface MsaTokens {
  accessToken: string;
  refreshToken: string;
  /** Epoch ms. */
  expiresAt: number;
}

export async function startDeviceCode(clientId: string, signal?: AbortSignal): Promise<DeviceCode> {
  const { status, body } = await postForm(`${MSA_AUTHORITY}/devicecode`, { client_id: clientId, scope: MSA_SCOPE }, signal);
  if (status !== 200 || !body.device_code) throw msaError(body, status);
  return {
    userCode: str(body.user_code),
    deviceCode: str(body.device_code),
    verificationUri: str(body.verification_uri) || 'https://www.microsoft.com/link',
    expiresAt: Date.now() + num(body.expires_in) * 1000,
    interval: num(body.interval) || 5,
    message: str(body.message),
  };
}

const sleep = (ms: number, signal?: AbortSignal) =>
  new Promise<void>((resolve, reject) => {
    const timer = setTimeout(resolve, ms);
    signal?.addEventListener('abort', () => {
      clearTimeout(timer);
      reject(signal.reason);
    }, { once: true });
  });

/** Polls until the user has signed in (or declined, or the code expired). */
export async function pollDeviceCode(
  clientId: string,
  code: DeviceCode,
  signal?: AbortSignal,
  wait: (ms: number, signal?: AbortSignal) => Promise<void> = sleep,
): Promise<MsaTokens> {
  let interval = code.interval;
  for (;;) {
    await wait(interval * 1000, signal);
    signal?.throwIfAborted();
    const { status, body } = await postForm(
      `${MSA_AUTHORITY}/token`,
      { client_id: clientId, grant_type: 'urn:ietf:params:oauth:grant-type:device_code', device_code: code.deviceCode },
      signal,
    );
    if (status === 200 && body.access_token) return tokens(body);
    const error = str(body.error);
    if (error === 'authorization_pending') continue;
    if (error === 'slow_down') {
      interval += 5;
      continue;
    }
    if (error === 'authorization_declined') throw new AuthError('msaDeclined', [], error, status);
    if (error === 'expired_token' || error === 'bad_verification_code' || Date.now() > code.expiresAt) {
      throw new AuthError('msaExpired', [], 'expired_token', status);
    }
    throw msaError(body, status);
  }
}

export async function refreshMsa(clientId: string, refreshToken: string): Promise<MsaTokens> {
  const { status, body } = await postForm(`${MSA_AUTHORITY}/token`, {
    client_id: clientId,
    grant_type: 'refresh_token',
    refresh_token: refreshToken,
    scope: MSA_SCOPE,
  });
  if (status !== 200 || !body.access_token) {
    const error = msaError(body, status);
    // invalid_grant: revoked, expired or issued to another client — sign in again.
    throw str(body.error) === 'invalid_grant' ? new AuthError('msaRelogin', [], 'relogin', status) : error;
  }
  return tokens(body, refreshToken);
}

function tokens(body: Record<string, unknown>, previousRefresh = ''): MsaTokens {
  return {
    accessToken: str(body.access_token),
    refreshToken: str(body.refresh_token) || previousRefresh,
    expiresAt: Date.now() + num(body.expires_in) * 1000,
  };
}

function msaError(body: Record<string, unknown>, status: number): AuthError {
  const error = str(body.error) || `http_${status}`;
  const description = str(body.error_description).split('\r\n')[0] || str(body.text).slice(0, 200);
  if (/AADSTS700016/.test(description)) {
    return new AuthError('msaAppNotFound', [], error, status);
  }
  if (/AADSTS70002|AADSTS7000218/.test(description)) {
    return new AuthError('msaPublicFlows', [], error, status);
  }
  return new AuthError('msaError', [description || error], error, status);
}
