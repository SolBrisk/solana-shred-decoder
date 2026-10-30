use crate::wire::{self, Shred, CODE_BYTES, CODE_HEADER, DATA_BYTES};
use anyhow::{bail, ensure, Context, Result};
use reed_solomon_erasure::galois_8::ReedSolomon;
use serde_json::{json, Value};
use solana_transaction::versioned::VersionedTransaction;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    time::{Duration, Instant},
};

#[derive(Debug, wincode::SchemaRead, wincode::SchemaWrite)]
pub struct Entry {
    pub num_hashes: u64,
    pub hash: [u8; 32],
    pub transactions: Vec<VersionedTransaction>,
}

pub fn entries(bytes: &[u8]) -> Result<Vec<Entry>> {
    ensure!(bytes.len() >= 8, "Entry batch is truncated");
    let count = u64::from_le_bytes(bytes[..8].try_into().unwrap());
    ensure!(count <= bytes.len() as u64 / 48, "Implausible entry count");
    let mut remaining = bytes;
    let result: Vec<Entry> =
        wincode::deserialize_from(&mut remaining).context("Entry batch decoding failed")?;
    // Full-sized data shreds can pad a short serialized batch with zeroes.
    ensure!(
        remaining.iter().all(|byte| *byte == 0),
        "Nonzero trailing entry bytes"
    );
    for entry in &result {
        for transaction in &entry.transactions {
            transaction
                .sanitize()
                .context("Invalid transaction structure")?;
        }
    }
    Ok(result)
}

