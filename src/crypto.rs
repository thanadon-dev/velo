use std::cell::RefCell;

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

pub struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    len: usize,
    total: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Sha256 { state: INIT, buf: [0; 64], len: 0, total: 0 }
    }
}

impl Sha256 {
    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.len > 0 {
            let take = (64 - self.len).min(data.len());
            self.buf[self.len..self.len + take].copy_from_slice(&data[..take]);
            self.len += take;
            data = &data[take..];
            if self.len < 64 {
                return;
            }
            let block = self.buf;
            self.compress(&block);
            self.len = 0;
        }
        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            self.compress(&block);
            data = &data[64..];
        }
        self.buf[..data.len()].copy_from_slice(data);
        self.len = data.len();
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.total.wrapping_mul(8);
        self.update_raw(&[0x80]);
        while self.len != 56 {
            self.update_raw(&[0]);
        }
        self.update_raw(&bits.to_be_bytes());
        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        out
    }

    fn update_raw(&mut self, data: &[u8]) {
        for b in data {
            self.buf[self.len] = *b;
            self.len += 1;
            if self.len == 64 {
                let block = self.buf;
                self.compress(&block);
                self.len = 0;
            }
        }
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[i * 4],
                block[i * 4 + 1],
                block[i * 4 + 2],
                block[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, add) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(add);
        }
    }
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::default();
    h.update(data);
    h.finish()
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&sha256(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= block[i];
        opad[i] ^= block[i];
    }
    let mut inner = Sha256::default();
    inner.update(&ipad);
    inner.update(data);
    let inner = inner.finish();
    let mut outer = Sha256::default();
    outer.update(&opad);
    outer.update(&inner);
    outer.finish()
}

fn pbkdf2(password: &[u8], salt: &[u8], rounds: u32) -> [u8; 32] {
    let mut msg = Vec::with_capacity(salt.len() + 4);
    msg.extend_from_slice(salt);
    msg.extend_from_slice(&1u32.to_be_bytes());
    let mut u = hmac_sha256(password, &msg);
    let mut out = u;
    for _ in 1..rounds.max(1) {
        u = hmac_sha256(password, &u);
        for (o, x) in out.iter_mut().zip(u.iter()) {
            *o ^= x;
        }
    }
    out
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xf) as usize] as char);
    }
    out
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let raw = text.as_bytes();
    let mut out = Vec::with_capacity(raw.len() / 2);
    for pair in raw.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn rounds() -> u32 {
    std::env::var("VELO_KDF_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(100_000)
}

pub fn password(plain: &str) -> String {
    let mut salt = [0u8; 16];
    random(&mut salt);
    let n = rounds();
    let dk = pbkdf2(plain.as_bytes(), &salt, n);
    format!("pbkdf2${n}${}${}", hex(&salt), hex(&dk))
}

pub fn verify(plain: &str, stored: &str) -> bool {
    let mut parts = stored.split('$');
    if parts.next() != Some("pbkdf2") {
        return false;
    }
    let Some(Ok(n)) = parts.next().map(str::parse::<u32>) else {
        return false;
    };
    let (Some(salt), Some(want)) = (parts.next().and_then(unhex), parts.next().and_then(unhex))
    else {
        return false;
    };
    if parts.next().is_some() || n == 0 || want.len() != 32 {
        return false;
    }
    ct_eq(&pbkdf2(plain.as_bytes(), &salt, n), &want)
}

thread_local! {
    static POOL: RefCell<Sha256Drbg> = const {
        RefCell::new(Sha256Drbg { key: [0; 32], counter: 0, block: [0; 32], used: 32, ready: false })
    };
}

const RATCHET_EVERY: u64 = 256;

struct Sha256Drbg {
    key: [u8; 32],
    counter: u64,
    block: [u8; 32],
    used: usize,
    ready: bool,
}

impl Sha256Drbg {
    fn seed(&mut self) {
        let mut seed = [0u8; 32];
        if !system_entropy(&mut seed) {
            panic!("velo: /dev/urandom is unreadable, refusing to generate weak randomness");
        }
        let mut h = Sha256::default();
        h.update(&seed);
        h.update(&(std::process::id() as u64).to_le_bytes());
        self.key = h.finish();
        self.ready = true;
    }

    fn refill(&mut self) {
        let mut h = Sha256::default();
        h.update(&self.key);
        h.update(&self.counter.to_le_bytes());
        self.counter += 1;
        self.block = h.finish();
        self.used = 0;
        if self.counter % RATCHET_EVERY == 0 {
            let mut step = Sha256::default();
            step.update(b"velo-ratchet");
            step.update(&self.key);
            step.update(&self.block);
            self.key = step.finish();
        }
    }

    fn fill(&mut self, out: &mut [u8]) {
        if !self.ready {
            self.seed();
        }
        let mut at = 0;
        while at < out.len() {
            if self.used == self.block.len() {
                self.refill();
            }
            let take = (self.block.len() - self.used).min(out.len() - at);
            out[at..at + take].copy_from_slice(&self.block[self.used..self.used + take]);
            self.used += take;
            at += take;
        }
    }
}

fn system_entropy(out: &mut [u8; 32]) -> bool {
    use std::io::Read;
    match std::fs::File::open("/dev/urandom") {
        Ok(mut f) => f.read_exact(out).is_ok(),
        Err(_) => false,
    }
}

pub fn random(out: &mut [u8]) {
    POOL.with(|cell| cell.borrow_mut().fill(out));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_published_vectors() {
        assert_eq!(
            hex(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let long = vec![b'a'; 1_000_000];
        assert_eq!(
            hex(&sha256(&long)),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        let mut split = Sha256::default();
        for piece in b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".chunks(7) {
            split.update(piece);
        }
        assert_eq!(
            hex(&split.finish()),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn hmac_matches_published_vectors() {
        assert_eq!(
            hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hex(&hmac_sha256(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn pbkdf2_matches_published_vectors() {
        assert_eq!(
            hex(&pbkdf2(b"password", b"salt", 1)),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        assert_eq!(
            hex(&pbkdf2(b"password", b"salt", 2)),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
        );
        assert_eq!(
            hex(&pbkdf2(b"password", b"salt", 4096)),
            "c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"
        );
    }

    #[test]
    fn a_password_verifies_only_against_itself() {
        std::env::set_var("VELO_KDF_ROUNDS", "1000");
        let stored = password("hunter2");
        assert!(stored.starts_with("pbkdf2$1000$"));
        assert!(verify("hunter2", &stored));
        assert!(!verify("hunter3", &stored));
        assert!(!verify("", &stored));
        assert_ne!(stored, password("hunter2"));
        assert!(!verify("hunter2", "pbkdf2$1000$zz$zz"));
        assert!(!verify("hunter2", "hunter2"));
        assert!(!verify("hunter2", ""));
        std::env::remove_var("VELO_KDF_ROUNDS");
    }

    #[test]
    fn random_bytes_do_not_repeat() {
        let mut a = [0u8; 48];
        let mut b = [0u8; 48];
        random(&mut a);
        random(&mut b);
        assert_ne!(a, b);
        assert!(a.iter().any(|x| *x != 0));
        let mut seen = std::collections::HashSet::new();
        for _ in 0..2_000 {
            let mut small = [0u8; 7];
            random(&mut small);
            assert!(seen.insert(small), "a buffered draw repeated");
        }
    }
}
