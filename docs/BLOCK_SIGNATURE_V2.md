# KUNA Block Signature (NACRT v2, `KUNA/block/v1`)

Status: **DRAFT**, čeka zajedničko zamrzavanje Foundation/Consensus/Network u KVNC-Execs (SOP). Ništa se ne implementira prije zamrzavanja. Baza: `main` `8170a82`. Vlasnik: Exec-Foundation (`kvnc-types`/`kvnc-crypto`). Zamrzavanje: Foundation + Consensus + Network (SOP). Konvencije (LE, `DomainTag16`, Ed25519 `ed25519-dalek` 2.x, BLAKE3 `=1.5.1` keyed) su iste kao u `docs/SIGNATURE_FORMAT.md`.

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
Nema length prefiksa za `parents`/`txs`; parsiranje je jednoznačno jer je merkle fiksnih 32 B, a broj tx-ova nije u digestu (napomena: preimage nije samorazgraničen, ali je digest kolizijski siguran uz fiksne zapise i ne treba se parsirati). `statements` i `signature` NISU u digestu.
Potpis danas: `Ed25519.sign(sk, digest)` (32 B) – bez chain_id/epohe/taga (`kvnc-dag/src/block_manager.rs:277-278`).

## v2: signing preimage (88 B, potpisuje se izravno, bez prehasha)

| Offset | Size | Polje      | Kodiranje |
|-------:|-----:|------------|-----------|
| 0  | 16 | domain tag | `DomainTag16("KUNA/block/v1")` (13 B + 3×0x00) |
| 16 | 8  | chain_id   | u64 LE (registar: 1/2/3/1337) |
| 24 | 8  | epoch      | u64 LE (epoha committeeja; `0` dok epohe nisu žive) |
| 32 | 32 | digest     | `compute_digest(author, round, parents, txs)`, **nepromijenjen** (gore) |

`signature = Ed25519.sign(sk_author, preimage)`; verifikacija **`verify_strict`** (kao glasovi).
Verifier gradi preimage iz **vlastitog** `SigningContext { chain_id, epoch }`, nikad iz poruke, i najprije sam ponovno računa `digest` (i merkle) iz sadržaja bloka.

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

### Otvoreno za zamrzavanje
- **Epoha bloka:** blok se potpisuje i provjerava s epohom committeeja za `block.round`, ne s „trenutnom lokalnom” epohom. To zahtijeva **mapiranje round → epoha od Consensusa** u trenutku rotacije (dio dizajna rotacije epoha, točka 7). **Do rotacije je epoha 0 svugdje.**
- **Kontekst na gossip rubu (Network):** `GossipValidator::verify_block` dobiva `SigningContext` **ubrizgan iz nodea** (iz `chain_id` configa provjerenog prema genesisu, epoha od Consensusa), **ne** preko `mempool.signing_context()`.
Genesis (round 0) se ne potpisuje (signature = 0×64) i validira se kanonski, kao danas.

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
**`verify_batch` MORA koristiti identičan 88-bajtni preimage kao pojedinačna provjera** (i u fallbacku); nema zasebnog batch formata.

## Call-site checklist (8170a82, putanje od `crates/`)
**Foundation – kvnc-types / kvnc-crypto**
- `kvnc-types/src/signing.rs:30` – dodati `BLOCK_DOMAIN_TAG`; test paddinga.
- `kvnc-types/src/block.rs:43` `compute_digest` – **bez promjene**; dodati `signature_data(ctx)` + golden test.
- `kvnc-types/src/hash.rs:30` `DOMAIN_BLOCK` – bez promjene.
- `kvnc-crypto/src/lib.rs:54` `sign` / `:61` `verify` – generički, ostaju; doc „usually a block digest” ispraviti.
- `kvnc-crypto/src/lib.rs:73` `verify_block_signature(pk, digest, sig)` – nova signatura s `ctx`, `verify_strict`.
- `kvnc-crypto/src/lib.rs:127-165` `verify_batch` – poruke `b.digest` (`:149`, fallback `:161`) → `signature_data(ctx)`; dodati `ctx` parametar.
- `kvnc-crypto/src/lib.rs:190-191` test (potpis nad digestom), `kvnc-crypto/tests/audit_31_hot_path.rs:12,24,31`.

**Consensus / DAG – kvnc-dag, kvnc-consensus**
- `kvnc-dag/src/block_manager.rs:276-278` `sign_block` – potpisuje `block.digest` → `sign_block(sk, ctx, digest)`; BlockManager treba `SigningContext` (sada ga nema; engine ga ima, `kvnc-consensus/src/engine.rs:~555`).
- `kvnc-dag/src/block_manager.rs:283` `validate_block` → `:303` `validate_block_content` (`:315` digest, ostaje) → `:343` `verify_block_signature` – dodati ctx.
- `kvnc-dag/src/block_manager.rs:487` `process_block` (zove validate) – bez promjene osim ctx-a.
- `kvnc-dag/src/block_manager.rs` testovi `:760-930` (ručno potpisani blokovi `sign(digest)` na 765/778/791/855/909/926) – potpisati preimage.
- `kvnc-consensus/src/engine.rs:437` `propose_block` / `:565` `process_block` – prosljeđuju ctx BlockManageru.
- `kvnc-consensus/src/engine.rs:1024, 1132` (testovi, mock verify) – nova API.
- `kvnc-consensus/tests/common/mod.rs:452` (mock potpis bloka) – provjeriti.

**Network – kvnc-network**
- `kvnc-network/src/validation.rs:123` `verify_block` (`:124` digest ostaje, `:139` potpis) – `GossipValidator::verify_block` (`:198-199`) dobiva ctx ubrizgan iz nodea, ne preko `mempool.signing_context()`.
- `kvnc-network/src/service.rs:853, 953` – pozivi `verify_block` (gossip rub).
- `kvnc-network/src/service.rs:495` `process_block` (sync roditelja) – samo `ByHash(parent.digest)`; **bez promjene** jer digest ostaje.
- `kvnc-network/src/validation.rs:235, 317-364` testovi (`signed_block`) – potpisati preimage.

**Node – kvnc-node**
- `kvnc-node/src/main.rs:987` `verify_batch(&[block])` – dodati ctx (iz `chain_id` configa).
- `kvnc-node/src/main.rs:203, 863` genesis digest – bez promjene (genesis nepotpisan).
- `kvnc-node/src/main.rs:1268-1281` `NodeBlockManager` wrapper – prosljeđuje ctx.
- `kvnc-node/src/main.rs:2020-2365` testovi – oni koji potpisuju blok.

**Ostalo (samo digest, bez promjene):** `kvnc-storage/src/block_store.rs:60`, `kvnc-consensus/src/linearizer.rs:160`, `kvnc-execution/src/lib.rs:1049,1073`, `kvnc-dag/tests/{pruning,mergeset}.rs:20`.

## Migracija
- **Tvrdi prijelaz, jedan release** (kao v1): paralelne grane Foundation/Consensus/Network, merge redom types/crypto → dag/consensus → network/node; međustanja crvena.
- Stari potpisi (nad samim digestom) se odbijaju bez fallbacka; nema dual-verify.
- Digest nepromijenjen → DAG store je formatski kompatibilan, ali postojeći blokovi nose stare potpise ⇒ **reset devneta/testneta** (wipe DB, novi genesis, `chain_id` iz registra obavezan).
- Docs: nakon zamrzavanja ovaj dokument postaje FINAL; `docs/SIGNATURE_FORMAT.md` dobiva poveznicu.
