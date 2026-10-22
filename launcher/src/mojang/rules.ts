import type { Rule } from './types';

/**
 * The platform our JVM pretends to be. Java-side code (Minecraft, LWJGL, Netty)
 * sees `os.name=Linux`, but all native code is our own WASM, so the arch matches
 * none of Mojang's arch rules and OS-specific native jars are filtered separately.
 */
export interface Platform {
  os: 'linux' | 'windows' | 'osx';
  arch: string;
  osVersion: string;
}

export const BROWSER_PLATFORM: Platform = { os: 'linux', arch: 'wasm32', osVersion: '' };

export type Features = Record<string, boolean>;

/** Mojang semantics: no rules = allowed; otherwise the last matching rule wins, default deny. */
export function rulesAllow(rules: Rule[] | undefined, platform: Platform, features: Features = {}): boolean {
  if (!rules || rules.length === 0) return true;
  let allowed = false;
  for (const rule of rules) {
    if (ruleMatches(rule, platform, features)) allowed = rule.action === 'allow';
  }
  return allowed;
}

function ruleMatches(rule: Rule, platform: Platform, features: Features): boolean {
  const os = rule.os;
  if (os) {
    if (os.name !== undefined && os.name !== platform.os) return false;
    if (os.arch !== undefined && os.arch !== platform.arch) return false;
    if (os.version !== undefined && !new RegExp(os.version).test(platform.osVersion)) return false;
    if (os.versionRange) {
      if (!platform.osVersion) return false;
      const { min, max } = os.versionRange;
      if (min !== undefined && compareVersions(platform.osVersion, min) < 0) return false;
      if (max !== undefined && compareVersions(platform.osVersion, max) >= 0) return false;
    }
  }
  if (rule.features) {
    for (const [name, expected] of Object.entries(rule.features)) {
      if ((features[name] ?? false) !== expected) return false;
    }
  }
  return true;
}

export function compareVersions(a: string, b: string): number {
  const pa = a.split('.').map(Number);
  const pb = b.split('.').map(Number);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa[i] ?? 0) - (pb[i] ?? 0);
    if (d !== 0) return Math.sign(d);
  }
  return 0;
}
