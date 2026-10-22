import { describe, expect, it } from 'vitest';
import { versionCompat, manifestCompat } from '../src/mojang/compat';
import { collectDownloads, isEagerAsset } from '../src/mojang/downloads';
import { buildLaunchPlan } from '../src/mojang/launchPlan';
import { BROWSER_PLATFORM } from '../src/mojang/rules';
import { isVmRuntimeFile, runtimeDownloads, selectRuntime, type RuntimeIndex, type RuntimeManifest } from '../src/mojang/runtime';
import { STUB_NATIVES, vmJvmArgs, vmProcess } from '../src/mojang/vm';
import { assets, version } from './fixture';

const raw = (name: string) => ({ type: 'file' as const, executable: false, downloads: { raw: { sha1: `sha-${name}`, size: 1, url: `https://piston-data.mojang.com/${name}` } } });

describe('java runtime', () => {
  const index: RuntimeIndex = {
    linux: {
      'java-runtime-epsilon': [
        { availability: { group: 1, progress: 100 }, manifest: { sha1: 'm', size: 1, url: 'https://piston-meta.mojang.com/m.json' }, version: { name: '25.0.1', released: '' } },
      ],
    },
  };

  it('selects the linux build of a component', () => {
    expect(selectRuntime(index, 'java-runtime-epsilon').version.name).toBe('25.0.1');
    expect(() => selectRuntime(index, 'java-runtime-delta')).toThrow();
  });

  it('keeps the class library and configuration, not host binaries', () => {
    expect(['lib/modules', 'conf/security/java.security', 'lib/tzdb.dat', 'lib/security/cacerts', 'release'].every(isVmRuntimeFile)).toBe(true);
    expect(['bin/java', 'lib/libjava.so', 'lib/server/libjvm.so', 'lib/jspawnhelper', 'legal/java.base/LICENSE', 'lib/ct.sym'].some(isVmRuntimeFile)).toBe(false);
    const manifest: RuntimeManifest = {
      files: { bin: { type: 'directory' }, 'bin/java': raw('java'), 'lib/modules': raw('modules'), 'lib/jvm.cfg': raw('cfg'), 'legal/x': { type: 'link', target: '../y' } },
    };
    const jobs = runtimeDownloads('java-runtime-epsilon', manifest);
    expect(jobs.map((j) => j.path)).toEqual(['runtime/java-runtime-epsilon/lib/modules', 'runtime/java-runtime-epsilon/lib/jvm.cfg']);
    expect(jobs[0]).toMatchObject({ sha1: 'sha-modules', verify: 'sha1' });
  });

  it('fails without lib/modules', () => {
    expect(() => runtimeDownloads('x', { files: { 'lib/jvm.cfg': raw('cfg') } })).toThrow(/lib\/modules/);
  });
});

describe('assets', () => {
  it('defers sounds and translations to first use', () => {
    expect(isEagerAsset('minecraft/sounds/random/click.ogg')).toBe(false);
    expect(isEagerAsset('minecraft/lang/de_de.json')).toBe(false);
    expect(isEagerAsset('minecraft/lang/en_us.json')).toBe(true);
    expect(isEagerAsset('minecraft/sounds.json')).toBe(true);
    expect(isEagerAsset('icons/icon_32x32.png')).toBe(true);
    expect(collectDownloads(version, assets, BROWSER_PLATFORM, isEagerAsset).some((j) => j.path.startsWith('assets/objects'))).toBe(false);
  });
});

