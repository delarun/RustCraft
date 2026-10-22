# Optimisations: what has been done and what could come next

This document collects what we know about where rjava spends its time when it runs
Minecraft 26.3 in the browser, what has already been done about it, and the ideas that
could make it faster. The main bottleneck is the integrated server: in singleplayer it
runs inside the page, in the same VM as the client.

## How to measure

- **Benchmark.** `.scratch/web/bench.sh <log> [seconds]` (a local script, not in git). It
  launches the client straight into a fixed world (quick play), starts the Java stack
  sampler and reports the server thread's *busy* share: the time it does not spend
  parked between ticks. The world is `bench-calm` (seed 12345, peaceful, mob spawning off,
  render and simulation distance 4, client capped at 30 fps), which makes runs repeatable
  to about ±2–3 %. Changes smaller than that need several runs. Worlds with natural mob
  spawning scatter by ±5 % or more.
- **Java stack sampler.** `RJAVA_JSAMPLE=<thread name>[,ms]`. It prints inclusive and self
  time per Java method, a "caller → top" table, and the share of samples that are running,
  blocked or waiting for a collection. `RJAVA_JSAMPLE_GROUPS=<class prefix>,…` adds the
  share of samples spent under those classes, with the place where the stack first
  enters the first of them. Compiled frames are included: compiled code clears its frame
  records while sampling is on.
- **V8 CPU profile.** `CPU_PROFILE=<start>:<seconds>:<dir>` with `RJAVA_NO_JSPI=1`; the
  V8 profiler cannot see wasm frames on stacks that JSPI can suspend. Then run
  `node e2e/profile-summary.mjs <dir>` for self time and `.scratch/web/inclusive.mjs`
  for inclusive time. This shows VM (Rust) functions and compiled Java methods by name,
  but not interpreted Java methods.
- **JIT statistics.** `RJAVA_JIT_STATS=1`, every 10 s:
  - compiled calls;
  - imports by kind (`step`, `invoke`, `select`, call stub);
  - generic invokes by reason, with their target and caller;
  - methods that keep running interpreted.
- **GC log.** `RJAVA_GC_LOG=1`: phase times, compactions, fragmentation and time to safepoint.
- **Native runs.** `rjava` with the wasmtime backend runs the dedicated server headless:
  the unbundled `server-26.3.jar` plus the libraries. `RJAVA_JIT_DUMP=<dir>` keeps the
  compiled modules; any wasm disassembler (wasmprinter) shows them with method names.

## Where the time goes (October 2026)

On the server thread, with a player in a world:
- **VM overhead, about 7 %:**
  - interpreter;
  - calls into methods without compiled code;
  - `pure` and `subtype` imports;
  - generic invokes;
  - collections.

  Before the work below it was about 30 %.
- **Compiled Java code, about 90 %.** The profile is flat: entity ticking (AI, movement,
  collisions) and block state lookups. No single method stands out.
- **The gap to HotSpot is roughly 10×,** measured on micro-benchmarks of call-heavy code.
  Native wasmtime runs the same compiled code about 1.6× faster than V8 in the browser.
- **The JIT is a baseline compiler.** Bytecode becomes wasm almost instruction by
  instruction:
  - values live in wasm locals within a basic block;
  - Java locals stay in wasm locals across blocks and are written back lazily (only
    before calls and imports);
  - small callees (≤ 80 bytes) are inlined, virtual ones behind a monomorphic class
    guard taken from the interpreter's inline cache;
  - every compiled method keeps the interpreter's frame layout in linear memory, so
    that the interpreter can take over at any block boundary.

## Done so far

- **Collector:**
  - a persistent pool of helper threads; in the browser every thread is a Web Worker,
    and spawning ~16 of them per collection was very expensive;
  - parallel reference updating during compaction, and forwarding through a
    block-indexed table instead of a binary search;
  - compaction only when it can help (bump region low and the top moved, or the largest
    free chunk shrank), plus an emergency compaction before OutOfMemoryError;
  - default heap 1.5 GB: Minecraft keeps about 400 MB live, and 1 GB led to compaction storms.
