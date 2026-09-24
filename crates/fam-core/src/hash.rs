//! SHA-256, and the lowercase hex rendering of a digest.
//!
//! Two services need this now - the audit chain and the MCP token digests - so it lives in the
//! shared foundation rather than being copied. It is written out rather than pulled in as a
//! dependency because the workspace carries no crypto crate and `sha2` would bring five with it,
//! while what both callers need is one thing: a collision-resistant one-way function over a byte
//! string. No HMAC schedule, no constant-time requirement, nothing an implementation bug could turn
//! into an *authentication* weakness beyond the digest itself.
//!
//! The trade is only worth making because the result is checkable, so the known-answer vectors below
//! are the published ones - and the round and IV tables were re-derived from their definition
//! (fractional parts of cube and square roots of primes) after five of the sixty-four were found to
//! have been typed from memory wrongly. Any single wrong constant fails every vector, which is what
//! makes the table trustworthy without a second implementation.

/// The IV of FIPS 180-4 section 5.3.3.
const INITIAL_STATE: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// Round constants, FIPS 180-4 section 4.2.2.
const ROUND_KEYS: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

const HEX: &[u8; 16] = b"0123456789abcdef";

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut state = INITIAL_STATE;
    let mut blocks = data.chunks_exact(64);
    for block in &mut blocks {
        compress(&mut state, block);
    }
    // `remainder` is empty when the length is a multiple of 64, which is the case that still needs a
    // whole padding block: the marker bit and the length cannot share the last byte.
    let tail = blocks.remainder();
    let mut last = [0u8; 128];
    last[..tail.len()].copy_from_slice(tail);
    last[tail.len()] = 0x80;
    let bits = (data.len() as u64).wrapping_mul(8);
    let pad_len = if tail.len() < 56 { 64 } else { 128 };
    last[pad_len - 8..pad_len].copy_from_slice(&bits.to_be_bytes());
    for block in last[..pad_len].chunks_exact(64) {
        compress(&mut state, block);
    }
    let mut digest = [0u8; 32];
    for (slot, word) in digest.chunks_exact_mut(4).zip(state) {
        slot.copy_from_slice(&word.to_be_bytes());
    }
    digest
}

pub fn hex_lower(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 0x0f) as usize] as char);
    }
    text
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex_lower(&sha256(data))
}

fn compress(state: &mut [u32; 8], block: &[u8]) {
    debug_assert_eq!(block.len(), 64);
    let mut schedule = [0u32; 64];
    for (word, chunk) in schedule.iter_mut().zip(block.chunks_exact(4)) {
        *word = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    for index in 16..64 {
        let s0 = schedule[index - 15].rotate_right(7)
            ^ schedule[index - 15].rotate_right(18)
            ^ (schedule[index - 15] >> 3);
        let s1 = schedule[index - 2].rotate_right(17)
            ^ schedule[index - 2].rotate_right(19)
            ^ (schedule[index - 2] >> 10);
        schedule[index] = schedule[index - 16]
            .wrapping_add(s0)
            .wrapping_add(schedule[index - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for index in 0..64 {
        let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choose = (e & f) ^ (!e & g);
        let temp1 = h
            .wrapping_add(sum1)
            .wrapping_add(choose)
            .wrapping_add(ROUND_KEYS[index])
            .wrapping_add(schedule[index]);
        let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = sum0.wrapping_add(majority);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }
    for (slot, word) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *slot = slot.wrapping_add(word);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(input: &[u8], expected: &str) {
        assert_eq!(sha256_hex(input), expected);
    }

    /// Every value here was produced by three independent programs on this machine - coreutils
    /// `sha256sum`, Python's `hashlib` and OpenSSL - which is the only reason a hand-written hash
    /// is worth believing.
    #[test]
    fn the_empty_message() {
        check(
            b"",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
    }

    #[test]
    fn the_fips_example() {
        check(
            b"abc",
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        );
    }

    /// 56 bytes: the length that puts the padding marker in the *second* block, which is where a
    /// hand-written padding routine goes wrong.
    #[test]
    fn the_padding_boundary() {
        check(
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        );
    }

    #[test]
    fn an_arbitrary_short_message() {
        check(
            b"hello world",
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9",
        );
    }

    /// 1000 bytes spans 16 blocks plus the tail, and 1e6 spans 15625 - both long enough that a
    /// mistake in the chaining of `state` cannot hide.
    #[test]
    fn long_messages() {
        check(
            &vec![b'a'; 1000],
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3",
        );
        check(
            &vec![b'a'; 1_000_000],
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
        );
    }

    /// The three lengths a hand-written padding routine gets wrong: 55 bytes still fits its length
    /// in the same block, 56 does not, and a multiple of the block size needs a padding block of
    /// its own because `0x80` will not share the last byte.
    #[test]
    fn every_padding_boundary() {
        for (len, expected) in [
            (
                55,
                "d5e285683cd4efc02d021a5c62014694958901005d6f71e89e0989fac77e4072",
            ),
            (
                56,
                "04c26261370ee7541549d16dee320c723e3fd14671e66a099afe0a377c16888e",
            ),
            (
                57,
                "ae14a2563ccf969d99aca69ce6bb74981f734bbf9f655f73b8f06db68cab5217",
            ),
            (
                63,
                "75220b47218278e656f2013bb8f0c455a25eaf01e86c64924e9d48d89776d6f2",
            ),
            (
                64,
                "7ce100971f64e7001e8fe5a51973ecdfe1ced42befe7ee8d5fd6219506b5393c",
            ),
            (
                65,
                "9537c5fdf120482f7d58d25e9ed583f52c02b4e304ea814db1633ad565aed7e9",
            ),
            (
                128,
                "24da1b81d0b16df6428eee73c69fcb2a93c76bc6df706f0c6670fe6bfe800464",
            ),
        ] {
            check(&vec![b'x'; len], expected);
        }
    }

    #[test]
    fn hex_is_lower_and_two_chars_per_byte() {
        assert_eq!(hex_lower(&[0x00, 0x0f, 0xff]), "000fff");
    }
}
