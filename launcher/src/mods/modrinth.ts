// Modrinth (modrinth.com), the mod index: search, and picking the files to
// install for a mod with its required dependencies. api.modrinth.com and
// cdn.modrinth.com send `Access-Control-Allow-Origin: *`, so the page talks to
// them directly. Modrinth asks API clients for a descriptive User-Agent, which
// browsers do not let a page set; the page keeps its requests few instead
// (debounced search, nothing before the Mods section is opened).

import { LocalizedError } from '../i18n/error';

export const MODRINTH_API_URL = 'https://api.modrinth.com/v2';
export const MODRINTH_SITE_URL = 'https://modrinth.com';
/** The only loader the launcher runs. */
export const LOADER = 'fabric';

export interface ModrinthHit {
  project_id: string;
  slug: string;
  title: string;
  description: string;
  downloads: number;
  icon_url: string | null;
  author: string;
}

export interface ModrinthSearch {
  hits: ModrinthHit[];
  total_hits: number;
}

export interface ModrinthProject {
  id: string;
  slug: string;
  title: string;
  icon_url: string | null;
}

export interface ModrinthFile {
  url: string;
  filename: string;
  primary: boolean;
  size: number;
  hashes: { sha1?: string; sha512?: string };
}

export interface ModrinthDependency {
  project_id: string | null;
  version_id: string | null;
  file_name?: string | null;
  dependency_type: 'required' | 'optional' | 'incompatible' | 'embedded';
}

export interface ModrinthVersion {
  id: string;
  project_id: string;
  name: string;
  version_number: string;
  version_type: 'release' | 'beta' | 'alpha';
  date_published: string;
  game_versions: string[];
  loaders: string[];
  files: ModrinthFile[];
  dependencies: ModrinthDependency[];
}

const query = (params: Record<string, string>) => new URLSearchParams(params).toString();

export function searchUrl(text: string, gameVersion: string, limit = 20): string {
  const facets = [['project_type:mod'], [`categories:${LOADER}`], [`versions:${gameVersion}`]];
  // Without a query, the most downloaded mods first.
  return `${MODRINTH_API_URL}/search?${query({
    query: text,
    facets: JSON.stringify(facets),
    index: text ? 'relevance' : 'downloads',
    limit: String(limit),
  })}`;
}

export function projectVersionsUrl(project: string, gameVersion: string): string {
  return `${MODRINTH_API_URL}/project/${encodeURIComponent(project)}/version?${query({
    loaders: JSON.stringify([LOADER]),
    game_versions: JSON.stringify([gameVersion]),
  })}`;
}

export const projectPageUrl = (slug: string) => `${MODRINTH_SITE_URL}/mod/${encodeURIComponent(slug)}`;

const isCompatible = (version: ModrinthVersion, gameVersion: string) =>
  version.loaders.includes(LOADER) && version.game_versions.includes(gameVersion);

const RANK: Record<ModrinthVersion['version_type'], number> = { release: 0, beta: 1, alpha: 2 };

/** The newest release for Fabric and the game version (else the newest beta, then alpha); null if none. */
export function pickVersion(versions: readonly ModrinthVersion[], gameVersion: string): ModrinthVersion | null {
  const candidates = versions
    .filter((version) => isCompatible(version, gameVersion) && version.files.length > 0)
    .sort((a, b) => RANK[a.version_type] - RANK[b.version_type] || Date.parse(b.date_published) - Date.parse(a.date_published));
  return candidates[0] ?? null;
}

/** The file to put in mods/ (versions may carry extra files: sources, dev jars). */
export function primaryFile(version: ModrinthVersion): ModrinthFile {
  const file = version.files.find((f) => f.primary) ?? version.files[0];
  if (!file) throw new LocalizedError('modNoFile', [version.version_number]);
  return file;
}

/** What resolving needs from Modrinth (injected, so the logic is tested without network). */
export interface ModrinthApi {
  project(id: string): Promise<ModrinthProject>;
  /** The project's versions for Fabric and the game version. */
  versions(project: string, gameVersion: string): Promise<ModrinthVersion[]>;
  version(id: string): Promise<ModrinthVersion>;
}

export interface PlannedMod {
  project: ModrinthProject;
  version: ModrinthVersion;
  file: ModrinthFile;
}

/**
 * The files that installing a project takes: its newest compatible version
 * and, recursively, the required dependencies not installed yet, dependencies
 * first. A dependency pinned to a version gets that version when it supports
 * Fabric and the game version. Optional and embedded dependencies are skipped;
 * an incompatible one that is installed or planned is an error.
 *
 * @param installed project id → title of the mods already in mods/.
 */
