import { describe, expect, it } from 'vitest';
import { collectDownloads } from '../src/mojang/downloads';
import { fabricLoadersUrl, fabricProfileId, fabricProfileUrl, libraryKey, mergeProfile, pickLoader, type FabricProfile } from '../src/mojang/fabric';
import { buildLaunchPlan } from '../src/mojang/launchPlan';
import { repositoryUrl, resolveLibraries } from '../src/mojang/libraries';
import { BROWSER_PLATFORM } from '../src/mojang/rules';
import { vmProcess } from '../src/mojang/vm';
import { assets, version } from './fixture';

const MAVEN = 'https://maven.fabricmc.net/';

// meta.fabricmc.net's profile for 26.3 (trimmed; hashes shortened), after the
// installer filled in fabric-loader's missing SHA-1 and size.
const profile: FabricProfile = {
  id: 'fabric-loader-0.20.1-26.3',
  inheritsFrom: '26.3',
  type: 'release',
  mainClass: 'net.fabricmc.loader.impl.launch.knot.KnotClient',
  arguments: { game: [], jvm: ['-DFabricMcEmu= net.minecraft.client.main.Main '] },
  libraries: [
    { name: 'org.ow2.asm:asm:9.10.1', url: MAVEN, sha1: 'ada2141c', size: 126151 },
    { name: 'net.fabricmc:sponge-mixin:0.17.5+mixin.0.8.7', url: MAVEN, sha1: '0cef8519', size: 1551593 },
    { name: 'net.fabricmc:fabric-loader:0.20.1', url: MAVEN, sha1: '7c7f9831', size: 1988880 },
  ],
};

// A vanilla version that (unlike 26.3) ships an older ASM.
const vanilla = {
  ...version,
  libraries: [
    ...version.libraries,
    {
      name: 'org.ow2.asm:asm:9.3',
      downloads: { artifact: { path: 'org/ow2/asm/asm/9.3/asm-9.3.jar', sha1: 'old', size: 1, url: 'https://libraries.minecraft.net/org/ow2/asm/asm/9.3/asm-9.3.jar' } },
    },
  ],
};

const options = { username: 'Steve', uuid: '0', accessToken: 't', xuid: '0', userType: 'msa' as const, width: 854, height: 480 };

describe('Fabric meta', () => {
  it('builds the meta URLs and the profile id', () => {
    expect(fabricLoadersUrl('26.3')).toBe('https://meta.fabricmc.net/v2/versions/loader/26.3');
    expect(fabricProfileUrl('26.3', '0.20.1')).toBe('https://meta.fabricmc.net/v2/versions/loader/26.3/0.20.1/profile/json');
    expect(fabricProfileId('26.3', '0.20.1')).toBe('fabric-loader-0.20.1-26.3');
  });

  it('picks the newest stable loader', () => {
    const entry = (version: string, stable: boolean) => ({ loader: { version, stable, maven: `net.fabricmc:fabric-loader:${version}` } });
    expect(pickLoader([entry('0.21.0-beta.1', false), entry('0.20.1', true), entry('0.20.0', true)])).toBe('0.20.1');
    expect(pickLoader([entry('0.1.0', false)])).toBe('0.1.0');
    expect(pickLoader([])).toBeNull();
  });
});

describe('Fabric libraries', () => {
  it('lives at the repository URL plus the maven path', () => {
    expect(repositoryUrl(MAVEN, 'net.fabricmc:sponge-mixin:0.17.5+mixin.0.8.7')).toBe(
      'https://maven.fabricmc.net/net/fabricmc/sponge-mixin/0.17.5+mixin.0.8.7/sponge-mixin-0.17.5+mixin.0.8.7.jar',
    );
    expect(repositoryUrl('https://maven.fabricmc.net', 'org.ow2.asm:asm:9.10.1')).toBe(
      'https://maven.fabricmc.net/org/ow2/asm/asm/9.10.1/asm-9.10.1.jar',
    );
  });

  it('resolves to artifacts with the profile hashes', () => {
    const { classpath } = resolveLibraries({ ...version, libraries: profile.libraries }, BROWSER_PLATFORM);
    expect(classpath[2]).toEqual({
      name: 'net.fabricmc:fabric-loader:0.20.1',
      path: 'net/fabricmc/fabric-loader/0.20.1/fabric-loader-0.20.1.jar',
      artifact: {
        path: 'net/fabricmc/fabric-loader/0.20.1/fabric-loader-0.20.1.jar',
        url: 'https://maven.fabricmc.net/net/fabricmc/fabric-loader/0.20.1/fabric-loader-0.20.1.jar',
        sha1: '7c7f9831',
        size: 1988880,
      },
    });
  });

  it('refuses a library whose hash was never filled in', () => {
    const incomplete = { ...version, libraries: [{ name: 'net.fabricmc:fabric-loader:0.20.1', url: MAVEN }] };
    expect(() => resolveLibraries(incomplete, BROWSER_PLATFORM)).toThrow(/fabric-loader.*SHA-1/);
  });

  it('identifies libraries by group, artifact and classifier', () => {
    expect(libraryKey('org.ow2.asm:asm:9.10.1')).toBe('org.ow2.asm:asm');
    expect(libraryKey('org.lwjgl:lwjgl:3.4.3:natives-linux')).toBe('org.lwjgl:lwjgl:natives-linux');
  });
});

