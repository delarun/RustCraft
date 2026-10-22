//! Intrinsics: Rust replacements for hot Java methods (HotSpot replaces the
//! same @IntrinsicCandidate methods with machine code). Each must behave
//! exactly like the Java code it replaces.

use super::*;
use crate::heap::ARRAY_BASE_OFFSET;

pub(super) fn register(n: &mut Natives) {
    n.add("java/lang/Integer.rotateLeft(II)I", |_, a| ret_i((a[0] as u32).rotate_left(a[1] as u32 & 31) as i32));
    n.add("java/lang/Integer.rotateRight(II)I", |_, a| ret_i((a[0] as u32).rotate_right(a[1] as u32 & 31) as i32));
    n.add("java/lang/Long.rotateLeft(JI)J", |_, a| ret_j(a[0].rotate_left(a[2] as u32 & 63) as i64));
    n.add("java/lang/Long.rotateRight(JI)J", |_, a| ret_j(a[0].rotate_right(a[2] as u32 & 63) as i64));
    n.add("java/lang/Integer.reverseBytes(I)I", |_, a| ret_i((a[0] as u32).swap_bytes() as i32));
    n.add("java/lang/Long.reverseBytes(J)J", |_, a| ret_j(a[0].swap_bytes() as i64));
    n.add("java/lang/Short.reverseBytes(S)S", |_, a| ret_i((a[0] as u16).swap_bytes() as i16 as i32));
    n.add("java/lang/Character.reverseBytes(C)C", |_, a| ret_i((a[0] as u16).swap_bytes() as i32));
    n.add("java/lang/Integer.bitCount(I)I", |_, a| ret_i((a[0] as u32).count_ones() as i32));
    n.add("java/lang/Long.bitCount(J)I", |_, a| ret_i(a[0].count_ones() as i32));
    n.add("java/lang/Integer.numberOfLeadingZeros(I)I", |_, a| ret_i((a[0] as u32).leading_zeros() as i32));
    n.add("java/lang/Integer.numberOfTrailingZeros(I)I", |_, a| ret_i((a[0] as u32).trailing_zeros() as i32));
    n.add("java/lang/Long.numberOfLeadingZeros(J)I", |_, a| ret_i(a[0].leading_zeros() as i32));
    n.add("java/lang/Long.numberOfTrailingZeros(J)I", |_, a| ret_i(a[0].trailing_zeros() as i32));
    n.add("java/lang/Byte.toUnsignedInt(B)I", |_, a| ret_i(a[0] as u8 as i32));

    n.add("java/lang/Math.max(II)I", |_, a| ret_i((a[0] as i32).max(a[1] as i32)));
    n.add("java/lang/Math.min(II)I", |_, a| ret_i((a[0] as i32).min(a[1] as i32)));
    n.add("java/lang/Math.max(JJ)J", |_, a| ret_j((a[0] as i64).max(a[2] as i64)));
    n.add("java/lang/Math.min(JJ)J", |_, a| ret_j((a[0] as i64).min(a[2] as i64)));
    n.add("java/lang/Math.abs(I)I", |_, a| ret_i((a[0] as i32).wrapping_abs()));
    n.add("java/lang/Math.abs(J)J", |_, a| ret_j((a[0] as i64).wrapping_abs()));
    n.add("java/lang/Math.abs(D)D", |_, a| ret_j((f64::from_bits(a[0]).abs()).to_bits() as i64));
    n.add("java/lang/Math.abs(F)F", |_, a| ret_i(f32::from_bits(a[0] as u32).abs().to_bits() as i32));
    n.add("java/lang/Math.sqrt(D)D", |_, a| ret_j(f64::from_bits(a[0]).sqrt().to_bits() as i64));
    n.add("java/lang/StrictMath.sqrt(D)D", |_, a| ret_j(f64::from_bits(a[0]).sqrt().to_bits() as i64));
    n.add("java/lang/Math.floor(D)D", |_, a| ret_j(f64::from_bits(a[0]).floor().to_bits() as i64));
    n.add("java/lang/Math.ceil(D)D", |_, a| ret_j(f64::from_bits(a[0]).ceil().to_bits() as i64));
    n.add("java/lang/StrictMath.floor(D)D", |_, a| ret_j(f64::from_bits(a[0]).floor().to_bits() as i64));
    n.add("java/lang/StrictMath.ceil(D)D", |_, a| ret_j(f64::from_bits(a[0]).ceil().to_bits() as i64));

    n.add("java/util/Objects.requireNonNull(Ljava/lang/Object;)Ljava/lang/Object;", |t, a| {
        ret_ref(t.null_check(arg_ref(a, 0))?)
    });

    // Unaligned access with explicit endianness (byte-array VarHandles, ByteBuffers).
    n.add("jdk/internal/misc/Unsafe.getIntUnaligned(Ljava/lang/Object;J)I", |t, a| ret_i(load::<i32>(t, a)));
    n.add("jdk/internal/misc/Unsafe.getIntUnaligned(Ljava/lang/Object;JZ)I", |t, a| {
        let v = load::<i32>(t, a);
        ret_i(if a[4] != 0 { v.to_be() } else { v.to_le() })
    });
    n.add("jdk/internal/misc/Unsafe.getLongUnaligned(Ljava/lang/Object;J)J", |t, a| ret_j(load::<i64>(t, a)));
    n.add("jdk/internal/misc/Unsafe.getLongUnaligned(Ljava/lang/Object;JZ)J", |t, a| {
        let v = load::<i64>(t, a);
        ret_j(if a[4] != 0 { v.to_be() } else { v.to_le() })
    });
    n.add("jdk/internal/misc/Unsafe.getShortUnaligned(Ljava/lang/Object;J)S", |t, a| ret_i(load::<i16>(t, a) as i32));
    n.add("jdk/internal/misc/Unsafe.getShortUnaligned(Ljava/lang/Object;JZ)S", |t, a| {
        let v = load::<i16>(t, a);
        ret_i((if a[4] != 0 { v.to_be() } else { v.to_le() }) as i32)
    });
    n.add("jdk/internal/misc/Unsafe.putIntUnaligned(Ljava/lang/Object;JI)V", |t, a| {
        store(t, a, a[4] as i32);
        ret_void()
    });
    n.add("jdk/internal/misc/Unsafe.putIntUnaligned(Ljava/lang/Object;JIZ)V", |t, a| {
        let v = a[4] as i32;
        store(t, a, if a[5] != 0 { v.to_be() } else { v.to_le() });
        ret_void()
    });
    n.add("jdk/internal/misc/Unsafe.putLongUnaligned(Ljava/lang/Object;JJ)V", |t, a| {
        store(t, a, a[4] as i64);
        ret_void()
    });
    n.add("jdk/internal/misc/Unsafe.putLongUnaligned(Ljava/lang/Object;JJZ)V", |t, a| {
        let v = a[4] as i64;
        store(t, a, if a[6] != 0 { v.to_be() } else { v.to_le() });
        ret_void()
    });

    n.add("sun/security/provider/SHA2.implCompress0([BI)V", |t, a| digest_blocks(t, a, Digest::Sha256, false));
    n.add("sun/security/provider/SHA5.implCompress0([BI)V", |t, a| digest_blocks(t, a, Digest::Sha512, false));
    n.add("sun/security/provider/SHA.implCompress0([BI)V", |t, a| digest_blocks(t, a, Digest::Sha1, false));
    n.add("sun/security/provider/MD5.implCompress0([BI)V", |t, a| digest_blocks(t, a, Digest::Md5, false));
    n.add("sun/security/provider/DigestBase.implCompressMultiBlock0([BII)I", |t, a| {
        let digest = match t.vm.class_of(arg_ref(a, 0)).super_classes().find_map(|c| match c.name.as_str() {
            "sun/security/provider/SHA2" => Some(Digest::Sha256),
            "sun/security/provider/SHA5" => Some(Digest::Sha512),
            "sun/security/provider/SHA" => Some(Digest::Sha1),
            "sun/security/provider/MD5" => Some(Digest::Md5),
            _ => None,
        }) {
            Some(digest) => digest,
            None => return Err(t.throw_new("java/lang/InternalError", Some("unknown digest"))),
        };
        digest_blocks(t, a, digest, true)
    });
}

