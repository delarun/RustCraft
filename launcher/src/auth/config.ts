// The Azure (Microsoft identity platform) application the launcher signs in as.
// Precedence: the "Sign-in settings" field on the page (localStorage) >
// VITE_MS_CLIENT_ID at build time > DEFAULT_CLIENT_ID below. Client IDs of
// public-client apps are not secret.
//
// The app is a public client ("Mobile and desktop applications", "Allow public
// client flows" = yes): sign-in uses the device code flow through our proxy
// (see msa.ts), so no redirect to this page is involved. Mojang must approve
// the app for the Minecraft API (https://aka.ms/mce-reviewappid); until then
// login_with_xbox answers 403 "Invalid app registration" (see minecraft.ts).

/** RustCraft's own Azure application (rustcraft.online). */
export const DEFAULT_CLIENT_ID = '41f23721-33c5-4533-923d-f981d3f65040';
export const MSA_SCOPE = 'XboxLive.signin offline_access';
/** Personal Microsoft accounts (the only kind that can own Minecraft). */
export const MSA_AUTHORITY = 'https://login.microsoftonline.com/consumers/oauth2/v2.0';

const OVERRIDE_KEY = 'mcweb.msClientId';

export function configuredClientId(): string | null {
  try {
    return localStorage.getItem(OVERRIDE_KEY) || null;
  } catch {
    return null;
  }
}

export function setConfiguredClientId(id: string | null) {
  try {
    if (id) localStorage.setItem(OVERRIDE_KEY, id);
    else localStorage.removeItem(OVERRIDE_KEY);
  } catch {
    /* storage unavailable */
  }
}

/** VITE_MS_CLIENT_ID of this build (from the environment or .env.local). */
export function buildClientId(): string | null {
  const id = (import.meta.env?.VITE_MS_CLIENT_ID as string | undefined)?.trim();
  return id || null;
}

/** The client ID to sign in with; null only if the built-in default is blanked. */
export function clientId(): string | null {
  return configuredClientId() ?? buildClientId() ?? (DEFAULT_CLIENT_ID.trim() || null);
}

export const isClientId = (id: string) => /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id);
