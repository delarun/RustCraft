# RustCraft launcher

The web page that runs the original Minecraft: Java Edition client in the browser on
our own JVM (`rjava.wasm`, built from `../vm`). Everything Minecraft-related is downloaded by the
player's browser from Mojang's servers into the browser's own storage (OPFS); this site
never hosts or mirrors game files. Players sign in with their own Microsoft account and need
their own copy of the game.

NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT.

The product name lives in `src/brand.ts` (and, identically, in `index.html`, checked by
`test/brand.test.ts`). Mojang's Usage Guidelines discourage names that suggest an official
product; a safer descriptive alternative is noted there.

## Development

```sh
pnpm install
pnpm dev          # http://localhost:5173/ (dev proxy and /rjava.wasm come from vite.config.ts)
pnpm typecheck
pnpm test
```

`e2e/launcher.mjs` drives the page in headless Chrome (install, OPFS checks, UI, Play).

## Microsoft sign-in: the client ID

Sign-in uses RustCraft's own Azure (Microsoft identity platform) application,
`41f23721-33c5-4533-923d-f981d3f65040` (`DEFAULT_CLIENT_ID` in `src/auth/config.ts`; client IDs
of public-client apps are not secret). Precedence:

1. the "Sign-in settings" field on the page (per browser, localStorage) — handy for testing;
2. `VITE_MS_CLIENT_ID` at build time (environment or `launcher/.env.local`, git-ignored);
3. the built-in default above.

**Mojang must approve this app** for the Minecraft services API:
<https://aka.ms/mce-reviewappid>. Until then Microsoft and Xbox sign-in succeed, but
`api.minecraftservices.com/authentication/login_with_xbox` answers **403 "Invalid app
registration"**; the page then says the app is not approved by Mojang, and Play stays off.

The app registration (<https://entra.microsoft.com> → App registrations):

- Supported account types: **Personal Microsoft accounts only**.
- Authentication → **Mobile and desktop applications** platform (the suggested redirect URI
  `https://login.microsoftonline.com/common/oauth2/nativeclient` is fine). The launcher uses the
  **device code flow** (the player opens microsoft.com/link and types a code), so no redirect
  to rustcraft.online is involved and no SPA platform is needed.
- Authentication → Advanced settings → **Allow public client flows: Yes**.
- API permissions: `XboxLive.signin` (delegated), requested with `offline_access`.

## Languages

The page is in English and Russian: `src/i18n/en.ts` is the reference catalog,
`src/i18n/ru.ts` must have the same keys (a missing key fails `pnpm typecheck`;
`test/i18n.test.ts` also checks for empty messages). The language follows
`navigator.languages` (`ru*` → Russian, otherwise English) until the player picks one in the
header. The legal disclaimer stays in English in every language.

## Production deployment (https://rustcraft.online)

The site is static (`pnpm build` → `dist/`, including `rjava.wasm` and
`third-party-notices.txt`), with two requirements:

- **HTTPS and cross-origin isolation**: every page must be served with
  `Cross-Origin-Opener-Policy: same-origin` and `Cross-Origin-Embedder-Policy: require-corp`
  (the JVM's threads need SharedArrayBuffer), and `rjava.wasm` as `application/wasm`.
- **The CORS proxy** at `https://rustcraft.online/proxy/<host>/…`, on the same origin as the
  page. Microsoft's device code and token endpoints, Xbox XSTS errors, the Minecraft services
  API, `libraries.minecraft.net` and `resources.download.minecraft.net` send no usable CORS
  headers (see `src/mojang/mirror.ts`). The page uses relative URLs, so the same build works on
  any origin that has the proxy.

`proxy/server.ts` does both, without dependencies (Node ≥ 23.6 runs the TypeScript directly):

```sh
pnpm build
STATIC_DIR=dist PORT=8080 node proxy/server.ts            # behind an HTTPS reverse proxy (recommended)
TLS_CERT=fullchain.pem TLS_KEY=privkey.pem STATIC_DIR=dist PORT=443 HOST=0.0.0.0 node proxy/server.ts
```

The proxy (`proxy/proxy.ts`, tested in `proxy/proxy.test.ts`):

- forwards only to the allowlisted hosts — sign-in: `login.microsoftonline.com`,
  `user.auth.xboxlive.com`, `xsts.auth.xboxlive.com`, `api.minecraftservices.com` (GET/POST);
  files: `libraries.minecraft.net`, `resources.download.minecraft.net` (GET/HEAD only);
- serves only our own pages: requests with another site's `Origin` (anything but the server's
  own origin and `ALLOWED_ORIGINS`, default `https://rustcraft.online`) or
  `Sec-Fetch-Site: cross-site` get 403; it sends no CORS headers;
- forwards only the request headers the services need, so Origin, Referer and Cookie never
  reach them; drops Set-Cookie; caps sign-in request bodies at 64 KiB;
- streams responses through without storing them and marks every response
  `Cache-Control: no-store`, so neither we nor a CDN in front of us becomes a mirror (the
  browser keeps game files in OPFS);
- logs one line per request — method, host, status, duration — never paths, query strings,
  headers, bodies or tokens.

If the static files are hosted elsewhere (a CDN), run only the proxy (`node proxy/server.ts`)
and route `https://rustcraft.online/proxy/` to it; the CDN must add the COOP/COEP headers.
Consider rate limiting `/proxy/` at the reverse proxy.

`jvm.html` (a developer console that reads host files through the dev server) is also part of
the build; it does nothing useful in production.

## Third-party notices

`public/third-party-notices.txt` (linked from the page footer) lists what the site ships:
the Rust crates, Rust standard library and C/C++ libraries (shaderc, glslang, SPIRV-Tools,
SPIRV-Headers, SPIRV-Cross, stb, libc++/libc++abi, wasi-libc) statically linked into
`rjava.wasm`, and the page's own JavaScript, with their license texts. Regenerate it after
changing `vm/Cargo.lock`, the Rust toolchain or the C libraries:

```sh
pnpm notices      # node scripts/third-party-notices.ts (offline; reads cargo metadata,
                  # ~/.cargo/registry, $RJAVA_WASM_LIBS or .scratch/wasm-libs, scripts/licenses/)
```

## Privacy

What the page tells players (section "Конфиденциальность") matches the code: sign-in requests
pass through the proxy only because of CORS; tokens stay in the player's browser
(localStorage, removed on sign-out) and are handed only to the game running in that browser;
game files come from Mojang into OPFS. No cookies, analytics or ads.
