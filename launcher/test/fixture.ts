import type { AssetIndex, VersionJson } from '../src/mojang/types';

const artifact = (path: string) => ({
  path,
  sha1: `sha1-of-${path}`,
  size: 100,
  url: `https://libraries.minecraft.net/${path}`,
});

// A trimmed-down version JSON in the 26.x format.
export const version: VersionJson = {
  id: '26.3',
  type: 'release',
  mainClass: 'net.minecraft.client.main.Main',
  assets: '34',
  assetIndex: { id: '34', sha1: 'idx', size: 10, totalSize: 30, url: 'https://piston-meta.mojang.com/v1/packages/idx/34.json' },
  downloads: {
    client: { sha1: 'client-sha1', size: 1000, url: 'https://piston-data.mojang.com/v1/objects/c/client.jar' },
  },
  javaVersion: { component: 'java-runtime-epsilon', majorVersion: 25 },
  libraries: [
    { name: 'com.google.code.gson:gson:2.14.0', downloads: { artifact: artifact('com/google/code/gson/gson/2.14.0/gson-2.14.0.jar') } },
    {
      name: 'ca.weblite:java-objc-bridge:1.1',
      downloads: { artifact: artifact('ca/weblite/java-objc-bridge/1.1/java-objc-bridge-1.1.jar') },
      rules: [{ action: 'allow', os: { name: 'osx' } }],
    },
    { name: 'org.lwjgl:lwjgl:3.4.3', downloads: { artifact: artifact('org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3.jar') } },
    {
      name: 'org.lwjgl:lwjgl:3.4.3:natives-linux',
      downloads: { artifact: artifact('org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-linux.jar') },
      rules: [{ action: 'allow', os: { name: 'linux' } }],
    },
    {
      name: 'io.netty:netty-transport-native-epoll:4.2.16.Final:linux-x86_64',
      downloads: { artifact: artifact('io/netty/netty-transport-native-epoll/4.2.16.Final/x.jar') },
      rules: [{ action: 'allow', os: { name: 'linux' } }],
    },
    // Duplicate entries occur in real manifests and must not repeat on the classpath.
    { name: 'com.google.code.gson:gson:2.14.0', downloads: { artifact: artifact('com/google/code/gson/gson/2.14.0/gson-2.14.0.jar') } },
  ],
  arguments: {
    game: [
      '--username', '${auth_player_name}',
      '--version', '${version_name}',
      '--gameDir', '${game_directory}',
      '--assetsDir', '${assets_root}',
      '--assetIndex', '${assets_index_name}',
      '--uuid', '${auth_uuid}',
      '--accessToken', '${auth_access_token}',
      { rules: [{ action: 'allow', features: { is_demo_user: true } }], value: '--demo' },
      { rules: [{ action: 'allow', features: { has_custom_resolution: true } }], value: ['--width', '${resolution_width}', '--height', '${resolution_height}'] },
    ],
    jvm: [
      { rules: [{ action: 'allow', os: { name: 'osx' } }], value: ['-XstartOnFirstThread'] },
      { rules: [{ action: 'allow', os: { arch: 'x86' } }], value: '-Xss1M' },
      '-Djava.library.path=${natives_directory}/java',
      '-Dminecraft.launcher.brand=${launcher_name}',
      '-cp', '${classpath}',
    ],
    'default-user-jvm': [
      { value: ['-Xms2G', '-Xmx4G'] },
      { rules: [{ action: 'allow', os: { name: 'windows', versionRange: { max: '10.0.17134' } } }], value: ['-XX:+UseG1GC'] },
    ],
  },
  logging: {
    client: {
      argument: '-Dlog4j.configurationFile=${path}',
      file: { id: 'client-1.21.2.xml', sha1: 'log-sha1', size: 1073, url: 'https://piston-data.mojang.com/v1/objects/log/client-1.21.2.xml' },
      type: 'log4j2-xml',
    },
  },
};

export const assets: AssetIndex = {
  objects: {
    'minecraft/sounds/a.ogg': { hash: 'aa11111111111111111111111111111111111111', size: 10 },
    'minecraft/sounds/b.ogg': { hash: 'bb22222222222222222222222222222222222222', size: 20 },
    'minecraft/sounds/a-copy.ogg': { hash: 'aa11111111111111111111111111111111111111', size: 10 },
  },
};
