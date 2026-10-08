# Setup: OpenCode + kvnc (specijalni agent)

Ovaj paket priprema OpenCode da radi **samo** na kvnc-u i ne miješa se s drugim projektima (posebno kovanica-protocol).

## Brzi start (5 minuta)

### 1. Kopiraj datoteke u svoj kvnc repo root

```bash
# Pretpostavka: nalaziš se u root-u svog lokalnog kvnc klona
cp /path/to/kvnc-opencode-setup/AGENTS.md .
mkdir -p .opencode/agents
cp /path/to/kvnc-opencode-setup/.opencode/agents/kvnc.md .opencode/agents/
cp /path/to/kvnc-opencode-setup/opencode.json .
# opcionalno
mkdir -p prompts
cp /path/to/kvnc-opencode-setup/prompts/kvnc-system.txt prompts/
```

### 2. Instaliraj OpenCode (ako još nemaš)

```bash
curl -fsSL https://opencode.ai/install | bash
# ili
npm install -g opencode-ai
```

### 3. Pokreni u kvnc root-u

```bash
cd /path/to/your/kvnc
opencode
```

Unutar OpenCode-a:
- `/init` (ako želiš da regenerira AGENTS.md – ali mi smo već stavili naš)
- Prebaci se na **kvnc** agenta (Tab ili @kvnc)
- Za planiranje bez izmjena: koristi **kvnc-plan**

### 4. Preporučeni workflow s Grok-om

```
Ti → Grok:   "Želim unbond period u staking crate-u, 21 dan"
Grok → Ti:   detaljan plan + interfejsi + edge caseovi + testovi
Ti → OpenCode (kvnc agent):  zalijepi plan i reci "implementiraj ovo"
OpenCode radi file edit + cargo test
Ti → Grok:   zalijepi diff / test output
Grok radi review + eventualne korekcije
```

## Što je u paketu

| Datoteka                          | Svrha                                      |
|-----------------------------------|--------------------------------------------|
| `AGENTS.md`                       | Glavna uputstva za sve agente u projektu   |
| `.opencode/agents/kvnc.md`        | Specijalni primary agent (stroga izolacija)|
| `opencode.json`                   | Konfiguracija agenata + permissions        |
| `prompts/kvnc-system.txt`         | Alternativni system prompt                 |
| `docs/SETUP-OPENCODE-KVNC.md`     | Ovaj vodič                                 |

## Permissions (sigurnost)

- `kvnc` agent: edit + bash **allow**, ali `external_directory: deny` (ne može dirati fileove izvan worktree-a)
- `kvnc-plan` agent: edit **deny**, bash **ask** (siguran za planiranje)

## Multi-session preporuka

U OpenCode-u pokreni odvojene sessione:
- Session "kvnc-network"
- Session "kvnc-staking"
- Session "drugi-projekt" (nikad ne miješaj)

Tako kontekst ostaje čist.

## Grok pretplata

Trenutno još nisi napravio pretplatu na Grok.  
Kad je aktiviraš, možeš koristiti ovaj workflow punom snagom (arhitektura + review + planiranje).

Do tada možeš koristiti OpenCode samostalno s ovim agentom – on je već dovoljno specijaliziran.

## Ako nešto zapne

- Provjeri da si u root-u kvnc-a (gdje je Cargo.toml)
- Provjeri da `opencode.json` i `.opencode/agents/kvnc.md` postoje
- Restartaj OpenCode session
- Ako trebaš, reci Grok-u "regeneriraj AGENTS.md" ili "update kvnc agent"

Sretno s razvojem kvnc-a.
