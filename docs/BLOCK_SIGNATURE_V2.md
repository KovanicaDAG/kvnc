# KUNA Block Signature (NACRT v2, `KUNA/block/v1`)

Status: **DRAFT (rev. 2, review KVNC-Execs)**, čeka zajedničko zamrzavanje Foundation/Consensus/Network u KVNC-Execs (SOP). Ništa se ne implementira prije zamrzavanja. Baza: `main` `95e99cc`. Vlasnik: Exec-Foundation (`kvnc-types`/`kvnc-crypto`). Zamrzavanje: Foundation + Consensus + Network (SOP). Konvencije (LE, `DomainTag16`, Ed25519 `ed25519-dalek` 2.x, BLAKE3 `=1.5.1` keyed) su iste kao u `docs/SIGNATURE_FORMAT.md`.

## Ciljevi
1. Domain separation: potpis bloka nikad nije valjan kao potpis glasa/tx-a (i obrnuto).
2. Zaštita od replaya među mrežama (`chain_id`) i epohama (`epoch`).
3. **Identitet bloka (`digest` / `BlockReference`) se NE mijenja.**

## Danas (8170a82)
`StatementBlock::compute_digest` (`crates/kvnc-types/src/block.rs:43-65`):
```
data = author u16 LE ‖ round u64 LE
     ‖ za svaki parent: parent.author u16 LE ‖ parent.round u64 LE ‖ parent.digest 32 B
     ‖ merkle_root 32 B            (compute_merkle_root(txs); prazno = 32×0x00)
     ‖ za svaki tx: tx.hash() 32 B (polje Transaction.hash)
digest = BLAKE3_keyed(key = "KVNC-BLOCK-v1" ‖ 0x00×19, data)       // Hash::DOMAIN_BLOCK
```
Nema length prefiksa za `parents`/`txs`: **preimage digesta nije samorazgraničen** (granica parents/merkle/tx-ovi nije kodirana). To je **prihvaćeno** i ne mijenja se u v2: verifier digest uvijek računa sam iz strukturiranog bloka (nikad ne parsira preimage), a promjena bi promijenila identitet bloka. `statements` i `signature` NISU u digestu.
Potpis danas: `Ed25519.sign(sk, digest)` (32 B) – bez chain_id/epohe/taga (`kvnc-dag/src/block_manager.rs:277-278`).

## v2: signing preimage (88 B, potpisuje se izravno, bez prehasha)

| Offset | Size | Polje      | Kodiranje |
|-------:|-----:|------------|-----------|
| 0  | 16 | domain tag | `DomainTag16("KUNA/block/v1")` (13 B + 3×0x00) |
| 16 | 8  | chain_id   | u64 LE (registar: 1/2/3/1337) |
| 24 | 8  | epoch      | u64 LE (epoha committeeja; `0` dok epohe nisu žive) |
| 32 | 32 | digest     | `compute_digest(author, round, parents, txs)`, **nepromijenjen** (gore) |

`signature = Ed25519.sign(sk_author, preimage)`; verifikacija **isključivo `verify_strict`** (§Strogo svugdje).
Verifier gradi preimage iz `signing_ctx_for_round(schedule, chain_id, block.round)` (§Epoha po roundu), nikad iz poruke, i najprije sam ponovno računa `digest` (i merkle) iz sadržaja bloka.

API (prijedlog, `kvnc_types::signing` / `kvnc-crypto`):
```rust
pub const BLOCK_DOMAIN_TAG: [u8;16] = domain_tag16(b"KUNA/block/v1");
impl StatementBlock { pub fn signature_data(&self, ctx: &SigningContext) -> [u8; 88]; }
pub fn sign_block(sk: &SigningKey, ctx: &SigningContext, digest: &Hash) -> Signature;
pub fn verify_block_signature(ctx: &SigningContext, block: &StatementBlock, pk: &PublicKey) -> Result<(), SigError>; // zamjenjuje (pk, digest, sig)
pub fn verify_batch(ctx: &SigningContext, blocks: &[StatementBlock]) -> Result<bool, CryptoError>;
```

