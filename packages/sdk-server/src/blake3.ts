/**
 * BLAKE3 in plain TypeScript: the default hash mode, 32 bytes of output, any input length.
 *
 * # Why this file exists
 *
 * A feature flag puts a visitor in a bucket with `blake3(salt ‖ subject)`, because that
 * is what the server does (`micaforge_core::flags::bucket`, which reuses the cookieless
 * visitor hash so the product has one hashing primitive). The SDKs decide flags locally
 * as well, from a ruleset or a fallback rule, and a local decision is only worth having
 * if it lands every visitor in the bucket the server would choose. So the SDKs need the
 * same hash, byte for byte. Web Crypto has no BLAKE3, `node:crypto` has none either, and
 * the SDKs take no dependencies, so it is written out here from the BLAKE3 specification
 * and its reference implementation.
 *
 * # What it covers
 *
 * The whole hash, not a short-input shortcut: input is split into 1024-byte chunks, each
 * chunk is compressed 64 bytes at a time, and chunk results are merged pairwise up the
 * binary tree, so an input of any length hashes correctly. Keyed hashing, key derivation
 * and extended output are left out because nothing here uses them. Flag keys and
 * subjects are short, so the compression function is written for clarity and for the
 * one-block case rather than for throughput.
 *
 * Strings are encoded by {@link utf8} rather than `TextEncoder`. `TextEncoder` is a web
 * API, not part of the language, and this module is meant to need nothing beyond the
 * language itself, so it runs unchanged in every runtime either SDK targets.
 *
 * The browser SDK and the Node SDK each hold a byte-identical copy of this file, so
 * neither package depends on the other. A test in the Node package fails when the two
 * copies differ.
 */

/** The initialisation vector, the same eight words SHA-256 starts from. */
const IV = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

/** Domain flags, mixed into every compression so a chunk, a parent and the root can never collide. */
const CHUNK_START = 1;
const CHUNK_END = 2;
const PARENT = 4;
const ROOT = 8;

const BLOCK_LEN = 64;
const CHUNK_LEN = 1024;

/**
 * The message word order for each of the seven rounds.
 *
 * Round 0 reads the words in order; every later round applies the BLAKE3 permutation
 * `2 6 3 10 7 0 4 13 1 11 12 5 9 14 15 8` to the previous order. Written out rather than
 * computed so the module has no start-up work.
 */