#[derive(Default, serde::Serialize)]
pub struct Stats {
    pub packets: u64,
    pub invalid: u64,
    pub duplicates: u64,
    pub conflicts: u64,
    pub recovered_data_shreds: u64,
    pub fec_failures: u64,
    pub fec_identity_conflicts: u64,
    pub batches: u64,
    pub entries: u64,
    pub transactions: u64,
    pub decode_failures: u64,
    pub evicted_slots: u64,
    pub buffered_bytes: usize,
}
struct Data {
    bytes: Vec<u8>,
    flags: u8,
    fec: u32,
    signature: [u8; 64],
    parent: u16,
}
struct Fec {
    signature: [u8; 64],
    root: [u8; 32],
    chain: [u8; 32],
    proof: usize,
    resigned: bool,
    data: HashMap<usize, Vec<u8>>,
    code: HashMap<usize, Vec<u8>>,
    geometry: Option<(usize, usize, u32)>,
    done: bool,
    disabled: bool,
}
struct Slot {
    created: Instant,
    version: u16,
    data: BTreeMap<u32, Data>,
    ends: BTreeSet<u32>,
    emitted: BTreeSet<u32>,
    failed: BTreeMap<u32, u32>,
    fec: HashMap<u32, Fec>,
    bytes: usize,
    poisoned: bool,
}
impl Slot {
    fn new(version: u16) -> Self {
        Self {
            created: Instant::now(),
            version,
            data: BTreeMap::new(),
            ends: BTreeSet::new(),
            emitted: BTreeSet::new(),
            failed: BTreeMap::new(),
            fec: HashMap::new(),
            bytes: 0,
            poisoned: false,
        }
    }
}
pub struct Decoder {
    slots: BTreeMap<u64, Slot>,
    max_slots: usize,
    max_bytes: usize,
    ttl: Duration,
    pub stats: Stats,
    pub include_raw: bool,
}
impl Decoder {
    pub fn new(max_slots: usize, max_bytes: usize, ttl: Duration, include_raw: bool) -> Self {
        Self {
            slots: BTreeMap::new(),
            max_slots,
            max_bytes,
            ttl,
            stats: Stats::default(),
            include_raw,
        }
    }
    fn trim(&mut self) {
        let expired = self
            .slots
            .iter()
            .filter(|(_, s)| s.created.elapsed() > self.ttl)
            .map(|(k, _)| *k)
            .collect::<Vec<_>>();
        for key in expired {
            self.slots.remove(&key);
            self.stats.evicted_slots += 1;
        }
        while self.slots.len() > self.max_slots
            || self.slots.values().map(|s| s.bytes).sum::<usize>() > self.max_bytes
        {
            if let Some(key) = self
                .slots
                .iter()
                .min_by_key(|(_, s)| s.created)
                .map(|(k, _)| *k)
            {
                self.slots.remove(&key);
                self.stats.evicted_slots += 1;
            } else {
                break;
            }
        }
        self.stats.buffered_bytes = self.slots.values().map(|s| s.bytes).sum();
    }
    pub fn push(&mut self, packet: &[u8]) -> Result<Vec<Value>> {
        self.stats.packets += 1;
        self.trim();
        let shred = match Shred::parse(packet) {
            Ok(s) => s,
            Err(e) => {
                self.stats.invalid += 1;
                return Err(e);
            }
        };
        let slot = self
            .slots
            .entry(shred.slot)
            .or_insert_with(|| Slot::new(shred.version));
        if slot.poisoned {
            return Ok(Vec::new());
        }
        if slot.version != shred.version {
            slot.poisoned = true;
            self.stats.conflicts += 1;
            bail!("Conflicting versions in one slot");
        }
        let fec = slot.fec.entry(shred.fec_index).or_insert_with(|| Fec {
            signature: shred.signature,
            root: shred.root,
            chain: shred.chain,
            proof: shred.proof_size,
            resigned: shred.resigned,
            data: HashMap::new(),
            code: HashMap::new(),
            geometry: None,
            done: false,
            disabled: false,
        });
        if fec.signature != shred.signature {
            slot.poisoned = true;
            self.stats.conflicts += 1;
            bail!("Conflicting leader signatures; slot quarantined");
        }
        if !fec.disabled
            && (fec.root != shred.root
                || fec.chain != shred.chain
                || fec.proof != shred.proof_size
                || fec.resigned != shred.resigned)
        {
            // Incompatible relay representations cannot be mixed for FEC.
            // Direct non-conflicting data remains usable for inspection.
            fec.disabled = true;
            slot.bytes -= fec.data.values().map(Vec::len).sum::<usize>()
                + fec.code.values().map(Vec::len).sum::<usize>();
            fec.data.clear();
            fec.code.clear();
            self.stats.fec_identity_conflicts += 1;
        }
        if shred.code {
            let geometry = (
                shred.data_count,
                shred.code_count,
                shred.index - shred.position as u32,
            );
            if fec.geometry.is_some_and(|g| g != geometry) {
                slot.poisoned = true;
                self.stats.conflicts += 1;
                bail!("Conflicting FEC geometry");
            }
            fec.geometry = Some(geometry);
        }
        let existing = if shred.code {
            &mut fec.code
        } else {
            &mut fec.data
        };
        if !fec.disabled {
            if let Some(old) = existing.get(&shred.position) {
                if old.as_slice() != shred.erasure() {
                    slot.poisoned = true;
                    self.stats.conflicts += 1;
                    bail!("Conflicting shred payload");
                }
                self.stats.duplicates += 1;
                return Ok(Vec::new());
            }
        }
        if !fec.done && !fec.disabled {
            existing.insert(shred.position, shred.erasure().to_vec());
            slot.bytes += shred.erasure().len();
        }
        let duplicate_data = !shred.code && slot.data.contains_key(&shred.index);
        let no_recovery_work = shred.code && (fec.done || fec.disabled);
        if !shred.code {
            if let Err(error) = insert_data(slot, &shred) {
                slot.poisoned = true;
                self.stats.conflicts += 1;
                return Err(error);
            }
        }
        if duplicate_data {
            self.stats.duplicates += 1;
            return Ok(Vec::new());
        }
        if no_recovery_work {
            return Ok(Vec::new());
        }
        let recovered = match recover(slot, shred.slot, shred.fec_index) {
            Ok(data) => data,
            Err(_) => {
                self.stats.fec_failures += 1;
                Vec::new()
            }
        };
        for packet in recovered {
            let s = Shred::parse(&packet)?;
            if !slot.data.contains_key(&s.index) {
                self.stats.recovered_data_shreds += 1;
                insert_data(slot, &s)?;
            }
        }
        let mut output = Vec::new();
        let mut previous_end = None;
        for end in slot.ends.iter().copied() {
            let start = previous_end.map_or(0, |index: u32| index + 1);
            previous_end = Some(end);
            if slot.emitted.contains(&end) || slot.failed.get(&end) == Some(&start) {
                continue;
            }
            if !slot.data.contains_key(&start) || !slot.data.contains_key(&end) {
                continue;
            }
            let range = slot.data.range(start..=end).collect::<Vec<_>>();
            if range.len() != (end - start + 1) as usize {
                continue;
            }
            let bytes = range
                .iter()
                .flat_map(|(_, d)| d.bytes.iter().copied())
                .collect::<Vec<_>>();
            match entries(&bytes) {
                Ok(batch) => {
                    slot.emitted.insert(end);
                    self.stats.batches += 1;
                    for (entry_index, entry) in batch.into_iter().enumerate() {
                        self.stats.entries += 1;
                        self.stats.transactions += entry.transactions.len() as u64;
                        let transactions = entry
                            .transactions
                            .iter()
                            .map(|t| transaction_json(t, self.include_raw))
                            .collect::<Vec<_>>();
                        output.push(json!({"kind":"entry","slot":shred.slot,"shred_start":start,"shred_end":end,"entry_index_in_batch":entry_index,"num_hashes":entry.num_hashes,"hash":bs58::encode(entry.hash).into_string(),"is_tick":entry.transactions.is_empty(),"transactions":transactions,"execution_status":"unknown","leader_signature_verified":false}));
                    }
                }
                Err(_) => {
                    slot.failed.insert(end, start);
                    self.stats.decode_failures += 1;
                }
            }
        }
        self.trim();
        Ok(output)
    }
}

