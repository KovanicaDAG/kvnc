# Poziv za eksternu reviziju — kvnc (f2b05bf / 025ee12)

Status: **interne verifikacija završena** (`AUDIT_REPORT.md`, `docs/AUDIT-CHECKLIST.md`); eksterni audit **nije izvršen** — potreban za bilo kakvu glavnu mrežu (mainnet) ili izdanje.

Identitet projekta: **kvnc** (nezavisni Rust Layer-1, od-scratch; NE kovanica-protocol, NE stariji Kovanica repo).
Koordinat: `main` branch (`025ee12`, `4f3100f`, `76e52db`, `319427e`).
Datum poziva: 2026-10-09.

---

## Što je već provjereno (interno — PASS s napomenama)

Referiraj `AUDIT_REPORT.md` (§2, §3, §4) i `docs/AUDIT-CHECKLIST.md`.

| Područje | Status | Ključni dokaz (file / redak) |
|---|---|---|
| Tokenomics / cap | PASS | `docs/GOVERNANCE.md`; `crates/kvnc-staking/src/lib.rs:68` (`MIN_VALIDATOR_STAKE`); `engine.rs` (`leader_timeout_ms: 3000`, era 2 050 000) |
| Konsenzus (Mysticeti, uncertified, wave=3, k=3) | PASS (struktura) | `crates/kvnc-consensus/src/engine.rs`; `crates/kvnc-dag/src/dag_store.rs` |
| Izvođenje (Wasmi, determinističko) | PASS (kostur) | `docs/EXECUTION-GAS-SKELETON.md`; `crates/kvnc-execution/src/lib.rs` — **potrebna dublja revizija plin/metrike** |
| Staking / valjani skup | PASS | `docs/A8.2-DELEGATION-SKELETON.md`; `crates/kvnc-staking/src/lib.rs` (`bond`/`unbond`/`commission_bps`/`UNBONDING_ROUNDS`) |
| Kripto (Ed25519, 64-byte sig, 128 hex) | PASS | `kvnc-crypto` — **potreban formalni audit kripto korektnosti** |
| P2P / mreža (plaintext TCP, gossip) | PASS (struktura) | `crates/kvnc-network/src/service.rs` — **potrebna revizija otpornosti na Sybil/eclipse** |
| CLI / novčanik / HTLC | PASS (kostur) | `crates/kvnc-cli/src/main.rs`; `docs/A8.2-DELEGATION-SKELETON.md` — delegate deferred Phase 18 |
| Env / sigurnost | PASS | `docs/SETUP-OPENCODE-KVNC.md`; `AUDIT-CHECKLIST.md` (§3); `ALLOW_RESET=0`, `MINE=0`, `OPERATOR=0` |
| Seed / DNS | **GAP — potvrđen** | `docs/SEED-DNS-AUDIT.md`; `/api/bootstrap` vratio `502` pri auditu; prethodni log pokazuje `listen 0.0.0.0:8000` (ne 9000); samo `seed.kovanica.online` |
| Live endpoint | **GAP — blokiran** | `https://explorer.kovanica.online/api/bootstrap` — 502; mora se ponoviti kad je dostupan |

---

## Što eksterni auditor mora provjeriti (prioriteti)

### P0 — konsenzus i sigurnost (blokira glavnu mrežu)
1. **Uncertified DAG (Mysticeti, wave=3)**: provjeriti `kvnc-consensus/src/engine.rs` i `kvnc-dag/src/block_manager.rs` za ispravnost `CommittedSubDag` pravila i odsutnost certifikata; potvrditi da nema regresije na `k=3`.
2. **WASMI deterministički izvršavanje**: `kvnc-execution/src/lib.rs` + `kvnc-runtime` — potvrditi da svaki ugovor (HTLC, Vault, Multisig, Token) daje identičan rezultat na istom ulazu; provjeriti plin (gas) metering (`docs/EXECUTION-GAS-SKELETON.md`).
3. **Tokenomics invarijante**: tvrdi cap 90 200 000 KVNC (`9_020_000_000_000_000` atoma); `subsidy / 500_000` fee floor; 75% burn / 25% producer; maturity 100 blokova (`docs/AUDIT-CHECKLIST.md` §2).
4. **Ed25519 kripto**: `kvnc-crypto` — formalni pregled (key derivation, address format, replay protection, signature verification). Ključevi moraju ostati isključivo klijentski (`node` ne prima `seed` ili `private_key`).

