// Which versions our VM can run. It implements the natives of LWJGL 3.4's SDL
// bindings (windowing, input) and ships no GLFW, and it runs the Java 25 class
// library, so only versions that use lwjgl-sdl on java-runtime-epsilon qualify.

import type { Text } from '../i18n/error';
import type { VersionJson, VersionManifestEntry } from './types';

/** Played through to the title screen and in-game on our VM. */
export const TESTED_VERSIONS = ['26.3'];
/** The first version on LWJGL's SDL bindings (26.3-snapshot-4). */
const FIRST_SDL_RELEASE_TIME = '2026-07-16T13:59:30+00:00';
const SUPPORTED_JAVA = 25;

export type Compat = { level: 'tested' | 'untested' | 'unsupported'; reason: Text };

/** A guess from the manifest alone (no version JSON yet): by release date. */
export function manifestCompat(entry: VersionManifestEntry): Compat['level'] {
  if (TESTED_VERSIONS.includes(entry.id)) return 'tested';
  return Date.parse(entry.releaseTime) >= Date.parse(FIRST_SDL_RELEASE_TIME) ? 'untested' : 'unsupported';
}

export function versionCompat(version: VersionJson): Compat {
  const java = version.javaVersion?.majorVersion ?? 8;
  if (java !== SUPPORTED_JAVA) {
    return { level: 'unsupported', reason: { key: 'compatJava', args: [java, SUPPORTED_JAVA] } };
  }
  const sdl = version.libraries.some((lib) => lib.name.startsWith('org.lwjgl:lwjgl-sdl:'));
  if (!sdl) return { level: 'unsupported', reason: { key: 'compatGlfw' } };
  if (TESTED_VERSIONS.includes(version.id)) return { level: 'tested', reason: { key: 'compatTested' } };
  return { level: 'untested', reason: { key: 'compatUntested' } };
}