- **GL.** CPU shadow copies of mapped buffers, so `glMapBufferRange` no longer reads
  buffers back from the GPU.
- **JIT:**
  - a `subtype` import for `checkcast`/`instanceof` misses;
  - a `pure` import that runs without flushing the frame, with inline fast paths for
    `currentThread`, `getClass`, `hashCode`/`identityHashCode`, `isInstance`,
    `allocateInstance` and the `aastore` check;
  - `Unsafe` get/put/CAS compiled to plain and atomic loads and stores (wasm only);
  - `frem`/`drem` through an `fmod` import;
  - class-literal `ldc` read from the mirror cell;
  - linked invokedynamic and VarHandle/MethodHandle call sites called directly, with the
    appendix read from a cell the collector updates;
  - recompilation, up to 3 versions per method, when call sites that went through the
    generic `invoke` (target not yet loaded or initialised) or `ldc` can now be direct;
    old versions hand over to the newest at their entry;
  - unused wasm locals dropped;
  - frame records written with fewer stores.
- **Records.** `equals` and `hashCode` that javac compiles to
  `invokedynamic ObjectMethods.bootstrap` are rewritten into plain bytecode when the
  class is defined. Results are identical to HotSpot.
- **VM:**
  - a cached interface closure, so `is_subclass_of` no longer allocates;
  - a fast hasher for monitors and call-site maps;
  - fixed a class-loading race that defined classes twice.

Tried, but no measurable gain:
- larger or smaller inlining limits;
- a lower JIT threshold;
- fewer Minecraft background threads (`max.bg.threads`);
- compiling synchronized methods (kept behind `RJAVA_JIT_SYNC_METHODS`);
- refilling allocation buffers only from large free chunks (`RJAVA_TLAB_MIN`).

## Ideas for further speed-ups

Roughly from the largest expected gain to the smallest.

### 1. An optimizing JIT tier (largest gain, largest effort)

A second compiler for the hottest methods, like C2 in HotSpot. It would parse bytecode
into an SSA intermediate representation with Java-level knowledge, optimise it, and emit
wasm. V8 already optimises the wasm it gets, but it only sees memory operations: it cannot
know that a reference is non-null, that an interface has one implementation, or that an
object never escapes. Within such a tier:

- **Inlining driven by type profiles,** collected in compiled code rather than only by the
  interpreter's last receiver. Bimorphic or polymorphic guards, inlining whole call chains
  (e.g. `Level.getBlockState` → `LevelChunk` → `PalettedContainer` → `Palette`/`BitStorage`),
  and a much larger inlining budget once calls no longer cost a frame each.
- **Class hierarchy analysis.** Treat a virtual method with a single loaded implementation
  as final: direct call or inline with no guard. This needs dependency tracking and
  invalidation when a class that overrides it is loaded. Running activations must be
  handled too (deoptimisation, or keep a cheap guard that falls back).
- **Null and bounds check elimination,** using dominating checks, loop induction
  variables and range analysis.
- **Escape analysis and scalar replacement.** Minecraft allocates many short-lived small
  objects: `BlockPos`, `Vec3`, `AABB`, iterators and lambdas. Keeping them in wasm locals
  removes the allocation, the zeroing, the GC pressure and the memory traffic.
- **Loop optimisations:** hoisting invariant loads (field reads, array lengths) and
  strength reduction.
- **Deoptimisation** back to the interpreter frame layout at the points where the
  optimised code's assumptions break. The baseline's "interpreter can take over at block
  boundaries" design is a good foundation for this.

### 2. A cheaper calling convention

Today every compiled→compiled call writes the caller's dirty locals and its operands to
memory, writes a frame record, charges the native-stack budget, counts safepoint ticks
(with an atomic load of the stop flag), checks the method's version and goes through
`call_indirect`. The arguments and the result pass through memory. Measured cost: about
10 ns natively, against about 1 ns in HotSpot.

