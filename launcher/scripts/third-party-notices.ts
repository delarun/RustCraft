// Generates public/third-party-notices.txt: the licenses of everything the
// deployed site ships — rjava.wasm (Rust crates, the Rust standard library and
// the C/C++ libraries statically linked into it, see vm/crates/jvm/build.rs)
// and the launcher's own JavaScript.
//
//   node scripts/third-party-notices.ts
//
// Reads (offline): `cargo metadata` of vm/ for the wasm32-wasip1-threads target,
// license files of the crates in ~/.cargo/registry, the C/C++ sources in
// $RJAVA_WASM_LIBS (default .scratch/wasm-libs, see its build.sh), the Rust
// toolchain's license texts, and scripts/licenses/ (wasi-libc and libc++
// texts fetched from their repositories at the versions wasi-sdk 34 uses).
// Rerun it whenever vm/Cargo.lock, the toolchain or the C libraries change.

import { execFileSync } from 'node:child_process';
import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';

const launcher = resolve(import.meta.dirname, '..');
const repo = resolve(launcher, '..');
const wasmLibs = resolve(process.env.RJAVA_WASM_LIBS ?? join(repo, '.scratch/wasm-libs'));
const localTexts = join(launcher, 'scripts/licenses');
const sysroot = execFileSync('rustc', ['--print', 'sysroot'], { encoding: 'utf8' }).trim();
const rustLicenses = join(sysroot, 'share/doc/rust/licenses');
const rustVersion = execFileSync('rustc', ['--version'], { encoding: 'utf8' }).trim();

interface Component {
  name: string;
  version: string;
  license: string;
  url: string;
  /** License files (path, label) to include verbatim. */
  files: string[];
  note?: string;
}

const read = (path: string) => readFileSync(path, 'utf8').replace(/\r\n/g, '\n').trimEnd();
const isLicenseFile = (name: string) => /^(LICEN[CS]E|COPYING|UNLICENSE|NOTICE|COPYRIGHT)/i.test(name) && !/\.(cfg|json|py|toml)$/i.test(name);
/** License files of a source tree, including SPDX texts in its LICENSES/ directory. */
const licenseFilesIn = (dir: string): string[] => {
  const files = readdirSync(dir)
    .filter((name) => isLicenseFile(name) && statSync(join(dir, name)).isFile())
    .sort()
    .map((name) => join(dir, name));
  const spdx = join(dir, 'LICENSES');
  if (existsSync(spdx) && statSync(spdx).isDirectory()) files.push(...readdirSync(spdx).sort().map((name) => join(spdx, name)));
  return files;
};

/** Standard texts for SPDX ids, from the Rust toolchain. */
function spdxTexts(expression: string): string[] {
  const ids = expression.match(/[A-Za-z0-9.-]+/g) ?? [];
  const files: string[] = [];
  for (const id of ids) {
    if (['OR', 'AND', 'WITH'].includes(id)) continue;
    const file = join(rustLicenses, `${id}.txt`);
    if (existsSync(file)) files.push(file);
  }
  return files;
}

// --- Rust crates linked into rjava.wasm ---------------------------------------

interface Metadata {
  packages: { id: string; name: string; version: string; license: string | null; repository: string | null; source: string | null; manifest_path: string }[];
  resolve: { nodes: { id: string; deps: { pkg: string; dep_kinds: { kind: string | null }[] }[] }[] };
}

const metadata = JSON.parse(
  execFileSync('cargo', ['metadata', '--format-version', '1', '--offline', '--locked', '--filter-platform', 'wasm32-wasip1-threads'], {
    cwd: join(repo, 'vm'),
    encoding: 'utf8',
    maxBuffer: 64 << 20,
  }),
) as Metadata;
const packages = new Map(metadata.packages.map((p) => [p.id, p]));
const nodes = new Map(metadata.resolve.nodes.map((n) => [n.id, n]));
const root = metadata.packages.find((p) => p.name === 'rjava');
if (!root) throw new Error('no rjava package in vm/');
const linked = new Set<string>();
const stack = [root.id];
while (stack.length) {
  const id = stack.pop()!;
  if (linked.has(id)) continue;
  linked.add(id);
  for (const dep of nodes.get(id)?.deps ?? []) {
    if (dep.dep_kinds.some((k) => k.kind === null || k.kind === 'normal')) stack.push(dep.pkg);
  }
}
const crates: Component[] = [...linked]
  .map((id) => packages.get(id)!)
  .filter((p) => p.source !== null) // the workspace's own crates
  .sort((a, b) => a.name.localeCompare(b.name))
  .map((p) => {
    const files = licenseFilesIn(dirname(p.manifest_path));
    return {
      name: p.name,
      version: p.version,
      license: p.license ?? 'see files',
      url: p.repository ?? `https://crates.io/crates/${p.name}`,
      files: files.length ? files : spdxTexts(p.license ?? ''),
    };
  });