describe('compatibility', () => {
  it('needs Java 25 and LWJGL SDL', () => {
    const sdl = { ...version, libraries: [...version.libraries, { name: 'org.lwjgl:lwjgl-sdl:3.4.3' }] };
    expect(versionCompat(sdl).level).toBe('tested');
    expect(versionCompat({ ...sdl, id: '26.4-snapshot-1' }).level).toBe('untested');
    expect(versionCompat(version).level).toBe('unsupported'); // GLFW only
    expect(versionCompat({ ...sdl, javaVersion: { component: 'java-runtime-delta', majorVersion: 21 } }).level).toBe('unsupported');
  });

  it('guesses from the release date', () => {
    const entry = { id: 'x', type: 'release' as const, url: '', sha1: '', time: '', complianceLevel: 1 };
    expect(manifestCompat({ ...entry, id: '26.3', releaseTime: '2026-09-15T11:23:02+00:00' })).toBe('tested');
    expect(manifestCompat({ ...entry, releaseTime: '2026-10-20T12:15:06+00:00' })).toBe('untested');
    expect(manifestCompat({ ...entry, releaseTime: '2025-12-09T12:00:00+00:00' })).toBe('unsupported');
  });
});

describe('VM process', () => {
  const plan = buildLaunchPlan(version, {
    username: 'Notch',
    uuid: '069a79f4-44e9-4726-a5be-fca90e38aaf5',
    accessToken: 'mc-token',
    xuid: '2535',
    userType: 'msa',
    width: 1280,
    height: 720,
  });

  it('keeps only the JVM flags rjava understands', () => {
    expect(
      vmJvmArgs([
        '-XX:StackShadowPages=32', '--enable-native-access=ALL-UNNAMED', '--add-exports', 'java.base/jdk.internal.misc=ALL-UNNAMED',
        '-Xss1M', '-Djava.library.path=/x', '-Dlog4j.configurationFile=/y', '-Dminecraft.launcher.brand=b', '-cp', 'a.jar:b.jar',
      ]),
    ).toEqual(['-Dminecraft.launcher.brand=b', '-cp', 'a.jar:b.jar']);
  });

  it('runs the game from OPFS with Mojang\'s runtime and stub natives', () => {
    const config = vmProcess(plan, { heapMb: 1024 });
    const args = config.args;
    expect(args.slice(0, 4)).toEqual(['rjava', '--modules', '/minecraft/runtime/java-runtime-epsilon/lib/modules', '-Xmx1024m']);
    expect(args).toContain('-Dorg.lwjgl.librarypath=/natives');
    expect(args).toContain('-Djava.library.path=/natives');
    expect(args).toContain('-Dorg.lwjgl.system.allocator=system');
    expect(args).toContain('-Djdk.util.jar.enableMultiRelease=false');
    expect(args.filter((a) => a.startsWith('-Djava.library.path'))).toHaveLength(1);
    const main = args.indexOf('net.minecraft.client.main.Main');
    expect(args.slice(4, main).every((a, i, all) => a.startsWith('-D') || a === '-cp' || all[i - 1] === '-cp')).toBe(true);
    const game = args.slice(main + 1);
    expect(game.slice(game.indexOf('--accessToken'), game.indexOf('--accessToken') + 2)).toEqual(['--accessToken', 'mc-token']);
    expect(game.slice(game.indexOf('--uuid'), game.indexOf('--uuid') + 2)).toEqual(['--uuid', '069a79f4-44e9-4726-a5be-fca90e38aaf5']);
    expect(game).toContain('--gameDir');
    expect(game[game.indexOf('--gameDir') + 1]).toBe('/minecraft');
    expect(config.netBridge).toBe('ws://127.0.0.1:25580');
    expect(vmProcess(plan, { heapMb: 1024, netBridge: 'ws://10.0.0.2:25580' }).netBridge).toBe('ws://10.0.0.2:25580');
    expect(config.mounts).toEqual([
      { path: '/minecraft', kind: 'opfs', root: 'minecraft', lazy: { dir: '/assets/objects', index: '/assets/indexes/34.json', url: '/proxy/resources.download.minecraft.net' } },
      { path: '/natives', kind: 'memory', files: STUB_NATIVES },
      { path: '/tmp', kind: 'memory' },
    ]);
  });
});
