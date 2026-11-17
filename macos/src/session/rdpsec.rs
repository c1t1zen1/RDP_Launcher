//! Standard RDP Security (MS-RDPBCGR 5.3): the RSA key exchange and RC4 + MAC packet
//! encryption that servers such as xrdp with `security_layer=rdp` still use instead of
//! TLS. IronRDP has none of it; [`crate::session::legacy`] applies it to the byte stream.

use anyhow::{bail, ensure, Context as _};
use ironrdp_pdu::gcc::EncryptionMethod;
use md5::{Digest as _, Md5};
use num_bigint::BigUint;
use sha1::Sha1;

// Basic Security Header flags (MS-RDPBCGR 2.2.8.1.1.2.1).
pub const SEC_EXCHANGE_PKT: u16 = 0x0001;
pub const SEC_TRANSPORT_REQ: u16 = 0x0002;
pub const SEC_TRANSPORT_RSP: u16 = 0x0004;
pub const SEC_ENCRYPT: u16 = 0x0008;
pub const SEC_INFO_PKT: u16 = 0x0040;
pub const SEC_LICENSE_PKT: u16 = 0x0080;
pub const SEC_LICENSE_ENCRYPT_CS: u16 = 0x0200;
pub const SEC_REDIRECTION_PKT: u16 = 0x0400;
pub const SEC_SECURE_CHECKSUM: u16 = 0x0800;
pub const SEC_AUTODETECT_REQ: u16 = 0x1000;
pub const SEC_AUTODETECT_RSP: u16 = 0x2000;
pub const SEC_HEARTBEAT: u16 = 0x4000;

pub const CLIENT_RANDOM_LEN: usize = 32;
pub const SIGNATURE_LEN: usize = 8;

/// Keys are refreshed after this many packets in one direction (MS-RDPBCGR 5.3.7).
const KEY_UPDATE_INTERVAL: u32 = 4096;
const PAD1: [u8; 40] = [0x36; 40];
const PAD2: [u8; 48] = [0x5c; 48];

/// The negotiated RC4 key strength. FIPS (3DES) is not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strength {
    Bits40,
    Bits56,
    Bits128,
}

impl Strength {
    pub fn from_method(method: EncryptionMethod) -> Option<Self> {
        match method {
            m if m == EncryptionMethod::BIT_40 => Some(Self::Bits40),
            m if m == EncryptionMethod::BIT_56 => Some(Self::Bits56),
            m if m == EncryptionMethod::BIT_128 => Some(Self::Bits128),
            _ => None,
        }
    }

    fn key_len(self) -> usize {
        match self {
            Self::Bits128 => 16,
            Self::Bits40 | Self::Bits56 => 8,
        }
    }

    /// 40- and 56-bit keys are 64-bit keys with their first bytes fixed (5.3.5.1).
    fn salt(self, key: &mut [u8]) {
        match self {
            Self::Bits40 => key[..3].copy_from_slice(&[0xd1, 0x26, 0x9e]),
            Self::Bits56 => key[0] = 0xd1,
            Self::Bits128 => {}
        }
    }
}

/// Session keys derived from the client and server randoms (5.3.5.1, non-FIPS).
pub struct SessionKeys {
    strength: Strength,
    mac: [u8; 16],
    /// Client to server.
    encrypt: [u8; 16],
    /// Server to client.
    decrypt: [u8; 16],
}