const rustStd: Component = {
  name: 'Rust standard library (std, core, alloc, compiler_builtins, …)',
  version: rustVersion,
  license: 'MIT OR Apache-2.0 (with third-party parts listed at the end of this file)',
  url: 'https://github.com/rust-lang/rust',
  files: spdxTexts('MIT OR Apache-2.0'),
};

// --- C/C++ libraries linked into rjava.wasm -----------------------------------

const src = join(wasmLibs, 'src');
const shaderc = join(src, 'shaderc');
const shadercVersion = /^v(\S+)/m.exec(read(join(shaderc, 'CHANGES')))?.[1] ?? '?';
const glslangVersion = /Glslang (\S+?),/.exec(read(join(shaderc, 'CHANGES')))?.[1] ?? '?';
const spirvToolsVersion = /SPIRV-Tools (\S+)/.exec(read(join(shaderc, 'CHANGES')))?.[1] ?? '?';
const wasiSdk = read(join(wasmLibs, 'wasi-sdk/VERSION'));
const wasiSdkVersion = wasiSdk.split('\n')[0];
const wasiLibcRev = /wasi-libc: (\S+)/.exec(wasiSdk)?.[1] ?? '?';
const llvmVersion = /llvm-version: (\S+)/.exec(wasiSdk)?.[1] ?? '?';
const texts = (...names: string[]) => names.map((name) => join(localTexts, name));

const native: Component[] = [
  {
    name: 'shaderc',
    version: shadercVersion,
    license: 'Apache-2.0',
    url: 'https://github.com/google/shaderc',
    files: licenseFilesIn(shaderc),
  },
  {
    name: 'glslang (in shaderc)',
    version: glslangVersion,
    license: 'BSD-3-Clause AND BSD-2-Clause AND MIT AND Apache-2.0 AND GPL-3.0-or-later WITH Bison-exception-2.2 (parts)',
    url: 'https://github.com/KhronosGroup/glslang',
    files: licenseFilesIn(join(shaderc, 'third_party/glslang')),
  },
  {
    name: 'SPIRV-Tools (in shaderc)',
    version: spirvToolsVersion,
    license: 'Apache-2.0',
    url: 'https://github.com/KhronosGroup/SPIRV-Tools',
    files: licenseFilesIn(join(shaderc, 'third_party/spirv-tools')),
  },
  {
    name: 'SPIRV-Headers (in shaderc)',
    version: 'as bundled with shaderc ' + shadercVersion,
    license: 'MIT',
    url: 'https://github.com/KhronosGroup/SPIRV-Headers',
    files: licenseFilesIn(join(shaderc, 'third_party/spirv-headers')),
  },
  {
    name: 'SPIRV-Cross',
    version: 'vulkan-sdk-1.4.363.0',
    license: 'Apache-2.0 (some files MIT)',
    url: 'https://github.com/KhronosGroup/SPIRV-Cross',
    files: licenseFilesIn(join(src, 'SPIRV-Cross')),
  },
  {
    name: 'stb (stb_image, stb_image_write, stb_image_resize2)',
    version: 'git a7d4c21',
    license: 'MIT OR Unlicense (public domain)',
    url: 'https://github.com/nothings/stb',
    files: licenseFilesIn(join(src, 'stb')),
  },
  {
    name: 'libc++ and libc++abi (LLVM, from wasi-sdk)',
    version: `LLVM ${llvmVersion}, wasi-sdk ${wasiSdkVersion}`,
    license: 'Apache-2.0 WITH LLVM-exception',
    url: 'https://github.com/llvm/llvm-project',
    files: texts('llvm-libcxx_LICENSE.TXT', 'llvm-libcxxabi_LICENSE.TXT'),
  },
  {
    name: 'wasi-libc (from wasi-sdk; also used by the Rust standard library)',
    version: `git ${wasiLibcRev}, wasi-sdk ${wasiSdkVersion}`,
    license: 'Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT; musl: MIT; cloudlibc: BSD-2-Clause; dlmalloc: CC0',
    url: 'https://github.com/WebAssembly/wasi-libc',
    files: texts(
      'wasi-libc-LICENSE',
      'wasi-libc-LICENSE-APACHE-LLVM',
      'wasi-libc-LICENSE-APACHE',
      'wasi-libc-LICENSE-MIT',
      'wasi-libc-libc-top-half_musl_COPYRIGHT',
      'wasi-libc-libc-bottom-half_cloudlibc_LICENSE',
    ),
    note: 'dlmalloc is dedicated to the public domain (CC0) by Doug Lea; no notice required.',
  },
];

