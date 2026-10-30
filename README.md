# Solana Raw Shred Decoder

[简体中文](README.zh-CN.md) · [SolBrisk](https://solbrisk.com/) · [Raw Shred connection guide](https://solbrisk.com/docs)

Receive native Solana UDP Merkle shreds, reassemble complete entry batches,
recover missing data using Reed–Solomon coding shreds, and emit ledger entries
and transactions as JSONL. Supports legacy, v0 and v1 transaction decoding,
reordered packets, duplicates, late joins and length-prefixed capture/replay.

Built by [SolBrisk](https://solbrisk.com/), a Frankfurt and Amsterdam Raw Shred
and Yellowstone gRPC provider. Get a UDP subscription at
[solbrisk.com](https://solbrisk.com/) and configure your destination using the
[official connection guide](https://solbrisk.com/docs). Native compatible feeds
from other providers also work. [Network status](https://solbrisk.com/status).

## Build

Requires Rust 1.89+ and a C toolchain. No validator, RocksDB, RPC endpoint or
protobuf compiler is required.

```bash
cargo build --release --locked
./target/release/solana-shred-decoder --help
```

## Receive live shreds

```bash
./target/release/solana-shred-decoder \
  --bind 0.0.0.0:21001 --duration 60 \
  --raw-transactions --output entries.jsonl --capture shreds.dat
```

Set your subscription destination to **your server's public IP:21001**, not the
literal `0.0.0.0` bind address. Allow this UDP port through your own firewall.
This command exclusively binds a free local port; it does not share, redirect
or modify an existing validator/trading socket. You do not need root for these
example ports. `--duration 0` keeps running until Ctrl+C.

The listener recognizes SolBrisk's endpoint-verification packets and prints the
48-character code to stderr. Paste that code into the website verification form
while keeping this receiver running. Control packets are not decoded as shreds.
`--source-ip` optionally restricts packets to an expected sender IP; use values
confirmed by your provider, not a guessed gateway address.

## Replay without network access

```bash
./target/release/solana-shred-decoder \
  --input shreds.dat --raw-transactions --output replay.jsonl
```

Capture format: repeated **u32 little-endian datagram length + exact datagram
bytes**. There is no PCAP header or timestamp. `--input` opens no UDP socket;
the tool does not read PCAP directly. Truncated or oversized capture records are
reported as errors. Files use exclusive creation and are never overwritten.

To generate a small synthetic sample containing a transaction and a tick:

```bash
cargo run --locked --example make_capture -- demo.dat
./target/release/solana-shred-decoder --input demo.dat --raw-transactions
```

The synthetic sample has placeholder signatures and is for offline decoding,
not a signed mainnet fixture or a transaction to submit.

## Output

One JSON object per entry; progress and diagnostics go to stderr. Fields include:

- `slot`, `shred_start`, `shred_end`, `entry_index_in_batch`, PoH hash, hash count
  and `is_tick`.
- Transaction signatures, version, static account keys, instructions and v0
  address-table lookup descriptors. Binary instruction data is base64.
- `--raw-transactions` adds `transaction_base64` using the transaction's wire
  encoding. No transactions are sent anywhere.
- `execution_status: "unknown"` and `leader_signature_verified: false` are
  explicit. Raw shreds precede execution; decoding does not prove that a
  transaction succeeded, finalized or even remained on the canonical fork.

Address lookup table addresses are **not resolved**: descriptors are included,
but historical ALT state requires separate authenticated data/state. A program
ID is null when its instruction index refers to unresolved loaded addresses.

## Recovery, compatibility and resource use

See [FORMAT.md](FORMAT.md) for offsets, recovery rules and trust boundaries.
The decoder handles chained Merkle variants `0x90`, `0xb0`, `0x60`, `0x70`
(high nibbles), depth at most eight and optional four-byte repair nonces.
Legacy non-Merkle shreds and provider-specific wrappers are rejected.

Reconstruction requires compatible coding geometry and enough distinct shards.
Recovered data must pass full-FEC Merkle-root recomputation and header checks.
If relay tail representations disagree, that FEC set's recovery is disabled;
consistent directly received data may still decode. Conflicting application
payloads or leader signatures quarantine the slot. The tool does not download
leader schedules or authenticate leader Ed25519 signatures. Use trusted feeds
and separate verification before relying on decoded data for trading or consensus.

Zero padding after a complete serialized entry batch is supported; nonzero
trailing bytes are rejected.

Statistics include invalid packets, duplicates, payload conflicts,
`fec_identity_conflicts`, recovered data, failed recovery, decoded batches,
entries, transactions and evicted slots. A mid-stream join can skip the initial
incomplete batch and resume after an observed data-complete boundary. Missing
data beyond available erasure redundancy cannot be reconstructed.

Defaults: 16 retained slots, 15-second slot TTL and 64 MiB of accounted payload
buffers. Configure `--max-slots`, `--slot-ttl` and `--max-buffer-mib` as needed.
These are bounded payload caches, not a precise process-RSS limit. Map/JSON
overhead adds memory; deduplication lasts only while the slot is retained; eviction counters make insufficient retention visible.
The requested `--receive-buffer-mib 32` may be clamped by OS settings; the
effective SO_RCVBUF is printed. Output/capture storage can grow without bound
unless you set a duration or rotate files externally. This project does not
alter sysctls, firewall rules or running services.

No complete decoded batch produces a nonzero exit code. Once some data is
decoded, packet errors are reported in counters; exit success does not mean
perfect coverage. Live output/disk stalls can overflow UDP receive buffers.

## Development

```bash
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
```

Tests cover malformed lengths, erasure recovery (including coding-only and
re-signed layouts), duplicate/conflicting data, late joins, and legacy/v0/v1
transactions. Pinned transaction/wincode dependencies track the documented wire
format. MIT license; [THIRD-PARTY.md](THIRD-PARTY.md).

Support: [SolBrisk docs](https://solbrisk.com/docs),
[Discord tickets](https://discord.gg/P9jep5pvky), support@solbrisk.com.

Validation details: [VALIDATION.md](VALIDATION.md). Contribution guide: [CONTRIBUTING.md](CONTRIBUTING.md).