- **Arguments and results as wasm parameters and results.** Materialise the callee's frame
  in memory only when it needs one: before an import, a safepoint, or a call to code that
  needs frames.
- **Precise GC stack maps instead of conservative scanning of frames.** References could
  then stay in wasm locals across calls (with a spill only at safepoints), and objects
  referenced from frames would no longer be pinned.
- **Leaf entry points** for small leaf callees that cannot be inlined (e.g. when
  recursive), with typed signatures.
- **Safepoint ticks per thread,** in the thread block, instead of a counter per module
  plus an atomic load of a global flag.

### 3. The V8 tier-up

Running with `--no-liftoff` (everything compiled by TurboFan at once) made the server
about 5 % faster. This shows that a part of the hot code runs in V8's baseline tier.
These flags cannot be set for users, but:

- **Fewer modules, compiled fewer times.** Every worker compiles every module it runs
  separately, and every recompiled version starts over in Liftoff. Compiling a module once
  and sharing the `WebAssembly.Module` between workers via `postMessage` would let all
  threads share TurboFan code. Workers busy in wasm do not process messages, so this needs
  a pull model: a shared registry the thread polls at safepoints.
- **Batch hot methods** into larger modules, so that V8 inlines direct wasm-to-wasm calls
  between them; V8 inlines within a module only.
- **Watch for compile hints** (custom sections for compilation priority or tiering) as
  they ship in Chrome.

### 4. Allocation and the collector

- **A generational collector** with a nursery for young objects and a write barrier in
  compiled code. Most Minecraft garbage dies young; a scavenge of the nursery would
  replace most full mark-sweep pauses (100–200 ms today, compaction about 500 ms).
- **Concurrent marking** with an incremental-update barrier, so that pauses shrink to
  root scanning and remark.
- **Parallel compaction** of the plan and move phases (both single-threaded now, about
  200 ms and 100 ms).
- **Smarter allocation buffers.** Refilling from tiny free-list holes forces a refill every
  few objects. Segregated small-object pages or a nursery would avoid it.
- **Zeroing freed memory in parallel during sweep** instead of at allocation time.

### 5. Locks and threads

- **Thin locks.** Today every `monitorenter` and synchronized method takes a global mutex
  and a hash map lookup. A lock word in the object header with a CAS on the uncontended
  path, and inflation to a monitor only under contention, would make uncontended locking
  nearly free. This also makes compiling synchronized methods worthwhile.
- **Fewer worker threads competing with the server thread.** Browsers cannot set thread
  priorities, but the worker pools (chunk generation, I/O) could be sized so that the
  server and render threads always have a core.

### 6. VM fast paths still missing

- **`System.arraycopy`** as a compiled intrinsic (`memory.copy` with the type checks
  inline), about 450 K calls per second at times.
- **`System.nanoTime`, `Thread.onSpinWait`, `Object.clone` of arrays** as intrinsics.
- **VarHandle access modes** compiled inline when the VarHandle is a constant (a static
  final field): field and array element get/set/CAS.
- **A per-site cache in the interpreter** for `link_indy` and `link_handle_site`, instead of
  a lookup under a global mutex. This matters for code that stays interpreted.
- **On-stack replacement:** a long-running loop in an old compiled version should switch
  to the newest version, which today happens only at method entry.

### 7. Rendering and the client

- **Client frame time** is fine at 30–60 fps, but the render thread competes with the
  server for CPU. Cap the frame rate by default, or adapt it when the server falls behind.
- **Batch GL calls** into a command buffer replayed in JS, instead of one wasm-to-JS call
  per GL call (there are tens of thousands per frame).
- **A WebGPU backend** instead of WebGL2, matching Minecraft's newer render pipeline more
  closely.

### 8. Game settings

These are not VM optimisations, but they decide what players experience:
- **The simulation distance** costs the most: entity ticking is about 70 % of the server's
  time. The launcher defaults to render distance 8 and simulation distance 6 and suggests
  lowering them when the server falls behind.
- **Render distance** drives chunk generation and lighting on the server's worker
  threads, especially in a new world.