describe('Fabric profile merge', () => {
  const merged = mergeProfile(vanilla, profile);

  it('takes the main class and id from the profile, the rest from vanilla', () => {
    expect(merged.id).toBe('fabric-loader-0.20.1-26.3');
    expect(merged.jar).toBe('26.3');
    expect(merged.mainClass).toBe('net.fabricmc.loader.impl.launch.knot.KnotClient');
    expect(merged.assetIndex).toBe(vanilla.assetIndex);
    expect(merged.downloads).toBe(vanilla.downloads);
    expect(merged.javaVersion).toEqual({ component: 'java-runtime-epsilon', majorVersion: 25 });
    expect(merged.logging).toBe(vanilla.logging);
  });

  it('puts Fabric libraries first and lets them replace vanilla ones', () => {
    const names = merged.libraries.map((lib) => lib.name);
    expect(names.slice(0, 3)).toEqual(profile.libraries.map((lib) => lib.name));
    expect(names).not.toContain('org.ow2.asm:asm:9.3');
    expect(names).toContain('com.google.code.gson:gson:2.14.0');
  });

  it('appends the profile arguments', () => {
    expect(merged.arguments!.jvm.at(-1)).toBe('-DFabricMcEmu= net.minecraft.client.main.Main ');
    expect(merged.arguments!.jvm.slice(0, -1)).toEqual(vanilla.arguments!.jvm);
    expect(merged.arguments!.game).toEqual(vanilla.arguments!.game);
    expect(merged.arguments!['default-user-jvm']).toBe(vanilla.arguments!['default-user-jvm']);
  });

  it('rejects a profile for another version', () => {
    expect(() => mergeProfile({ ...vanilla, id: '26.2' }, profile)).toThrow(/inherits from 26.3/);
  });

  it('launches Knot with Fabric on the classpath and the vanilla client jar', () => {
    const plan = buildLaunchPlan(merged, options);
    expect(plan.versionId).toBe('fabric-loader-0.20.1-26.3');
    expect(plan.classpath).toEqual([
      '/minecraft/libraries/org/ow2/asm/asm/9.10.1/asm-9.10.1.jar',
      '/minecraft/libraries/net/fabricmc/sponge-mixin/0.17.5+mixin.0.8.7/sponge-mixin-0.17.5+mixin.0.8.7.jar',
      '/minecraft/libraries/net/fabricmc/fabric-loader/0.20.1/fabric-loader-0.20.1.jar',
      '/minecraft/libraries/com/google/code/gson/gson/2.14.0/gson-2.14.0.jar',
      '/minecraft/libraries/org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3.jar',
      '/minecraft/versions/26.3/26.3.jar',
    ]);
    const args = vmProcess(plan, { heapMb: 1024 }).args;
    const main = args.indexOf('net.fabricmc.loader.impl.launch.knot.KnotClient');
    expect(main).toBeGreaterThan(0);
    expect(args.slice(0, main)).toContain('-DFabricMcEmu= net.minecraft.client.main.Main ');
    // The game directory, whose mods/ Fabric loads.
    expect(args[args.indexOf('--gameDir') + 1]).toBe('/minecraft');
  });

  it('downloads the Fabric libraries from Fabric’s Maven and the vanilla client', () => {
    const jobs = collectDownloads(merged, assets, BROWSER_PLATFORM, () => false);
    expect(jobs[0]).toMatchObject({ path: 'versions/26.3/26.3.jar', sha1: 'client-sha1' });
    expect(jobs.slice(1, 4)).toEqual(
      profile.libraries.map((lib) => ({
        url: repositoryUrl(MAVEN, lib.name),
        path: `libraries/${repositoryUrl(MAVEN, lib.name).slice(MAVEN.length)}`,
        size: lib.size,
        sha1: lib.sha1,
        verify: 'sha1',
      })),
    );
  });
});
