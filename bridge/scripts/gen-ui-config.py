#!/usr/bin/env python3
"""Generate the files a Hyperlane Warp UI (and a hyperlane-registry PR) need
for this route, from the deploy outputs. Writes into bridge/ui/:

  chains/ultrachain/metadata.yaml    registry chain entry
  chains/ultrachain/addresses.yaml   registry core addresses
  warp-route-config.yaml             WarpCoreConfig for the USDC route

Formats follow hyperlane-registry (e.g. deployments/warp_routes/USDT/
solanamainnet-sonicsvm-config.yaml). Stdlib only; YAML is written by hand.
"""
import importlib.util
import json
import pathlib

BRIDGE = pathlib.Path(__file__).resolve().parent.parent
ENV = BRIDGE / "environments/mainnet"
OUT = BRIDGE / "ui"
USDC = "EPjFWdd5AufqSSqeM2qN1xDybapC8G4wEGGkZwyTDt1v"

_spec = importlib.util.spec_from_file_location("cs", BRIDGE / "scripts/check-supply.py")
cs = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cs)


def main() -> None:
    core = json.load(open(ENV / "ultrachain/core/program-ids.json"))
    warp = json.load(open(ENV / "warp-routes/USDC-solanamainnet-ultrachain/program-ids.json"))
    sol, ultra = warp["solanamainnet"]["base58"], warp["ultrachain"]["base58"]
    rpc = cs.rpc_from_registry("ultrachain")

    chain = OUT / "chains/ultrachain"
    chain.mkdir(parents=True, exist_ok=True)
    (chain / "metadata.yaml").write_text(f"""# yaml-language-server: $schema=../schema.json
# TODO: set the real native token, explorer and block time.
blocks:
  confirmations: 1
  estimateBlockTime: 0.4
  reorgPeriod: 0
chainId: 1970041970
displayName: Ultrachain
domainId: 1970041970
name: ultrachain
nativeToken:
  decimals: 9
  name: Ultra
  symbol: ULTRA
protocol: sealevel
rpcUrls:
  - http: {rpc}
technicalStack: other
""")
    (chain / "addresses.yaml").write_text(f"""interchainGasPaymaster: {core["igp_account"]}
interchainSecurityModule: {core["multisig_ism_message_id"]}
mailbox: {core["mailbox"]}
merkleTreeHook: {core["mailbox"]}
validatorAnnounce: {core["validator_announce"]}
""")
    (OUT / "warp-route-config.yaml").write_text(f"""# yaml-language-server: $schema=../schema.json
tokens:
  - addressOrDenom: {sol}
    chainName: solanamainnet
    coinGeckoId: usd-coin
    collateralAddressOrDenom: {USDC}
    connections:
      - token: sealevel|ultrachain|{ultra}
    decimals: 6
    name: USD Coin
    standard: SealevelHypCollateral
    symbol: USDC
  - addressOrDenom: {ultra}
    chainName: ultrachain
    collateralAddressOrDenom: {cs.mint_address(ultra)}
    connections:
      - token: sealevel|solanamainnet|{sol}
    decimals: 6
    name: USD Coin
    standard: SealevelHypSynthetic
    symbol: USDC
""")
    print(f"wrote {OUT}/chains/ultrachain/*.yaml and {OUT}/warp-route-config.yaml")


if __name__ == "__main__":
    main()
