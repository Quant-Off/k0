# Signing oracle (key vault)

<div align="center">

English | [한국어](../kr/vault_KR.md)

</div>

This note describes the first service on top of K0's isolation core. A **vault** task holds one Ed25519 key. Client tasks get signatures and the public key over IPC, and they never get the key. The code is in `userspace/k0-vault` (the crate), `userspace/root-task/src/oracle.rs` (wiring and self-test) and `crates/k0-boot` (the boot seed).

> [!NOTE]
> The vault is optional and off by default. The default image has no vault and no crypto code.

## Purpose and scope

The vault shows that the isolation core can keep a secret away from the tasks that use it. It offers a small, fixed service: report status, return the public key, and sign a message of up to 1024 bytes.

The algorithm is Ed25519 (PureEdDSA with SHA-512, as in FIPS 186-5 and RFC 8032). The implementation is the vendored crate `ed25519-dalek` 2.2.0 and has no validation of any kind. The key does not come from an approved random bit generator. 14 crates were added to `vendor/` for it, and half of them are build-time or other-target code that is not compiled into a K0 image.

## Objects and who can map what

![The root task builds the vault, the client, one endpoint and two frames. The client holds a send-only badged endpoint capability. The request frame is read-write for the client and read-only for the vault. The response frame is read-write for the vault and read-only for the client.](../../assets/vault-flow.svg)

The root task creates the following objects:

- **one endpoint**: the vault receives on it. A client's CNode holds only a send-only, badged copy of it (and a report endpoint in the self-test).
- **the vault task**: it has its own address space and its own stack. The key lives there.
- **per caller, a request frame and a response frame**: the request frame is read-write for the caller and read-only for the vault. The response frame is read-write for the vault and read-only for the caller. This is enforced by reduced-rights copies of the frame capabilities.

> [!WARNING]
> The root task builds all of this, so it is outside the isolation and could read the vault. The statement that holds is "no task other than the root task can map the vault's memory".

## The protocol

A client uses `CALL` (see [ipc.md](ipc.md)). The request registers are the following:

| Register | Content |
| --- | --- |
| MR0 | the operation and the algorithm (`proto::word`) |
| MR1 | the length of the body in the request frame |
| MR2 | a nonce chosen by the caller |

The reply sets all four registers:

| Register | Content |
| --- | --- |
| MR0 | the status |
| MR1 | the length (for `STATUS`, the vault state) |
| MR2 | the indicator bits |
| MR3 | the nonce |

The operations are `STATUS`, `PUBLIC_KEY`, `SIGN` (a message of up to 1024 bytes in the request frame) and, for the admin badge only, `INIT` (the seed and the seed info, 40 bytes, once) and `ZEROIZE`.

**The single fetch**: `server::fetch` fixes the length first, then reads the body once into a private buffer with volatile reads. Only the copy is used, so a caller that changes the frame during the call gains nothing. The response frame starts with the nonce and the length, and `server::publish` writes the nonce last. A client must trust the frame only after an `OK` reply that carries its own nonce.

The state machine is `Vault::handle` in `userspace/k0-vault/src/server.rs`.

## The fixed policy and badges

The policy is fixed at build time and keyed by the endpoint badge. The kernel delivers the badge, and a client cannot forge it (see [ipc.md](ipc.md)). The roles are the following:

| Role | Operations |
| --- | --- |
| admin (the root task) | `STATUS`, `INIT`, `ZEROIZE`, not `SIGN` |
| signer | `STATUS`, `PUBLIC_KEY`, `SIGN` |
| reader | `STATUS`, `PUBLIC_KEY` |

A call without a badge or with an unknown badge is refused. No operation changes the policy. All signers share the one key, so a signature carries no client identity.

## The provider boundary

`userspace/k0-vault/src/provider.rs` is the only place that touches the crypto crate.

- **Self-test**: at start the vault runs a known-answer test with two RFC 8032 vectors (public key, signature, verification, and rejection of a changed message and of a changed signature).
- **Key generation and the entropy rule**: the vault refuses to make a key (status `NO_ENTROPY`) unless the boot seed contains at least 32 bytes of `rng-seed` or 256 bits from RNDR. QEMU virt provides a 32-byte `rng-seed`.
- **Pairwise test**: after key generation the vault checks that its own signature verifies. A failed self-test or a failed pairwise test puts the vault into a failed state that refuses everything but `STATUS`.
- **Strict verification**: verification uses `verify_strict`.
- **No key export**: no operation returns the key.
- **Zeroization**: `ZEROIZE` drops the key (the crate wipes it). The vault then refuses `SIGN` and `PUBLIC_KEY`.
- **Indicator bits**: every reply carries four bits: the algorithm is in FIPS 186-5, the self-tests passed, at least 32 `rng-seed` bytes went into the key seed, and RNDR went into the key seed.

## The boot seed path

The kernel derives 32 bytes for the root task with SHA-256 under its own domain string from the DTB entropy, the counter and, when the CPU has RNDR, four fresh RNDR words (`k0_boot::derive_root_seed`). It is independent of the PAC keys by domain separation. The seed is as good as its inputs and no better.