const SCHEDULE = [
  [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
  [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8],
  [3, 4, 10, 12, 13, 2, 7, 14, 6, 5, 9, 0, 11, 15, 8, 1],
  [10, 7, 12, 9, 14, 3, 13, 15, 4, 0, 11, 2, 5, 8, 1, 6],
  [12, 13, 9, 11, 15, 10, 14, 8, 7, 2, 5, 3, 0, 1, 6, 4],
  [9, 14, 11, 5, 8, 12, 15, 1, 13, 3, 0, 10, 2, 6, 4, 7],
  [11, 15, 5, 0, 1, 9, 8, 6, 14, 10, 2, 12, 3, 4, 7, 13],
];

const rotr = (x: number, n: number): number => (x >>> n) | (x << (32 - n));

/**
 * The quarter-round. `v` is a `Uint32Array`, so every store wraps modulo 2^32 and the
 * additions need no masking.
 */
const g = (v: Uint32Array, a: number, b: number, c: number, d: number, x: number, y: number): void => {
  v[a] = v[a] + v[b] + x;
  v[d] = rotr(v[d] ^ v[a], 16);
  v[c] = v[c] + v[d];
  v[b] = rotr(v[b] ^ v[c], 12);
  v[a] = v[a] + v[b] + y;
  v[d] = rotr(v[d] ^ v[a], 8);
  v[c] = v[c] + v[d];
  v[b] = rotr(v[b] ^ v[c], 7);
};

/**
 * The compression function, truncated to the eight words a chaining value or a 32-byte
 * root output needs.
 *
 * `counter` is the 64-bit chunk counter as a plain number: the low word is stored as is
 * (the typed array keeps the low 32 bits) and the high word is the quotient by 2^32,
 * which stays exact for any input a JavaScript runtime can hold in memory.
 */
function compress(
  cv: ArrayLike<number>,
  m: Uint32Array,
  counter: number,
  len: number,
  flags: number,
): Uint32Array {
  const v = new Uint32Array(16);
  for (let i = 0; i < 8; i++) v[i] = cv[i];
  v[8] = IV[0];
  v[9] = IV[1];
  v[10] = IV[2];
  v[11] = IV[3];
  v[12] = counter;
  v[13] = counter / 0x100000000;
  v[14] = len;
  v[15] = flags;
  for (const s of SCHEDULE) {
    g(v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
    g(v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
    g(v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
    g(v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
    g(v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
    g(v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
    g(v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
    g(v, 3, 4, 9, 14, m[s[14]], m[s[15]]);
  }
  const out = new Uint32Array(8);
  for (let i = 0; i < 8; i++) out[i] = v[i] ^ v[i + 8];
  return out;
}

/** Up to 64 bytes as sixteen little-endian words, zero-padded. */
function words(bytes: Uint8Array, start: number, end: number): Uint32Array {
  const out = new Uint32Array(16);
  for (let i = start; i < end; i++) out[(i - start) >> 2] |= bytes[i] << (((i - start) & 3) * 8);
  return out;
}

/**
 * The last compression of a node, held back until the caller knows whether that node is
 * the root: the root is compressed with the `ROOT` flag and counter zero, every other
 * node without, and nothing about the node itself says which it is.
 */
interface Pending {
  cv: ArrayLike<number>;
  block: Uint32Array;
  counter: number;
  len: number;
  flags: number;
}

const chainingValue = (p: Pending): Uint32Array => compress(p.cv, p.block, p.counter, p.len, p.flags);

/** One chunk: every block but the last compressed in turn, the last one held back. */
function chunk(input: Uint8Array, start: number, end: number, counter: number): Pending {
  let cv: ArrayLike<number> = IV;
  let flags = CHUNK_START;
  let at = start;
  while (end - at > BLOCK_LEN) {
    cv = compress(cv, words(input, at, at + BLOCK_LEN), counter, BLOCK_LEN, flags);
    flags = 0;
    at += BLOCK_LEN;
  }
  return { cv: cv, block: words(input, at, end), counter: counter, len: end - at, flags: flags | CHUNK_END };
}

/** A parent node over two chaining values. */
function parent(left: Uint32Array, right: Uint32Array): Pending {
  const block = new Uint32Array(16);
  block.set(left);
  block.set(right, 8);
  return { cv: IV, block: block, counter: 0, len: BLOCK_LEN, flags: PARENT };
}

/**
 * The BLAKE3 digest of `input`: 32 bytes, default hash mode.
 *
 * The tree is built the way the reference implementation builds it incrementally: a
 * chunk is only finished once more input is known to follow it (the final chunk may be
 * the root), and after the n-th chunk is pushed, one parent merge happens for every
 * trailing zero bit of n, which keeps the stack holding exactly the roots of the
 * complete subtrees so far. At the end the final chunk is folded into that stack from
 * the top down, and the last node standing is compressed as the root.
 */
export function blake3(input: Uint8Array): Uint8Array {
  const stack: Uint32Array[] = [];
  let counter = 0;
  let at = 0;
  while (input.length - at > CHUNK_LEN) {
    let cv = chainingValue(chunk(input, at, at + CHUNK_LEN, counter));
    counter++;
    for (let n = counter; n % 2 == 0; n /= 2) cv = chainingValue(parent(stack.pop() as Uint32Array, cv));
    stack.push(cv);
    at += CHUNK_LEN;
  }
  let node = chunk(input, at, input.length, counter);
  while (stack.length) node = parent(stack.pop() as Uint32Array, chainingValue(node));
  const h = compress(node.cv, node.block, 0, node.len, node.flags | ROOT);
  const out = new Uint8Array(32);
  for (let i = 0; i < 32; i++) out[i] = h[i >> 2] >>> ((i & 3) * 8);
  return out;
}

/**
 * A string as UTF-8 bytes, the way `TextEncoder` writes it.
 *
 * A surrogate pair becomes one four-byte sequence; a lone surrogate, which a JavaScript
 * string can hold and a UTF-8 string cannot, becomes U+FFFD, exactly as `TextEncoder`
 * replaces it. The Rust server never sees a lone surrogate (its strings cannot hold
 * one), so for every string both sides can represent the bytes are identical.
 */
export function utf8(text: string): Uint8Array {
  const out: number[] = [];
  for (let i = 0; i < text.length; i++) {
    let c = text.charCodeAt(i);
    if (c >= 0xd800 && c < 0xdc00 && i + 1 < text.length) {
      const low = text.charCodeAt(i + 1);
      if (low >= 0xdc00 && low < 0xe000) {
        c = 0x10000 + ((c - 0xd800) << 10) + (low - 0xdc00);
        i++;
      }
    }
    if (c >= 0xd800 && c < 0xe000) c = 0xfffd;
    if (c < 0x80) out.push(c);
    else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
    else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
    else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
  }
  return new Uint8Array(out);
}