// --- the launcher page ---------------------------------------------------------

const viteLicense = read(join(launcher, 'node_modules/vite/LICENSE.md')).split('\n# Licenses of bundled dependencies')[0]!;
const vitePackage = JSON.parse(read(join(launcher, 'node_modules/vite/package.json'))) as { version: string };
const page: Component[] = [
  {
    name: 'Vite (module preload polyfill included in the built page)',
    version: vitePackage.version,
    license: 'MIT',
    url: 'https://github.com/vitejs/vite',
    files: [],
    note: viteLicense,
  },
];

// --- output -------------------------------------------------------------------

const textIds = new Map<string, number>();
const textUsers: string[][] = [];
const textBodies: string[] = [];
function textId(file: string, user: string): number {
  const body = read(file);
  let id = textIds.get(body);
  if (id === undefined) {
    id = textBodies.length + 1;
    textIds.set(body, id);
    textBodies.push(body);
    textUsers.push([]);
  }
  textUsers[id - 1]!.push(user);
  return id;
}

const out: string[] = [];
const line = (s = '') => out.push(s);
line('RustCraft — third-party software notices');
line('========================================');
line('Site: https://rustcraft.online/ (this file: https://rustcraft.online/third-party-notices.txt)');
line(`Generated by launcher/scripts/third-party-notices.ts on ${new Date().toISOString().slice(0, 10)}. Do not edit by hand.`);
line();
line('This site ships the launcher page (HTML, CSS, JavaScript) and rjava.wasm, our Java virtual');
line('machine compiled to WebAssembly, which statically links the components listed below.');
line();
line('NOT shipped by this site: Minecraft (client jar, assets), its libraries (LWJGL, Netty, …) and');
line('the Java runtime. Your browser downloads them from Mojang\'s servers into its own storage,');
line('under Mojang\'s and their authors\' terms.');
line();
line('NOT AN OFFICIAL MINECRAFT PRODUCT. NOT APPROVED BY OR ASSOCIATED WITH MOJANG OR MICROSOFT.');
line();

function section(title: string, components: Component[]) {
  line(title);
  line('-'.repeat(title.length));
  for (const c of components) {
    const ids = c.files.map((file) => textId(file, `${c.name} ${c.version}`));
    line(`* ${c.name} ${c.version}`);
    line(`  License: ${c.license}`);
    line(`  Source: ${c.url}`);
    if (ids.length) line(`  License text: ${[...new Set(ids)].map((id) => `[${id}]`).join(' ')}`);
    if (c.note) line(c.note.split('\n').map((l) => `  ${l}`).join('\n'));
  }
  line();
}
section('1. rjava.wasm: Rust crates', crates);
section('2. rjava.wasm: Rust standard library', [rustStd]);
section('3. rjava.wasm: C and C++ libraries', native);
section('4. Launcher page', page);

line('LICENSE TEXTS');
line('=============');
textBodies.forEach((body, i) => {
  line();
  line(`[${i + 1}] Used by: ${[...new Set(textUsers[i])].join(', ')}`);
  line('-'.repeat(72));
  line(body);
});

const stdNotices = join(sysroot, 'share/doc/rust/COPYRIGHT-library.html');
if (existsSync(stdNotices)) {
  const text = read(stdNotices)
    .replace(/<(script|style)[\s\S]*?<\/\1>/g, '')
    .replace(/<br\s*\/?>|<\/(p|div|li|h\d|tr|pre)>/gi, '\n')
    .replace(/<[^>]+>/g, '')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, '&')
    .split('\n')
    .map((l) => l.trimEnd())
    .join('\n')
    .replace(/\n{3,}/g, '\n\n');
  line();
  line(`RUST STANDARD LIBRARY COPYRIGHT NOTICES (${rustVersion}, from COPYRIGHT-library.html)`);
  line('='.repeat(72));
  line(text.trim());
}

const target = join(launcher, 'public/third-party-notices.txt');
writeFileSync(target, `${out.join('\n')}\n`);
console.log(`${target}: ${crates.length} crates, ${native.length} C/C++ libraries, ${textBodies.length} license texts, ${(out.join('\n').length / 1024).toFixed(0)} KiB`);
