// Rewrites GLSL ES 3.00 from SPIRV-Cross for what WebGL2 lacks:
// - fragment output arrays may only be indexed by constants: the shader
//   writes a plain array, copied into one output per element at the end;
// - texel buffers (samplerBuffer) do not exist: they become 2D textures
//   (see the GL side's glTexBuffer), addressed row by row.

/** Desktop GLSL as GLSL ES 3.00 (SPIRV-Cross is asked for ES already). */
export function toEs(source: string): string {
  if (/^\s*#version\s+300\s+es/.test(source)) return source;
  return source.replace(/^\s*#version\s+\d+(\s+core)?/, '#version 300 es\nprecision highp float;\nprecision highp int;\nprecision highp sampler2D;');
}

/** The rewritten source, and the names of its texel buffer samplers. */
export function rewriteForWebGl(source: string, fragment: boolean): { source: string; texelBuffers: string[] } {
  let s = toEs(source);
  const names: string[] = [];
  s = texelBuffers(s, names);
  if (fragment) s = outputArrays(s);
  return { source: s, texelBuffers: names };
}

/** The index of the parenthesis or brace closing the one at `open`. */
function closing(s: string, open: number): number {
  const pair = s[open] === '(' ? ')' : '}';
  let depth = 0;
  for (let i = open; i < s.length; i++) {
    if (s[i] === s[open]) depth++;
    else if (s[i] === pair && --depth === 0) return i;
  }
  return -1;
}

/** Inserts `text` after the #version, #extension and precision lines. */
function afterHeader(s: string, text: string): string {
  const header = /^(?:\s*(?:#version[^\n]*|#extension[^\n]*|precision[^\n]*)\n)*/.exec(s)![0];
  return header + text + s.slice(header.length);
}

function texelBuffers(source: string, names: string[]): string {
  let s = source.replace(/^\s*#extension\s+GL_(EXT|OES)_texture_buffer\s*:\s*\w+\s*\n/gm, '');
  s = s.replace(/uniform\s+(\w+\s+)?([iu]?)samplerBuffer\s+(\w+)\s*;/g, (_m, precision: string | undefined, kind: string, name: string) => {
    names.push(name);
    return `uniform ${precision ?? 'highp '}${kind}sampler2D ${name};`;
  });
  if (!names.length) return s;
  for (const name of names) {
    // texelFetch(name, index) → texelFetch(name, row/column of index, 0).
    const call = new RegExp(`texelFetch\\(\\s*${name}\\s*,`, 'g');
    let match: RegExpExecArray | null;
    while ((match = call.exec(s))) {
      const open = match.index + 'texelFetch'.length;
      const close = closing(s, open);
      const index = s.slice(match.index + match[0].length, close);
      const replacement = `texelFetch(${name}, _texel_buffer_coord(int(${index}), textureSize(${name}, 0).x), 0)`;
      s = s.slice(0, match.index) + replacement + s.slice(close + 1);
      call.lastIndex = match.index + replacement.length;
    }
  }
  return afterHeader(s, 'ivec2 _texel_buffer_coord(int i, int w) { return ivec2(i % w, i / w); }\n');
}

function outputArrays(source: string): string {
  const arrays: { name: string; type: string; count: number }[] = [];
  let s = source.replace(
    /layout\(location\s*=\s*(\d+)\)\s*out\s+((?:(?:highp|mediump|lowp)\s+)?\w+)\s+(\w+)\[(\d+)\]\s*;/g,
    (_m, location: string, type: string, name: string, count: string) => {
      const n = Number(count);
      arrays.push({ name, type, count: n });
      const outputs = Array.from({ length: n }, (_, i) => `layout(location = ${Number(location) + i}) out ${type} ${name}_${i};`);
      return `${outputs.join('\n')}\n${type} ${name}[${n}];`;
    },
  );
  if (!arrays.length) return s;
  const copy = arrays.map(({ name, count }) => Array.from({ length: count }, (_, i) => `${name}_${i} = ${name}[${i}];`).join(' ')).join(' ');
  const main = /void\s+main\s*\(\s*\)\s*\{/.exec(s);
  if (!main) return s;
  const open = main.index + main[0].length - 1;
  const close = closing(s, open);
  const body = s.slice(open + 1, close).replace(/\breturn\s*;/g, `{ ${copy} return; }`);
  return `${s.slice(0, open + 1)}${body}    ${copy}\n${s.slice(close)}`;
}