impl SessionKeys {
    pub fn derive(client_random: &[u8; 32], server_random: &[u8; 32], strength: Strength) -> Self {
        let salted_hash = |secret: &[u8; 48], label: &[u8]| -> [u8; 16] {
            let sha = Sha1::new()
                .chain_update(label)
                .chain_update(secret)
                .chain_update(client_random)
                .chain_update(server_random)
                .finalize();
            Md5::new().chain_update(secret).chain_update(sha).finalize().into()
        };
        let hash48 = |secret: &[u8; 48], labels: [&[u8]; 3]| -> [u8; 48] {
            let mut out = [0; 48];
            for (chunk, label) in out.chunks_mut(16).zip(labels) {
                chunk.copy_from_slice(&salted_hash(secret, label));
            }
            out
        };
        let final_hash = |key: &[u8]| -> [u8; 16] {
            Md5::new().chain_update(key).chain_update(client_random).chain_update(server_random).finalize().into()
        };

        let mut pre_master = [0; 48];
        pre_master[..24].copy_from_slice(&client_random[..24]);
        pre_master[24..].copy_from_slice(&server_random[..24]);
        let master = hash48(&pre_master, [b"A", b"BB", b"CCC"]);
        let blob = hash48(&master, [b"X", b"YY", b"ZZZ"]);

        let mut keys = Self {
            strength,
            mac: blob[..16].try_into().expect("16 bytes"),
            decrypt: final_hash(&blob[16..32]),
            encrypt: final_hash(&blob[32..48]),
        };
        for key in [&mut keys.mac, &mut keys.encrypt, &mut keys.decrypt] {
            strength.salt(key);
        }
        keys
    }

    /// (client → server, server → client) ciphers for the client side of the connection.
    pub fn client_ciphers(&self) -> (Cipher, Cipher) {
        (Cipher::new(self.strength, self.mac, self.encrypt), Cipher::new(self.strength, self.mac, self.decrypt))
    }

    /// The server side's (outgoing, incoming) ciphers, for tests.
    #[cfg(test)]
    pub fn server_ciphers(&self) -> (Cipher, Cipher) {
        (Cipher::new(self.strength, self.mac, self.decrypt), Cipher::new(self.strength, self.mac, self.encrypt))
    }
}

/// One direction of an encrypted connection: RC4 plus the MAC signature, with the
/// periodic key update. Packets must be processed in the order they are sent.
pub struct Cipher {
    strength: Strength,
    mac_key: [u8; 16],
    initial_key: [u8; 16],
    key: [u8; 16],
    rc4: Rc4,
    /// Packets since the last key update.
    since_update: u32,
    /// Packets so far, for salted MACs.
    count: u32,
}

impl Cipher {
    fn new(strength: Strength, mac_key: [u8; 16], key: [u8; 16]) -> Self {
        let rc4 = Rc4::new(&key[..strength.key_len()]);
        Self { strength, mac_key, initial_key: key, key, rc4, since_update: 0, count: 0 }
    }

    /// Sign `data` (plain MAC), then encrypt it in place. Returns the signature.
    pub fn seal(&mut self, data: &mut [u8]) -> [u8; SIGNATURE_LEN] {
        let signature = self.signature(data, None);
        self.apply(data);
        signature
    }

    /// Decrypt `data` in place and check its signature (salted when `salted`).
    pub fn open(&mut self, data: &mut [u8], signature: &[u8], salted: bool) -> anyhow::Result<()> {
        let count = self.count;
        self.apply(data);
        let expected = self.signature(data, salted.then_some(count));
        ensure!(expected.as_slice() == signature, "packet signature mismatch (data corrupted or tampered with)");
        Ok(())
    }

    fn apply(&mut self, data: &mut [u8]) {
        if self.since_update == KEY_UPDATE_INTERVAL {
            self.update_key();
        }
        self.rc4.apply(data);
        self.since_update += 1;
        self.count = self.count.wrapping_add(1);
    }

    /// MS-RDPBCGR 5.3.7.
    fn update_key(&mut self) {
        let len = self.strength.key_len();
        let sha = Sha1::new()
            .chain_update(&self.initial_key[..len])
            .chain_update(PAD1)
            .chain_update(&self.key[..len])
            .finalize();
        let md5 = Md5::new().chain_update(&self.initial_key[..len]).chain_update(PAD2).chain_update(sha).finalize();
        let mut key = [0; 16];
        key[..len].copy_from_slice(&md5[..len]);
        Rc4::new(&key[..len]).apply(&mut key[..len]);
        self.strength.salt(&mut key);
        self.key = key;
        self.rc4 = Rc4::new(&key[..len]);
        self.since_update = 0;
    }