fn address(t: &Thread<'_>, a: &[u64]) -> *mut u8 {
    let (base, offset) = (arg_ref(a, 1), arg_j(a, 2));
    if base.is_null() { offset as usize as *mut u8 } else { t.vm.heap.ptr(base, offset as u32) }
}

fn load<T: Copy>(t: &Thread<'_>, a: &[u64]) -> T {
    // SAFETY: Unsafe contract.
    unsafe { (address(t, a) as *const T).read_unaligned() }
}

fn store<T: Copy>(t: &Thread<'_>, a: &[u64], value: T) {
    // SAFETY: Unsafe contract.
    unsafe { (address(t, a) as *mut T).write_unaligned(value) }
}

#[derive(Clone, Copy)]
enum Digest {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

/// implCompress0(byte[] b, int ofs) for one block, or implCompressMultiBlock0(
/// byte[] b, int ofs, int limit) for all blocks with ofs <= limit (returns the
/// final offset). Operates on the digest's `state` array.
fn digest_blocks(t: &mut Thread<'_>, a: &[u64], digest: Digest, multi: bool) -> JResult<Option<u64>> {
    let this = arg_ref(a, 0);
    let input = t.null_check(arg_ref(a, 1))?;
    let mut ofs = arg_i(a, 2);
    let limit = if multi { arg_i(a, 3) } else { ofs };
    let (class, block) = match digest {
        Digest::Md5 => ("sun/security/provider/MD5", 64),
        Digest::Sha1 => ("sun/security/provider/SHA", 64),
        Digest::Sha256 => ("sun/security/provider/SHA2", 64),
        Digest::Sha512 => ("sun/security/provider/SHA5", 128),
    };
    let state = t.field_ref(this, class, "state");
    let heap = &t.vm.heap;
    let input_len = heap.array_length(input);
    while ofs <= limit {
        if ofs < 0 || ofs as i64 + block as i64 > input_len as i64 {
            return Err(t.throw_new("java/lang/ArrayIndexOutOfBoundsException", None));
        }
        let data = heap.bytes(input, ARRAY_BASE_OFFSET + ofs as u32, block).to_vec();
        match digest {
            Digest::Sha512 => {
                let words = heap.bytes_mut(state, ARRAY_BASE_OFFSET, 64);
                let mut h: [u64; 8] = std::array::from_fn(|i| u64::from_ne_bytes(words[i * 8..i * 8 + 8].try_into().unwrap()));
                sha512_compress(&mut h, &data);
                for (i, v) in h.iter().enumerate() {
                    words[i * 8..i * 8 + 8].copy_from_slice(&v.to_ne_bytes());
                }
            }
            _ => {
                let words_len = if matches!(digest, Digest::Sha256) { 8 } else if matches!(digest, Digest::Sha1) { 5 } else { 4 };
                let words = heap.bytes_mut(state, ARRAY_BASE_OFFSET, words_len * 4);
                let mut h: Vec<u32> = (0..words_len).map(|i| u32::from_ne_bytes(words[i * 4..i * 4 + 4].try_into().unwrap())).collect();
                match digest {
                    Digest::Sha256 => sha256_compress(&mut h, &data),
                    Digest::Sha1 => sha1_compress(&mut h, &data),
                    _ => md5_compress(&mut h, &data),
                }
                for (i, v) in h.iter().enumerate() {
                    words[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
                }
            }
        }
        ofs += block as i32;
    }
    if multi { ret_i(ofs) } else { ret_void() }
}

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
    0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
    0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256_compress(h: &mut [u32], block: &[u8]) {
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]];
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(SHA256_K[i]).wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (state, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
        *state = state.wrapping_add(v);
    }
}

