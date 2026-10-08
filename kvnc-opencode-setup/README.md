# kvnc ↔ OpenCode Integration Pack

Pripremljeni paket da **OpenCode** i **Grok** rade zajedno na **kvnc**-u bez miješanja poslova.

## Sadržaj

```
kvnc-opencode-setup/
├── AGENTS.md                          ← stavi u root kvnc repo-a
├── opencode.json                      ← project config
├── .opencode/agents/kvnc.md           ← specijalni agent
├── prompts/kvnc-system.txt            ← alternativni prompt
├── docs/SETUP-OPENCODE-KVNC.md        ← detaljne upute
└── README.md                          ← ovaj file
```

## Brza instalacija

1. Kopiraj cijeli sadržaj u **root** svog lokalnog kvnc klona.
2. Pokreni `opencode` unutar tog root-a.
3. Prebaci se na agenta **kvnc** (Tab ili `@kvnc`).

Detaljne upute: `docs/SETUP-OPENCODE-KVNC.md`

## Podjela uloga

| Tko          | Uloga                                      |
|--------------|--------------------------------------------|
| **Grok**     | Arhitekt, planer, reviewer, tokenomics, dizajn |
| **OpenCode (kvnc agent)** | Lokalna implementacija, file edit, testovi, git |

## Status pretplate

- Grok pretplata: još nije aktivna (prema tvojoj izjavi)
- OpenCode agent: potpuno spreman za korištenje odmah

Kad aktiviraš Grok pretplatu, workflow postaje:
**Grok plan → OpenCode implementacija → Grok review**.

---

*Generirano za kvnc projekt – ne miješati s kovanica-protocol.*
