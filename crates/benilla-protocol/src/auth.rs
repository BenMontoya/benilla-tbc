//! The Burning Crusade 2.4.3 realmd wire protocol: logon challenge, logon proof and realm list,
//! the three exchanges [`crate::logon`] performs. Login packets are not header-encrypted and
//! each is self-delimiting, so they are read in request/response order.

use std::io::{Read, Write};
use std::net::Ipv4Addr;

use anyhow::{bail, Result};
use sha1::{Digest, Sha1};

use crate::wire::{read_array, read_cstring, read_f32_le, read_u16_le, read_u32_le, read_u8};
use crate::{RealmBuild, RealmInfo};

const CMD_AUTH_LOGON_CHALLENGE: u8 = 0x00;
const CMD_AUTH_LOGON_PROOF: u8 = 0x01;
const CMD_REALM_LIST: u8 = 0x10;

const PROTOCOL_VERSION_THREE: u8 = 3;
const GAME_NAME_WOW: u32 = 0x0057_6f57; // "WoW\0" little-endian
const PLATFORM_X86: u32 = 0x0078_3836; // "x86\0"
                                       // Tags are little-endian u32s, so they reach the
                                       // wire reversed (`0x0057696e` is `n i W \0`) and
                                       // realmd reverses them back. vmangos fails a session
                                       // on anything but "Win" or "OSX".
const OS_WINDOWS: u32 = 0x0057_696e; // "Win\0"
const OS_MACOS: u32 = 0x004F_5358; // "OSX\0"

/// The host's stock-client OS tag: `OSX` on macOS, else `Win`. TBC realmd uses it to select the
/// build-integrity hash and the world server uses it for platform-specific anticheat behavior.
const fn client_os() -> u32 {
    if cfg!(target_os = "macos") {
        OS_MACOS
    } else {
        OS_WINDOWS
    }
}
const LOCALE_EN_US: u32 = 0x656e_5553; // "enUS"

/// A non-success auth result byte, typed so the app can map it to its `AUTH_*` glue string; it
/// survives [`crate::logon`]'s `anyhow` contexts via `downcast_ref`. vmangos answers an unknown
/// account and a wrong password alike with 0x04 (`WOW_FAIL_UNKNOWN_ACCOUNT`, `AuthCodes.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthReject {
    /// The grunt result byte (`WOW_FAIL_*`).
    pub code: u8,
}

impl std::fmt::Display for AuthReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "server rejected logon: result {:#04x}", self.code)
    }
}

impl std::error::Error for AuthReject {}

/// The account requires the 2.4.3 authenticator extension, but the caller supplied no token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthenticatorRequired;

impl std::fmt::Display for AuthenticatorRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("server requires an authenticator token")
    }
}

impl std::error::Error for AuthenticatorRequired {}

/// The SRP6 inputs from a successful `CMD_AUTH_LOGON_CHALLENGE_Server`.
pub struct ChallengeReply {
    pub server_public_key: [u8; 32],
    pub generator: u8,
    pub large_safe_prime: [u8; 32],
    pub salt: [u8; 32],
    /// The version challenge answered by `crc_hash` ([`version_proof`]); not an SRP6 input.
    pub crc_salt: [u8; 16],
    /// TBC security flag 0x04. When set, the proof appends a length-prefixed decimal token.
    pub authenticator_required: bool,
}

/// Send `CMD_AUTH_LOGON_CHALLENGE_Client`. `account_name` must be uppercased, as the SRP6 hashes
/// use it; the hard fork advertises version 2.4.3/build 8606.
pub fn write_logon_challenge(
    w: &mut impl Write,
    account_name: &str,
    build: u16,
) -> std::io::Result<()> {
    // Everything after the 2-byte size field, assembled first so we can prefix its length.
    let mut body = Vec::with_capacity(34 + account_name.len());
    body.extend_from_slice(&GAME_NAME_WOW.to_le_bytes());
    body.push(2); // version major
    body.push(4); // version minor
    body.push(3); // version patch
    body.extend_from_slice(&build.to_le_bytes());
    body.extend_from_slice(&PLATFORM_X86.to_le_bytes());
    body.extend_from_slice(&client_os().to_le_bytes());
    body.extend_from_slice(&LOCALE_EN_US.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes()); // utc_timezone_offset
    body.extend_from_slice(&Ipv4Addr::LOCALHOST.octets()); // client_ip_address (network order)
    body.push(account_name.len() as u8);
    body.extend_from_slice(account_name.as_bytes());

    let mut packet = Vec::with_capacity(4 + body.len());
    packet.push(CMD_AUTH_LOGON_CHALLENGE);
    packet.push(PROTOCOL_VERSION_THREE);
    packet.extend_from_slice(&(body.len() as u16).to_le_bytes());
    packet.extend_from_slice(&body);
    w.write_all(&packet)
}

