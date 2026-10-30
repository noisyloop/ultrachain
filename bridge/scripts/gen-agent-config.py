#!/usr/bin/env python3
"""Write the Hyperlane agent config (chain metadata + deployed addresses) and
the relayer's whitelist from the deploy outputs.

Solana mainnet addresses are Hyperlane's own core deployment (copied from
hyperlane-monorepo rust/main/config/mainnet_config.json); only Ultrachain's
come from our `core` stage.
"""
import argparse
import json
import pathlib

ULTRACHAIN_DOMAIN = 1970041970
SOLANA_DOMAIN = 1399811149


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--core", required=True, help="ultrachain core program-ids.json")
    ap.add_argument("--warp", required=True, help="warp route program-ids.json")
    ap.add_argument("--ultrachain-rpc", required=True)
    ap.add_argument("--solana-rpc", required=True)
    ap.add_argument("--solana-index-from", type=int, required=True)
    ap.add_argument("--out-dir", required=True)
    a = ap.parse_args()

    core = json.load(open(a.core))
    out = pathlib.Path(a.out_dir)

    config = {
        "chains": {
            "ultrachain": {
                "name": "ultrachain",
                "displayName": "Ultrachain",
                "domainId": ULTRACHAIN_DOMAIN,
                "chainId": ULTRACHAIN_DOMAIN,
                "protocol": "sealevel",
                "rpcUrls": [{"http": a.ultrachain_rpc}],
                # TODO: set to Ultrachain's real native token.
                "nativeToken": {"name": "Ultra", "symbol": "ULTRA", "decimals": 9},
                "blocks": {"confirmations": 1, "reorgPeriod": 0, "estimateBlockTime": 0.4},
                # Fresh mailbox, so index from the first message.
                "index": {"from": 0, "mode": "sequence", "chunk": 20},
                "mailbox": core["mailbox"],
                # On Sealevel the merkle tree hook lives in the mailbox.
                "merkleTreeHook": core["mailbox"],
                "interchainGasPaymaster": core["igp_account"],
                "validatorAnnounce": core["validator_announce"],
                "interchainSecurityModule": core["multisig_ism_message_id"],
            },
            "solanamainnet": {
                "name": "solanamainnet",
                "displayName": "Solana",
                "domainId": SOLANA_DOMAIN,
                "chainId": SOLANA_DOMAIN,
                "protocol": "sealevel",
                "rpcUrls": [{"http": a.solana_rpc}],
                "nativeToken": {"name": "Solana", "symbol": "SOL", "decimals": 9, "denom": "lamport"},
                "blocks": {"confirmations": 1, "reorgPeriod": 0, "estimateBlockTime": 0.4},
                "index": {"from": a.solana_index_from, "mode": "sequence", "chunk": 20},
                "maxSupportedTransactionVersion": 1,
                "mailbox": "E588QtVUvresuXq2KoNEwAmoifCzYGpRBdHByN9KQMbi",
                "merkleTreeHook": "E588QtVUvresuXq2KoNEwAmoifCzYGpRBdHByN9KQMbi",
                "interchainGasPaymaster": "JAvHW21tYXE9dtdG83DReqU2b4LUexFuCbtJT5tF8X6M",
                "validatorAnnounce": "pRgs5vN4Pj7WvFbxf6QDHizo2njq2uksqEUbaSghVA8",
                "interchainSecurityModule": "CaLiNiZpuWz9ddBqthAdX22QMRELEiw2pm1UQnsRnC91",
                "mailboxProcessAlt": "5iPyGCTQ2xHaCxv9A8GDJzt2tHWL8t9FK8UwG3KoQsYo",
            },
        }
    }
    (out / "agent-config.json").write_text(json.dumps(config, indent=2) + "\n")

    # Only relay our own warp route. Without this, a relayer with gas payment
    # enforcement off would pay to deliver anyone's messages on these chains.
    whitelist = []
    warp_path = pathlib.Path(a.warp)
    if warp_path.exists():
        warp = json.load(open(warp_path))
        routes = {
            "ultrachain": (ULTRACHAIN_DOMAIN, warp["ultrachain"]["hex"]),
            "solanamainnet": (SOLANA_DOMAIN, warp["solanamainnet"]["hex"]),
        }
        for src, (src_domain, src_router) in routes.items():
            for dst, (dst_domain, dst_router) in routes.items():
                if src != dst:
                    # Lowercase keys: the relayer deserializes these verbatim.
                    whitelist.append({
                        "origindomain": src_domain,
                        "senderaddress": src_router,
                        "destinationdomain": dst_domain,
                        "recipientaddress": dst_router,
                    })
        (out / "relayer-whitelist.json").write_text(json.dumps(whitelist) + "\n")
    else:
        print(f"note: {warp_path} not found; run the 'warp' stage, then re-run this to write the relayer whitelist")

    print(f"wrote {out / 'agent-config.json'}" + (f" and {out / 'relayer-whitelist.json'}" if whitelist else ""))


if __name__ == "__main__":
    main()
