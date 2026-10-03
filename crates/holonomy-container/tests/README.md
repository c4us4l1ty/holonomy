# Reference implementations

`xchacha20poly1305_ref.py` is a from-scratch XChaCha20-Poly1305 (HChaCha20 key derivation
plus RFC 8439 ChaCha20-Poly1305 AEAD) written in plain Python. It shares no code with the
Rust implementation and exists so `src/aead.rs`'s `matches_an_independent_vector` test is a
real cross-check rather than a round trip.

Run it to regenerate the expected values:

```sh
python3 tests/xchacha20poly1305_ref.py
```

The outputs quoted in `src/aead.rs` must match this exactly. Python's `cryptography`
package would have been the obvious choice, but it is not installed on the build host and
adding a Python dependency to generate a constant is worse than writing 90 lines of
reference arithmetic.

It is a test fixture. Nothing in the shipped binary depends on it, and it is not compiled or
executed by `cargo test` -- the values are pinned in the Rust test.

## Reference tables in `src/sp800_22.rs`

`src/sp800_22.rs` asserts `erfc` and `log_gamma` against reference values generated from
CPython's stdlib:

```python
import math
for x in [0.0, 0.1, 0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0]:
    print(x, math.erfc(x))
for x in [0.5, 1.0, 2.0, 5.0, 9.5, 100.0, 4194304.0]:
    print(x, math.lgamma(x))
```

`math.erfc` and `math.lgamma` are independent implementations in a different language, which
is what makes the comparison a test rather than a snapshot of this crate's own output. The
`erfc` points straddle the branch inside `gammq` at `x = sqrt(1.5)`, where the implementation
switches from the cancelling series to the continued fraction.

The sample-entropy spread quoted in `the_entropy_floor_is_valid_at_the_container_size` was
measured the same way -- 60 draws at 2^20 and 2^22 bytes, 12 draws at 2^24 and 2^26 -- and is
recorded in the test's doc comment because it contradicts the textbook asymptotic result
(`sd ∝ 1/sqrt(m)`; measured `sd ∝ 1/m`, because the first-order multinomial fluctuation
cancels exactly for a uniform distribution).

Neither file is compiled or executed by `cargo test`; the values are pinned in the Rust tests.
