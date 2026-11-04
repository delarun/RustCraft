// Horizon (github.com/horizon-rust/horizon, MIT): Minecraft: Java Edition rewritten in Rust,
// built for the browser by horizon/build.sh and served under /horizon/. It plays one pinned
// version, from resources its own preparer makes out of the player's game files: here
// horizon-prepare.wasm, whose exporters (Horizon's Java code calling Mojang's) run on our JVM
// as its child processes (src/jvm/children.ts). Everything stays in the browser's storage:
//   horizon/root          what the preparer reads from Horizon's tree (root.pack) and its scratch
//   horizon/resources/<v> the prepared resources, which the game reads
//   horizon/game          the game's own directory: options, worlds, logs

import { JvmProcess } from './jvm/process';
import { ensurePack } from './jvm/pack';
import type { MountSpec, ProcessConfig } from './jvm/protocol';
import { GAME_DIR, layout } from './mojang/layout';
import { mirrorUrl } from './mojang/mirror';
import { isHostNativeClassifier, mavenPath, parseMaven } from './mojang/maven';
import { BROWSER_PLATFORM, rulesAllow } from './mojang/rules';
import type { Artifact, VersionJson } from './mojang/types';
import { modulesPath, vmMounts } from './mojang/vm';
import { readFileBytes, writeFile } from './storage/opfs';
import { RJAVA_WASM_URL } from './ui/game';

export const HORIZON_URL = `${import.meta.env.BASE_URL}horizon/`;

/** horizon/dist/horizon.json. */
export interface HorizonInfo {
  /** Horizon's commit. */
  commit: string;
  /** The Minecraft version it plays. */
  version: string;
  client: string;
  prepare: string;
  pack: string;
  packTag: string;
}

let info: Promise<HorizonInfo | null> | null = null;

/** What this site serves of Horizon; null when it serves none. */
export function horizonInfo(): Promise<HorizonInfo | null> {
  info ??= fetch(`${HORIZON_URL}horizon.json`).then(async (response) => {
    if (response.status === 404) return null;
    if (!response.ok) throw new Error(`horizon.json: HTTP ${response.status}`);
    return (await response.json()) as HorizonInfo;
  });
  info.catch(() => (info = null));
  return info;
}

const ROOT = '/horizon/root';
const resources = (version: string) => `/horizon/resources/${version}`;
const serverJar = (id: string) => `versions/${id}/${id}-server.jar`;

export interface PrepareReport {
  status(line: string): void;
  /** The fraction done, 0 to 1. */
  progress(fraction: number): void;
  /** Every line the preparer and its exporters print (for the console). */
  log(line: string, error: boolean): void;
}

/** The mounts the preparer and its exporters see: the game directory and Horizon's. */
function prepareMounts(version: VersionJson): MountSpec[] {
  // Shared: the preparer and its exporters (two JVMs at times) open the same files at once.
  const shared = (mount: MountSpec): MountSpec => (mount.kind === 'opfs' ? { ...mount, shared: true } : mount);
  return [
    ...vmMounts(version.assetIndex.id).filter((m) => m.path !== '/tmp').map(shared),
    { path: '/horizon', kind: 'opfs', root: 'horizon', shared: true },
    { path: '/tmp', kind: 'memory' },
  ];
}

/** Runs horizon-prepare.wasm with `args`; resolves with its exit code. */
async function runPrepare(wasm: ArrayBuffer, version: VersionJson, args: string[], report: PrepareReport): Promise<number> {
  const config: ProcessConfig = {
    args: ['horizon-prepare', ...args],
    env: { HOME: '/horizon', PWD: ROOT, TMPDIR: '/tmp' },
    mounts: prepareMounts(version),
    children: {
      java: {
        wasm: new URL(RJAVA_WASM_URL, location.href).href,
        args: ['rjava', '--modules', modulesPath(version.javaVersion?.component ?? 'java-runtime-epsilon')],
        env: { HOME: '/horizon', RJAVA_MEMORY_MB: '2560' },
      },
    },
  };
  let failure: string | null = null;
  const code = await new Promise<number>((resolve, reject) => {
    JvmProcess.start(wasm, config, {
      onOutput(fd, line) {
        if (fd === 1 && line.startsWith('{')) {
          try {
            const message = JSON.parse(line) as { message?: string; fraction?: number; error?: string };
            if (message.message !== undefined) report.status(message.message);
            if (message.fraction !== undefined) report.progress(message.fraction);
            if (message.error !== undefined) failure = message.error;
            report.log(line, message.error !== undefined);
            return;
          } catch {
            // Not a progress line.
          }
        }
        report.log(line, fd === 2);
      },
      onExit: resolve,
      onError: (message) => report.log(message, true),
    }).catch(reject);
  });
  if (code !== 0 && code !== 1) throw new Error(failure ?? `horizon-prepare stopped (exit ${code})`);
  if (failure) throw new Error(failure);
  return code;
}

