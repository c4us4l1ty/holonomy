"""Independent XChaCha20-Poly1305 (RFC 8439 + HChaCha20). Shares no code with the Rust impl."""
M32 = 0xffffffff
P1305 = (1 << 130) - 5

def rotl(v, n): return ((v << n) & M32) | (v >> (32 - n))

def qr(x, a, b, c, d):
    x[a] = (x[a] + x[b]) & M32; x[d] = rotl(x[d] ^ x[a], 16)
    x[c] = (x[c] + x[d]) & M32; x[b] = rotl(x[b] ^ x[c], 12)
    x[a] = (x[a] + x[b]) & M32; x[d] = rotl(x[d] ^ x[a], 8)
    x[c] = (x[c] + x[d]) & M32; x[b] = rotl(x[b] ^ x[c], 7)

CONST = [0x61707865, 0x3320646e, 0x79622d32, 0x6b206574]

def chacha_state(key, counter, nonce):
    st = CONST + [int.from_bytes(key[i*4:i*4+4], 'little') for i in range(8)]
    st += [counter]
    st += [int.from_bytes(nonce[i*4:i*4+4], 'little') for i in range(3)]
    return st

def rounds(st, n):
    x = list(st)
    for _ in range(n // 2):
        qr(x,0,4,8,12); qr(x,1,5,9,13); qr(x,2,6,10,14); qr(x,3,7,11,15)
        qr(x,0,5,10,15); qr(x,1,6,11,12); qr(x,2,7,8,13); qr(x,3,4,9,14)
    return [(x[i] + st[i]) & M32 for i in range(16)]

def block(key, counter, nonce):
    return b''.join(w.to_bytes(4,'little') for w in rounds(chacha_state(key, counter, nonce), 20))

def hchacha20(key, n16):
    """HChaCha20: 20 rounds with NO feed-forward addition.

    This is the one thing that distinguishes it from ChaCha20's core, and getting it
    wrong produces a plausible-looking subkey that matches nothing. Verified against
    draft-irtf-cfrg-xchacha sec 2.2.1 by `self_test` below.
    """
    st = CONST + [int.from_bytes(key[i*4:i*4+4],'little') for i in range(8)]
    st += [int.from_bytes(n16[i*4:i*4+4],'little') for i in range(4)]
    x = list(st)
    for _ in range(10):
        qr(x,0,4,8,12); qr(x,1,5,9,13); qr(x,2,6,10,14); qr(x,3,7,11,15)
        qr(x,0,5,10,15); qr(x,1,6,11,12); qr(x,2,7,8,13); qr(x,3,4,9,14)
    return b''.join(x[i].to_bytes(4,'little') for i in (0,1,2,3,12,13,14,15))

def chacha20_xor(key, counter, nonce, data):
    out = bytearray()
    for i in range(0, len(data), 64):
        ks = block(key, counter + i // 64, nonce)
        chunk = data[i:i+64]
        out += bytes(a ^ b for a, b in zip(chunk, ks))
    return bytes(out)

def le_num(b): return int.from_bytes(b, 'little')
def poly1305(key, msg):
    r = le_num(key[:16]) & 0x0ffffffc0ffffffc0ffffffc0fffffff
    s = le_num(key[16:32])
    a = 0
    for i in range(0, len(msg), 16):
        n = le_num(msg[i:i+16] + b'\x01')
        a = (a + n) % P1305
        a = (a * r) % P1305
    return ((a + s) & ((1 << 128) - 1)).to_bytes(16, 'little')

def pad16(b): return b'\x00' * ((16 - len(b) % 16) % 16)

def xchacha20poly1305_encrypt(key, nonce24, plaintext, aad):
    subkey = hchacha20(key, nonce24[:16])
    n = b'\x00' * 4 + nonce24[16:24]
    polykey = block(subkey, 0, n)[:32]
    ct = chacha20_xor(subkey, 1, n, plaintext)
    mac_data = aad + pad16(aad) + ct + pad16(ct) + len(aad).to_bytes(8,'little') + len(ct).to_bytes(8,'little')
    return ct, poly1305(polykey, mac_data)

def chaff_at(key, offset, length):
    """The container chaff keystream: ChaCha20, fixed zero nonce, counter = block index.

    Mirrors crates/holonomy-container/src/chaff.rs. `offset` must be a multiple of 64.
    """
    assert offset % 64 == 0
    n = b'\x00' * 12
    out = b''
    counter = offset // 64
    while len(out) < length:
        out += block(key, counter, n)
        counter += 1
    return out[:length]


def self_test():
    """Check against published vectors before trusting anything derived from this file."""
    ok = True

    # RFC 8439 sec 2.8.2: the full AEAD. Passes, which validates the ChaCha20 keystream,
    # the counter start at 1, the Poly1305 core and the mac_data field ordering together.
    key = bytes(range(0x80, 0xa0))
    nonce = bytes.fromhex('070000004041424344454647')
    aad = bytes.fromhex('50515253c0c1c2c3c4c5c6c7')
    pt = (b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip "
          b"for the future, sunscreen would be it.")
    polykey = block(key, 0, nonce)[:32]
    ct = chacha20_xor(key, 1, nonce, pt)
    mac_data = (aad + pad16(aad) + ct + pad16(ct)
                + len(aad).to_bytes(8,'little') + len(ct).to_bytes(8,'little'))
    tag = poly1305(polykey, mac_data)
    ok &= ct.hex().startswith('d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6')
    ok &= tag.hex() == '1ae10b594f09e26a7e902ecbd0600691'
    print(f"  RFC 8439 sec 2.8.2 AEAD      {'PASS' if ok else 'FAIL'}")

    # draft-irtf-cfrg-xchacha sec 2.2.1: HChaCha20 key derivation.
    sub = hchacha20(bytes(range(32)), bytes.fromhex('000000090000004a0000000031415927'))
    h_ok = sub.hex() == '82413b4227b27bfed30e42508a877d73a0f9e4d58a74a853c12ec41326d3ecdc'
    ok &= h_ok
    print(f"  XChaCha draft sec 2.2.1 HChaCha20 {'PASS' if h_ok else 'FAIL'}")

    # draft-irtf-cfrg-xchacha sec 2.4.2: the full AEAD.
    k2 = bytes.fromhex('808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f')
    n2 = bytes.fromhex('404142434445464748494a4b4c4d4e4f5051525354555657')
    a2 = bytes.fromhex('50515253c0c1c2c3c4c5c6c7')
    p2 = (b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip "
          b"for the future, sunscreen would be it.")
    c2, t2 = xchacha20poly1305_encrypt(k2, n2, p2, a2)
    e_ok = (c2.hex().startswith('bd6d179d3e83d43b9576579493c0e939572a1700252bfaccbed2902c21396cbb')
            and t2.hex() == 'c0875924c1c7987947deafd8780acf49')
    ok &= e_ok
    print(f"  XChaCha draft sec 2.4.2 AEAD  {'PASS' if e_ok else 'FAIL'}")

    # The chaff keystream at a non-zero counter. `block` is already validated above, so this
    # checks the rule the Rust side relies on: the container's chaff advances with the
    # absolute block index. A Rust round-trip test cannot check that rule, because both
    # sides of a round trip would use it.
    ck = bytes([0x21]) * 32
    c_ok = chaff_at(ck, 0, 64) != chaff_at(ck, 1 << 20, 64)
    ok &= c_ok
    print(f"  chaff: block 0 != block 16384      {'PASS' if c_ok else 'FAIL'}")

    return ok


if __name__ == '__main__':
    print("self-test:")
    if not self_test():
        raise SystemExit("reference implementation is wrong; do not trust its output")
    print()

    print()
    KEY = bytes([0x2B])*32
    NONCE = bytes([0x7C])*24
    SLOT, TAGOFF, CPLEN = 65536, 65520, 65520

    # Vector A: plaintext fills the slot exactly (no random gap), so ct and tag are both
    # deterministic. Mirrors what the Rust test does.
    PT = bytes((i % 251) for i in range(CPLEN))
    AAD = (0).to_bytes(8,'big')
    ct, tag = xchacha20poly1305_encrypt(KEY, NONCE, PT, AAD)
    # Chaff vectors at three offsets, including a non-zero one.
    ck = bytes([0x21]) * 32
    for off in (0, 4096, 1 << 20):
        print(f"CHAFF off={off} first32 = {chaff_at(ck, off, 32).hex()}")

    print()
    print("A ct[:32] =", ct[:32].hex())
    print("A tag     =", tag.hex())

    # Vector B: index 5, different N_root, short plaintext. Proves the nonce XOR.
    KEY2 = bytes([0x2B])*32
    NONCE2 = bytes([0x7C])*24
    # "N_root XOR i" means XOR with i encoded as an 8-byte big-endian integer into the
    # low 8 bytes -- not with the integer value repeated per byte. Getting that wrong gives
    # a7..a0 instead of a0..a0 and a nonce that matches nothing.
    n2 = bytearray(NONCE2); n2[16:24] = bytes([0xA5] * 8)
    idx = (5).to_bytes(8, 'big')
    for j in range(8): n2[16+j] ^= idx[j]
    PT2 = bytes((i % 253) for i in range(CPLEN))
    AAD2 = (5).to_bytes(8,'big')
    ct2, tag2 = xchacha20poly1305_encrypt(KEY2, bytes(n2), PT2, AAD2)
    print("B ct[:32] =", ct2[:32].hex())
    print("B tag     =", tag2.hex())
    print("B nonce   =", bytes(n2).hex())
