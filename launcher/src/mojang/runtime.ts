// Mojang's Java runtimes (the ones the official launcher installs): an index of
// components per platform (JAVA_RUNTIMES_URL) and a file manifest per runtime.
// Our VM runs the JDK class library from the runtime's jimage, lib/modules, plus
// the configuration files next to it (conf/, lib/security, tzdb.dat, ...); the
// host executables and .so libraries are useless in the browser and skipped.

import { LocalizedError } from '../i18n/error';
import type { DownloadJob } from './downloads';
import { layout } from './layout';

/** The platform whose runtime builds we take the (platform-neutral) class library from. */
export const RUNTIME_PLATFORM = 'linux';

export interface RuntimeEntry {
  availability: { group: number; progress: number };
  manifest: { sha1: string; size: number; url: string };
  version: { name: string; released: string };
}

/** all.json: platform → component → builds. */
export type RuntimeIndex = Record<string, Record<string, RuntimeEntry[]>>;

export type RuntimeFile =
  | {
      type: 'file';
      executable: boolean;
      downloads: { raw: { sha1: string; size: number; url: string }; lzma?: { sha1: string; size: number; url: string } };
    }
  | { type: 'directory' }
  | { type: 'link'; target: string };

export interface RuntimeManifest {
  files: Record<string, RuntimeFile>;
}

export function selectRuntime(index: RuntimeIndex, component: string, platform = RUNTIME_PLATFORM): RuntimeEntry {
  const entry = index[platform]?.[component]?.[0];
  if (!entry) throw new LocalizedError('runtimeMissing', [component, platform]);
  return entry;
}

/** Files the VM reads: everything except host binaries, native libraries, headers and docs. */
export function isVmRuntimeFile(path: string): boolean {
  if (/^(bin|legal|man|include|jmods)\//.test(path)) return false;
  if (path.startsWith('lib/server/') || path.startsWith('lib/client/')) return false;
  if (/\.(so|dylib|dll|jsa|diz|debuginfo)$/.test(path)) return false;
  // Host helpers and javac's symbol table.
  return !['lib/jexec', 'lib/jspawnhelper', 'lib/ct.sym'].includes(path);
}

export function runtimeDownloads(component: string, manifest: RuntimeManifest): DownloadJob[] {
  const jobs: DownloadJob[] = [];
  for (const [path, file] of Object.entries(manifest.files)) {
    if (file.type !== 'file' || !isVmRuntimeFile(path)) continue;
    const { url, sha1, size } = file.downloads.raw;
    jobs.push({ url, path: layout.runtimeFile(component, path), size, sha1, verify: 'sha1' });
  }
  if (!jobs.some((job) => job.path === layout.runtimeFile(component, 'lib/modules'))) {
    throw new LocalizedError('runtimeNoModules', [component]);
  }
  return jobs;
}
