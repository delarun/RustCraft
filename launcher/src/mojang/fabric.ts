// Fabric Loader (fabricmc.net): its meta server lists the loader builds for a
// game version and serves a launcher profile per build, a partial version JSON
// that inherits from the vanilla one ("inheritsFrom"). Merged with it, the
// profile runs Fabric's Knot launcher, which loads the jars in <gameDir>/mods.
// meta.fabricmc.net and maven.fabricmc.net send `Access-Control-Allow-Origin: *`,
// so both are fetched directly. Minecraft 26.x is not obfuscated: the
// "intermediary" mappings are 0.0.0 and nothing is remapped.

import { parseMaven } from './maven';
import type { Argument, Library, VersionJson, VersionType } from './types';

export const FABRIC_META_URL = 'https://meta.fabricmc.net/v2';

/** An entry of /versions/loader/<game version> (newest first). */
export interface FabricLoaderEntry {
  loader: { version: string; stable: boolean; maven: string };
  intermediary?: { version: string };
}

/** /versions/loader/<game>/<loader>/profile/json. */
export interface FabricProfile {
  id: string;
  inheritsFrom: string;
  type?: VersionType;
  mainClass: string;
  arguments?: { game?: Argument[]; jvm?: Argument[] };
  /** Maven repository form: name + url, sha1 and size for some. */
  libraries: Library[];
}

export const fabricLoadersUrl = (game: string) => `${FABRIC_META_URL}/versions/loader/${encodeURIComponent(game)}`;
export const fabricProfileUrl = (game: string, loader: string) =>
  `${fabricLoadersUrl(game)}/${encodeURIComponent(loader)}/profile/json`;
/** The profile's id, also its directory under versions/ (like the official launcher's). */
export const fabricProfileId = (game: string, loader: string) => `fabric-loader-${loader}-${game}`;

/** The newest stable loader build, else the newest build; null when Fabric does not support the version. */
export function pickLoader(entries: readonly FabricLoaderEntry[]): string | null {
  return (entries.find((entry) => entry.loader.stable) ?? entries[0])?.loader.version ?? null;
}

/** group:artifact[:classifier], the identity of a library regardless of its version. */
export function libraryKey(name: string): string {
  const { group, artifact, classifier } = parseMaven(name);
  return [group, artifact, classifier].filter(Boolean).join(':');
}

/**
 * The version a profile describes: Fabric's main class and libraries (which
 * replace vanilla's of the same group:artifact, e.g. a newer ASM), both
 * argument lists extended, and everything else (client jar, assets, Java
 * runtime, logging) from vanilla. As in Mojang's launcher, the inheriting
 * profile's libraries come first on the classpath.
 */
export function mergeProfile(vanilla: VersionJson, profile: FabricProfile): VersionJson {
  if (profile.inheritsFrom !== vanilla.id) {
    throw new Error(`${profile.id} inherits from ${profile.inheritsFrom}, not ${vanilla.id}`);
  }
  const replaced = new Set(profile.libraries.map((lib) => libraryKey(lib.name)));
  const merged: VersionJson = {
    ...vanilla,
    id: profile.id,
    jar: vanilla.jar ?? vanilla.id,
    type: profile.type ?? vanilla.type,
    mainClass: profile.mainClass,
    libraries: [...profile.libraries, ...vanilla.libraries.filter((lib) => !replaced.has(libraryKey(lib.name)))],
  };
  if (vanilla.arguments) {
    merged.arguments = {
      ...vanilla.arguments,
      game: [...vanilla.arguments.game, ...(profile.arguments?.game ?? [])],
      jvm: [...vanilla.arguments.jvm, ...(profile.arguments?.jvm ?? [])],
    };
  }
  return merged;
}
