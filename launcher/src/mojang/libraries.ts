import { isHostNativeClassifier, mavenPath, parseMaven } from './maven';
import { rulesAllow, type Platform } from './rules';
import type { Artifact, VersionJson } from './types';

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
    const artifact = lib.downloads?.artifact;
    if (!artifact) continue;
    const path = artifact.path ?? mavenPath(coordinate);
    if (seen.has(path)) continue;
    seen.add(path);
    classpath.push({ name: lib.name, path, artifact });
  }
  return { classpath, hostNatives };
}