/// Read the challenge reply; a non-success result is an [`AuthReject`].
pub fn read_challenge_reply(r: &mut impl Read) -> Result<ChallengeReply> {
    let opcode = read_u8(r)?;
    if opcode != CMD_AUTH_LOGON_CHALLENGE {
        bail!("expected CMD_AUTH_LOGON_CHALLENGE (0x00), got {opcode:#x}");
    }
    let _protocol_version = read_u8(r)?;
    let result = read_u8(r)?;
    if result != 0 {
        return Err(AuthReject { code: result }.into());
    }
    let server_public_key = read_array::<32>(r)?;
    let generator_len = read_u8(r)?;
    let mut generator = vec![0u8; generator_len as usize];
    r.read_exact(&mut generator)?;
    let generator = match generator.first() {
        Some(&g) => g,
        None => bail!("server sent an empty generator"),
    };
    let prime_len = read_u8(r)? as usize;
    let mut prime = vec![0u8; prime_len];
    r.read_exact(&mut prime)?;
    let large_safe_prime: [u8; 32] = prime
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("large safe prime was {prime_len} bytes, expected 32"))?;
    let salt = read_array::<32>(r)?;
    // Consume every security extension so the next packet remains aligned. Build 8606 normally
    // uses only AUTHENTICATOR (0x04); PIN belongs to Classic and matrix input is not implemented.
    let crc_salt = read_array::<16>(r)?;
    let security_flag = read_u8(r)?;
    if security_flag & 0x01 != 0 {
        let _pin_grid_seed = read_u32_le(r)?;
        let _pin_salt = read_array::<16>(r)?;
    }
    if security_flag & 0x02 != 0 {
        let _matrix_width = read_u8(r)?;
        let _matrix_height = read_u8(r)?;
        let _matrix_digit_count = read_u8(r)?;
        let _matrix_challenge_count = read_u8(r)?;
        let _matrix_seed = read_array::<8>(r)?;
    }
    let authenticator_required = security_flag & 0x04 != 0;
    if authenticator_required {
        let _authenticator_mode = read_u8(r)?;
    }
    if security_flag & !0x07 != 0 {
        bail!("unsupported auth security flags {security_flag:#04x}");
    }
    if security_flag & 0x03 != 0 {
        bail!("2.4.3 client does not support legacy PIN/matrix security mode {security_flag:#04x}");
    }
    Ok(ChallengeReply {
        server_public_key,
        generator,
        large_safe_prime,
        salt,
        crc_salt,
        authenticator_required,
    })
}

// --- the version (client-integrity) proof --------------------------------------------------------
//
// The proof's `crc_hash` answers the challenge's `crc_salt`: the 2.4.3 client hashes its own
// binaries under that salt into a 20-byte digest `H` and sends `SHA1(A ‖ H)`. realmd recomputes it
// from a stored `H` (`AuthSocket::VerifyVersion`) and, under `StrictVersionCheck = 1` (the shipped
// `realmd.conf.example` value), refuses a mismatch with `WOW_FAIL_VERSION_INVALID` (0x09).

/// The `crc_salt` every mangos-family realmd sends: a constant, not a nonce (vmangos
/// `AuthSocket.cpp:65`, cmangos `AuthSocket.cpp:187`), so the answer is a per-build constant.
const MANGOS_VERSION_CHALLENGE: [u8; 16] = [
    0xba, 0xa3, 0x1e, 0x99, 0xa0, 0x0b, 0x21, 0x57, 0xfc, 0x37, 0x3f, 0xb3, 0x69, 0xcd, 0xd2, 0xf1,
];

/// `H` for the stock build-8606 Windows client under [`MANGOS_VERSION_CHALLENGE`].
const INTEGRITY_HASH_8606_WINDOWS: [u8; 20] = [
    0x31, 0x9A, 0xFA, 0xA3, 0xF2, 0x55, 0x96, 0x82, 0xF9, 0xFF, 0x65, 0x8B, 0xE0, 0x14, 0x56, 0x25,
    0x5F, 0x45, 0x6F, 0xB1,
];

/// The stock build-8606 Mac integrity hash.
const INTEGRITY_HASH_8606_MACOS: [u8; 20] = [
    0xD8, 0xB0, 0xEC, 0xFE, 0x53, 0x4B, 0xC1, 0x13, 0x1E, 0x19, 0xBA, 0xD1, 0xD4, 0xC0, 0xE8, 0x13,
    0xEE, 0xE4, 0x99, 0x4F,
];