### Odluka: potpis nad zasebnim preimageom, digest ostaje isti (preporuka)
Alternative: (a) staviti tag/chain_id/epoch u `compute_digest` (mijenja `digest`); (b) potpisati tag‖chain‖epoch‖puna polja (varijabilna duljina).
Preporuka je **88-bajtni preimage nad nepromijenjenim digestom**:
- `digest` je identitet bloka: ključ u `DagStore`/`block_store`, `BlockReference.digest` u parentima, `leader_hash` u glasovima (već vezan na chain/epohu kroz `KUNA/vote/v1`), sync `ByHash`, mergeset, kanonski genesis digest (`compute_digest(0,0,[],[])`). Promjena (a) bi dirala sve to i ~70 test fixturea, a ne dodaje sigurnost: replay štiti potpis, ne identitet.
- Digest već kriptografski veže sva polja, pa je (b) jednako siguran kao 88 B, ali skuplji, varijabilan i lošiji za batch verify.
- Fiksnih 88 B = isti obrazac kao `KUNA/vote/v1`; batch verify ostaje jednostavan.
- Posljedica koju prihvaćamo: isti sadržaj bloka na dvije mreže ima isti digest; to je bezopasno jer blok bez valjanog potpisa za lokalni chain_id/epohu nikad ne ulazi u DAG.
- `KVNC-BLOCK-v1` (ključ digesta) ostaje; preimenovanje u `KUNA` bi promijenilo identitet → izvan opsega (može uz rename `kuna-*`, ako vlasnik želi, ali kao zasebna odluka).


## Strogo svugdje (NORMATIVNO, blocker)

Pojedinačna i batch provjera **MORAJU** dati identičan odgovor za svaki blok. Mjerenjem (vektori N1–N3 niže, `ed25519-dalek` 2.1) `VerifyingKey::verify` (ne-strogi) i `ed25519_dalek::verify_batch` (kofaktorska jednadžba) **nisu** ekvivalentni `verify_strict`:
- N1: small-order pubkey prihvaćaju `verify` i `verify_batch`, `verify_strict` odbija;
- N3: `verify_batch` prihvaća potpis koji odbijaju i `verify` i `verify_strict`.
Strogi pre-checkovi (slab ključ, nekanonski R/A, `S ≥ l`) ne zatvaraju razliku u potpunosti: batch koristi kofaktorsku, a `verify_strict` nekofaktorsku jednadžbu, pa točke s torzijskom komponentom (koje nisu small-order) i dalje mogu dati različit ishod.

**Pravilo (odabrano kao sigurno):**
1. Jedina odlučujuća provjera potpisa bloka je **`verify_strict` po stavci** nad 88-bajtnim preimageom. Vrijedi za pojedinačnu provjeru, `verify_batch` i svaki fallback.
2. `kvnc_crypto::verify_batch` u v2 **MORA** vratiti isto što i `all(verify_strict(item))`. Dopuštena implementacija: petlja `verify_strict`. `ed25519_dalek::verify_batch` se **NE** koristi za prihvaćanje; smije se koristiti samo kao akcelerator (npr. rani signal greške), a **svaki** ishod (prihvat ili odbijanje) ponovno odlučuje `verify_strict` po stavci.
3. Ključevi committeeja se pri učitavanju provjeravaju (`!is_weak()`, kanonska enkodacija); slab ključ u konfiguraciji/genesisu je greška pri pokretanju.
4. Optimizacija batcha (pre-check + provjera prime-order za A i R) može doći kasnije, samo uz dokaz ekvivalencije i ove vektore kao testove.

Trenutni kod je ne-strog i **mora se promijeniti**: `kvnc-crypto/src/lib.rs:151` (`dalek_verify_batch`) i `:161` (fallback `vk.verify`), kao i `:73` `verify_block_signature` (`verify` → `verify_strict`).

## Epoha po roundu (NORMATIVNO)

