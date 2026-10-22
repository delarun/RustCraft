// The signed-in account: the whole chain from Microsoft tokens to a Minecraft
// access token, license and profile, kept in localStorage so a reload stays
// signed in. The Minecraft token lasts ~24 h; the Microsoft refresh token
// (~90 days, renewed on every use) buys a new one without user interaction.
//
// localStorage is readable by any script on this origin, so the page must not
// run third-party scripts; the tokens never leave the browser except to the
// Microsoft/Xbox/Mojang endpoints (through our same-origin proxy).

import { AuthError } from './http';
import { entitlements, GAME_ENTITLEMENTS, loginWithXbox, profile, type Profile } from './minecraft';
import { refreshMsa, type MsaTokens } from './msa';
import { xboxUserToken, xstsToken } from './xbox';

export interface Account {
  clientId: string;
  msa: MsaTokens;
  minecraft: { accessToken: string; expiresAt: number };
  xuid: string;
  entitlements: string[];
  profile: Profile | null;
  /** Owns Java Edition and has a profile to play with. */
  licensed: boolean;
  /** Epoch ms of the last successful chain. */
  checkedAt: number;
}

const STORAGE_KEY = 'mcweb.account';
/** Renew this long before the Minecraft token expires. */
const MARGIN_MS = 10 * 60_000;

export function loadAccount(): Account | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    return raw ? (JSON.parse(raw) as Account) : null;
  } catch {
    return null;
  }
}

export function saveAccount(account: Account) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(account));
  } catch {
    /* storage unavailable: signed in for this page only */
  }
}

export function clearAccount() {
  try {
    localStorage.removeItem(STORAGE_KEY);
  } catch {
    /* storage unavailable */
  }
}

export type Step = 'msa' | 'xbox' | 'xsts' | 'minecraft' | 'license' | 'profile';

/** Microsoft tokens → Xbox → XSTS → Minecraft token → entitlements → profile. */
export async function signInWithMsa(clientId: string, msa: MsaTokens, onStep: (step: Step) => void = () => {}): Promise<Account> {
  onStep('xbox');
  const user = await xboxUserToken(msa.accessToken);
  onStep('xsts');
  const xsts = await xstsToken(user.token);
  onStep('minecraft');
  const minecraft = await loginWithXbox(xsts.userHash, xsts.token);
  onStep('license');
  const owned = await entitlements(minecraft.accessToken);
  onStep('profile');
  const player = await profile(minecraft.accessToken);
  // Game Pass subscribers may lack a store entitlement but do get a profile;
  // a profile exists only for accounts that can play.
  const licensed = player !== null && (owned.some((name) => GAME_ENTITLEMENTS.includes(name)) || owned.length === 0);
  return {
    clientId,
    msa,
    minecraft,
    xuid: user.xuid ?? xsts.xuid ?? '0',
    entitlements: owned,
    profile: player,
    licensed,
    checkedAt: Date.now(),
  };
}

export const needsRenewal = (account: Account, now = Date.now()) => account.minecraft.expiresAt - MARGIN_MS <= now;

/** A usable account: renewed through the refresh token when the Minecraft token is about to expire. */
export async function freshAccount(account: Account, onStep?: (step: Step) => void): Promise<Account> {
  if (!needsRenewal(account)) return account;
  onStep?.('msa');
  const msa = account.msa.expiresAt - MARGIN_MS > Date.now() ? account.msa : await refreshMsa(account.clientId, account.msa.refreshToken);
  return signInWithMsa(account.clientId, msa, onStep);
}

/** Why the account cannot play (a message key), or null. */
export function licenseProblem(account: Account): 'noLicense' | 'noProfile' | 'licenseNotFound' | null {
  if (account.licensed) return null;
  if (!account.profile && !account.entitlements.some((name) => GAME_ENTITLEMENTS.includes(name))) return 'noLicense';
  if (!account.profile) return 'noProfile';
  return 'licenseNotFound';
}

export { AuthError };