const SHA512_K: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc, 0x3956c25bf348b538, 0x59f111f1b605d019,
    0x923f82a4af194f9b, 0xab1c5ed5da6d8118, 0xd807aa98a3030242, 0x12835b0145706fbe, 0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235, 0xc19bf174cf692694, 0xe49b69c19ef14ad2, 0xefbe4786384f25e3,
    0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65, 0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5,
    0x983e5152ee66dfab, 0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4, 0xc6e00bf33da88fc2, 0xd5a79147930aa725,
    0x06ca6351e003826f, 0x142929670a0e6e70, 0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed, 0x53380d139d95b3df,
    0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b, 0xa2bfe8a14cf10364, 0xa81a664bbc423001,
    0xc24b8b70d0f89791, 0xc76c51a30654be30, 0xd192e819d6ef5218, 0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8, 0x1e376c085141ab53, 0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8, 0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb,
    0x5b9cca4f7763e373, 0x682e6ff3d6b2b8a3, 0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b, 0xca273eceea26619c, 0xd186b8c721c0c207,
    0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178, 0x06f067aa72176fba, 0x0a637dc5a2c898a6, 0x113f9804bef90dae, 0x1b710b35131c471b,
    0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc, 0x431d67c49c100d4c, 0x4cc5d4becb3e42b6, 0x597f299cfc657e2a,
    0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