fn insert_data(slot: &mut Slot, s: &Shred<'_>) -> Result<()> {
    if let Some(old) = slot.data.get(&s.index) {
        ensure!(
            old.bytes == s.data()
                && old.fec == s.fec_index
                && old.signature == s.signature
                && old.parent == s.parent_offset
                && old.flags == s.flags,
            "Conflicting reconstructed data"
        );
        return Ok(());
    }
    slot.bytes += s.data().len();
    slot.data.insert(
        s.index,
        Data {
            bytes: s.data().to_vec(),
            flags: s.flags,
            fec: s.fec_index,
            signature: s.signature,
            parent: s.parent_offset,
        },
    );
    if s.flags & 0x40 != 0 {
        slot.ends.insert(s.index);
    }
    Ok(())
}

fn recover(slot: &mut Slot, slot_number: u64, fec_index: u32) -> Result<Vec<Vec<u8>>> {
    let fec = slot.fec.get_mut(&fec_index).unwrap();
    if fec.done || fec.disabled {
        return Ok(Vec::new());
    }
    let Some((n, k, code_base)) = fec.geometry else {
        return Ok(Vec::new());
    };
    ensure!(
        fec.data.keys().all(|p| *p < n),
        "Data exceeds coding geometry"
    );
    if fec.data.len() == n {
        fec.done = true;
        return Ok(Vec::new());
    }
    if fec.data.len() + fec.code.len() < n {
        return Ok(Vec::new());
    }
    let mut shards = vec![None; n + k];
    for (p, data) in &fec.data {
        shards[*p] = Some(data.clone());
    }
    for (p, code) in &fec.code {
        shards[n + *p] = Some(code.clone());
    }
    ReedSolomon::new(n, k)?.reconstruct(&mut shards)?;
    let mut packets = Vec::with_capacity(n + k);
    let mut leaves = Vec::with_capacity(n + k);
    for (position, shard) in shards.iter().enumerate() {
        let code = position >= n;
        let size = if code { CODE_BYTES } else { DATA_BYTES };
        let mut p = vec![0; size];
        p[..64].copy_from_slice(&fec.signature);
        let proof_offset = size - fec.proof * 20 - usize::from(fec.resigned) * 64;
        let chain_offset = proof_offset - 32;
        if code {
            p[64] = if fec.resigned { 0x70 } else { 0x60 } | fec.proof as u8;
            p[65..73].copy_from_slice(&slot_number.to_le_bytes());
            p[73..77].copy_from_slice(&(code_base + (position - n) as u32).to_le_bytes());
            p[77..79].copy_from_slice(&slot.version.to_le_bytes());
            p[79..83].copy_from_slice(&fec_index.to_le_bytes());
            p[83..85].copy_from_slice(&(n as u16).to_le_bytes());
            p[85..87].copy_from_slice(&(k as u16).to_le_bytes());
            p[87..89].copy_from_slice(&((position - n) as u16).to_le_bytes());
        }
        let offset = if code { CODE_HEADER } else { 64 };
        let bytes = shard.as_ref().unwrap();
        ensure!(
            bytes.len() == chain_offset - offset,
            "FEC shard size mismatch"
        );
        p[offset..chain_offset].copy_from_slice(bytes);
        p[chain_offset..proof_offset].copy_from_slice(&fec.chain);
        leaves.push(wire::leaf(&p[64..proof_offset]));
        packets.push(p);
    }
    let mut levels = vec![leaves];
    while levels.last().unwrap().len() > 1 {
        let current = levels.last().unwrap();
        let next = current
            .chunks(2)
            .map(|pair| wire::join(&pair[0], pair.get(1).unwrap_or(&pair[0])))
            .collect::<Vec<_>>();
        levels.push(next);
    }
    ensure!(
        levels.last().unwrap()[0] == fec.root,
        "Recovered FEC Merkle root mismatch"
    );
    let mut result = Vec::new();
    for (position, mut p) in packets.into_iter().take(n).enumerate() {
        if fec.data.contains_key(&position) {
            continue;
        }
        let mut index = position;
        let proof_offset = DATA_BYTES - fec.proof * 20 - usize::from(fec.resigned) * 64;
        for (depth, level) in levels.iter().take(fec.proof).enumerate() {
            let sibling = &level[(index ^ 1).min(level.len() - 1)];
            p[proof_offset + depth * 20..proof_offset + (depth + 1) * 20]
                .copy_from_slice(&sibling[..20]);
            index >>= 1;
        }
        let s = Shred::parse(&p)?;
        ensure!(
            !s.code
                && s.slot == slot_number
                && s.fec_index == fec_index
                && s.index == fec_index + position as u32
                && s.version == slot.version
                && s.root == fec.root,
            "Recovered shred header mismatch"
        );
        result.push(p);
    }
    fec.done = true;
    Ok(result)
}

