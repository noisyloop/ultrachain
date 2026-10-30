# Ultrachain ↔ Solana bridge (Hyperlane)

This directory deploys a [Hyperlane](https://hyperlane.xyz) **USDC warp route** between Ultrachain and Solana mainnet:

- **Solana → Ultrachain:** USDC is locked in a collateral program on Solana, and the same amount of synthetic USDC is minted on Ultrachain.
- **Ultrachain → Solana:** the synthetic USDC is burned, and the locked USDC is released on Solana.

Hyperlane's deploy tooling is the Rust `hyperlane-sealevel-client`. Every command and file format here was checked against the source of [hyperlane-monorepo](https://github.com/hyperlane-xyz/hyperlane-monorepo) at commit `18c39da` (30 Oct 2026). That commit is pinned in `scripts/deploy.sh`. Nothing here has been run against a live chain yet.

## How it fits together

```
           Solana mainnet                                  Ultrachain
 ┌──────────────────────────────┐              ┌──────────────────────────────┐
 │ Hyperlane Mailbox (existing) │              │ Mailbox            (you)     │
 │ Warp route: USDC collateral  │◀── relayer ─▶│ Warp route: USDC synthetic   │
 │ ISM: trusts YOUR validators  │   (you run)  │ ISM: trusts Solana validators│
 └──────────────────────────────┘              │ IGP, ValidatorAnnounce       │
                                               └──────────────────────────────┘
        ▲ Hyperlane's validators sign              ▲ YOUR validators sign
          Solana checkpoints                         Ultrachain checkpoints
```

| Direction | Who secures it |
|---|---|
| Solana → Ultrachain | Hyperlane's 4 Solana mainnet validators, 3 must sign (`config/ultrachain-multisig-config.json`). |
| Ultrachain → Solana | **Your** validators (`config/solana-multisig-config.json`). |

**The second row is the one that matters.** Anyone who controls a threshold of your validator keys can release every USDC locked on Solana. Most of the large bridge hacks were exactly this. Treat these keys like the treasury:
- For production, run at least 3 validators with a threshold of 2.
- Put them on separate machines, ideally with separate operators.
- Use KMS or HSM-backed keys where you can.

## Ultrachain requirements

The Hyperlane programs assume a standard Solana runtime. Before deploying, check that Ultrachain has all of the following:

- **Program deployment:** SBF program deployment with the upgradeable BPF loader. The programs are built with Solana CLI 3.0.14.
- **Token programs:** SPL Token, Token-2022 and Associated Token Account at their canonical addresses. The synthetic token is a Token-2022 mint.
- **Runtime features:** the Compute Budget program, and v0 transactions with address lookup tables.
- **RPC methods:** a standard Solana JSON-RPC. The agents call `getSignaturesForAddress`, `getTransaction` and `getProgramAccounts`.
- **Chain ID:** a fixed Hyperlane domain ID. This uses `1970041970` (ASCII "ultr"). No chain in Hyperlane's registry used it as of this commit. **Never change it after deployment.**

If Ultrachain changed any of these, find out on a local Ultrachain validator first.

## Keys you need

| Key | Type | Funded with | Used for |
|---|---|---|---|
| Deployer | Solana keypair | SOL **and** Ultrachain gas | Deploying everything. It becomes the owner and upgrade authority, so move both to a multisig afterwards. |
| Validator signer (×N) | secp256k1 (EVM-style) | nothing | Signing Ultrachain checkpoints. Its `0x…` address goes in `config/solana-multisig-config.json`. |
| Validator chain signer (×N) | Solana keypair | a little Ultrachain gas | A one-time self-announce on Ultrachain. |
| Relayer | Solana keypair(s) | SOL and Ultrachain gas | Delivering messages in both directions. |

Agents take Solana keypairs as hex. `scripts/keypair-to-hex.py key.json` converts one.

## Deploy

Do the whole flow on a test setup first: an Ultrachain testnet, with Solana testnet (domain `1399811150`) in place of mainnet. That means swapping the Solana entries for Hyperlane's testnet ones. Once the full round trip works there, do it again on mainnet.

```sh
export DEPLOYER_KEYPAIR=~/keys/deployer.json
export SOLANA_RPC_URL=https://<paid-solana-rpc>   # the public RPC throttles program uploads
cd bridge
```

0. **Fill in placeholders.** Set the Ultrachain RPC in `registry/chains/metadata.yaml`. Each stage refuses to run while a file it reads still contains `REPLACE_ME`.
1. **Build.** Run `scripts/deploy.sh build`. This clones the monorepo at the pinned commit, builds the Sealevel programs (it installs the pinned Solana toolchain) and builds the client.
2. **Ultrachain core.** Run `scripts/deploy.sh core`. This deploys the Mailbox, the multisig ISM (set as the Mailbox default), the IGP and ValidatorAnnounce. It writes the program IDs to `environments/mainnet/ultrachain/core/program-ids.json`.
3. **Trust Solana on Ultrachain.** Run `scripts/deploy.sh ism-ultrachain`. This loads Hyperlane's Solana validator set into Ultrachain's ISM.
   - That set was copied from the monorepo at the pinned commit. Check it against Hyperlane's registry before mainnet, and update it whenever Hyperlane rotates validators.
4. **Start your validators.** Generate the validator keys and put their addresses in `config/solana-multisig-config.json`. Then start each validator with the `validator` service in `agents/docker-compose.yml`.
   - Each validator needs an S3 bucket that anyone can read.
   - They need `agent-config.json`, so run step 7 once with a dummy `SOLANA_INDEX_FROM` (the validator only reads the Ultrachain entry), or start them after step 7.
5. **Trust your validators on Solana.** Run `scripts/deploy.sh ism-solana`. This deploys a multisig ISM program on Solana that only trusts your Ultrachain validators.
   - Put the program ID it prints into `solanamainnet.interchainSecurityModule` in the token config.
6. **Warp route.** Set `owner` and `uri` in `environments/mainnet/warp-routes/USDC-solanamainnet-ultrachain/token-config.json`. The `uri` points to the token's metadata JSON. Then run `scripts/deploy.sh warp`.
   - This deploys the collateral program on Solana and the synthetic program on Ultrachain, then enrolls each as the other's router.
7. **Agent config.** Get Solana's current mailbox nonce:
   ```sh
   .build/hyperlane-monorepo/rust/sealevel/target/release/hyperlane-sealevel-client \
     -u "$SOLANA_RPC_URL" mailbox query --program-id E588QtVUvresuXq2KoNEwAmoifCzYGpRBdHByN9KQMbi
   ```
   Then run `SOLANA_INDEX_FROM=<outbox count> scripts/deploy.sh agent-config`.
   - This writes `agents/agent-config.json` and `agents/relayer-whitelist.json`.
   - Without `SOLANA_INDEX_FROM`, the relayer would index every message Solana has ever dispatched.
8. **Relayer.** Run `cd agents && cp .env.example .env`, fill it in, then `docker compose up -d`.
   - The relayer only relays your warp route (the whitelist) and doesn't require gas payments. It's your relayer, paying for your own route.
9. **Test.** Send a small amount and watch the relayer logs. Then send it back from Ultrachain.
   ```sh
   scripts/deploy.sh test-transfer ~/keys/sender.json 1000000 <ultrachain-recipient-pubkey>
   ```
   `1000000` base units is 1 USDC.

## Before mainnet value goes through it

- **Move ownership to a multisig.** Transfer ownership and upgrade authority of every program (ISMs, both warp routes, the Ultrachain core) from the deployer key to a multisig. The client has `token transfer-ownership`, `multisig-ism-message-id transfer-ownership`, `mailbox transfer-ownership` and `squads` subcommands.
- **Add a rate limit.** The Sealevel composite ISM (`rust/sealevel/programs/ism/composite-ism`) supports a `RateLimited` node: a 24-hour token bucket. Wrapping the Solana-side ISM in one caps how much can be drained if your validators are ever compromised. This isn't set up here; it's the next hardening step.
- **Monitor everything.** That means:
  - validator checkpoint lag;
  - relayer queue length and signer balances (an empty signer stops deliveries);
  - USDC locked on Solana compared with synthetic supply on Ultrachain (these must match).
- **Budget for Solana rent.** Two new Solana programs (the ISM and the collateral route) need rent for roughly 0.5 MB programs, which can be several SOL. The deployer also funds ATA payers (`--ata-payer-funding-amount`).
- **Back up the program keypairs.** The keypairs written under `environments/**/keys/` are git-ignored here. Back them up if you might redeploy.

## Relation to the trading bot

The bot (`../src`) trades Solana liquidity through Jupiter. This route is how USDC held on Ultrachain would get to Solana, and back, for that. The bot is still paper-only and doesn't call the bridge.
