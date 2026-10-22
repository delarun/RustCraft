// Turns a launch plan into a process for our VM (rjava.wasm): the JVM flags it
// understands, the extra flags it needs, and the file system it sees.

import { DEFAULT_BRIDGE } from '../jvm/net';
import type { MountSpec, ProcessConfig } from '../jvm/protocol';
import { GAME_DIR, inGameDir, layout, OPFS_ROOT } from './layout';
import type { LaunchPlan } from './launchPlan';
import { mirrorUrl, RESOURCES_URL } from './mirror';

/**
 * LWJGL looks its shared libraries up by file name before "loading" them; the
 * VM implements their functions itself, so empty files with the right names do.
 */
export const NATIVES_DIR = '/natives';
export const STUB_NATIVES = [
  'libfreetype.so',
  'libGL.so',
  'libGL.so.1',
  'libGLX.so.0',
  'libjemalloc.so',
  'liblwjgl_opengl.so',
  'liblwjgl.so',
  'liblwjgl_stb.so',
  'liblwjgl_vma.so',
  'libopenal.so',
  'libSDL3.so',
  'libshaderc.so',
  'libspirv-cross.so',
];

/** Flags our VM needs on top of the version's. */
export const VM_JVM_ARGS = [
  // The VM reads multi-release jars as plain jars.
  '-Djdk.util.jar.enableMultiRelease=false',
  `-Djava.library.path=${NATIVES_DIR}`,
  `-Dorg.lwjgl.librarypath=${NATIVES_DIR}`,
  // No jemalloc: LWJGL allocates through the VM's malloc.
  '-Dorg.lwjgl.system.allocator=system',
];

export interface VmOptions {
  /** Java heap in MiB. */
  heapMb: number;
  /** rustcraft-bridge for the game's TCP connections (default DEFAULT_BRIDGE). */
  netBridge?: string;
}

/**
 * rjava takes only -D, -Xmx and -cp before the main class (anything else would
 * be read as the main class). Mojang's HotSpot flags (-XX:, -Xss,
 * --add-exports, --enable-native-access) mean nothing to it and are dropped,
 * as are the flags we set ourselves. The log4j config is dropped too: Mojang's
 * writes XML events for its launcher; the jar's default logs readable lines.
 */
export function vmJvmArgs(args: string[]): string[] {
  const out: string[] = [];
  for (let i = 0; i < args.length; i++) {
    const arg = args[i]!;
    if (arg === '-cp' || arg === '-classpath' || arg === '--class-path') {
      out.push('-cp', args[++i] ?? '');
    } else if (arg === '--add-exports' || arg === '--add-opens' || arg === '--add-modules' || arg === '--add-reads') {
      i++;
    } else if (arg.startsWith('-D')) {
      const key = arg.slice(2).split('=')[0]!;
      if (['java.library.path', 'org.lwjgl.librarypath', 'log4j.configurationFile'].includes(key)) continue;
      out.push(arg);
    } else if (arg.startsWith('-Xmx')) {
      // The heap is ours to choose.
    }
  }
  return out;
}

export function modulesPath(runtime: string): string {
  return inGameDir(layout.runtimeFile(runtime, 'lib/modules'));
}

export function vmMounts(assetIndex: string): MountSpec[] {
  return [
    {
      path: GAME_DIR,
      kind: 'opfs',
      root: OPFS_ROOT,
      lazy: { dir: '/assets/objects', index: `/${layout.assetIndex(assetIndex)}`, url: mirrorUrl(`${RESOURCES_URL}/`).replace(/\/$/, '') },
    },
    { path: NATIVES_DIR, kind: 'memory', files: STUB_NATIVES },
    { path: '/tmp', kind: 'memory' },
  ];
}

export function vmProcess(plan: LaunchPlan, options: VmOptions): ProcessConfig {
  return {
    args: [
      'rjava',
      '--modules',
      modulesPath(plan.javaComponent),
      `-Xmx${options.heapMb}m`,
      ...VM_JVM_ARGS,
      ...vmJvmArgs(plan.jvmArgs),
      plan.mainClass,
      ...plan.gameArgs,
    ],
    env: { HOME: GAME_DIR, PWD: GAME_DIR },
    mounts: vmMounts(plan.assetIndex),
    netBridge: options.netBridge ?? DEFAULT_BRIDGE,
  };
}