async function sha1Hex(data: Uint8Array<ArrayBuffer>): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-1', data));
  return Array.from(digest, (b) => b.toString(16).padStart(2, '0')).join('');
}

/** `artifact` at `path` in the game directory, downloaded from Mojang unless it is there already. */
async function ensureArtifact(path: string, artifact: Artifact, report: PrepareReport) {
  const existing = await readFileBytes(path);
  if (existing && existing.byteLength === artifact.size && (await sha1Hex(existing)) === artifact.sha1) return;
  report.status(`Downloading ${path.slice(path.lastIndexOf('/') + 1)} from Mojang`);
  const response = await fetch(mirrorUrl(artifact.url));
  if (!response.ok) throw new Error(`${artifact.url}: HTTP ${response.status}`);
  const data = new Uint8Array(await response.arrayBuffer());
  if ((await sha1Hex(data)) !== artifact.sha1) throw new Error(`${artifact.url}: not the file Mojang lists (SHA-1)`);
  await writeFile(path, data);
}

/**
 * The host-native jars (natives-linux, Netty's linux-x86_64) that the launcher's installer skips,
 * since our JVM brings its own native code, but horizon-prepare insists on: it checks every
 * library the official launcher installs, and on wasm32 it picks Linux's x64 ones.
 */
function nativeLibraries(version: VersionJson): { path: string; artifact: Artifact }[] {
  const arm = /-(arm64|aarch64|aarch_64)$/;
  return version.libraries.flatMap((lib) => {
    const artifact = lib.downloads?.artifact;
    const classifier = parseMaven(lib.name).classifier;
    if (!artifact || !isHostNativeClassifier(classifier) || arm.test(`-${classifier}`)) return [];
    if (!rulesAllow(lib.rules, BROWSER_PLATFORM)) return [];
    return [{ path: `libraries/${artifact.path ?? mavenPath(parseMaven(lib.name))}`, artifact }];
  });
}

/** Mojang's files the exporters need that the launcher does not install: the server jar, native jars. */
async function ensureGameFiles(version: VersionJson, report: PrepareReport) {
  const server = version.downloads.server;
  if (!server) throw new Error(`${version.id} has no server jar`);
  await ensureArtifact(serverJar(version.id), server, report);
  for (const { path, artifact } of nativeLibraries(version)) await ensureArtifact(path, artifact, report);
}

/**
 * Makes Horizon's resources for `version` (its pinned one) unless they are ready: Horizon's
 * files, Mojang's server jar and native jars, then horizon-prepare (a few minutes the first time).
 */
export async function prepareHorizon(info: HorizonInfo, version: VersionJson, report: PrepareReport) {
  await ensurePack({ url: `${HORIZON_URL}${info.pack}`, root: 'horizon/root', tag: info.packTag }, report.status);
  const response = await fetch(`${HORIZON_URL}${info.prepare}`);
  if (!response.ok) throw new Error(`${info.prepare}: HTTP ${response.status}`);
  const wasm = await response.arrayBuffer();
  const output = resources(info.version);
  // Reads no game files and runs no Java: 0 when the resources are complete and current.
  if ((await runPrepare(wasm, version, ['--check', '--root', ROOT, '--output', output], report)) === 0) return;
  await ensureGameFiles(version, report);
  const id = version.id;
  const code = await runPrepare(
    wasm,
    version,
    [
      '--prepare',
      '--root',
      ROOT,
      '--output',
      output,
      '--server-jar',
      `${GAME_DIR}/${serverJar(id)}`,
      '--version-json',
      `${GAME_DIR}/${layout.versionJson(id)}`,
      '--client-jar',
      `${GAME_DIR}/${layout.clientJar(id)}`,
      '--libraries-dir',
      `${GAME_DIR}/libraries`,
      '--assets-dir',
      `${GAME_DIR}/assets`,
      '--complete',
      '--progress',
      'json',
    ],
    report,
  );
  if (code !== 0) throw new Error('horizon-prepare could not complete the resources');
}

/** The game: horizon-client.wasm on the prepared resources. */
export function horizonProcess(info: HorizonInfo, player: string): ProcessConfig {
  return {
    args: ['horizon-client', '--resources', '/res', '--game-dir', '/work', '--name', player],
    env: { HOME: '/work', PWD: '/work', TMPDIR: '/work/tmp' },
    mounts: [
      { path: '/work', kind: 'opfs', root: 'horizon/game' },
      { path: '/res', kind: 'opfs', root: `horizon/resources/${info.version}`, readonly: true },
    ],
  };
}

export const horizonClientUrl = (info: HorizonInfo) => `${HORIZON_URL}${info.client}`;
