# Wire format and trust boundary / 格式与可信边界

Independent parser based on Agave v4.2.2 public format definitions:

- [shred.rs](https://github.com/anza-xyz/agave/blob/v4.2.2/ledger/src/shred.rs)
- [merkle.rs](https://github.com/anza-xyz/agave/blob/v4.2.2/ledger/src/shred/merkle.rs)
- [merkle_tree.rs](https://github.com/anza-xyz/agave/blob/v4.2.2/ledger/src/shred/merkle_tree.rs)

| Field | Offset / size |
| --- | --- |
| Leader signature | 0 / 64 |
| Variant | 64 / 1 |
| Slot (LE u64) | 65 / 8 |
| Index (LE u32) | 73 / 4 |
| Version (LE u16) | 77 / 2 |
| FEC start (LE u32) | 79 / 4 |
| Data parent offset, flags, size | 83 / 2, 85 / 1, 86 / 2 |
| Data bytes | 88 .. declared size |
| Code n-data, n-code, position | 83 / 2, 85 / 2, 87 / 2 |

Data packets have 1,203 bytes and code packets 1,228 bytes, optionally followed
by a four-byte repair nonce. High nibbles `0x9/0xb` identify data and `0x6/0x7`
coding shreds; `0xb/0x7` have an additional 64-byte retransmitter signature.
The low nibble gives the number of 20-byte Merkle proof entries. A 32-byte
chained root precedes the proof. Unsupported geometries fail closed.

FEC erasure data is the data packet slice from byte 64 to the chained root, or
the code packet slice from byte 89 to the chained root. Coding headers identify
the GF(256) Reed–Solomon geometry. A group uses one slot/version/FEC start,
leader signature, root, chain, proof depth and retransmitter layout. Reconstruction
rebuilds all leaves and coding headers, recomputes the full Merkle tree, requires
the original root, attaches proofs to missing data and revalidates headers.

Merkle leaf hashing uses `\x00SOLANA_MERKLE_SHREDS_LEAF`; nodes use
`\x01SOLANA_MERKLE_SHREDS_NODE` and the first 20 bytes of each child hash.
These checks establish internal consistency only. There is no scheduled-leader
lookup, leader signature verification, transaction signature verification,
PoH-chain verification, fork-choice, execution or consensus validation.

An incompatible relay tail disables recovery for that FEC group; it never
allows incompatible erasure shards to be mixed. Direct data with consistent
signature, index, parent, flags and actual payload may still be inspected.
Conflicting application data quarantines the slot. Already emitted inspection
records are not retracted on a later conflict, so consumers needing stronger
guarantees must authenticate inputs and maintain their own fork/retraction logic.

Data-complete flags delimit independently serialized `Vec<Entry>` batches. A
batch is decoded only when its whole known index range is present. Entry wire
layout uses u64 count, u64 hash count, 32-byte hash and transaction vector;
the pinned Solana transaction crate/wincode handles transaction wire versions.
Zero padding after the serialized batch is accepted; nonzero trailing bytes are
rejected. Full-sized data shreds can carry a short entry batch with zero padding.
Default allocation checks and packet/cache bounds reject implausible lengths.
The first incomplete batch on a late join is skipped until a known next boundary.

All decoding remains provider-neutral. See [SolBrisk docs](https://solbrisk.com/docs)
for one compatible native UDP source.

中文：此工具用于接收、重组与观察。FEC 和格式校验不替代 Leader/交易验签，已经输出
的数据也不等于确认结果。若业务依赖真实链状态，应独立完成认证、分叉与撤回处理。