fn transaction_json(transaction: &VersionedTransaction, raw: bool) -> Value {
    use base64::Engine;
    let message = &transaction.message;
    let keys = message.static_account_keys();
    let instructions=message.instructions().iter().map(|ix|json!({"program_id_index":ix.program_id_index,"program_id":keys.get(ix.program_id_index as usize).map(ToString::to_string),"account_indices":ix.accounts,"data_base64":base64::engine::general_purpose::STANDARD.encode(&ix.data)})).collect::<Vec<_>>();
    let mut out = json!({"signature":transaction.signatures.first().map(ToString::to_string),"signatures":transaction.signatures.iter().map(ToString::to_string).collect::<Vec<_>>(),"version":format!("{:?}",transaction.version()),"static_account_keys":keys.iter().map(ToString::to_string).collect::<Vec<_>>(),"recent_blockhash":message.recent_blockhash().to_string(),"instructions":instructions,"address_table_lookups":message.address_table_lookups().unwrap_or_default().iter().map(|l|json!({"account_key":l.account_key.to_string(),"writable_indices":l.writable_indexes,"readonly_indices":l.readonly_indexes})).collect::<Vec<_>>(),"lookup_addresses_resolved":false});
    if raw {
        if let Ok(bytes) = wincode::serialize(transaction) {
            out["transaction_base64"] =
                json!(base64::engine::general_purpose::STANDARD.encode(bytes));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tick_batch_round_trip_and_hostile_lengths() {
        let batch = vec![Entry {
            num_hashes: 5,
            hash: [7; 32],
            transactions: vec![],
        }];
        let bytes = wincode::serialize(&batch).unwrap();
        assert_eq!(entries(&bytes).unwrap().len(), 1);
        assert!(entries(&u64::MAX.to_le_bytes()).is_err());
        let mut extra = bytes.clone();
        extra.push(0);
        assert_eq!(entries(&extra).unwrap().len(), 1);
        extra.push(1);
        assert!(entries(&extra).is_err());
    }
}