    /// MS-RDPBCGR 5.3.6.1, and 5.3.6.1.1 when salted with the packet count.
    fn signature(&self, data: &[u8], salt: Option<u32>) -> [u8; SIGNATURE_LEN] {
        let key = &self.mac_key[..self.strength.key_len()];
        let length = u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes();
        let mut sha = Sha1::new().chain_update(key).chain_update(PAD1).chain_update(length).chain_update(data);
        if let Some(count) = salt {
            sha.update(count.to_le_bytes());
        }
        let md5 = Md5::new().chain_update(key).chain_update(PAD2).chain_update(sha.finalize()).finalize();
        md5[..SIGNATURE_LEN].try_into().expect("8 bytes")
    }
}

struct Rc4 {
    state: [u8; 256],
    i: u8,
    j: u8,
}

impl Rc4 {
    fn new(key: &[u8]) -> Self {
        let mut state = [0u8; 256];
        for (i, s) in state.iter_mut().enumerate() {
            *s = i as u8;
        }
        let mut j = 0u8;
        for i in 0..256 {
            j = j.wrapping_add(state[i]).wrapping_add(key[i % key.len()]);
            state.swap(i, usize::from(j));
        }
        Self { state, i: 0, j: 0 }
    }

    fn apply(&mut self, data: &mut [u8]) {
        for byte in data {
            self.i = self.i.wrapping_add(1);
            self.j = self.j.wrapping_add(self.state[usize::from(self.i)]);
            self.state.swap(usize::from(self.i), usize::from(self.j));
            let k = self.state[usize::from(self.state[usize::from(self.i)].wrapping_add(self.state[usize::from(self.j)]))];
            *byte ^= k;
        }
    }
}

/// The server's RSA public key, from the certificate in its Server Security Data.
#[derive(Debug, PartialEq, Eq)]
pub struct ServerKey {
    /// Little-endian, without the 8 padding bytes.
    modulus: Vec<u8>,
    exponent: u32,
}

const CERT_CHAIN_VERSION_1: u32 = 1;
const CERT_CHAIN_VERSION_2: u32 = 2;
const BB_RSA_KEY_BLOB: u16 = 0x0006;
const RSA_MAGIC: u32 = 0x3141_5352; // "RSA1"

impl ServerKey {
    /// Parse a proprietary server certificate (MS-RDPBCGR 2.2.1.4.3.1.1). Its signature uses
    /// a well-known key and proves nothing, so the server's identity is trusted on first use.
    pub fn from_certificate(cert: &[u8]) -> anyhow::Result<Self> {
        let mut r = Reader(cert);
        let version = r.u32()? & 0x7fff_ffff;
        if version == CERT_CHAIN_VERSION_2 {
            bail!("the server uses an X.509 certificate chain for Standard RDP Security, which isn't supported");
        }
        ensure!(version == CERT_CHAIN_VERSION_1, "unknown server certificate version {version}");
        let _signature_algorithm = r.u32()?;
        let _key_algorithm = r.u32()?;
        ensure!(r.u16()? == BB_RSA_KEY_BLOB, "server certificate has no RSA public key");
        let blob_len = usize::from(r.u16()?);
        let mut blob = Reader(r.take(blob_len)?);
        ensure!(blob.u32()? == RSA_MAGIC, "malformed RSA public key in server certificate");
        let key_len = blob.u32()? as usize;
        let bit_len = blob.u32()? as usize;
        let _data_len = blob.u32()?;
        let exponent = blob.u32()?;
        let modulus = blob.take(key_len)?;
        let modulus_len = bit_len.div_ceil(8);
        ensure!(modulus_len >= 64 && modulus_len <= key_len, "unsupported RSA key size ({bit_len} bits)");
        Ok(Self { modulus: modulus[..modulus_len].to_vec(), exponent })
    }

