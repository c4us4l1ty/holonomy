# THIRD-PARTY CODE IN THIS TREE

Two kinds of entry: **dependencies**, whose record is `Cargo.lock`, and **adapted source** —
permissively licensed code that was read, changed and committed here. This file is the ledger for
the second kind, because the licences require attribution and a comment in one module is not an
attribution trail.

## Adapted source

| origin | licence | version / commit | what was taken | files |
| --- | --- | --- | --- | --- |
| [s1dashu/quiet-md](https://github.com/s1dashu/quiet-md) | MIT, © 2026 s1dashu | v0.1.13, `main` read on 2026-11-17 | **Nothing yet.** Read as prior art for the Block 6 shell and the Block 7 ribbon: the quiet page, the paper feel, the ribbon menus. **OQ-13 in `AUDIT.md` records why it is not a dependency, and what that costs us.** | — |

### The rules for an entry appearing above

1. **The file says so.** Adapted code carries a provenance line in its module docs — origin,
   licence, upstream commit, and whether it is *copied* (byte-identical apart from formatting) or
   *adapted* (and what changed). A reader of the file must not have to find this ledger to know.
2. **The licence text stays.** Anything taken brings `LICENSES/quiet-md-MIT.txt` with it in the same
   pull request.
3. **Same pull request, never afterwards.** Code and ledger entry land together; a follow-up PR is a
   PR in which the tree is unlicensed for a while.
4. **Nothing that needs a `[patch.crates-io]` section.** Block 1 forbids it in the core workspace by
   name. This is not decoration: a dependency's own patch section does not travel, which is precisely
   why quiet-md cannot be depended on (OQ-13).

## Dependencies

`Cargo.lock` is the record of versions; `docs/THREAT-MODEL.md` is the record of what may hold
plaintext. One dependency claim is enforced in CI rather than written here: the `zeroize` feature
graph across `argon2`, `chacha20`, `curve25519-dalek`, `digest`, `ml-kem`, `poly1305`, `sha3` and
`sponge-cursor` is checked in the `test (stable)` job, so a dependency that quietly stops zeroizing
fails the build rather than the audit.