export async function resolveInstall(
  root: string,
  gameVersion: string,
  installed: ReadonlyMap<string, string>,
  api: ModrinthApi,
): Promise<PlannedMod[]> {
  const planned = new Map<string, PlannedMod | null>();
  const order: PlannedMod[] = [];
  const incompatible: { project: string; by: ModrinthProject }[] = [];

  const choose = async (project: string, pinned: string | null): Promise<ModrinthVersion | null> => {
    if (pinned) {
      const version = await api.version(pinned);
      if (isCompatible(version, gameVersion)) return version;
    }
    return pickVersion(await api.versions(project, gameVersion), gameVersion);
  };

  const visit = async (project: string, pinned: string | null, requiredBy: ModrinthProject | null) => {
    if (planned.has(project) || (requiredBy && installed.has(project))) return;
    planned.set(project, null); // in progress: a dependency cycle ends here
    const info = await api.project(project);
    const version = await choose(project, pinned);
    if (!version) {
      throw requiredBy
        ? new LocalizedError('modDependencyMissing', [info.title, requiredBy.title, gameVersion])
        : new LocalizedError('modNoVersion', [info.title, gameVersion]);
    }
    for (const dependency of version.dependencies) {
      if (dependency.dependency_type !== 'required' && dependency.dependency_type !== 'incompatible') continue;
      const id = dependency.project_id ?? (dependency.version_id ? (await api.version(dependency.version_id)).project_id : null);
      if (!id) continue; // a file outside Modrinth
      if (dependency.dependency_type === 'incompatible') incompatible.push({ project: id, by: info });
      if (dependency.dependency_type === 'required') await visit(id, dependency.version_id, info);
    }
    const mod = { project: info, version, file: primaryFile(version) };
    planned.set(project, mod);
    order.push(mod);
  };

  await visit(root, null, null);
  for (const { project, by } of incompatible) {
    const other = installed.get(project) ?? planned.get(project)?.project.title;
    if (other !== undefined) throw new LocalizedError('modIncompatible', [by.title, other]);
  }
  return order;
}

// --- HTTP -------------------------------------------------------------------------

async function getJson<T>(url: string, signal?: AbortSignal): Promise<T> {
  let response: Response;
  try {
    response = await fetch(url, { signal });
  } catch (error) {
    if (signal?.aborted) throw error;
    throw new LocalizedError('network', [new URL(url).host, error instanceof Error ? error.message : String(error)]);
  }
  if (response.status === 429) throw new LocalizedError('modrinthRateLimited');
  if (!response.ok) throw new LocalizedError('modrinthFailed', [response.status]);
  return (await response.json()) as T;
}

export function searchMods(text: string, gameVersion: string, signal?: AbortSignal): Promise<ModrinthSearch> {
  return getJson<ModrinthSearch>(searchUrl(text, gameVersion), signal);
}

/** Modrinth's API over fetch, each project and version fetched once. */
export function modrinthApi(): ModrinthApi {
  const cache = new Map<string, Promise<unknown>>();
  const cached = <T>(url: string) => {
    let pending = cache.get(url) as Promise<T> | undefined;
    if (!pending) {
      pending = getJson<T>(url);
      cache.set(url, pending);
      pending.catch(() => cache.delete(url));
    }
    return pending;
  };
  return {
    project: (id) => cached(`${MODRINTH_API_URL}/project/${encodeURIComponent(id)}`),
    versions: (project, gameVersion) => cached(projectVersionsUrl(project, gameVersion)),
    version: (id) => cached(`${MODRINTH_API_URL}/version/${encodeURIComponent(id)}`),
  };
}

/** Downloads a mod file and checks it against Modrinth's SHA-512 and SHA-1. */
export async function downloadFile(file: ModrinthFile, signal?: AbortSignal): Promise<Uint8Array<ArrayBuffer>> {
  let response: Response;
  try {
    response = await fetch(file.url, { signal });
  } catch (error) {
    if (signal?.aborted) throw error;
    throw new LocalizedError('network', [new URL(file.url).host, error instanceof Error ? error.message : String(error)]);
  }
  if (!response.ok) throw new LocalizedError('modDownloadFailed', [file.filename, `HTTP ${response.status}`]);
  const data = new Uint8Array(await response.arrayBuffer());
  const checks: [AlgorithmIdentifier, string | undefined][] = [
    ['SHA-512', file.hashes.sha512],
    ['SHA-1', file.hashes.sha1],
  ];
  if (!checks.some(([, hash]) => hash)) throw new LocalizedError('modDownloadFailed', [file.filename, 'no hash']);
  for (const [algorithm, expected] of checks) {
    if (expected && (await digestHex(algorithm, data)) !== expected.toLowerCase()) {
      throw new LocalizedError('modDownloadFailed', [file.filename, `${String(algorithm)} mismatch`]);
    }
  }
  return data;
}

export async function digestHex(algorithm: AlgorithmIdentifier, data: Uint8Array<ArrayBuffer>): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest(algorithm, data));
  return Array.from(digest, (b) => b.toString(16).padStart(2, '0')).join('');
}
