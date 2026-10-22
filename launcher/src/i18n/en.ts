// English messages: the reference catalog. Every other catalog has type
// Messages, so a missing or extra key fails the typecheck. Keys ending in Html
// are trusted markup (inserted as HTML); everything else is plain text.

type N = number;
type S = string;

export const en = {
  // --- header ---
  language: 'Language',
  clickToCaptureMouse: 'Click to control the mouse',
  tagline: 'Your own copy of Minecraft: Java Edition, running in the browser on a Java virtual machine written in Rust.',

  // --- account ---
  accountTitle: 'Account',
  accountIntro: 'Playing requires a Minecraft: Java Edition license. Sign in with the Microsoft account that owns the game.',
  noClientIdNotice: 'Microsoft sign-in is not set up in this build: no application client ID (VITE_MS_CLIENT_ID).',
  signIn: 'Sign in with Microsoft',
  deviceOpenBefore: 'Open',
  deviceOpenAfter: '(on this or another device) and enter the code:',
  copy: 'Copy',
  cancel: 'Cancel',
  signOut: 'Sign out',
  signInSettings: 'Sign-in settings',
  clientIdLabel: 'Azure client ID',
  save: 'Save',
  stepMsa: 'Renewing the Microsoft sign-in…',
  stepXbox: 'Signing in to Xbox Live…',
  stepXsts: 'Authorizing with XSTS…',
  stepMinecraft: 'Signing in to Minecraft…',
  stepLicense: 'Checking the license…',
  stepProfile: 'Loading the profile…',
  finishSignIn: 'Finish signing in to your Microsoft account.',
  signInToPlay: 'Sign in with a Microsoft account that owns Minecraft: Java Edition.',
  signInUnavailable: 'Microsoft sign-in is unavailable: no client ID is set.',
  notSignedIn: 'Not signed in',
  requestingCode: 'Requesting a sign-in code…',
  waitingForSignIn: 'Waiting for you to sign in on the Microsoft page…',
  codeExpiresIn: (minutes: N, seconds: S) => `The code expires in ${minutes}:${seconds}`,
  signInCancelled: 'Sign-in cancelled.',
  signedOut: 'You signed out. The tokens were removed from this browser.',
  clientIdInvalid: 'A client ID is a GUID like 00000000-0000-0000-0000-000000000000.',
  clientIdFromSettings: (id: S) => `Using client ID ${id} (from this browser's settings).`,
  clientIdFromBuild: (id: S) => `Using client ID ${id} (from the build, VITE_MS_CLIENT_ID).`,
  clientIdDefault: (id: S) => `Using RustCraft’s own client ID ${id}.`,
  noClientId:
    'This build has no Microsoft application client ID (VITE_MS_CLIENT_ID), so sign-in is unavailable. ' +
    'Site administrators: see README.md. To test, you can enter your own client ID under “Sign-in settings”.',
  noJavaProfile: 'no Java Edition profile',
  licenseOk: '✓ Minecraft: Java Edition license confirmed',
  noLicense: 'This account has no Minecraft: Java Edition license. Buy the game at minecraft.net and sign in again.',
  noProfile: 'The game is owned, but no profile exists yet: choose a name at minecraft.net and sign in again.',
  licenseNotFound: 'No Minecraft: Java Edition license found.',

  // --- sign-in errors ---
  network: (host: S, message: S) => `Network unavailable (${host}): ${message}`,
  msaDeclined: 'Sign-in was declined in the Microsoft window.',
  msaExpired: 'The sign-in code expired. Start signing in again.',
  msaRelogin: 'Your Microsoft session expired. Please sign in again.',
  msaAppNotFound: 'No Azure application with this client ID exists (check the client ID in the settings).',
  msaPublicFlows: 'The Azure application does not allow “public client flows” (needed for sign-in with a code).',
  msaError: (description: S) => `Microsoft: ${description}`,
  xblFailed: (status: N) => `Xbox Live: sign-in failed (HTTP ${status}).`,
  xstsBanned: 'This Xbox account is banned.',
  xstsChild: 'This is a child account: an adult must allow online play in Microsoft Family settings.',
  xstsNoXbox: 'This Microsoft account has no Xbox profile. Create one at xbox.com and sign in again.',
  xstsTerms: 'Accept the Xbox terms at xbox.com and sign in again.',
  xstsCountry: 'Xbox Live is not available in the account’s country.',
  xstsAgeVerification: 'The account needs age verification (South Korea) at xbox.com.',
  xstsMinor: 'The account belongs to a minor: an adult must add it to a Microsoft family.',
  xstsFailed: (status: N, xerr: S) => `Xbox XSTS: authorization failed (HTTP ${status}${xerr ? `, XErr ${xerr}` : ''}).`,
  xboxNoUserHash: 'Xbox Live: the response has no user hash.',
  mcNotApproved:
    'Minecraft refused the sign-in: this Azure application is not approved by Mojang for the Minecraft API ' +
    '(https://aka.ms/mce-reviewappid). Use an approved client ID.',
  mcRateLimited: 'Too many Minecraft sign-in attempts. Wait a minute.',
  mcLoginFailed: (status: N, message: S) => `Minecraft: sign-in failed (HTTP ${status}${message ? `: ${message}` : ''}).`,
  entitlementsFailed: (status: N) => `Minecraft: could not check the license (HTTP ${status}).`,
  profileFailed: (status: N) => `Minecraft: could not load the profile (HTTP ${status}).`,

  // --- version ---
  versionTitle: 'Version',
  versionAria: 'Game version',
  snapshots: 'snapshots',
  memoryLabel: 'Java memory',
  memory1g: '1 GB',
  memory15g: '1.5 GB',
  memory2g: '2 GB',
  loadingVersions: 'Loading the version list…',
  manifestFailed: (error: S) => `Could not load the version list from piston-meta.mojang.com: ${error}`,
  latestRelease: ' (latest release)',
  latestSnapshot: ' (latest snapshot)',
  markTested: ' ✓ tested',
  markUnsupported: ' — not supported',
  checkingCompat: 'Checking compatibility…',
  compatTested: 'tested on our JVM',
  compatUntested: 'should run, but has not been tested',
  compatJava: (java: N, supported: N) => `needs Java ${java}; our JVM currently runs only the Java ${supported} class library`,
  compatGlfw: 'this version uses GLFW, while our JVM implements only LWJGL’s SDL windows',
  compatDownload: (java: S) => ` The game and ${java} are downloaded from Mojang’s servers on first launch.`,
  versionJsonFailed: (error: S) => `Could not load the version description: ${error}`,

  // --- play ---
  playTitle: 'Game',
  eulaHtml:
    'I have read and accept the <a href="https://www.minecraft.net/eula" target="_blank" rel="noopener noreferrer">Minecraft End User License Agreement (EULA)</a>',
  play: 'Play',
  verifyFiles: 'Verify files',
  failuresSummary: 'Download errors',
  browserMissing: (features: S) => `This browser lacks: ${features}.`,
  blockedIsolation: 'The game cannot run here: this page is not cross-origin isolated (the server must send COOP/COEP headers).',
  blockedSharedMemory: 'The game cannot run in this browser: it does not support SharedArrayBuffer. Try a current Chrome, Edge or Firefox.',
  featureMissing: (feature: S, why: S) => `${feature} is not available in this browser: ${why}.`,
  whyIsolation: 'the page must be served with COOP/COEP headers',
  whySharedMemory: 'needed for the JVM’s threads',
  whyOpfs: 'needed to store the game files',
  whyWebgl: 'needed for graphics',
  whyAudio: 'needed for sound',
  noManifest: 'The version list is not loaded.',
  versionUnsupported: (id: S) => `Version ${id} will not run on our JVM.`,
  acceptEula: 'To play, accept the Minecraft EULA.',
  untestedWarning: 'This version has not been tested: expect problems.',
  checkingSignIn: 'Checking the sign-in…',
  versionJsonMissing: 'The version description was not saved to storage',
  gameExited: (code: N) => `The game exited (code ${code}).`,

  // --- install ---
  installMetadata: (id: S) => `Loading the metadata of ${id}`,
  installRuntime: (component: S) => `Loading the description of Java runtime ${component}`,
  installChecking: (full: boolean, files: N) => `${full ? 'Verifying (SHA-1)' : 'Checking'} and downloading ${files} files`,
  installBusy: 'An installation is already running',
  installCancelled: 'Installation cancelled',
  installFailed: (count: N) => `Files that failed to download: ${count}`,
  installDone: (version: S, runtime: S, downloaded: N, present: N) =>
    `Version ${version} and Java (${runtime}) are ready: ${downloaded} files downloaded, ${present} were already present.`,
  progress: (done: N, total: N, doneBytes: S, totalBytes: S) => `${done} / ${total} files · ${doneBytes} / ${totalBytes}`,
  progressPresent: (count: N) => ` · ${count} already present`,
  runtimeMissing: (component: S, platform: S) => `Mojang publishes no Java runtime ${component} for ${platform}`,
  runtimeNoModules: (component: S) => `Java runtime ${component} has no lib/modules`,

  // --- settings ---
  settingsTitle: 'Settings',
  systemCheckTitle: 'System check',
  systemCheckOk: 'This browser has everything the game needs.',
  systemCheckProblems: 'Some features this game needs are missing:',

  // --- launch plan ---
  planTitle: 'Launch plan',
  planArgv: 'JVM command line',
  planVersion: 'Version',
  planJava: 'Java',
  planJavaValue: (major: N, component: S) => `${major} (${component} from Mojang)`,
  planMainClass: 'Main class',
  planClasspath: 'Classpath',
  planClasspathValue: (jars: N) => `${jars} jars`,
  planNatives: 'Native modules (implemented by our JVM)',
  planUnresolved: 'Unfilled variables',
  hidden: '<hidden>',

  // --- game ---
  loadingJvm: 'Loading the JVM…',
  gameStarted: 'The game is running',
  jvmExited: (code: N) => `— the JVM exited with code ${code}`,
  confirmStop: 'Stop the game? Unsaved progress will be lost.',
  stop: 'Stop',

  // --- privacy ---
  privacyTitle: 'Privacy',
  privacySignInHtml:
    'Sign-in happens on the servers of Microsoft, Xbox Live and Minecraft. Requests to them pass through our proxy (<code>/proxy/…</code>) ' +
    'only because those servers do not accept requests from other websites (CORS). The proxy forwards the requests as they are and neither ' +
    'stores nor logs request or response contents, Authorization headers or tokens.',
  privacyTokensHtml:
    'Your tokens (Microsoft and Minecraft) and your profile are kept only in this browser (localStorage) and are removed by “Sign out”. ' +
    'The Minecraft token is handed to the game running here, in this browser, as the official launcher does.',
  privacyFilesHtml:
    'Game files are downloaded from Mojang’s servers into this browser’s storage (OPFS). Libraries and assets pass through the same proxy ' +
    '(for the same reason, CORS), streamed and not stored by us.',
  privacySettingsHtml:
    'The selected version, your EULA acceptance, the language and other settings are kept in this browser’s localStorage. ' +
    'We use no cookies, analytics or ads.',

  // --- storage ---
  storageTitle: 'Storage',
  storageNote: 'Everything is downloaded from Mojang’s servers and kept only in this browser (OPFS). Worlds and game settings live there too.',
  clearDownloads: 'Delete downloaded files',
  clearAll: 'Delete everything, including worlds',
  storageUsage: (used: S, quota: S) => `Used: ${used} · browser quota: ${quota}`,
  storagePersistent: ' · storage is persistent',
  storageEvictable: ' · the browser may clear the storage when space runs low',
  storageUnavailable: (error: S) => `Storage unavailable: ${error}`,
  confirmClearAll: 'Delete EVERYTHING: downloaded files, worlds, settings and screenshots?',
  confirmClearDownloads: 'Delete downloaded versions, libraries, assets and Java? Worlds and settings stay.',
  clearedAll: 'All game files deleted.',
  clearedDownloads: 'Downloaded files deleted.',
  byteUnits: 'B KB MB GB TB',

  // --- footer ---
  /** A translation shown next to the English legal disclaimer (null: none). */
  disclaimerLocal: null as string | null,
  footerHtml:
    'Minecraft is a trademark of Mojang Synergies AB. The game and its files are downloaded from Mojang’s servers; you need your own license to play. ' +
    'RustCraft is free software under the GNU AGPL-3.0: <a href="https://github.com/delarun/RustCraft" target="_blank" rel="noopener">source code</a> · ' +
    '<a href="third-party-notices.txt" target="_blank" rel="noopener">Third-party licenses</a>',

  /** An untranslated message (e.g. from the network stack). */
  text: (message: S) => message,
};

export type Messages = typeof en;
export type MessageKey = keyof Messages;
