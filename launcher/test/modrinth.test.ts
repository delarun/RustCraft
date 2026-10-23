import { describe, expect, it } from 'vitest';
import { LocalizedError } from '../src/i18n/error';
import {
  pickVersion,
  primaryFile,
  projectVersionsUrl,
  resolveInstall,
  searchUrl,
  type ModrinthApi,
  type ModrinthDependency,
  type ModrinthProject,
  type ModrinthVersion,
} from '../src/mods/modrinth';
import { isModFileName } from '../src/mods/store';

const GAME = '26.3';

let published = 0;
function v(project: string, number: string, extra: Partial<ModrinthVersion> = {}): ModrinthVersion {
  published++;
  return {
    id: `${project}-${number}`,
    project_id: project,
    name: number,
    version_number: number,
    version_type: 'release',
    date_published: `2026-10-${String(published).padStart(2, '0')}T00:00:00Z`,
    game_versions: [GAME],
    loaders: ['fabric'],
    files: [
      { url: `https://cdn.modrinth.com/data/${project}/${number}-sources.jar`, filename: `${project}-${number}-sources.jar`, primary: false, size: 1, hashes: { sha1: 'b' } },
      { url: `https://cdn.modrinth.com/data/${project}/${number}.jar`, filename: `${project}-${number}.jar`, primary: true, size: 2, hashes: { sha1: 'a', sha512: 'aa' } },
    ],
    dependencies: [],
    ...extra,
  };
}

const dep = (project_id: string | null, dependency_type: ModrinthDependency['dependency_type'], version_id: string | null = null): ModrinthDependency => ({
  project_id,
  version_id,
  dependency_type,
});

/** An in-memory Modrinth: projects and their versions, recording requests. */
function fakeApi(versions: ModrinthVersion[]): ModrinthApi & { requests: string[] } {
  const requests: string[] = [];
  const projects = new Set(versions.map((version) => version.project_id));
  return {
    requests,
    async project(id) {
      requests.push(`project ${id}`);
      if (!projects.has(id)) throw new Error(`no project ${id}`);
      return { id, slug: id.toLowerCase(), title: `Title ${id}`, icon_url: null } satisfies ModrinthProject;
    },
    async versions(project, gameVersion) {
      requests.push(`versions ${project}`);
      // The API filters by loader and game version too.
      return versions.filter((x) => x.project_id === project && x.game_versions.includes(gameVersion)).reverse();
    },
    async version(id) {
      requests.push(`version ${id}`);
      const version = versions.find((x) => x.id === id);
      if (!version) throw new Error(`no version ${id}`);
      return version;
    },
  };
}

describe('Modrinth URLs', () => {
  it('searches Fabric mods for the game version', () => {
    const url = new URL(searchUrl('mod menu', GAME));
    expect(url.origin + url.pathname).toBe('https://api.modrinth.com/v2/search');
    expect(url.searchParams.get('query')).toBe('mod menu');
    expect(JSON.parse(url.searchParams.get('facets')!)).toEqual([['project_type:mod'], ['categories:fabric'], ['versions:26.3']]);
    expect(url.searchParams.get('limit')).toBe('20');
    expect(new URL(searchUrl('', GAME)).searchParams.get('index')).toBe('downloads');
  });

  it('lists a project’s versions for Fabric and the game version', () => {
    const url = new URL(projectVersionsUrl('fabric-api', GAME));
    expect(url.pathname).toBe('/v2/project/fabric-api/version');
    expect(JSON.parse(url.searchParams.get('loaders')!)).toEqual(['fabric']);
    expect(JSON.parse(url.searchParams.get('game_versions')!)).toEqual(['26.3']);
  });
});

describe('Modrinth version choice', () => {
  const old = v('A', '1.0.0');
  const current = v('A', '1.1.0');
  const beta = v('A', '1.2.0-beta', { version_type: 'beta' });
  const forge = v('A', '1.3.0', { loaders: ['forge'] });
  const other = v('A', '1.4.0', { game_versions: ['26.2'] });

  it('takes the newest release for Fabric and the game version', () => {
    expect(pickVersion([old, current, beta, forge, other], GAME)?.version_number).toBe('1.1.0');
  });

  it('falls back to the newest beta when there is no release', () => {
    expect(pickVersion([beta, v('A', '0.9-alpha', { version_type: 'alpha' })], GAME)?.version_number).toBe('1.2.0-beta');
    expect(pickVersion([forge, other], GAME)).toBeNull();
  });

  it('installs the primary file', () => {
    expect(primaryFile(current).filename).toBe('A-1.1.0.jar');
    expect(primaryFile({ ...current, files: current.files.map((f) => ({ ...f, primary: false })) }).filename).toBe('A-1.1.0-sources.jar');
    expect(() => primaryFile({ ...current, files: [] })).toThrow(LocalizedError);
  });
});

