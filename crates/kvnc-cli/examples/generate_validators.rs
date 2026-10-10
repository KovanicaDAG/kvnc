#!/usr/bin/env rust
//! Generate 4 validator keystores and genesis_validators.toml for Phase 16.6

use std::fs;
use std::path::Path;

use kvnc_cli::wallet;

const PASSPHRASE: &str = "devnet-phase16.6";
const OUTDIR: &str = "/tmp/kvnc-validators";

fn main() -> anyhow::Result<()> {
    fs::create_dir_all(OUTDIR)?;

    let mut validators = Vec::new();

    for i in 1..=4 {
        println!("Generating validator {i}...");
        let keystore = wallet::keygen(PASSPHRASE)?;

        let keystore_path = format!("{OUTDIR}/validator{i}.keystore");
        wallet::save_replace(Path::new(&keystore_path), &keystore)?;

        // Export raw seed
        let seed = wallet::secret_seed(&keystore, Some(PASSPHRASE))?;
        let seed_path = format!("{OUTDIR}/validator{i}.pem");
        fs::write(&seed_path, hex::encode(&seed))?;

        let address = wallet::address(&keystore)?;
        let public_key = wallet::public_key(&keystore)?;
        let public_key_hex = hex::encode(public_key.as_bytes());

        println!("  Address: {}", address);
        println!("  Public key: {}", public_key_hex);
        println!("  Seed: {}", hex::encode(&seed));

        validators.push((address, public_key_hex, seed));

        println!("  Saved to {}", keystore_path);
    }

    // Generate genesis_validators.toml with [[validator]] array-of-tables format
    let mut toml = String::from("# Phase 16.6 — 4-node validator set for genesis\n");
    toml.push_str("# Stakes = MIN_VALIDATOR_STAKE = 50_000 KUNA (50_000_000_000_000 atoms)\n");
    toml.push_str("# Generated from keystores\n\n");

    for (_address, public_key_hex, _seed) in &validators {
        toml.push_str("[[validator]]\n");
        // Use raw public key hex (64 chars) as address - parse_address_hex expects 32-byte hex
        toml.push_str(&format!("address = \"{public_key_hex}\"\n"));
        toml.push_str("stake = 50000000000000\n");
        toml.push_str(&format!("public_key = \"{public_key_hex}\"\n\n"));
    }

    let toml_path = format!("{OUTDIR}/genesis_validators.toml");
    fs::write(&toml_path, &toml)?;
    println!("Generated {toml_path}");

    // Also copy to ops/docker/validators/ for docker-compose
    let docker_toml_path = "/root/Projects/kvnc/ops/docker/validators/genesis_validators.toml";
    fs::write(docker_toml_path, &toml)?;
    println!("Updated {docker_toml_path}");

    // Copy pem files to docker validators dir
    for i in 1..=4 {
        let src = format!("{OUTDIR}/validator{i}.pem");
        let dst = format!("/root/Projects/kvnc/ops/docker/validators/val{i}.pem");
        fs::copy(&src, &dst)?;
        println!("Copied {src} -> {dst}");
    }

    println!("\nAll 4 validator keys generated successfully!");
    Ok(())
}
