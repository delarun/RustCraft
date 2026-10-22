// Minecraft services: the XSTS token becomes a Minecraft access token; with it
// we read the account's entitlements (does it own Java Edition?) and profile.

import { AuthError, getJson, num, postJson, str } from './http';

const SERVICES = 'https://api.minecraftservices.com';

export interface MinecraftToken {
  accessToken: string;
  /** Epoch ms. */
  expiresAt: number;
}

export interface Profile {
  /** UUID without dashes, as the services return it. */
  id: string;
  name: string;
  skinUrl?: string;
}

/** Entitlements that grant Minecraft: Java Edition. */
export const GAME_ENTITLEMENTS = ['game_minecraft', 'product_minecraft'];

export async function loginWithXbox(userHash: string, xstsToken: string): Promise<MinecraftToken> {
  const { status, body } = await postJson(`${SERVICES}/authentication/login_with_xbox`, {
    identityToken: `XBL3.0 x=${userHash};${xstsToken}`,
  });
  if (status === 200 && body.access_token) {
    return { accessToken: str(body.access_token), expiresAt: Date.now() + num(body.expires_in) * 1000 };
  }
  const message = `${str(body.errorMessage) || str(body.error) || ''}`;
  if (status === 403 || /Invalid app registration/i.test(message)) {
    // New Azure apps get 403 here until Mojang approves them for the Minecraft API.
    throw new AuthError('mcNotApproved', [], 'app_not_approved', status);
  }
  if (status === 429) throw new AuthError('mcRateLimited', [], 'rate_limited', status);
  throw new AuthError('mcLoginFailed', [status, message], 'minecraft_login', status);
}

export async function entitlements(accessToken: string): Promise<string[]> {
  const { status, body } = await getJson(`${SERVICES}/entitlements/mcstore`, accessToken);
  if (status !== 200) throw new AuthError('entitlementsFailed', [status], 'entitlements', status);
  const items = Array.isArray(body.items) ? (body.items as { name?: unknown }[]) : [];
  return items.map((item) => str(item.name)).filter(Boolean);
}

/** The Java Edition profile; null when the account has none (no game, or no name chosen yet). */
export async function profile(accessToken: string): Promise<Profile | null> {
  const { status, body } = await getJson(`${SERVICES}/minecraft/profile`, accessToken);
  if (status === 404) return null;
  if (status !== 200 || !body.id) throw new AuthError('profileFailed', [status], 'profile', status);
  const skins = Array.isArray(body.skins) ? (body.skins as { state?: string; url?: string }[]) : [];
  return { id: str(body.id), name: str(body.name), skinUrl: skins.find((s) => s.state === 'ACTIVE')?.url };
}

export function dashedUuid(id: string): string {
  const hex = id.replaceAll('-', '').toLowerCase();
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