/// The integrity digest `H` for `crc_salt`, known only for [`MANGOS_VERSION_CHALLENGE`].
/// Deviation: a stored per-OS constant, not the reference's HMAC over its own executables
/// (`0x5b1170`), because every mangos-family realmd issues this one salt.
fn integrity_hash(crc_salt: &[u8; 16]) -> Option<[u8; 20]> {
    if *crc_salt != MANGOS_VERSION_CHALLENGE {
        return None;
    }
    Some(if client_os() == OS_MACOS {
        INTEGRITY_HASH_8606_MACOS
    } else {
        INTEGRITY_HASH_8606_WINDOWS
    })
}

/// The proof's `crc_hash`: `SHA1(A ‖ H)` over `A`'s wire bytes (realmd hashes `lp->A` as
/// received), or twenty zeros for an unknown salt, which only a strict server refuses.
pub fn version_proof(crc_salt: &[u8; 16], client_public_key: &[u8; 32]) -> [u8; 20] {
    match integrity_hash(crc_salt) {
        Some(h) => {
            let mut sha = Sha1::new();
            sha.update(client_public_key);
            sha.update(h);
            sha.finalize().into()
        }
        None => [0u8; 20],
    }
}

/// Send `CMD_AUTH_LOGON_PROOF_Client`, computing `crc_hash` from the challenge's `crc_salt`.
pub fn write_logon_proof(
    w: &mut impl Write,
    client_public_key: &[u8; 32],
    client_proof: &[u8; 20],
    crc_salt: &[u8; 16],
    authenticator: Option<&str>,
) -> std::io::Result<()> {
    if let Some(token) = authenticator {
        if token.is_empty() || token.len() > 16 || !token.bytes().all(|b| b.is_ascii_digit()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "authenticator token must be 1..=16 ASCII digits",
            ));
        }
    }
    let mut packet = Vec::with_capacity(1 + 32 + 20 + 20 + 1 + 1 + 1 + 16);
    packet.push(CMD_AUTH_LOGON_PROOF);
    packet.extend_from_slice(client_public_key);
    packet.extend_from_slice(client_proof);
    packet.extend_from_slice(&version_proof(crc_salt, client_public_key));
    packet.push(0); // number_of_telemetry_keys
    packet.push(if authenticator.is_some() { 0x04 } else { 0x00 });
    if let Some(token) = authenticator {
        packet.push(token.len() as u8);
        packet.extend_from_slice(token.as_bytes());
    }
    w.write_all(&packet)
}

/// Read the proof reply: the server's `M2`, or an [`AuthReject`].
pub fn read_proof_reply(r: &mut impl Read) -> Result<[u8; 20]> {
    let opcode = read_u8(r)?;
    if opcode != CMD_AUTH_LOGON_PROOF {
        bail!("expected CMD_AUTH_LOGON_PROOF (0x01), got {opcode:#x}");
    }
    let result = read_u8(r)?;
    if result != 0 {
        // A wrong password fails here, where `M1` does not verify: vmangos answers 0x04.
        return Err(AuthReject { code: result }.into());
    }
    let server_proof = read_array::<20>(r)?;
    let _account_flags = read_u32_le(r)?;
    let _survey_id = read_u32_le(r)?;
    let _unknown_flags = read_u16_le(r)?;
    Ok(server_proof)
}

/// Send `CMD_REALM_LIST_Client` (opcode + a `u32` padding of 0).
pub fn write_realm_list_request(w: &mut impl Write) -> std::io::Result<()> {
    let mut packet = [0u8; 5];
    packet[0] = CMD_REALM_LIST;
    w.write_all(&packet)
}