### P1 — P2P, mreža, otpornost (blokira testnet stabilnost)
5. **P2P (plaintext TCP)**: `kvnc-network/src/service.rs`; potvrditi da nema `libp2p` ovisnosti koje bi uvele nepredvidljivo ponašanje; provjeriti `seed.kovanica.online:9000` (ili potvrđeni port) i dodati `3+` redundanciju.
6. **Port 8000 vs 9000**: potvrditi živi port iz `/api/bootstrap`; uskladiti `docs/`, `env`, `AGENTS.md` i `operating` konfiguraciju (`ops/deploy/`).
7. **Eclipse / Sybil otpornost**: provjeriti `peer` ograničenja, `ping` timeout, `ban` pravila (`service.rs` linije 33–40); preporuka: dodati eksplicitnu `Sybil` / `eclipse` simulaciju.

### P2 — staking, upravljanje, operacija (blokira Phase 18 / 25)
8. **Staking ekonomija**: `crates/kvnc-staking/src/lib.rs` — `bond`/`unbond`/`commission`/`slash` pravila; potvrditi da `MIN_VALIDATOR_STAKE = 50_000 KVNC` i `UNBONDING_ROUNDS` ne dozvoljavaju napade na aktivni skup (15–21).
9. **Governance (Phase 25)**: `docs/GOVERNANCE.md` + `docs/B-TREASURY-PROPOSAL.md` — provjeriti da `TreasuryProposal` i `vote_on_proposal()` ne mijenjaju konsenzus pravila (trenutno `docs-only` / `client-only`); aktivacija tek nakon Phase 18.
10. **CLI delegate / claim**: `kvnc-cli/src/main.rs` (`delegate_skeleton`, `claim_rewards_skeleton`); potvrditi da `HTLC` claim (`A8.2-DELEGATION-SKELETON.md`) ne uvodi neodređenost.

---

## Što auditor treba dostaviti (izlaz)

- Pismeni izvještaj s klasifikacijom nalaza: `BLOCKER` / `CRITICAL` / `WARNING` / `PASS`.
- Za svaki `BLOCKER` / `CRITICAL`: konkretan redak (`file:line`), opis, predložena popravka, verifikacija popravka.
- Potvrda da `tokenomics` (90.2 M cap, decay, maturity, fee split) ne može biti narušena kodom ili konfiguracijom.
- Potvrda da `Wasmi` izvršavanje je determinističko za sve ugovore (`HTLC`, `Vault`, `Multisig`, `Token`).
- Potvrda da `P2P` ne sadrži `libp2p` ovisnost koja bi mogla uvesti nepredvidljivo ponašanje; ako postoji — dokumentirati i opravdati.
- Potvrda `DNS` / `seed` redundancije (`3+`) i usklađenosti porta (8000 / 9000 / potvrđeni).
- Potpis i datum; preporuka za `mainnet readiness` (da / ne / uvjetno s popisom uvjeta).

---

## Rok i kontakt

- Preporučeni rok revizije: **2 tjedna** nakon primanja ovog poziva.
- Materijal za pregled: ovaj repo (`kvnc`), `docs/AUDIT-CHECKLIST.md`, `AUDIT_REPORT.md`, `docs/GOVERNANCE.md`, `docs/B-TREASURY-PROPOSAL.md`, `docs/A8.2-DELEGATION-SKELETON.md`, `docs/EXECUTION-GAS-SKELETON.md`, `crates/kvnc-staking/src/lib.rs`, `crates/kvnc-consensus/src/engine.rs`, `crates/kvnc-execution/src/lib.rs`.
- Koordinacija: `kvnc-specialist` (interne); eksterni auditor mora biti neovisan (nema veze s `kovanica-protocol` ili starijim Kovanica repo-ima).

---

## Izjava (interno)

Ovaj poziv ne predstavlja angažman — samo poziv za reviziju. Eksterni auditor mora potpisati vlastiti izvještaj; `kvnc` projekt ne preuzima odgovornost za nalaze eksternog auditora dok isti nije formalno prihvaćen. Svi kodni i dizajn odluke ostaju unutar `kvnc` i konzistentni su s `AGENTS.md`.
