# TLS 1.3 PSK auth — v0.2

> One paragraph summary: the pairing code becomes a PSK via HKDF-SHA256;
> the PSK becomes an Ed25519 keypair (also via HKDF); that keypair becomes a
> self-signed cert; both sides present the same cert and pin the peer's
> cert byte-for-byte. Wrong code → handshake fails before any application
> data flows.

## Why not external TLS-PSK?

The task brief asked for TLS 1.3 with external PSK. We shipped a different
shape, and it matters that you know why:

- **rustls 0.23** (the only mainline pure-Rust TLS stack) does not expose
  external PSK identities for TLS 1.3. The internal types exist, but the
  public `ConfigBuilder` chain has no way to install one. Patches exist in
  the wild but none are merged.
- Pulling in `openssl-sys` or `boring` just for PSK would balloon the
  receiver build, and BoringSSL doesn't ship in standard Linux distros.
- The brief explicitly permitted the fallback: "fall back to mutual cert
  auth with self-signed certs pinned via the pairing code — be explicit
  about the choice".

So this is that fallback. The pairing UX is identical; the wire shape
differs.

## Derivation

```
psk     = HKDF-SHA256(salt="zerowire/v1/psk", ikm=pairing_code, info="tls-psk", L=32)
cert_seed = HKDF-SHA256(salt="zerowire/v1/psk", ikm=pairing_code, info="zerowire/v1/tls-cert-key", L=32)
ed25519_key = SigningKey::from_bytes(cert_seed)
cert       = self_signed(ed25519_key,
                         serial = SHA256(cert_seed)[..16],
                         not_before = 2020-01-01,
                         not_after  = 2099-12-31,
                         SAN        = "zerowire-psk.local")
```

Both sides compute these identically. `protocol::psk::derive_psk` /
`derive_tls_cert_seed` (Rust) and `Psk.derivePsk` / `Psk.deriveTlsCertSeed`
(Kotlin) are pinned to the same vectors.

## Handshake

1. Receiver dials the sender, plain TCP.
2. `rustls::ClientConnection` is configured with:
   - Client cert + key = the deterministic identity.
   - Custom `ServerCertVerifier` that pins the peer cert to the same
     deterministic identity (byte-equal check, ED25519 sig scheme).
3. Sender's `rustls::ServerConnection` has the mirror config: identical
   server cert + key + a custom `ClientCertVerifier` doing the same pin.
4. TLS 1.3 handshake runs. If the codes match: both sides present the
   same cert, both pins succeed, application data starts flowing. If they
   don't match: the verifier returns `ApplicationVerificationFailure` and
   the handshake aborts.

## Threat model

| Threat                                                | Outcome |
| ----------------------------------------------------- | ------- |
| Passive WiFi eavesdropper, no PSK                     | TLS 1.3 confidentiality + integrity blocks it. |
| Active MITM between sender and receiver, no PSK       | Can't produce a matching cert → handshake fails. |
| Attacker who somehow obtains the PSK                  | Can MITM. Same threat as v0.1 — the PSK is the secret. Rotate the code by re-pairing. |
| Attacker who watches a successful handshake on-wire   | Doesn't learn the PSK (Ed25519 sig + ECDH key exchange). |

The "both sides hold the same private key" property looks weird but is
fine for a 1:1 pairing: anyone with the pairing code is by definition
authorized to be either side. There is no per-side asymmetric identity by
design.

## Loopback test

`tests/tls_psk_loopback.sh` runs the mock sender + receiver twice:

* Phase 1 — wrong PSK on the receiver. Expects: receiver exits non-zero
  AND no HID reports recorded in the simulate-source log.
* Phase 2 — matching PSK. Expects: bind_ack + N HID reports recorded.

Both phases must pass.
