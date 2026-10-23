import { GAME_DIR, inGameDir, layout } from './layout';
import { resolveLibraries } from './libraries';
import { BROWSER_PLATFORM, rulesAllow, type Features, type Platform } from './rules';
import type { Argument, VersionJson } from './types';

export const LAUNCHER_NAME = 'minecraft-web';
export const LAUNCHER_VERSION = '0.1.0';

export interface LaunchOptions {
  username: string;
  uuid: string;
  accessToken: string;
  xuid: string;
  userType: 'msa' | 'legacy';
  width: number;
  height: number;
  demo?: boolean;
}

/** Everything the JVM needs to start the game; paths are inside the JVM filesystem. */
export interface LaunchPlan {
  versionId: string;
  javaMajorVersion: number;
  /** Mojang Java runtime component, e.g. java-runtime-epsilon. */
  javaComponent: string;
  assetIndex: string;
  mainClass: string;
  classpath: string[];
  jvmArgs: string[];
  gameArgs: string[];
  /** Mojang's recommended user JVM flags (heap size, GC); informational for our JVM. */
  userJvmDefaults: string[];
  hostNatives: string[];
  /** Placeholders the version referenced that we could not fill in. */
  unresolved: string[];
}

const LEGACY_JVM_ARGS = ['-Djava.library.path=${natives_directory}', '-cp', '${classpath}'];

export function buildLaunchPlan(
  version: VersionJson,
  options: LaunchOptions,
  platform: Platform = BROWSER_PLATFORM,
): LaunchPlan {
  const { classpath: libraries, hostNatives } = resolveLibraries(version, platform);
  const classpath = [
    ...libraries.map((lib) => inGameDir(layout.library(lib.path))),
    inGameDir(layout.clientJar(version.jar ?? version.id)),
  ];

  const vars: Record<string, string> = {
    auth_player_name: options.username,
    auth_uuid: options.uuid,
    auth_access_token: options.accessToken,
    auth_xuid: options.xuid,
    user_type: options.userType,
    clientid: '',
    version_name: version.id,
    version_type: version.type,
    game_directory: GAME_DIR,
    assets_root: inGameDir('assets'),
    assets_index_name: version.assetIndex.id,
    natives_directory: inGameDir('natives'),
    library_directory: inGameDir('libraries'),
    launcher_name: LAUNCHER_NAME,
    launcher_version: LAUNCHER_VERSION,
    classpath: classpath.join(':'),
    classpath_separator: ':',
    resolution_width: String(options.width),
    resolution_height: String(options.height),
  };
  const features: Features = {
    is_demo_user: options.demo ?? false,
    has_custom_resolution: true,
  };
  const unresolved = new Set<string>();
  const expand = (args: Argument[]) =>
    flattenArguments(args, platform, features).map((arg) => substitute(arg, vars, unresolved));

  const jvmArgs = expand(version.arguments?.jvm ?? LEGACY_JVM_ARGS);
  const logging = version.logging?.client;
  if (logging) {
    const path = inGameDir(layout.logConfig(logging.file.id));
    jvmArgs.push(substitute(logging.argument, { path }, unresolved));
  }
  const gameArgs = version.arguments
    ? expand(version.arguments.game)
    : expand((version.minecraftArguments ?? '').split(' ').filter(Boolean));

  return {
    versionId: version.id,
    javaMajorVersion: version.javaVersion?.majorVersion ?? 8,
    javaComponent: version.javaVersion?.component ?? 'jre-legacy',
    assetIndex: version.assetIndex.id,
    mainClass: version.mainClass,
    classpath,
    jvmArgs,
    gameArgs,
    userJvmDefaults: expand(version.arguments?.['default-user-jvm'] ?? []),
    hostNatives,
    unresolved: [...unresolved],
  };
}

export function flattenArguments(args: Argument[], platform: Platform, features: Features): string[] {
  const out: string[] = [];
  for (const arg of args) {
    if (typeof arg === 'string') {
      out.push(arg);
    } else if (rulesAllow(arg.rules, platform, features)) {
      out.push(...(Array.isArray(arg.value) ? arg.value : [arg.value]));
    }
  }
  return out;
}

function substitute(arg: string, vars: Record<string, string>, unresolved: Set<string>): string {
  return arg.replace(/\$\{(\w+)\}/g, (placeholder, name: string) => {
    const value = vars[name];
    if (value === undefined) {
      unresolved.add(name);
      return placeholder;
    }
    return value;
  });
}
