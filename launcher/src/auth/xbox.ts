// Xbox Live: the Microsoft access token buys an Xbox Live user token, which
// buys an XSTS token for Minecraft's relying party.

import type { MessageKey } from '../i18n/en';
import { AuthError, num, postJson, str } from './http';

export interface XboxToken {
  token: string;
  userHash: string;
  /** Xbox user ID, when the token carries it. */
  xuid?: string;
  /** Epoch ms. */
  expiresAt: number;
}

export const MINECRAFT_RELYING_PARTY = 'rp://api.minecraftservices.com/';

export async function xboxUserToken(msaAccessToken: string): Promise<XboxToken> {
  const { status, body } = await postJson(
    'https://user.auth.xboxlive.com/user/authenticate',
    {
      Properties: { AuthMethod: 'RPS', SiteName: 'user.auth.xboxlive.com', RpsTicket: `d=${msaAccessToken}` },
      RelyingParty: 'http://auth.xboxlive.com',
      TokenType: 'JWT',
    },
    { 'x-xbl-contract-version': '1' },
  );
  if (status !== 200 || !body.Token) throw new AuthError('xblFailed', [status], 'xbl', status);
  return xboxToken(body);
}

export async function xstsToken(userToken: string): Promise<XboxToken> {
  const { status, body } = await postJson(
    'https://xsts.auth.xboxlive.com/xsts/authorize',
    { Properties: { SandboxId: 'RETAIL', UserTokens: [userToken] }, RelyingParty: MINECRAFT_RELYING_PARTY, TokenType: 'JWT' },
    { 'x-xbl-contract-version': '1' },
  );
  if (status === 200 && body.Token) return xboxToken(body);
  throw xstsError(num(body.XErr), status);
}

/** XErr codes documented by Xbox Live / known from other launchers. */
const XSTS_ERRORS: Record<number, MessageKey> = {
  2148916227: 'xstsBanned',
  2148916229: 'xstsChild',
  2148916233: 'xstsNoXbox',
  2148916234: 'xstsTerms',
  2148916235: 'xstsCountry',
  2148916236: 'xstsAgeVerification',
  2148916237: 'xstsAgeVerification',
  2148916238: 'xstsMinor',
};

export function xstsError(code: number, status: number): AuthError {
  const known = XSTS_ERRORS[code];
  const id = code ? `xsts_${code}` : 'xsts';
  return known ? new AuthError(known, [], id, status) : new AuthError('xstsFailed', [status, code ? String(code) : ''], id, status);
}

function xboxToken(body: Record<string, unknown>): XboxToken {
  const claims = (body.DisplayClaims as { xui?: { uhs?: string; xid?: string }[] } | undefined)?.xui?.[0];
  if (!claims?.uhs) throw new AuthError('xboxNoUserHash', [], 'xbl');
  return {
    token: str(body.Token),
    userHash: claims.uhs,
    xuid: claims.xid,
    expiresAt: Date.parse(str(body.NotAfter)) || Date.now() + 3600_000,
  };
}