    /// The Security Exchange PDU payload (2.2.1.10.1): the client random encrypted with
    /// the server's key, little-endian, followed by 8 bytes of zero padding.
    pub fn encrypt_client_random(&self, client_random: &[u8; CLIENT_RANDOM_LEN]) -> Vec<u8> {
        let n = BigUint::from_bytes_le(&self.modulus);
        let c = BigUint::from_bytes_le(client_random).modpow(&BigUint::from(self.exponent), &n);
        let mut out = c.to_bytes_le();
        out.resize(self.modulus.len() + 8, 0);
        out
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> anyhow::Result<&'a [u8]> {
        ensure!(self.0.len() >= n, "server certificate is truncated");
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }

    fn u16(&mut self) -> anyhow::Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().context("u16")?))
    }

    fn u32(&mut self) -> anyhow::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().context("u32")?))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    /// A throwaway 512-bit RSA key (little-endian), generated for these tests.
    pub(crate) const TEST_MODULUS_LE: &str = "1f4df3ac35420e859fbf549fc83ab0dc3e5e5f36df43e1f5b880b20c8010c589\
                                               b3e00444d6b4b18b0adc014d2906fad5f3ea47897b5375bf582e318df50e0598";
    const TEST_PRIVATE_EXPONENT_LE: &str = "c1b8f4180db76f1957d8e02c854f523f65c7c0fe1dbebad03fb67f4a99d035de\
                                            5466d10dedde6afc947531466999400a93298c7d870a1c68df9f1c9bfca55b2b";

    /// A proprietary server certificate around the test key, as xrdp would send it.
    pub(crate) fn test_certificate() -> Vec<u8> {
        let modulus = hex(TEST_MODULUS_LE);
        let key_len = modulus.len() as u32 + 8;
        let mut key = Vec::new();
        key.extend(RSA_MAGIC.to_le_bytes());
        key.extend(key_len.to_le_bytes());
        key.extend((modulus.len() as u32 * 8).to_le_bytes());
        key.extend((modulus.len() as u32 - 1).to_le_bytes());
        key.extend(65537u32.to_le_bytes());
        key.extend(&modulus);
        key.extend([0; 8]);

        let mut cert = Vec::new();
        cert.extend(CERT_CHAIN_VERSION_1.to_le_bytes());
        cert.extend(1u32.to_le_bytes()); // SIGNATURE_ALG_RSA
        cert.extend(1u32.to_le_bytes()); // KEY_EXCHANGE_ALG_RSA
        cert.extend(BB_RSA_KEY_BLOB.to_le_bytes());
        cert.extend((key.len() as u16).to_le_bytes());
        cert.extend(key);
        cert.extend(0x0008u16.to_le_bytes()); // BB_RSA_SIGNATURE_BLOB
        cert.extend(72u16.to_le_bytes());
        cert.extend([0xab; 72]);
        cert
    }

    /// What the server does with the Security Exchange payload.
    pub(crate) fn decrypt_client_random(exchange: &[u8]) -> [u8; CLIENT_RANDOM_LEN] {
        let n = BigUint::from_bytes_le(&hex(TEST_MODULUS_LE));
        let d = BigUint::from_bytes_le(&hex(TEST_PRIVATE_EXPONENT_LE));
        let mut random = BigUint::from_bytes_le(&exchange[..64]).modpow(&d, &n).to_bytes_le();
        random.resize(CLIENT_RANDOM_LEN, 0);
        random.try_into().unwrap()
    }

    #[test]
    fn rc4_matches_published_vectors() {
        for (key, plain, cipher) in [
            ("Key", "Plaintext", "bbf316e8d940af0ad3"),
            ("Wiki", "pedia", "1021bf0420"),
            ("Secret", "Attack at dawn", "45a01f645fc35b383552544b9bf5"),
        ] {
            let mut data = plain.as_bytes().to_vec();
            Rc4::new(key.as_bytes()).apply(&mut data);
            assert_eq!(data, hex(cipher));
        }
    }

    #[test]
    fn strength_follows_the_server_selected_method() {
        assert_eq!(Strength::from_method(EncryptionMethod::BIT_128), Some(Strength::Bits128));
        assert_eq!(Strength::from_method(EncryptionMethod::BIT_56), Some(Strength::Bits56));
        assert_eq!(Strength::from_method(EncryptionMethod::BIT_40), Some(Strength::Bits40));
        assert_eq!(Strength::from_method(EncryptionMethod::FIPS), None);
        assert_eq!(Strength::from_method(EncryptionMethod::empty()), None);
    }

    #[test]
    fn short_keys_are_salted() {
        let keys = SessionKeys::derive(&[1; 32], &[2; 32], Strength::Bits40);
        assert_eq!(keys.encrypt[..3], [0xd1, 0x26, 0x9e]);
        assert_eq!(keys.mac[..3], [0xd1, 0x26, 0x9e]);
        let keys = SessionKeys::derive(&[1; 32], &[2; 32], Strength::Bits56);
        assert_eq!(keys.decrypt[0], 0xd1);
    }

    #[test]
    fn both_sides_agree_across_key_updates() {
        for strength in [Strength::Bits40, Strength::Bits56, Strength::Bits128] {
            let keys = SessionKeys::derive(&[7; 32], &[9; 32], strength);
            let (mut client_out, mut client_in) = keys.client_ciphers();
            let (mut server_out, mut server_in) = keys.server_ciphers();
            for n in 0..(KEY_UPDATE_INTERVAL * 2 + 10) {
                let plain = format!("packet {n}").into_bytes();
                let mut data = plain.clone();
                let signature = client_out.seal(&mut data);
                assert_ne!(data, plain);
                server_in.open(&mut data, &signature, false).unwrap();
                assert_eq!(data, plain);

                let signature = server_out.seal(&mut data);
                client_in.open(&mut data, &signature, false).unwrap();
                assert_eq!(data, plain);
            }
        }
    }

    #[test]
    fn tampered_packets_are_rejected() {
        let keys = SessionKeys::derive(&[3; 32], &[4; 32], Strength::Bits128);
        let (mut client_out, _) = keys.client_ciphers();
        let (_, mut server_in) = keys.server_ciphers();
        let mut data = b"hello".to_vec();
        let signature = client_out.seal(&mut data);
        data[0] ^= 1;
        assert!(server_in.open(&mut data, &signature, false).is_err());
    }

    #[test]
    fn salted_signatures_include_the_packet_count() {
        let keys = SessionKeys::derive(&[5; 32], &[6; 32], Strength::Bits128);
        let (mut server_out, _) = keys.server_ciphers();
        let (_, mut client_in) = keys.client_ciphers();
        for count in 0..3u32 {
            let plain = b"salted".to_vec();
            let salted = server_out.signature(&plain, Some(count));
            let mut data = plain.clone();
            server_out.apply(&mut data);
            client_in.open(&mut data, &salted, true).unwrap();
            assert_eq!(data, plain);
        }
    }

    #[test]
    fn proprietary_certificate_key_round_trips_the_client_random() {
        let key = ServerKey::from_certificate(&test_certificate()).unwrap();
        assert_eq!(key.exponent, 65537);
        assert_eq!(key.modulus, hex(TEST_MODULUS_LE));
        let random: [u8; 32] = core::array::from_fn(|i| i as u8 + 1);
        let exchange = key.encrypt_client_random(&random);
        assert_eq!(exchange.len(), 64 + 8);
        assert_eq!(exchange[64..], [0; 8]);
        assert_eq!(decrypt_client_random(&exchange), random);
    }

    #[test]
    fn x509_and_malformed_certificates_are_rejected() {
        let mut x509 = CERT_CHAIN_VERSION_2.to_le_bytes().to_vec();
        x509.extend([0; 16]);
        assert!(ServerKey::from_certificate(&x509).unwrap_err().to_string().contains("X.509"));
        let cert = test_certificate();
        assert!(ServerKey::from_certificate(&cert[..40]).is_err());
        assert!(ServerKey::from_certificate(&[]).is_err());
    }
}
