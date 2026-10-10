# kvnc-faucet

Rate-limited testnet faucet (`POST /faucet`, `GET /health`).

```
kvnc-faucet --keystore faucet.keystore --rpc-url http://127.0.0.1:8545 --listen 0.0.0.0:8080 \
            [--dispense-kvnc 10] [--rate-limit-window 3600] [--max-requests 3]
```

## Keystore passphrase

There is **no `--passphrase` flag**. Command-line arguments are visible in `ps` and shell history.
For an encrypted keystore, the passphrase is taken from the first of these that is available:

1. `--passphrase-file <path>`: the file's contents, with trailing `\n`/`\r\n` trimmed. On unix the file
   must be mode `0600` or `0400`; otherwise the faucet refuses to start.
2. env `KVNC_FAUCET_PASSPHRASE` (non-empty).
3. an interactive no-echo prompt, but only when stdin is a TTY.

If none is available, the faucet exits with an error. The passphrase is never logged. It is kept in
zeroizing memory and wiped right after the keystore is decrypted.

Docker/compose: mount a secret file (e.g. `/run/secrets/faucet_passphrase`, mode 0400) and pass
`--passphrase-file /run/secrets/faucet_passphrase`.
