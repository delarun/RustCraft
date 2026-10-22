import { layout } from './layout';
import { resolveLibraries } from './libraries';
import { RESOURCES_URL } from './mirror';
import type { Platform } from './rules';
import type { AssetIndex, VersionJson } from './types';

export interface DownloadJob {
  url: string;
  /** Path relative to the game directory. */
  path: string;
  size: number;
  sha1: string;
  /**
   * Assets are content-addressed (the path is the hash), so a size check is
   * enough to trust an existing file; everything else is re-hashed on a full check.
   */
  verify: 'sha1' | 'size';
}

/**
 * Assets fetched at install time. Sounds (~360 MB) and the translations
 * (~85 MB) are fetched by the JVM's file system on first use instead (see
 * src/jvm/opfs.ts), so a first launch needs ~15 MB of assets, not ~470 MB.
 */
export function isEagerAsset(name: string): boolean {
  if (name.startsWith('minecraft/sounds/')) return false;
  if (name.startsWith('minecraft/lang/')) return name === 'minecraft/lang/en_us.json';
  return true;
}

export function assetUrl(hash: string): string {
  return `${RESOURCES_URL}/${hash.slice(0, 2)}/${hash}`;
}

export function collectDownloads(
  version: VersionJson,
  assets: AssetIndex,
  platform: Platform,
  eagerAsset: (name: string) => boolean = () => true,
): DownloadJob[] {
  const jobs: DownloadJob[] = [];
  const { client } = version.downloads;
  jobs.push({ url: client.url, path: layout.clientJar(version.id), size: client.size, sha1: client.sha1, verify: 'sha1' });

  for (const lib of resolveLibraries(version, platform).classpath) {
    const { url, size, sha1 } = lib.artifact;
    jobs.push({ url, path: layout.library(lib.path), size, sha1, verify: 'sha1' });
  }

  const logging = version.logging?.client?.file;
  if (logging) {
    jobs.push({ url: logging.url, path: layout.logConfig(logging.id), size: logging.size, sha1: logging.sha1, verify: 'sha1' });
  }

  const seenHashes = new Set<string>();
  for (const [name, { hash, size }] of Object.entries(assets.objects)) {
    if (seenHashes.has(hash) || !eagerAsset(name)) continue;
    seenHashes.add(hash);
    jobs.push({ url: assetUrl(hash), path: layout.assetObject(hash), size, sha1: hash, verify: 'size' });
  }
  return jobs;
}
