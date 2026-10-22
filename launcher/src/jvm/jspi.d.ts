// JavaScript Promise Integration (not yet in TypeScript's lib).
declare namespace WebAssembly {
  /** An import that suspends the calling wasm stack until its promise settles. */
  class Suspending {
    constructor(f: (...args: never[]) => Promise<unknown>);
  }
}
declare namespace WebAssembly {
  /** An export that runs on a suspendable stack, returning a promise. */
  function promising<A extends unknown[], R>(f: (...args: A) => R): (...args: A) => Promise<R>;
}
