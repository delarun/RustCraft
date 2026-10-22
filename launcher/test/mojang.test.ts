import { describe, expect, it } from 'vitest';
import { collectDownloads } from '../src/mojang/downloads';
import { buildLaunchPlan } from '../src/mojang/launchPlan';
import { isHostNativeClassifier, mavenPath, parseMaven } from '../src/mojang/maven';
import { mirrorUrl } from '../src/mojang/mirror';
import { isValidUsername, md5, offlineUuid } from '../src/mojang/offline';
import { BROWSER_PLATFORM, compareVersions, rulesAllow } from '../src/mojang/rules';
import { assets, version } from './fixture';

const hex = (bytes: Uint8Array) => Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');

describe('rules', () => {
  const linux = { os: 'linux', arch: 'x86_64', osVersion: '6.1' } as const;
  const windows = { os: 'windows', arch: 'x86_64', osVersion: '10.0.19045' } as const;

  it('allows when there are no rules', () => {
    expect(rulesAllow(undefined, linux)).toBe(true);
    expect(rulesAllow([], linux)).toBe(true);
  });

  it('lets the last matching rule win and denies by default', () => {
    const rules = [{ action: 'allow' as const }, { action: 'disallow' as const, os: { name: 'osx' } }];
    expect(rulesAllow(rules, linux)).toBe(true);
    expect(rulesAllow([{ action: 'allow', os: { name: 'osx' } }], linux)).toBe(false);
  });

  it('matches features and version ranges', () => {
    const demo = [{ action: 'allow' as const, features: { is_demo_user: true } }];
    expect(rulesAllow(demo, linux, {})).toBe(false);
    expect(rulesAllow(demo, linux, { is_demo_user: true })).toBe(true);
    const zgc = [{ action: 'allow' as const, os: { name: 'windows', versionRange: { min: '10.0.17134' } } }];
    expect(rulesAllow(zgc, windows)).toBe(true);
    expect(rulesAllow(zgc, { ...windows, osVersion: '10.0.17000' })).toBe(false);
    expect(compareVersions('10.0.17134', '10.0.9999')).toBe(1);
  });

  it('never matches arch rules in the browser', () => {
    expect(rulesAllow([{ action: 'allow', os: { arch: 'x86' } }], BROWSER_PLATFORM)).toBe(false);
  });
});

describe('maven', () => {
  it('builds repository paths', () => {
    expect(mavenPath(parseMaven('org.lwjgl:lwjgl:3.4.3:natives-linux'))).toBe(
      'org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-linux.jar',
    );
  });

  it('recognises host-native classifiers', () => {
    expect(isHostNativeClassifier('natives-windows-arm64')).toBe(true);
    expect(isHostNativeClassifier('linux-x86_64')).toBe(true);
    expect(isHostNativeClassifier(undefined)).toBe(false);
    expect(isHostNativeClassifier('sources')).toBe(false);
  });
});

describe('offline identity', () => {
  it('implements MD5', () => {
    expect(hex(md5(new Uint8Array()))).toBe('d41d8cd98f00b204e9800998ecf8427e');
    expect(hex(md5(new TextEncoder().encode('The quick brown fox jumps over the lazy dog')))).toBe(
      '9e107d9d372bb6826bd81d3542a419d6',
    );
    expect(hex(md5(new Uint8Array(1000).fill(97)))).toBe('cabe45dcc9ae5b66ba86600cca6b8ba8');
  });

  it('matches UUID.nameUUIDFromBytes("OfflinePlayer:" + name)', () => {
    expect(offlineUuid('Steve')).toBe('5627dd98-e6be-3c21-b8a8-e92344183641');
  });

  it('validates usernames', () => {
    expect(isValidUsername('Steve_42')).toBe(true);
    expect(isValidUsername('ab')).toBe(false);
    expect(isValidUsername('имя')).toBe(false);
  });
});

describe('launch plan', () => {
  const plan = buildLaunchPlan(version, {
    username: 'Steve',
    uuid: offlineUuid('Steve'),
    accessToken: '0',
    xuid: '0',
    userType: 'legacy',
    width: 1280,
    height: 720,
  });

  it('builds the classpath without host natives, OS-specific or duplicate jars', () => {
    expect(plan.classpath).toEqual([
      '/minecraft/libraries/com/google/code/gson/gson/2.14.0/gson-2.14.0.jar',
      '/minecraft/libraries/org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3.jar',
      '/minecraft/versions/26.3/26.3.jar',
    ]);
    expect(plan.hostNatives).toEqual([
      'org.lwjgl:lwjgl:3.4.3:natives-linux',
      'io.netty:netty-transport-native-epoll:4.2.16.Final:linux-x86_64',
    ]);
  });

  it('expands JVM arguments', () => {
    expect(plan.jvmArgs).toEqual([
      '-Djava.library.path=/minecraft/natives/java',
      '-Dminecraft.launcher.brand=minecraft-web',
      '-cp',
      plan.classpath.join(':'),
      '-Dlog4j.configurationFile=/minecraft/assets/log_configs/client-1.21.2.xml',
    ]);
    expect(plan.userJvmDefaults).toEqual(['-Xms2G', '-Xmx4G']);
  });

  it('expands game arguments', () => {
    expect(plan.gameArgs).toEqual([
      '--username', 'Steve',
      '--version', '26.3',
      '--gameDir', '/minecraft',
      '--assetsDir', '/minecraft/assets',
      '--assetIndex', '34',
      '--uuid', '5627dd98-e6be-3c21-b8a8-e92344183641',
      '--accessToken', '0',
      '--width', '1280', '--height', '720',
    ]);
    expect(plan.unresolved).toEqual([]);
    expect(plan.javaMajorVersion).toBe(25);
  });
});

describe('downloads', () => {
  it('collects client, libraries, logging config and deduplicated assets', () => {
    const jobs = collectDownloads(version, assets, BROWSER_PLATFORM);
    expect(jobs.map((job) => job.path)).toEqual([
      'versions/26.3/26.3.jar',
      'libraries/com/google/code/gson/gson/2.14.0/gson-2.14.0.jar',
      'libraries/org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3.jar',
      'assets/log_configs/client-1.21.2.xml',
      'assets/objects/aa/aa11111111111111111111111111111111111111',
      'assets/objects/bb/bb22222222222222222222222222222222222222',
    ]);
    expect(jobs.at(-1)).toMatchObject({
      url: 'https://resources.download.minecraft.net/bb/bb22222222222222222222222222222222222222',
      verify: 'size',
    });
  });

  it('routes CORS-less hosts through the same-origin proxy', () => {
    expect(mirrorUrl('https://libraries.minecraft.net/a/b.jar')).toBe('/proxy/libraries.minecraft.net/a/b.jar');
    expect(mirrorUrl('https://resources.download.minecraft.net/aa/aa11')).toBe('/proxy/resources.download.minecraft.net/aa/aa11');
    expect(mirrorUrl('https://api.minecraftservices.com/entitlements/license?requestId=1')).toBe(
      '/proxy/api.minecraftservices.com/entitlements/license?requestId=1',
    );
    const direct = 'https://piston-data.mojang.com/v1/objects/c/client.jar';
    expect(mirrorUrl(direct)).toBe(direct);
  });
});
