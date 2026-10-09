/// BLAKE3 in plain Swift: the default hash mode, 32 bytes of output, any input length.
///
/// # Why this file exists
///
/// A feature flag puts a visitor in a bucket with `blake3(salt ‖ subject)`, because that is
/// what the server does (`micaforge_core::flags::bucket`, which reuses the cookieless
/// visitor hash so the product has one hashing primitive). This SDK decides flags locally
/// as well, from a ruleset or a fallback rule, and a local decision is only worth having
/// if it lands every user in the bucket the server would choose. So the SDK needs the same
/// hash, byte for byte. CryptoKit has no BLAKE3 and the SDK takes no dependencies, so it is
/// written out here from the BLAKE3 specification, as a line-for-line port of the browser
/// SDK's copy (`packages/sdk/src/blake3.ts`). The shared vectors in
/// `packages/sdk/test/flag-vectors.json` hold all of them to the same digests.
///
/// # What it covers
///
/// The whole hash, not a short-input shortcut: input is split into 1024-byte chunks, each
/// chunk is compressed 64 bytes at a time, and chunk results are merged pairwise up the
/// binary tree, so an input of any length hashes correctly. Keyed hashing, key derivation
/// and extended output are left out because nothing here uses them. Flag keys and subjects
/// are short, so the compression function is written for clarity rather than throughput.
enum BLAKE3 {
    /// The initialisation vector, the same eight words SHA-256 starts from.
    private static let iv: [UInt32] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ]

    /// Domain flags, mixed into every compression so a chunk, a parent and the root can
    /// never collide.
    private static let chunkStart: UInt32 = 1
    private static let chunkEnd: UInt32 = 2
    private static let parentFlag: UInt32 = 4
    private static let rootFlag: UInt32 = 8

    private static let blockLen = 64
    private static let chunkLen = 1024

    /// The message word order for each of the seven rounds. Round 0 reads the words in
    /// order; every later round applies the BLAKE3 permutation to the previous order.
    /// Written out rather than computed so there is no start-up work.
    private static let schedule: [[Int]] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8],
        [3, 4, 10, 12, 13, 2, 7, 14, 6, 5, 9, 0, 11, 15, 8, 1],
        [10, 7, 12, 9, 14, 3, 13, 15, 4, 0, 11, 2, 5, 8, 1, 6],
        [12, 13, 9, 11, 15, 10, 14, 8, 7, 2, 5, 3, 0, 1, 6, 4],
        [9, 14, 11, 5, 8, 12, 15, 1, 13, 3, 0, 10, 2, 6, 4, 7],
        [11, 15, 5, 0, 1, 9, 8, 6, 14, 10, 2, 12, 3, 4, 7, 13],
    ]

    @inline(__always)
    private static func rotr(_ x: UInt32, _ n: UInt32) -> UInt32 { (x >> n) | (x << (32 - n)) }

    /// The quarter-round. Every addition wraps modulo 2^32.
    @inline(__always)
    private static func g(_ v: inout [UInt32], _ a: Int, _ b: Int, _ c: Int, _ d: Int, _ x: UInt32, _ y: UInt32) {
        v[a] = v[a] &+ v[b] &+ x
        v[d] = rotr(v[d] ^ v[a], 16)
        v[c] = v[c] &+ v[d]
        v[b] = rotr(v[b] ^ v[c], 12)
        v[a] = v[a] &+ v[b] &+ y
        v[d] = rotr(v[d] ^ v[a], 8)
        v[c] = v[c] &+ v[d]
        v[b] = rotr(v[b] ^ v[c], 7)
    }

    /// The compression function, truncated to the eight words a chaining value or a
    /// 32-byte root output needs.
    private static func compress(_ cv: [UInt32], _ m: [UInt32], _ counter: UInt64, _ len: Int, _ flags: UInt32) -> [UInt32] {
        var v = [UInt32](repeating: 0, count: 16)
        for i in 0..<8 { v[i] = cv[i] }
        v[8] = iv[0]
        v[9] = iv[1]
        v[10] = iv[2]
        v[11] = iv[3]
        v[12] = UInt32(truncatingIfNeeded: counter)
        v[13] = UInt32(truncatingIfNeeded: counter >> 32)
        v[14] = UInt32(len)
        v[15] = flags
        for s in schedule {
            g(&v, 0, 4, 8, 12, m[s[0]], m[s[1]])
            g(&v, 1, 5, 9, 13, m[s[2]], m[s[3]])
            g(&v, 2, 6, 10, 14, m[s[4]], m[s[5]])
            g(&v, 3, 7, 11, 15, m[s[6]], m[s[7]])
            g(&v, 0, 5, 10, 15, m[s[8]], m[s[9]])
            g(&v, 1, 6, 11, 12, m[s[10]], m[s[11]])
            g(&v, 2, 7, 8, 13, m[s[12]], m[s[13]])
            g(&v, 3, 4, 9, 14, m[s[14]], m[s[15]])
        }
        var out = [UInt32](repeating: 0, count: 8)
        for i in 0..<8 { out[i] = v[i] ^ v[i + 8] }
        return out
    }

    /// Up to 64 bytes as sixteen little-endian words, zero-padded.
    private static func words(_ bytes: [UInt8], _ start: Int, _ end: Int) -> [UInt32] {
        var out = [UInt32](repeating: 0, count: 16)
        var i = start
        while i < end {
            out[(i - start) >> 2] |= UInt32(bytes[i]) << UInt32(((i - start) & 3) * 8)
            i += 1
        }
        return out
    }

    /// The last compression of a node, held back until the caller knows whether that
    /// node is the root: the root is compressed with the root flag and counter zero, every
    /// other node without, and nothing about the node itself says which it is.
    private struct Pending {
        var cv: [UInt32]
        var block: [UInt32]
        var counter: UInt64
        var len: Int
        var flags: UInt32

        var chainingValue: [UInt32] { BLAKE3.compress(cv, block, counter, len, flags) }
    }

    /// One chunk: every block but the last compressed in turn, the last one held back.
    private static func chunk(_ input: [UInt8], _ start: Int, _ end: Int, _ counter: UInt64) -> Pending {
        var cv = iv
        var flags = chunkStart
        var at = start
        while end - at > blockLen {
            cv = compress(cv, words(input, at, at + blockLen), counter, blockLen, flags)
            flags = 0
            at += blockLen
        }
        return Pending(cv: cv, block: words(input, at, end), counter: counter, len: end - at, flags: flags | chunkEnd)
    }

    /// A parent node over two chaining values.
    private static func parent(_ left: [UInt32], _ right: [UInt32]) -> Pending {
        Pending(cv: iv, block: left + right, counter: 0, len: blockLen, flags: parentFlag)
    }

    /// The BLAKE3 digest of `input`: 32 bytes, default hash mode.
    ///
    /// The tree is built the way the reference implementation builds it incrementally: a
    /// chunk is only finished once more input is known to follow it (the final chunk may
    /// be the root), and after the n-th chunk is pushed, one parent merge happens for every
    /// trailing zero bit of n, which keeps the stack holding exactly the roots of the
    /// complete subtrees so far. At the end the final chunk is folded into that stack from
    /// the top down, and the last node standing is compressed as the root.
    static func hash(_ input: [UInt8]) -> [UInt8] {
        var stack: [[UInt32]] = []
        var counter: UInt64 = 0
        var at = 0
        while input.count - at > chunkLen {
            var cv = chunk(input, at, at + chunkLen, counter).chainingValue
            counter += 1
            var n = counter
            while n % 2 == 0 {
                cv = parent(stack.removeLast(), cv).chainingValue
                n /= 2
            }
            stack.append(cv)
            at += chunkLen
        }
        var node = chunk(input, at, input.count, counter)
        while let left = stack.popLast() {
            node = parent(left, node.chainingValue)
        }
        let h = compress(node.cv, node.block, 0, node.len, node.flags | rootFlag)
        var out = [UInt8](repeating: 0, count: 32)
        for i in 0..<32 { out[i] = UInt8(truncatingIfNeeded: h[i >> 2] >> UInt32((i & 3) * 8)) }
        return out
    }
}