/// Read a Burning Crusade 2.4.3 `CMD_REALM_LIST_Server`.
pub fn read_realm_list(r: &mut impl Read) -> Result<Vec<RealmInfo>> {
    let opcode = read_u8(r)?;
    if opcode != CMD_REALM_LIST {
        bail!("expected CMD_REALM_LIST (0x10), got {opcode:#x}");
    }
    let _size = read_u16_le(r)?;
    let _header_padding = read_u32_le(r)?;
    let number_of_realms = read_u16_le(r)?;
    let mut realms = Vec::with_capacity(number_of_realms as usize);
    for _ in 0..number_of_realms {
        let realm_type = u32::from(read_u8(r)?);
        let locked = read_u8(r)? != 0;
        let flags = read_u8(r)?;
        let name = read_cstring(r)?;
        let address = read_cstring(r)?;
        let population = read_f32_le(r)?;
        let characters = read_u8(r)?;
        let category = read_u8(r)?;
        let id = read_u8(r)?;
        let build = if flags & 0x04 != 0 {
            Some(RealmBuild {
                major: read_u8(r)?,
                minor: read_u8(r)?,
                patch: read_u8(r)?,
                build: read_u16_le(r)?,
            })
        } else {
            None
        };
        realms.push(RealmInfo {
            name,
            address,
            population,
            characters,
            realm_type,
            locked,
            flags,
            category,
            id,
            build,
        });
    }
    let _footer_padding = read_u16_le(r)?; // cMaNGOS emits 0x0010; consume for stream alignment
    Ok(realms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_tags_reverse_to_the_names_servers_match_on() {
        let spelled = |tag: u32| {
            let mut b = tag.to_le_bytes();
            b.reverse();
            String::from_utf8(b.iter().copied().filter(|&c| c != 0).collect()).unwrap()
        };
        assert_eq!(spelled(OS_WINDOWS), "Win");
        assert_eq!(spelled(OS_MACOS), "OSX");
        assert_eq!(spelled(PLATFORM_X86), "x86");
    }

    #[test]
    fn client_os_follows_the_host() {
        if cfg!(target_os = "macos") {
            assert_eq!(client_os(), OS_MACOS);
        } else {
            assert_eq!(client_os(), OS_WINDOWS);
        }
    }

    /// An arbitrary but fixed `A` for the version-proof vectors below.
    fn test_public_key() -> [u8; 32] {
        std::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(9))
    }

    /// Expected digests computed outside this crate (Python `hashlib`) as `SHA1(A ‖ H)`.
    #[test]
    fn version_proof_matches_the_realmd_expression() {
        let expected = if cfg!(target_os = "macos") {
            "11684dfad3d75e8946902fa67cdb5eff3daeef8a"
        } else {
            "21a0059db441e3e5fca60461fa642b2574ada8f9"
        };
        let got = version_proof(&MANGOS_VERSION_CHALLENGE, &test_public_key());
        assert_eq!(
            got.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            expected
        );
    }

    #[test]
    fn an_unknown_version_challenge_is_answered_with_zeros() {
        let mut salt = MANGOS_VERSION_CHALLENGE;
        salt[0] ^= 0xff;
        assert_eq!(version_proof(&salt, &test_public_key()), [0u8; 20]);
    }

    /// `opcode · A[32] · M1[20] · crc_hash[20] · num_keys · security_flag`, 75 bytes.
    #[test]
    fn the_proof_packet_carries_the_version_proof() {
        let a = test_public_key();
        let m1: [u8; 20] = std::array::from_fn(|i| (i as u8).wrapping_mul(13).wrapping_add(4));
        let mut packet = Vec::new();
        write_logon_proof(&mut packet, &a, &m1, &MANGOS_VERSION_CHALLENGE, None).unwrap();

        assert_eq!(packet.len(), 1 + 32 + 20 + 20 + 1 + 1);
        assert_eq!(packet[0], CMD_AUTH_LOGON_PROOF);
        assert_eq!(&packet[1..33], &a);
        assert_eq!(&packet[33..53], &m1);
        assert_eq!(
            &packet[53..73],
            &version_proof(&MANGOS_VERSION_CHALLENGE, &a)
        );
        assert_eq!(&packet[73..], &[0, 0]);
    }

    #[test]
    fn the_realm_list_keeps_every_field_the_wire_carries() {
        let mut body = Vec::new();
        body.push(8); // realm_type/icon: RPPVP
        body.push(1); // locked
        body.push(0x44); // offline | specify-build | recommended
        body.extend_from_slice(b"Onyxia\0");
        body.extend_from_slice(b"127.0.0.1:8085\0");
        body.extend_from_slice(&1.75f32.to_le_bytes());
        body.push(3); // characters
        body.push(2); // category
        body.push(0x2c);
        body.extend_from_slice(&[2, 4, 3]);
        body.extend_from_slice(&8606u16.to_le_bytes());

        let mut packet = vec![CMD_REALM_LIST];
        packet.extend_from_slice(&(body.len() as u16 + 8).to_le_bytes());
        packet.extend_from_slice(&0u32.to_le_bytes());
        packet.extend_from_slice(&1u16.to_le_bytes());
        packet.extend_from_slice(&body);
        packet.extend_from_slice(&0x0010u16.to_le_bytes());

        let realms = read_realm_list(&mut packet.as_slice()).unwrap();
        assert_eq!(realms.len(), 1);
        let r = &realms[0];
        assert_eq!(r.name, "Onyxia");
        assert_eq!(r.address, "127.0.0.1:8085");
        assert_eq!(r.realm_type, 8);
        assert!(r.locked);
        assert_eq!(r.flags, 0x44);
        assert_eq!(r.population, 1.75);
        assert_eq!(r.characters, 3);
        assert_eq!(r.category, 2);
        assert_eq!(r.id, 0x2c);
        assert_eq!(
            r.build,
            Some(RealmBuild {
                major: 2,
                minor: 4,
                patch: 3,
                build: 8606,
            })
        );
    }

}