fn sha512_compress(h: &mut [u64; 8], block: &[u8]) {
    let mut w = [0u64; 80];
    for i in 0..16 {
        w[i] = u64::from_be_bytes(block[i * 8..i * 8 + 8].try_into().unwrap());
    }
    for i in 16..80 {
        let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
        let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for i in 0..80 {
        let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
        let ch = (e & f) ^ (!e & g);
        let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(SHA512_K[i]).wrapping_add(w[i]);
        let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (state, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
        *state = state.wrapping_add(v);
    }
}

fn sha1_compress(h: &mut [u32], block: &[u8]) {
    let mut w = [0u32; 80];
    for i in 0..16 {
        w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let [mut a, mut b, mut c, mut d, mut e] = [h[0], h[1], h[2], h[3], h[4]];
    for (i, &wi) in w.iter().enumerate() {
        let (f, k) = match i {
            0..20 => ((b & c) | (!b & d), 0x5A827999),
            20..40 => (b ^ c ^ d, 0x6ED9EBA1),
            40..60 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
            _ => (b ^ c ^ d, 0xCA62C1D6),
        };
        let temp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(wi);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = temp;
    }
    for (state, v) in h.iter_mut().zip([a, b, c, d, e]) {
        *state = state.wrapping_add(v);
    }
}

fn md5_compress(h: &mut [u32], block: &[u8]) {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16,
        23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let m: [u32; 16] = std::array::from_fn(|i| u32::from_le_bytes(block[i * 4..i * 4 + 4].try_into().unwrap()));
    let [mut a, mut b, mut c, mut d] = [h[0], h[1], h[2], h[3]];
    for i in 0..64 {
        let (f, g) = match i {
            0..16 => ((b & c) | (!b & d), i),
            16..32 => ((d & b) | (!d & c), (5 * i + 1) % 16),
            32..48 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | !d), (7 * i) % 16),
        };
        let k = (((i + 1) as f64).sin().abs() * 4294967296.0) as u32;
        let sum = a.wrapping_add(f).wrapping_add(k).wrapping_add(m[g]);
        a = d;
        d = c;
        c = b;
        b = b.wrapping_add(sum.rotate_left(S[i]));
    }
    for (state, v) in h.iter_mut().zip([a, b, c, d]) {
        *state = state.wrapping_add(v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: impl IntoIterator<Item = u8>) -> String {
        bytes.into_iter().map(|b| format!("{b:02x}")).collect()
    }

    fn pad(message: &[u8], block: usize, big_endian: bool) -> Vec<u8> {
        let mut data = message.to_vec();
        data.push(0x80);
        let len_bytes = if block == 128 { 16 } else { 8 };
        while data.len() % block != block - len_bytes {
            data.push(0);
        }
        let bits = (message.len() as u128) * 8;
        if big_endian {
            data.extend_from_slice(&bits.to_be_bytes()[16 - len_bytes..]);
        } else {
            data.extend_from_slice(&(bits as u64).to_le_bytes());
        }
        data
    }

    #[test]
    fn digests_match_reference_vectors() {
        let message = b"abc";
        let mut h256 = vec![0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
        for block in pad(message, 64, true).chunks(64) {
            sha256_compress(&mut h256, block);
        }
        assert_eq!(hex(h256.iter().flat_map(|w| w.to_be_bytes())), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");

        let mut h1 = vec![0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
        for block in pad(message, 64, true).chunks(64) {
            sha1_compress(&mut h1, block);
        }
        assert_eq!(hex(h1.iter().flat_map(|w| w.to_be_bytes())), "a9993e364706816aba3e25717850c26c9cd0d89d");

        let mut h5 = vec![0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
        for block in pad(message, 64, false).chunks(64) {
            md5_compress(&mut h5, block);
        }
        assert_eq!(hex(h5.iter().flat_map(|w| w.to_le_bytes())), "900150983cd24fb0d6963f7d28e17f72");

        let mut h512 = [
            0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1, 0x510e527fade682d1, 0x9b05688c2b3e6c1f,
            0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
        ];
        for block in pad(message, 128, true).chunks(128) {
            sha512_compress(&mut h512, block);
        }
        assert_eq!(
            hex(h512.iter().flat_map(|w| w.to_be_bytes())),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }
}