describe('Modrinth dependencies', () => {
  const fabricApi = v('P7dR8mSH', '0.165.0+26.3');
  const placeholder = v('eXts2L7r', '3.0.0', { dependencies: [dep('P7dR8mSH', 'required')] });
  const modMenu = v('mOgUt4GM', '21.0.0', {
    dependencies: [dep('P7dR8mSH', 'required'), dep('eXts2L7r', 'required'), dep('OPTIONAL', 'optional'), dep(null, 'embedded', 'x')],
  });

  it('installs required dependencies recursively, dependencies first, each once', async () => {
    const plan = await resolveInstall('mOgUt4GM', GAME, new Map(), fakeApi([fabricApi, placeholder, modMenu]));
    expect(plan.map((mod) => mod.file.filename)).toEqual(['P7dR8mSH-0.165.0+26.3.jar', 'eXts2L7r-3.0.0.jar', 'mOgUt4GM-21.0.0.jar']);
    expect(plan[2]!.project.title).toBe('Title mOgUt4GM');
  });

  it('skips dependencies that are already installed', async () => {
    const api = fakeApi([fabricApi, placeholder, modMenu]);
    const plan = await resolveInstall('mOgUt4GM', GAME, new Map([['P7dR8mSH', 'Fabric API']]), api);
    expect(plan.map((mod) => mod.project.id)).toEqual(['eXts2L7r', 'mOgUt4GM']);
    expect(api.requests).not.toContain('versions P7dR8mSH');
  });

  it('survives dependency cycles', async () => {
    const a = v('A', '1', { dependencies: [dep('B', 'required')] });
    const b = v('B', '1', { dependencies: [dep('A', 'required')] });
    const plan = await resolveInstall('A', GAME, new Map(), fakeApi([a, b]));
    expect(plan.map((mod) => mod.project.id)).toEqual(['B', 'A']);
  });

  it('uses a pinned dependency version when it fits, the newest one otherwise', async () => {
    const lib1 = v('L', '1.0');
    const lib2 = v('L', '2.0');
    const libOld = v('L', '0.5', { game_versions: ['1.21.11'] });
    const pinned = v('A', '1', { dependencies: [dep('L', 'required', lib1.id)] });
    expect((await resolveInstall('A', GAME, new Map(), fakeApi([lib1, lib2, pinned])))[0]!.version.version_number).toBe('1.0');
    const stale = v('B', '1', { dependencies: [dep('L', 'required', libOld.id)] });
    expect((await resolveInstall('B', GAME, new Map(), fakeApi([lib1, lib2, libOld, stale])))[0]!.version.version_number).toBe('2.0');
    // Given only by version: the project comes from the version.
    const byVersion = v('C', '1', { dependencies: [dep(null, 'required', lib1.id)] });
    expect((await resolveInstall('C', GAME, new Map(), fakeApi([lib1, lib2, byVersion]))).map((mod) => mod.version.id)).toEqual([lib1.id, byVersion.id]);
  });

  it('fails when a required dependency has no version for the game', async () => {
    const lib = v('L', '1', { game_versions: ['26.2'] });
    const mod = v('A', '1', { dependencies: [dep('L', 'required')] });
    await expect(resolveInstall('A', GAME, new Map(), fakeApi([lib, mod]))).rejects.toMatchObject({
      key: 'modDependencyMissing',
      args: ['Title L', 'Title A', GAME],
    });
    await expect(resolveInstall('L', GAME, new Map(), fakeApi([lib]))).rejects.toMatchObject({ key: 'modNoVersion' });
  });

  it('refuses a mod that is incompatible with an installed or planned one', async () => {
    const sodiumLike = v('A', '1', { dependencies: [dep('OF', 'incompatible')] });
    await expect(resolveInstall('A', GAME, new Map([['OF', 'OptiFabric']]), fakeApi([sodiumLike]))).rejects.toMatchObject({
      key: 'modIncompatible',
      args: ['Title A', 'OptiFabric'],
    });
    expect(await resolveInstall('A', GAME, new Map(), fakeApi([sodiumLike]))).toHaveLength(1);
  });
});

describe('mod files', () => {
  it('accepts only plain .jar names', () => {
    expect(isModFileName('fabric-api-0.165.0+26.3.jar')).toBe(true);
    expect(isModFileName('Mod.JAR')).toBe(true);
    expect(isModFileName('rustcraft-mods.json')).toBe(false);
    expect(isModFileName('../evil.jar')).toBe(false);
    expect(isModFileName('.hidden.jar')).toBe(false);
    expect(isModFileName('a\\b.jar')).toBe(false);
  });
});
