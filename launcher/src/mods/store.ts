// The player's mods: the jars in <gameDir>/mods in OPFS (the game sees them in
// /minecraft/mods, where Fabric Loader looks), plus a small manifest of the
// ones installed from Modrinth (project, version, title), so the list can name
// them and the search can mark them installed.

import { layout } from '../mojang/layout';
import { listFiles, readJson, removeFile, writeFile } from '../storage/opfs';

export interface ModrinthSource {
  projectId: string;
  versionId: string;
  slug: string;
  title: string;
  versionNumber: string;
  gameVersions: string[];
  iconUrl: string | null;
}

interface Manifest {
  /** By file name in mods/. */
  mods: Record<string, ModrinthSource>;
}

export interface InstalledMod {
  file: string;
  size: number;
  /** Set for mods installed from Modrinth. */
  modrinth?: ModrinthSource;
}

/** A name that is safe as one file in mods/ and that Fabric loads. */
export function isModFileName(name: string): boolean {
  return /\.jar$/i.test(name) && !/[/\\\0]/.test(name) && !name.startsWith('.') && name.length <= 200;
}

async function readManifest(): Promise<Manifest> {
  const manifest = await readJson<Manifest>(layout.modsManifest).catch(() => null);
  return { mods: manifest?.mods ?? {} };
}

const writeManifest = (manifest: Manifest) => writeFile(layout.modsManifest, JSON.stringify(manifest, null, 1));

/** The jars in mods/, by name. Manifest entries whose file is gone are forgotten. */
export async function listMods(): Promise<InstalledMod[]> {
  const [files, manifest] = await Promise.all([listFiles(layout.mods), readManifest()]);
  const jars = files.filter((file) => isModFileName(file.name));
  const names = new Set(jars.map((file) => file.name));
  if (Object.keys(manifest.mods).some((name) => !names.has(name))) {
    for (const name of Object.keys(manifest.mods)) if (!names.has(name)) delete manifest.mods[name];
    await writeManifest(manifest);
  }
  return jars
    .map((file) => ({ file: file.name, size: file.size, modrinth: manifest.mods[file.name] }))
    .sort((a, b) => (a.modrinth?.title ?? a.file).localeCompare(b.modrinth?.title ?? b.file));
}

/**
 * Saves a jar into mods/. A Modrinth install replaces the project's previous
 * file; an upload under an existing name replaces that file and forgets its
 * Modrinth origin.
 */
export async function addMod(file: string, data: Blob | Uint8Array<ArrayBuffer>, source?: ModrinthSource) {
  if (!isModFileName(file)) throw new Error(`Not a mod file name: ${file}`);
  await writeFile(layout.mod(file), data);
  const manifest = await readManifest();
  if (source) {
    for (const [name, other] of Object.entries(manifest.mods)) {
      if (other.projectId === source.projectId && name !== file) {
        await removeFile(layout.mod(name));
        delete manifest.mods[name];
      }
    }
    manifest.mods[file] = source;
  } else {
    delete manifest.mods[file];
  }
  await writeManifest(manifest);
}

export async function removeMod(file: string) {
  if (!isModFileName(file)) return;
  await removeFile(layout.mod(file));
  const manifest = await readManifest();
  if (file in manifest.mods) {
    delete manifest.mods[file];
    await writeManifest(manifest);
  }
}