The seed is delivered in the root task's entry registers `x0` to `x3`. `x4` holds how many `rng-seed` bytes were used (bits 15 to 0), and bit 16 says that RNDR contributed. It is not in the bootinfo page, and `bootinfo::VERSION` is 3. The root task prints only `root: boot seed from rng-seed bytes 0x... and rndr` or `... without rndr`, never the seed. It then gives the seed to the vault with `INIT`.

**Seed wipe**: the FDT parser records where the `rng-seed` and `kaslr-seed` values are, and a second `/chosen` node is refused (`DuplicateChosen`, also in the UEFI loader's pre-check, which shares the parser). Right before the kernel enters the root task, it zeroes those bytes. `k0_mm::wipe_ro_alias` switches the read-only alias entry of the page to read-write in place (break-before-make), writes zeros, cleans and invalidates the cache lines, and switches it back. The kernel then reads the bytes back, wipes its own copy in `BootInfo`, prints `k0: dtb seeds wiped <n>B` (40B on QEMU virt) and zeroes the boot stack below the live frames. This is the only write the kernel makes through the DTB mapping, and it happens after the DTB was parsed and validated.

> [!NOTE]
> Copies that the firmware or the UEFI loader keep are not wiped, and neither is anything in the live frames of the boot stack. The scrubs of stack copies (the kernel boot stack, the vault stack after signing) are best effort.

## Features and the fixed image

The `k0-kernel` crate has two features, and features only add:

- `vault`: the service, its protocol, policy and self-test, with no signing algorithm. Every signing request answers `UNSUPPORTED`.
- `vault-ed25519`: adds Ed25519.

You can build it as follows:

```sh
$ cargo build -p k0-kernel --features console,vault-ed25519
```

`crates/k0-boot/build.rs` forwards the feature to the nested build of the root task. The vault's code is part of the root task image, which is checked by SHA-256 at boot like before. Nothing is loaded after boot. With Ed25519 the image has 24 frames of 4 KiB (the limit is 78).

## How it is tested

**Host tests** (`tools/host-test.sh` runs `k0-vault` with and without the algorithm feature). The tests are in `userspace/k0-vault/src/tests.rs`: 13 with Ed25519 and 7 without. The test names are the following:

- always: `badges_map_to_fixed_roles`, `policy_is_enforced_before_anything_else`, `malformed_requests_are_refused_whole`, `a_seed_without_real_entropy_makes_no_key`, `init_happens_once_and_zeroize_is_final`, `fetch_clamps_the_length_and_publish_writes_header_and_body`, and `without_an_algorithm_every_signing_request_is_unsupported` (the last one without the algorithm only).
- with Ed25519: `the_self_test_passes_and_is_reported`, `signatures_verify_and_the_key_follows_the_seed`, `a_public_key_request_carries_no_body`, `unknown_algorithms_are_unsupported`, `nothing_is_served_after_zeroize`, `a_small_order_public_key_is_refused_even_with_a_matching_signature`, `a_failed_vault_refuses_everything_but_status`.

With the other crates, the host suite has 276 tests on an ARM host and 190 on other hosts (without `k0-mm`).

**Root task self-test** (`test_oracle` in `userspace/root-task/src/oracle.rs`, only in images built with `vault`). It checks the following:

- the self-test is reported, and an unbadged call is refused,
- a key is made from the boot seed, and a second `INIT` is refused,
- the admin cannot sign,
- a signer client gets a signature that verifies, and that does not verify for a changed message or a changed signature,
- a reader is refused `SIGN`,
- a client that writes its read-only response frame and a client that reads where the vault sees another caller's request are killed by a fault (two more `k0: task fault` lines, so 8 instead of 6),
- after `ZEROIZE` signing is refused,
- a call to a deleted vault ends with `TIMEOUT`, and
- memory taken back from the vault reads as zero after retype.

The marker is `root: oracle tests pass`. The client tasks run one after another in one client address space, because root CNode slots are scarce. The test shows isolation between the vault and a client, not between clients.

**Boot matrix rows**: `vault`, `vault-max` (RNDR), `vault-el2`, `vault-16k`, `vault-no-alg`, `hvf-vault`, and the static row `vault-symbols` (the default image has no vault or crypto code, and the vault image has no writable static from them). The matrix has 61 rows on a Mac with HVF and 51 in CI. Every image now also prints `k0: dtb seeds wiped <n>B` and `root: boot seed from rng-seed bytes ...`.

## Known gaps

> [!NOTE]
> The implementation is unvalidated, there is no side-channel analysis of the crate on this kernel, and the oracle was only run in QEMU (TCG and HVF).

- **One key**: there is no persistence, no key import or export, no DRBG, no operator input and no post-quantum algorithm yet.
- **Availability is not protected**: clients share one endpoint and must use timeouts on every call (see [ipc.md](ipc.md)).
- **The root task is trusted**: it builds the vault and could read it.
- **No client identity**: all signers share the one key.
- **Entropy**: the seed is as good as its inputs. The key does not come from an approved random bit generator.
- **Best-effort wipes**: see the note in the boot seed section.