Tip i funkcija žive u **`kvnc-types` (Foundation)**; sadržaj puni i održava **Consensus** (model iz PR #39, `docs/design/epoch-rotation.md` §3.2):
```rust
pub struct EpochSchedule { /* epoch -> (start_round, committee keys) */ }
impl EpochSchedule {
    pub fn epoch_for_round(&self, round: Round) -> Option<u64>;
    pub fn author_key(&self, round: Round, author: AuthorityIndex) -> Option<PublicKey>;
}
/// Čista funkcija: bez I/O, bez globalnog stanja, deterministička.
pub fn signing_ctx_for_round(schedule: &EpochSchedule, chain_id: u64, round: Round) -> Option<SigningContext>;
```
- Koriste je **svi** potpisi vezani uz round: blokovi (`block.round`), glasovi (`vote.leader_round`), DoubleSignProof (round prekršaja), gossip rub, sync / `ByHash` / ponovna obrada orphana. Nitko ne koristi „trenutnu” epohu za tuđi potpis (danas `kvnc-consensus/src/engine.rs:557` uzima `self.committee.epoch()`; mijenja se).
- **Ključ autora** se uzima iz committeeja **istog rounda** (`author_key(block.round, block.author)`), nikad iz trenutnog committeeja.
- **Izvor schedulea ovisno o kontekstu:**
  - **Prijelaz stanja (execution, DoubleSignProof):** schedule **MORA** doći iz **commitanog stanja na visini sub-DAG-a** koji se izvršava. Consensus zapisuje promjene schedulea u **istoj redb transakciji** kao commit koji zatvara epohu (PR #39), pa je rezultat isti uživo, pri replayu i nakon restarta.
  - **Gossip rub (Network):** `Arc<RwLock<EpochSchedule>>` ubrizgan iz nodea, i to **samo** za gossip/sync rub. Nema statičkog ni globalnog ctx-a: `kvnc-network/src/service.rs:1035` (`self.mempool.signing_context()`) i globalni `get_validator_key` u `verify_batch` (`kvnc-crypto/src/lib.rs:147,159`) se uklanjaju.
- **Round koji nije u scheduleu (`None`), ishod ovisi o kontekstu:**
  - **gossip/mreža:** `Verdict::Ignore` (ne Reject, bez kažnjavanja peera); blok se smije ograničeno bufferirati i ponovno provjeriti kad stigne `EpochChanged`;
  - **execution:** **deterministički neuspjeh** transakcije (receipt `failed`, fee i nonce se naplaćuju), isto na svim čvorovima.

**Blok nakon granice epohe** (zrcali tablicu glasova iz PR #39 §3.3; epoha se izvodi iz `block.round`, ne iz poruke):

| `block.round` je u | Ishod | Ključ / ctx |
|---|---|---|
| trenutnoj epohi `e` | provjera | `C(e)`, epoha `e` |
| prethodnoj `e-1`, `round <= end_round(e-1)`, iznad lokalnog prune/decided ruba | provjera (kasni blokovi zatvaraju zadnji val) | `C(e-1)`, epoha `e-1` |
| `e-1`, ali ispod `last_decided_round` / prune ruba | Ignore (bez učinka, bez kazne) | — |
| `<= e-2` | Reject | — |
| budućoj epohi (start još nije lokalno poznat) | Ignore + ograničeni buffer, ponovna provjera nakon `EpochChanged` | — |

Do rotacije schedule ima samo epohu 0 od roundu 0, pa je ponašanje identično današnjem uz `epoch = 0`.

## Testni vektori
Generator: jednokratni program (izvan repozitorija) (stvarni `kvnc-types` @8170a82 za digest, `blake3 =1.5.1`, `ed25519-dalek` 2.1). Ključ: seed `07`×32 → pubkey `ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c` (isti kao v1). `chain_id = 2`, `epoch = 7`. Svaki vektor je provjeren `verify_strict`; isti potpis pada za chain 3 i za epohu 0.

**V1** author 0, round 1, bez parenata, bez tx-ova:
```
merkle_root = 0000000000000000000000000000000000000000000000000000000000000000
digest      = e46776df689e8fc8f7f1fda159fb9186c7a873aa5bf43b6f159dc7f5397d6b9e
preimage    = 4b554e412f626c6f636b2f763100000002000000000000000700000000000000e46776df689e8fc8f7f1fda159fb9186c7a873aa5bf43b6f159dc7f5397d6b9e
sig         = 0d47f09042364ef62c546d6b350da8807c919c48259ed2097ffafda931862c1c7c8574f7f5e3eebb2da0d3b665bb46bec93ab19b96ca5fc732500f447c5e290a
legacy sig nad samim digestom (MORA biti odbijen) = 50c72013faa2ad23a353810f5de864a747f1dc653d7368ea0d80fc87a59974925658617deee2a48c4724b33f184571c064426a563357b5d06d9cf493e156150a
```
**V2** author 3, round 5, parents `[(1,4,11×32),(2,4,22×32)]`, tx hashevi `[55×32, 66×32]`:
```
merkle_root = 72e3f4944c12d03b12e2ebb224197e4679a349d5c59eb964c4ee7ee8cea94813
digest      = 5b6bf646470ec423311839c84241b94a9fe28eb7df33d5b70b0b9095895050b0
preimage    = 4b554e412f626c6f636b2f7631000000020000000000000007000000000000005b6bf646470ec423311839c84241b94a9fe28eb7df33d5b70b0b9095895050b0
sig         = 1c090d015dd53c9508a6ec44abe5a1903095ac79c9a931d73bfac55a1cd90c802ada987ad8e03dacd2e3cbaf7dcd73f5294eb93d902f8bf459f10143056cdd06
legacy sig (MORA biti odbijen) = 47d1994bcdd5751d43e101a4e267b457cd962e231b8ecb1b379d85ad6a0e15143cfe1da97a1f26f0bfb003d7840bfd0a67a24027d84678c23976595aa27d8a03
```

**B – batch** (round 9, bez parenata i tx-ova, chain 2, epoha 7; autori 0/1/2 s ključevima seed `07`/`08`/`09`×32):
```
author 0  pubkey   = ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c
          preimage = 4b554e412f626c6f636b2f763100000002000000000000000700000000000000d01d83ca1071b3e3810833f449ad84cdd257f1c8b8743361e7761d398f31dd99
          sig      = 2efc0b0aa007dcc6c3e1f35b33b7fc34a6956a68a1b463f16d48ec16bcf30738f5491b670b0c7381934f40f607009c05a4f415ca7423570153d10e391193c003
author 1  pubkey   = 1398f62c6d1a457c51ba6a4b5f3dbd2f69fca93216218dc8997e416bd17d93ca
          preimage = 4b554e412f626c6f636b2f7631000000020000000000000007000000000000005ab99feeefc9ffdadb4d97bf913364773f975e7726daace56f0b6c88db45d53d
          sig      = 184ad84f684956b0d1c9aaa5b63fa3277db6c6dec7b285f82c8d1af15033c7fec115f8ab673bdf8367367101301c05c25fb2de5eb8dc52d17c14671a6f8c4305
author 2  pubkey   = fd1724385aa0c75b64fb78cd602fa1d991fdebf76b13c58ed702eac835e9f618
          preimage = 4b554e412f626c6f636b2f763100000002000000000000000700000000000000ef977ddce45b6fbff7b7a05475e917484c91569eb2cbe78e67fd32182d7073cd
          sig      = ee0aec0ba4e0718d3e93082937b1e659692a7f6ff1c2f73f1fb31765c5916b7e4cff50c373350b8d2541cc8d0a1ed62cf10068fbc86aa12586bb44cf7c733304
```
`ed25519_dalek::verify_batch` nad ova tri 88-bajtna preimagea prolazi; isti potpisi nad sirovim digestima padaju.

**Negativni vektori (strogo)**, preimage `KUNA/block/v1`, chain 2, epoha 7, digest `5b`×32 (generirano jednokratnim programom, `ed25519-dalek` 2.1):
```
message = 4b554e412f626c6f636b2f7631000000020000000000000007000000000000005b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b

N0 kontrola (seed 07)
 pubkey = ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c
 sig    = 04a7390ea076e0d0a8cfe92dc76fba5acd109c30f1e0036bdfa186d85151725ca0d1effde03e49939e8b26c2c3085565c968b9cda4bcc05472b222e544f5a408
 verify=ok  verify_strict=ok  verify_batch=ok

N1 small-order pubkey (identitet), R = identitet, S = 0
 pubkey = 0100000000000000000000000000000000000000000000000000000000000000
 sig    = 0100000000000000000000000000000000000000000000000000000000000000 0000000000000000000000000000000000000000000000000000000000000000
 verify=OK  verify_strict=REJECT  verify_batch=OK (sam i uz valjanu N0-stavku)   -> v2: REJECT

N2 nekanonski S (S+l potpisa N0)
 pubkey = ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c
 sig    = 04a7390ea076e0d0a8cfe92dc76fba5acd109c30f1e0036bdfa186d85151725c8da5e55afba15beb74281e65a202347ac968b9cda4bcc05472b222e544f5a418
 verify=REJECT  verify_strict=REJECT  verify_batch=REJECT   -> v2: REJECT

N3 small-order pubkey + nekanonski R (y = p+1), S = 0
 pubkey = 0100000000000000000000000000000000000000000000000000000000000000
 sig    = eeffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f 0000000000000000000000000000000000000000000000000000000000000000
 verify=REJECT  verify_strict=REJECT  verify_batch=OK (sam i uz N0)   -> v2: REJECT
```
(Potpisi su prikazani kao `R S`, bez razmaka u stvarnim bajtovima.) Svi N-vektori ulaze kao testovi za `verify_block_signature` **i** `verify_batch` (pojedinačno i pomiješano s N0); očekivano: N0 prihvaćen, N1–N3 odbijeni u oba puta.

**N4 DoubleSignProof / pogrešna epoha:** isti V1 blok (digest `e46776df…6b9e`, author 0, round 1), seed 07, potpisan s preimageom za **epohu 0**:
```
preimage(epoha 0) = 4b554e412f626c6f636b2f763100000002000000000000000000000000000000e46776df689e8fc8f7f1fda159fb9186c7a873aa5bf43b6f159dc7f5397d6b9e
sig               = e303f66f74e30265e0a6d18c59b348f7f0f704b205e2d2c3ce0c6b30faa0b85f9f5d1f84a78ef5e59770d568815ae585ad37eed8683fb2fd693deb63a1c89908
verify_strict uz ctx epohe 7 = REJECT;  uz ctx epohe 0 = ok
```
Ako `signing_ctx_for_round(schedule_na_visini, 2, 1)` daje epohu 7, DoubleSignProof koji sadrži ovaj potpis **MORA** biti odbijen (deterministički neuspjeh); V1 potpis za epohu 7 (gore) prolazi.

**Golden merkle (V2):** `compute_merkle_root([55×32, 66×32]) = 72e3f4944c12d03b12e2ebb224197e4679a349d5c59eb964c4ee7ee8cea94813` dodaje se kao golden test uz digest i potpis.

## Call-site checklist (`main` 95e99cc, putanje od `crates/`)
**Foundation – kvnc-types / kvnc-crypto**
- `kvnc-types/src/signing.rs:30` – `BLOCK_DOMAIN_TAG`; `EpochSchedule` + `signing_ctx_for_round` (novo); testovi.
- `kvnc-types/src/block.rs:43` `compute_digest` – bez promjene; `signature_data(ctx)` + golden testovi (V1, V2 uklj. merkle, B, N0–N3).
- `kvnc-types/src/hash.rs:30` `DOMAIN_BLOCK` – bez promjene.
- `kvnc-crypto/src/lib.rs:54` `sign` / `:61` `verify` – generički; doc ispraviti; blokovi više ne koriste ne-strogi `verify`.
- `kvnc-crypto/src/lib.rs:73` `verify_block_signature` – `(ctx, block, pk)`, `verify_strict`.
- `kvnc-crypto/src/lib.rs:127-165` `verify_batch` – uzima ctx/schedule i ključeve kao parametre (ukloniti globalni `get_validator_key` `:147,:159`); `:151` dalek batch i `:161` ne-strogi fallback zamijeniti pravilom iz §Strogo svugdje.
- `kvnc-crypto/src/lib.rs:190-191, :203` testovi; `kvnc-crypto/tests/audit_31_hot_path.rs:12,24,31`.

**Consensus / DAG – kvnc-dag, kvnc-consensus**
- `kvnc-dag/src/block_manager.rs:276-278` `sign_block` – potpisuje `signature_data(ctx za vlastiti round)`.
- `kvnc-dag/src/block_manager.rs:283` `validate_block` → `:303/:315` (digest ostaje) → `:343` provjera potpisa (`verify_strict`, ključ i ctx iz schedulea za `block.round`). **Jedino mjesto** kroz koje prolaze ingest (#42 `ingest_block`) i ponovna obrada orphana, preko `process_block` (`:487`).
- `kvnc-dag/src/block_manager.rs` testovi `:527, :583, :628, :669, :692, :710` i helper `signed()` `:1130-1143`; ručno potpisani blokovi `:760-930`.
- `kvnc-dag/tests/mergeset.rs:21`, `kvnc-dag/tests/pruning.rs:21` – potpisuju digest.
- `kvnc-consensus/src/engine.rs:557` – ctx iz trenutne epohe → `signing_ctx_for_round`.
- `kvnc-consensus/src/engine.rs:437` `propose_block` / `:565` `process_block` – schedule prema BlockManageru.
- `kvnc-consensus/src/engine.rs:1004-1011` (mock na `:1011` potpisuje sirovi digest), `:1024`, `:1132`, `:1141-1148`, `:1173-1180` – testovi.
- `kvnc-consensus/tests/engine.rs:263`, `kvnc-consensus/tests/common/mod.rs:452`.

**Network – kvnc-network**
- `kvnc-network/src/validation.rs:123` `verify_block` – **prvo jeftine provjere** (autor postoji u committeeju za `block.round`, round u scheduleu, inače Ignore), **tek onda** BLAKE3 digest (`:124`) i merkle, pa `verify_strict` (`:139`).
- `kvnc-network/src/validation.rs:198-199` `GossipValidator::verify_block` – `Arc<RwLock<EpochSchedule>>` ubrizgan iz nodea.
- `kvnc-network/src/service.rs:852-853` – **sync put** (odgovor na `ByHash`) zove `verify_block`; `:953` gossip.
- `kvnc-network/src/service.rs:1035` – `self.mempool.signing_context()` za glasove → schedule s ruba (ukloniti).
- `kvnc-network/src/service.rs:495` `process_block` (traženje roditelja) – bez promjene (digest isti).
- `kvnc-network/src/validation.rs:235, 317-364` testovi; **novi test:** blok s `round != 0` i potpisom `0×64` se odbija.

**Node – kvnc-node**
- `kvnc-node/src/main.rs:987` `verify_batch(&[block])` – **preporuka: ukloniti** (treća, redundantna provjera; isti blok već provjeravaju gossip/sync rub i `validate_block`). Odluka i vlasništvo: **Network**.
- `kvnc-node/src/main.rs` – gradi `EpochSchedule` iz genesisa i trajnog stanja, dijeli ga kao `Arc<RwLock<_>>` gossip rubu.
- `kvnc-node/src/main.rs:203, 863` genesis digest – bez promjene (genesis nepotpisan, validira se kanonski).
- `kvnc-node/src/main.rs:1268-1281` wrapper; testovi `:2020-2365`.

**Execution – DoubleSignProof (novi call site)**
- Danas `kvnc-staking/src/lib.rs:287` `DoubleSignEvidence { validator, height }` nema potpise (skeleton), a `kvnc-execution/src/lib.rs` `apply_double_sign_evidence` ih ne provjerava. Kad se uvede DoubleSignProof (dva potpisana headera istog `(author, round)` s različitim digestom): oba potpisa **MORAJU** proći `verify_strict` nad `signature_data(signing_ctx_for_round(schedule_na_visini_sub_dag_a, chain_id, offence_round))`, ključ `author_key(offence_round, author)`. Round izvan schedulea → deterministički neuspjeh (receipt failed, fee+nonce naplaćeni). Negativni vektor: N4.

**Ostalo (samo digest, bez promjene):** `kvnc-storage/src/block_store.rs:60`, `kvnc-consensus/src/linearizer.rs:160`, `kvnc-execution/src/lib.rs:1049,1073`.

## Migracija
- **Tvrdi prijelaz, jedan release** (kao v1): paralelne grane Foundation/Consensus/Network, merge redom types/crypto → dag/consensus → network/node; međustanja crvena.
- Stari potpisi (nad samim digestom) se odbijaju bez fallbacka; nema dual-verify.
- Digest nepromijenjen → DAG store je formatski kompatibilan, ali postojeći blokovi nose stare potpise ⇒ **reset devneta/testneta** (wipe DB, novi genesis, `chain_id` iz registra obavezan).
- Docs: nakon zamrzavanja ovaj dokument postaje FINAL; `docs/SIGNATURE_FORMAT.md` dobiva poveznicu.
