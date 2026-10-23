import { isHostNativeClassifier, mavenPath, parseMaven } from './maven';
import { rulesAllow, type Platform } from './rules';
import type { Artifact, Library, VersionJson } from './types';

export interface ResolvedLibrary {
  name: string;
  /** Path relative to the libraries directory. */
  path: string;
  artifact: Artifact;
}

export interface ResolvedLibraries {
  classpath: ResolvedLibrary[];
  /** Host-native jars we skip; their native code must be provided by our JVM as WASM. */
  hostNatives: string[];
}

export function resolveLibraries(version: VersionJson, platform: Platform): ResolvedLibraries {
  const classpath: ResolvedLibrary[] = [];
  const hostNatives: string[] = [];
  const seen = new Set<string>();

  for (const lib of version.libraries) {
    const coordinate = parseMaven(lib.name);
    if (lib.natives || isHostNativeClassifier(coordinate.classifier)) {
      hostNatives.push(lib.name);
      continue;
    }
    if (!rulesAllow(lib.rules, platform)) continue;
    const artifact = lib.downloads?.artifact ?? repositoryArtifact(lib);
    if (!artifact) continue;
    const path = artifact.path ?? mavenPath(coordinate);
    if (seen.has(path)) continue;
    seen.add(path);
    classpath.push({ name: lib.name, path, artifact });
  }
  return { classpath, hostNatives };
}

/** A library's jar in a Maven repository (`url` of Fabric's libraries). */
export function repositoryUrl(repository: string, name: string): string {
  return `${repository.replace(/\/?$/, '/')}${mavenPath(parseMaven(name))}`;
}

/** The artifact of a library given as repository + coordinate; its hash and size must be known by now. */
function repositoryArtifact(lib: Library): Artifact | null {
  if (!lib.url) return null;
  if (!lib.sha1 || lib.size === undefined) throw new Error(`${lib.name}: no SHA-1 or size`);
  return { path: mavenPath(parseMaven(lib.name)), url: repositoryUrl(lib.url, lib.name), sha1: lib.sha1, size: lib.size };
}
